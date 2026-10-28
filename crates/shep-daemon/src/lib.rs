//! The daemon: a process-supervision engine plus the control plane that
//! exposes it over a unix socket
//!
//! Every command that reaches the flock goes through one
//! [`SupervisorHandle`](supervisor::SupervisorHandle). Pure decision logic
//! (brain, backoff, entry assembly) is IO-free, so it tests under a paused
//! tokio clock. [`boot`] assembles the engine and the `RpcServer` into one
//! daemon. Three tiers, top down: the engine and its actor, the control
//! plane a CLI client talks to, and the platform glue under both.
//!
//! # Examples
//!
//! Builds a supervisor engine with a scripted fake runner, registers one app,
//! and lists the live processes. Needs `--all-features` (`test-fakes`).
//! ```no_run
//! # #[cfg(feature = "test-fakes")]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use shep_daemon::fake::{ProcScript, ScriptedRunner};
//! use shep_daemon::supervisor::spawn_supervisor;
//! use shep_core::config::AppConfig;
//! use shep_core::config::normalize;
//! use shep_core::paths::ShepPaths;
//! use std::path::Path;
//!
//! // Create a fake runner that spawns one process never exiting
//! let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
//!
//! // Set up temporary paths for this example
//! let paths = ShepPaths::resolve(&|_| None, Path::new("/tmp/shep-example"));
//!
//! // Create the event bus every subscriber reads
//! let events = shep_daemon::new_bus();
//!
//! // Spawn the supervisor actor
//! let handle = spawn_supervisor(runner, paths, events);
//!
//! // Build one app config and normalize it
//! let app = AppConfig::minimal("web", "./server");
//! let resolved = normalize(app)?;
//!
//! // Start the app (creates one instance)
//! let infos = handle.start(vec![resolved]).await?;
//! println!("Started: {} instance(s)", infos.len());
//!
//! // List all registered processes
//! let list = handle.list().await;
//! for info in &list {
//!     println!("  ID {} ({}): {:?}", info.id, info.name, info.status);
//! }
//!
//! // Gracefully shut down all processes
//! handle.shutdown().await;
//!
//! Ok(())
//! # }
//! # #[tokio::main]
//! # async fn main() {
//! #     #[cfg(feature = "test-fakes")]
//! #     example().await.ok();
//! # }
//! ```
//!
//! Boots a full daemon through [`boot`] on a temporary `$SHEP_HOME` with the
//! same scripted fake runner, then round-trips one `Ping` over the wire codec
//! `server` speaks. Needs `--all-features` on a unix target.
//! ```no_run
//! # #[cfg(all(unix, feature = "test-fakes"))]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use shep_daemon::boot::{BootOptions, boot};
//! use shep_daemon::fake::ScriptedRunner;
//! use shep_core::paths::ShepPaths;
//! use shep_core::protocol::{
//!     Envelope, Hello, HelloReply, PROTOCOL_VERSION, Request, ServerFrame, codec, decode_frame,
//!     encode_frame,
//! };
//! use tokio::net::UnixStream;
//! use tokio_util::codec::Framed;
//! use futures_util::{SinkExt, StreamExt};
//!
//! // A throwaway $SHEP_HOME: `boot` creates its 0700 layout inside it.
//! let paths = ShepPaths::resolve(&|_| None, std::path::Path::new("/tmp/shep-daemon-example"));
//!
//! // Boot with the scripted fake runner — no real children, just the plane.
//! let daemon = boot(ScriptedRunner::new(vec![]), paths, BootOptions::default()).await?;
//! let socket = daemon.socket().to_path_buf();
//! tokio::spawn(daemon.run());
//!
//! // Connect and speak the wire protocol directly: Hello, then Ping.
//! let stream = UnixStream::connect(&socket).await?;
//! let mut frames = Framed::new(stream, codec());
//! frames
//!     .send(encode_frame(&Hello {
//!         client_version: "0.1.0".to_string(),
//!         protocol: PROTOCOL_VERSION,
//!         // Only a dog names one; see `Hello::dog_name`.
//!         dog_name: None,
//!     })?)
//!     .await?;
//! let ack: HelloReply = decode_frame(&frames.next().await.unwrap()?)?;
//! let ack = ack.expect("the daemon must ack our protocol");
//! println!("daemon pid: {}", ack.pid);
//!
//! frames
//!     .send(encode_frame(&Envelope {
//!         id: 1,
//!         deadline_ms: Some(1_000),
//!         body: Request::Ping,
//!     })?)
//!     .await?;
//! let frame: ServerFrame = decode_frame(&frames.next().await.unwrap()?)?;
//! println!("reply: {frame:?}");
//!
//! Ok(())
//! # }
//! # #[tokio::main]
//! # async fn main() {
//! #     #[cfg(all(unix, feature = "test-fakes"))]
//! #     example().await.ok();
//! # }
//! ```

#![doc(test(attr(deny(warnings))))]
#![deny(unsafe_code)]

// Internal tier: nothing outside this crate's own `src` names these. A dog is
// a separate process speaking the protocol rather than linking this crate, so
// a dog author builds against `shep-core`. Widening one back to `pub` is an
// API decision.
pub(crate) mod backoff;
pub(crate) mod boot_order;
pub(crate) mod brain;
pub(crate) mod bus;
// The bus surface a caller of `supervisor::spawn_supervisor` needs, and no
// more: the module stays crate-private so its forwarders and topic
// bookkeeping never become API.
pub use bus::{Bus, SharedEvent, new_bus};
pub(crate) mod cron;
pub(crate) mod entry;
pub(crate) mod extras;
pub(crate) mod host;
// Unix only: `fcntl`, `execve` and raw descriptor numbers have no Windows
// equivalent, and `Arm::for_daemon` already returns the stop arm there.
#[cfg(unix)]
pub(crate) mod handover;
pub(crate) mod kill;
// Its reads are unix only, like both callers: a handover's adopted start
// times and the lamb sweep's liveness check. Its clock is everywhere.
pub(crate) mod proc_table;
// The provider values a dog has pushed, and the cache file they survive a
// restart in. Crate-private: a dog writes here over the socket, never by
// linking this crate.
pub(crate) mod secrets;
// Polls the operator's store for a value a running dog names.
pub(crate) mod secrets_watch;
pub(crate) mod sweep;
pub(crate) mod watch;

// Reachable tier: each of these is named from outside this crate's `src`, by
// `shep-cli`, an integration test, the bench crate, or a doc example. Every
// module here carries a note in its own header saying which consumer holds it
// open.
pub mod assemble;
pub mod channel;
// A `dogs::DogSpec` says which dogs to run and where their binaries come
// from, an answer only `shep.toml` holds, and every `shep.toml` read in this
// project happens in `shep-cli`. So the assembling caller is out of crate by
// construction.
pub mod dogs;
pub mod limits;
pub mod privilege;
pub mod probes;
pub mod rpc;
pub mod runner;
pub mod snapshot;
pub mod supervisor;

use std::time::SystemTime;

/// The one real-time read shared across this crate: wall-clock milliseconds
/// since the Unix epoch, for [`BusEvent::Process::at_ms`](shep_core::protocol::BusEvent::Process)
/// and [`FlockSnapshot::saved_at_ms`](snapshot::FlockSnapshot::saved_at_ms).
/// Everything else in the engine uses the paused-clock-aware
/// `tokio::time::Instant`.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
pub(crate) mod testing_http;

// Reachable tier: `shep-cli`'s hidden `daemon` subcommand calls `boot` and
// `RunningDaemon::run`, and `launch.rs` reads `DIR_MODE`. Portable on both
// platforms; `DIR_MODE` alone is gated unix-only. Doc lives inside boot.rs's
// own `//!` header, for the reason `server`'s note below gives.
pub mod boot;

// Unix only: `FromRawFd`/`RawFd` and this crate's whole unsafe surface there.
// `pub` for a consumer not written yet: `boot` does not call `adopt_fd`, so
// the ordering precondition needs a caller running before any runtime exists,
// `shep-cli`'s `main`. Doc lives inside sys.rs's own `//!` header.
#[cfg(unix)]
pub mod sys;

// The Windows counterpart to `sys`, and this crate's only unsafe surface
// there: the std-handle seal and the shepherd's pipe descriptor. `pub`
// because `seal_std_handles`' caller lives in shep-cli. A sheep's job object
// is `shep-tree`'s.
#[cfg(windows)]
pub mod sys_windows;

// Unix only: `UnixDatagram`, plus Linux's abstract-namespace address. Doc
// lives inside notify.rs's own `//!` header. `shep-cli`'s hidden `daemon`
// subcommand names `NOTIFY_SOCKET_ENV`, since every environment read happens
// there; this crate receives the resolved address instead.
#[cfg(unix)]
pub mod notify;

/// Real [`ProcessRunner`](runner::ProcessRunner) over actual OS processes.
///
/// Unix only: built on `nix` process-group signals and `command-fds` fd-3
/// passing, both `#[cfg(unix)]` deps. Public because `shep-cli` hands
/// [`TokioRunner`](tokio_runner::TokioRunner) to [`boot`], and
/// `tests/real_runner/` drives it against real children.
pub mod tokio_runner;

// Portable: it names `shep_core::transport`, so one accept loop and
// connection state machine serve both platforms; the same-uid `peer_cred`
// check is `#[cfg(unix)]` inside. Doc lives inside server.rs's own `//!`
// header: an outer `///` here would merge with it and misresolve its links.
pub(crate) mod server;

/// Deterministic scripted [`ProcessRunner`](runner::ProcessRunner), reused by
/// this crate's own tests and, behind `test-fakes`, by any other crate's.
///
/// Public because both doc examples above build one, and rustdoc compiles a
/// doc example as its own crate.
#[cfg(any(test, feature = "test-fakes"))]
pub mod fake;
