//! Real-daemon integration tier: boots shep-daemon on a temp `$SHEP_HOME`,
//! talks to it over the control socket with shep-core's own codec, and
//! drives real child processes.
//!
//! Real time throughout: a paused clock's auto-advance would expire timeouts
//! before IO wakeups arrive.

// Many cases here are `#[cfg(unix)]`, so on Windows those items are unreached.
#![cfg_attr(windows, allow(dead_code))]
// And so are the imports only those cases use.
#![cfg_attr(windows, allow(unused_imports))]

use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use shep_core::transport::{self, ClientStream};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use shep_core::config::{AppConfig, ProbeConfig, ProbeKind};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{
    ActionOutcome, BusEvent, ChildMessage, Envelope, Hello, HelloAck, HelloReply, LineOutcome,
    MIN_SUPPORTED, PROTOCOL_VERSION, ProcessEventKind, ProcessInfo, Reply, Request, Response,
    RpcError, RpcErrorCode, SelectorSpec, ServerFrame, codec, decode_frame, encode_frame,
};
use shep_core::status::ProcStatus;
use shep_core::values::UpDuration;

use shep_daemon::boot::{BootError, BootOptions, DIR_MODE, boot};
use shep_daemon::rpc::RpcContext;
use shep_daemon::tokio_runner::TokioRunner;

const RECV_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(unix)]
// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn kill_daemon_shuts_the_flock_down_and_unlinks_the_socket() {
    let mut fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let sleeper = |name: &str| {
        let mut app = AppConfig::minimal(name, "/bin/sh");
        app.interpreter = Some("none".to_string());
        app.args = vec!["-c".to_string(), "while :; do sleep 1; done".to_string()];
        app
    };
    let started = client
        .request(Request::Start {
            apps: vec![sleeper("one"), sleeper("two")],
        })
        .await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let pids: Vec<i32> = infos
        .iter()
        .map(|i| i32::try_from(i.pid.expect("a real spawn reports a real pid")).unwrap())
        .collect();
    assert_eq!(pids.len(), 2);

    let killed = client.request(Request::KillDaemon).await;
    assert_eq!(killed.result.unwrap(), Response::ShuttingDown);

    let socket = fixture.paths.socket.clone();
    let pidfile_path = shep_daemon::boot::pidfile(&fixture.paths);
    let run = fixture.run.take().expect("run is only ever taken once");
    tokio::time::timeout(RECV_TIMEOUT, run)
        .await
        .expect("teardown must not hang")
        .unwrap()
        .unwrap();

    assert!(
        !socket.exists(),
        "the control socket must be unlinked on teardown"
    );
    assert!(
        !pidfile_path.exists(),
        "the pidfile must be removed on teardown"
    );

    // Reaped, not merely signalled: `kill(pid, None)` still returns `Ok` for a
    // zombie, so only ESRCH proves the daemon's own `wait()` ran. The `sleep 1`
    // grandchildren are out of reach here; `real_runner.rs` covers them.
    for pid in pids {
        assert_reaped(pid).await;
    }

    // A fresh connect on the unlinked path must fail, not hang.
    assert!(
        transport::connect(&socket).await.is_err(),
        "the daemon must not still be answering after KillDaemon"
    );
}

/// Waits until nothing answers a connection at `socket`, failing at
/// [`RECV_TIMEOUT`].
///
/// Dropping a `UnixListener` does not unbind the socket: it lives as long as
/// its last descriptor, and a child parked between `fork` and `exec` holds a
/// copy until close-on-exec clears it. `bind_socket` reads such a socket as a
/// live daemon and refuses the boot with `AlreadyRunning`.
async fn await_stale_socket(socket: &std::path::Path) {
    let refused = tokio::time::timeout(RECV_TIMEOUT, async {
        // tokio's connector, not `std`'s: a full backlog parks the caller on
        // some Unixes, inside a syscall no timer can interrupt.
        while !matches!(
            transport::connect(socket).await,
            Err(err) if matches!(err.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound)
        ) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        refused.is_ok(),
        "{}: a crashed daemon's socket is still answering connections",
        socket.display()
    );
}

// `cfg(unix)`: a leftover socket file, which a named pipe never leaves.
#[cfg(unix)]
#[tokio::test]
async fn a_socket_left_behind_by_a_crash_does_not_block_the_next_boot() {
    let mut fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let socket = fixture.paths.socket.clone();

    // Simulate a crash: aborting the run loop unlinks neither the socket file
    // nor the pidfile. Awaiting the handle resolves once the task, and the
    // `UnixListener` it owned, has finished dropping.
    let run = fixture.run.take().expect("run is only ever taken once");
    run.abort();
    let outcome = run.await;
    assert!(
        outcome.is_err_and(|err| err.is_cancelled()),
        "the run task must have been cancelled, not completed on its own"
    );
    assert!(
        socket.exists(),
        "sanity: a crash leaves the socket file behind"
    );
    // Dropping that listener is not the socket going dead; the reboot needs
    // the second.
    await_stale_socket(&socket).await;

    // Same `$SHEP_HOME`: taking `dir` out of `fixture` keeps the leftover
    // socket file alive into the reboot.
    let dir = fixture.dir.take().expect("dir is only ever taken once");
    let rebooted = Fixture::boot(dir, false).await;
    let mut client = rebooted.connect().await;
    let pong = client.request(Request::Ping).await;
    assert_eq!(pong.result.unwrap(), Response::Pong);

    rebooted.shutdown().await;
}

#[cfg(unix)]
// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn muster_restores_the_flock_across_a_daemon_lifetime() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let alpha = forever_app("alpha");
    let mut beta = forever_app("beta");
    beta.instances = 2;
    let started = client
        .request(Request::Start {
            apps: vec![alpha, beta],
        })
        .await;
    let Response::Started(before) = started.result.unwrap() else {
        panic!("expected started")
    };
    assert_eq!(before.len(), 3, "alpha (1 instance) + beta (2 instances)");
    let old_pids: std::collections::HashSet<u32> = before.iter().map(|i| i.pid.unwrap()).collect();

    // Explicit write, no polling: the roll write is a call, not a race.
    fixture.ctx.snapshot_now().await.unwrap();
    let roll = shep_daemon::snapshot::read(&fixture.paths.snapshot).unwrap();
    let running_by_name: std::collections::HashMap<_, _> = roll
        .apps
        .iter()
        .map(|a| (a.app.name.clone(), a.instances_running))
        .collect();
    assert_eq!(running_by_name.get("alpha"), Some(&1));
    assert_eq!(running_by_name.get("beta"), Some(&2));

    let dir = fixture.shutdown().await; // same $SHEP_HOME survives the reboot

    // Reaped, not merely recorded in the roll: a stale pid the OS had not
    // reused would pass the fresh-pid assertion for the wrong reason.
    for &pid in &old_pids {
        assert_reaped(i32::try_from(pid).unwrap()).await;
    }

    let rebooted = Fixture::boot(dir, true).await;
    let listed = rebooted.connect().await.request(Request::ListFlock).await;
    let Response::Flock(after) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(
        after.len(),
        3,
        "both apps' full instance counts must come back"
    );
    for info in &after {
        assert_eq!(info.status, ProcStatus::Online);
        let pid = info.pid.expect("a restored sheep is a real live process");
        assert!(
            !old_pids.contains(&pid),
            "a restored sheep gets a fresh pid, id {}",
            info.id
        );
    }
    rebooted.shutdown().await;
}

/// Prepends `dir` to `PATH` for one test, restoring the original on drop.
///
/// Prepending, never replacing: a concurrently spawned sleeper's `/bin/sh`
/// has to keep finding `sleep` while this guard is active.
struct PathGuard {
    original: Option<String>,
}

impl PathGuard {
    fn prepend(dir: &std::path::Path) -> Self {
        let original = std::env::var("PATH").ok();
        let combined = match &original {
            Some(existing) => format!("{}:{existing}", dir.display()),
            None => dir.display().to_string(),
        };
        // SAFETY: `set_var`'s hazard is a concurrent raw `getenv`. Every read
        // of `PATH` in this binary goes through `std::env::var`, which std
        // serializes against `set_var`/`remove_var`.
        unsafe { std::env::set_var("PATH", combined) };
        Self { original }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.original {
            // SAFETY: every `PATH` read in this binary goes through
            // `std::env::var`, which std serializes against `set_var`.
            Some(value) => unsafe { std::env::set_var("PATH", value) },
            // SAFETY: every `PATH` read in this binary goes through
            // `std::env::var`, which std serializes against `remove_var`.
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

#[cfg(unix)]
// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn a_bare_interpreter_resolves_via_the_inherited_path() {
    // A throwaway-tempdir shim, not a bare `"sh"`: `execvp` falls back to
    // `_PATH_DEFPATH` when PATH is absent from the child's env, so a bare name
    // would resolve even with `base_env()`'s seeding reverted.
    use std::os::unix::fs::PermissionsExt as _;

    let shim_home = tempfile::tempdir().unwrap();
    let shim_dir = shim_home.path().join("bin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let shim_path = shim_dir.join("shep-test-interp");
    std::fs::write(&shim_path, "#!/bin/sh\necho shep-bare-interpreter-ok\n").unwrap();
    let mut perms = std::fs::metadata(&shim_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim_path, perms).unwrap();

    // The only test in this binary that mutates PATH.
    let _path_guard = PathGuard::prepend(&shim_dir);

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;
    // Subscribe before starting: a connection gets no events until it does.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    // Bare: only found via the seeded PATH now that it includes shim_dir.
    let mut app = AppConfig::minimal("bare", "unused");
    app.interpreter = Some("shep-test-interp".to_string());
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;

    // A failed exec lands the sheep in Errored, so Online is the assertion.
    let online = client
        .await_process_event(id, ProcessEventKind::Online)
        .await;
    assert_eq!(online.status, ProcStatus::Online);

    fixture.shutdown().await;
}

/// Serializes this file's reload measurements: each hands a fixed port to a
/// child that binds it twice over, so the two cannot interleave.
///
/// `tokio::sync::Mutex` because the guard is held across `.await`, where
/// clippy's `await_holding_lock` denies a blocking guard. It does not
/// serialize against other test binaries, which cargo also runs concurrently.
static RELOAD_PORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How long [`reuse_port_sheep`] holds a connection before answering it.
///
/// At [`CONNECT_INTERVAL`] this keeps around fifteen connections open at every
/// instant, which an instance killed mid-flight destroys.
const HOLD_MS: u64 = 60;

/// One new connection every 4ms for as long as a reload lasts.
///
/// Fast enough that the window between the drainee emptying its accept queue
/// and closing its listener is a real chance to lose something, slow enough
/// that a loss is never the fixture's queue overflowing.
const CONNECT_INTERVAL: Duration = Duration::from_millis(4);

/// How long one connection gets before it counts as lost. Two orders of
/// magnitude over [`HOLD_MS`]: slack for a loaded runner, not an expected
/// duration.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

/// The `AwaitReady` window a replacement gets, as `listen_timeout`.
///
/// The fixture signals no readiness, so this is the `Heuristic` case: the
/// deadline elapsing is the verdict, and it holds the drainee's kill ladder
/// back until the replacement has bound. Half a second against a process that
/// binds in single-digit milliseconds.
const READY_WINDOW: UpDuration = UpDuration::from_millis(500);

/// The drain window a replaced instance gets, as `graceful_timeout`, and for
/// an instance that will not take its stop signal, how long it is before
/// `SIGKILL`. Short because nothing here needs longer; the spec default is 8s.
const DRAIN_WINDOW: UpDuration = UpDuration::from_millis(1_000);

/// Connections opened at once before a reload and again after it, to establish
/// which process owns the port at each end of the swap.
const BURST: usize = 10;

/// What one connection to the fixture got.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Attempt {
    /// Answered, by the process with this pid, which keeps "the port
    /// answered" and "that process answered" separate claims.
    Served(u32),
    /// Refused, reset, timed out, or closed with nothing on it, carrying the
    /// reason. A connection accepted into a backlog whose listener then closed
    /// arrives as an empty answer, not a connect error.
    Failed(String),
}

impl Attempt {
    fn failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// One connection: open it, read what the server says, classify the outcome.
async fn attempt(port: u16) -> Attempt {
    let exchange = tokio::time::timeout(ATTEMPT_TIMEOUT, async {
        let mut conn = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        let mut answer = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut conn, &mut answer).await?;
        std::io::Result::Ok(answer)
    });
    match exchange.await {
        Err(_) => Attempt::Failed(format!("no answer inside {ATTEMPT_TIMEOUT:?}")),
        Ok(Err(error)) => Attempt::Failed(error.to_string()),
        Ok(Ok(answer)) => match answer.trim().parse() {
            Ok(pid) => Attempt::Served(pid),
            Err(_) => Attempt::Failed(format!("answered {answer:?}")),
        },
    }
}

/// Opens [`BURST`] connections at once and hands back what each got.
async fn burst(port: u16) -> Vec<Attempt> {
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..BURST {
        set.spawn(attempt(port));
    }
    let mut attempts = Vec::new();
    while let Some(outcome) = set.join_next().await {
        attempts.push(outcome.expect("an attempt cannot panic"));
    }
    attempts
}

/// A caller that keeps connecting for as long as a reload takes.
struct Hammer {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    task: tokio::task::JoinHandle<Vec<Attempt>>,
}

impl Hammer {
    /// Starts opening one connection every [`CONNECT_INTERVAL`].
    fn start(port: u16) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = tokio::spawn({
            let stop = std::sync::Arc::clone(&stop);
            async move {
                let mut set = tokio::task::JoinSet::new();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    set.spawn(attempt(port));
                    tokio::time::sleep(CONNECT_INTERVAL).await;
                }
                // Connections already open are waited out: those in flight at
                // the swap are the ones an instance killed mid-answer loses.
                let mut attempts = Vec::new();
                while let Some(outcome) = set.join_next().await {
                    attempts.push(outcome.expect("an attempt cannot panic"));
                }
                attempts
            }
        });
        Self { stop, task }
    }

    /// Stops connecting and reports every attempt, in-flight ones included.
    async fn finish(self) -> Vec<Attempt> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.task.await.expect("the hammer cannot panic")
    }
}

/// A one-line tally of a run of attempts, for a failure message.
fn tally(attempts: &[Attempt]) -> String {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for attempt in attempts {
        let key = match attempt {
            Attempt::Served(pid) => format!("served by {pid}"),
            Attempt::Failed(reason) => reason.clone(),
        };
        *counts.entry(key).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(reason, count)| format!("{count}x {reason}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Waits for `pid` to be the process answering `port`, failing at
/// [`RECV_TIMEOUT`].
///
/// The bus cannot say when this sheep has bound: it configures neither a
/// channel nor a probe, so it is `Online` from the moment it is spawned.
async fn await_serving(port: u16, pid: u32) {
    let serving = tokio::time::timeout(RECV_TIMEOUT, async {
        while attempt(port).await != Attempt::Served(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        serving.is_ok(),
        "pid {pid} must be the process answering 127.0.0.1:{port}"
    );
}

/// Polls `ListFlock` until `id` is `Online`.
///
/// A probed app reaches `Online` a probe interval after it starts answering,
/// so [`await_serving`] returning is not the same fact, and a reload arriving
/// in that gap finds nothing replaceable.
#[cfg(unix)]
async fn await_online(client: &mut Client, id: u32) {
    let online = tokio::time::timeout(RECV_TIMEOUT, async {
        loop {
            let listed = client.request(Request::ListFlock).await;
            let Response::Flock(flock) = listed.result.unwrap() else {
                panic!("expected flock")
            };
            if flock
                .iter()
                .any(|info| info.id == id && info.status == ProcStatus::Online)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(online.is_ok(), "id {id} must reach Online");
}

/// A port with nothing on it: bind `:0`, read what the OS chose, release it.
///
/// Check-then-use: a stranger can take the port before the fixture binds it.
/// That loss is loud, since the fixture panics with the bind error.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the OS must have a free loopback port")
        .local_addr()
        .expect("a bound listener has an address")
        .port()
}

/// The fixture server's binary, built as `examples/reuse_port_sheep.rs`.
///
/// Located rather than named: `env!("CARGO_BIN_EXE_<name>")` covers `[[bin]]`
/// targets only. Cargo puts an example at `<profile>/examples/<name>` and this
/// test binary at the sibling `deps/<name>-<hash>`.
fn reuse_port_sheep() -> std::path::PathBuf {
    let test_binary = std::env::current_exe().expect("a running test knows its own path");
    let path = test_binary
        .parent()
        .and_then(std::path::Path::parent)
        .expect("a test binary lives at <profile>/deps/<name>")
        .join("examples")
        .join("reuse_port_sheep");
    assert!(
        path.is_file(),
        "{} must exist: a plain `cargo test` builds the package's examples, so a \
         missing one means this test was run some way that does not",
        path.display()
    );
    path
}

#[cfg(unix)]
/// Reloads one `reuse_port_sheep` while a caller connects continuously, and
/// hands back every attempt made between the request and the swap finishing.
///
/// Asserts what holds whatever the app does with its stop signal: the swap
/// completes, the replacement answers the port, and the instance it replaced
/// is gone. The counting is the caller's.
async fn reload_under_load(name: &str, defiant: bool) -> Vec<Attempt> {
    let _port_guard = RELOAD_PORT_LOCK.lock().await;
    let port = free_port();

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let mut app = AppConfig::minimal(name, &reuse_port_sheep().display().to_string());
    app.interpreter = Some("none".to_string());
    app.env
        .insert("SHEEP_PORT_BASE".to_string(), port.to_string());
    app.env
        .insert("SHEEP_HOLD_MS".to_string(), HOLD_MS.to_string());
    if defiant {
        app.env.insert("SHEEP_DEFIANT".to_string(), "1".to_string());
    }
    app.listen_timeout = READY_WINDOW;
    app.graceful_timeout = DRAIN_WINDOW;
    // Teardown's ladder, not the reload's: a defiant replacement is SIGKILLed
    // at the end of the test too, and the spec's 1.6s default would be spent
    // waiting for a process that never answers.
    app.kill_timeout = DRAIN_WINDOW;
    // Nothing may respawn behind the measurement: a restart would put a third
    // process on this port.
    app.autorestart = false;

    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let drainee_id = infos[0].id;
    let drainee_pid = infos[0].pid.expect("a real spawn reports a real pid");
    client
        .await_process_event(drainee_id, ProcessEventKind::Online)
        .await;
    await_serving(port, drainee_pid).await;

    // Answering as the process this test thinks is on the port rules out a
    // stranger or a leftover being the reason a later attempt fails.
    let before = burst(port).await;
    assert_eq!(
        tally(&before),
        format!("{BURST}x served by {drainee_pid}"),
        "the sheep must own the port outright before its reload begins"
    );

    let hammer = Hammer::start(port);
    let accepted = client
        .request(Request::Reload {
            selector: SelectorSpec::Name(name.to_string()),
        })
        .await;
    let Response::Reloading { accepted, .. } = accepted.result.unwrap() else {
        panic!("expected an accepted reload")
    };
    assert_eq!(accepted.len(), 1);

    // `Reloaded` rather than a duration bounds the window to the reload.
    let replacement = client
        .await_any_process_event(ProcessEventKind::Reloaded)
        .await;
    let during = hammer.finish().await;
    let replacement_pid = replacement.pid.expect("a replacement has a pid");
    assert_ne!(replacement_pid, drainee_pid);
    assert_reaped(i32::try_from(drainee_pid).unwrap()).await;

    // One row, the replacement's: the drainee's registration went with the
    // process.
    let listed = client.request(Request::ListFlock).await;
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock.len(), 1);
    assert_eq!(flock[0].id, replacement.id);
    // The status catches a swap committed before its replacement could prove
    // anything, which costs no connections.
    assert_eq!(flock[0].status, ProcStatus::Online);

    // The fixture derives its port from `SHEP_INSTANCE`: a replacement in
    // another slot would answer somewhere else.
    let after = burst(port).await;
    assert_eq!(
        tally(&after),
        format!("{BURST}x served by {replacement_pid}"),
        "the replacement must own the port outright once the swap is done"
    );

    let dir = fixture.shutdown().await;
    assert_reaped(i32::try_from(replacement_pid).unwrap()).await;
    drop(dir);

    during
}

#[cfg(unix)]
/// shep promises the overlap, not zero downtime: a listener's accept backlog
/// is reset when it closes, so what is queued and unaccepted is lost unless
/// the app drains inside `graceful_timeout`.
///
/// The count is asserted on Linux only. Linux load-balances new connections
/// over every listener in the `SO_REUSEPORT` group, so the drainee keeps a
/// share until it closes; macOS gives every new connection to the last
/// socket to bind, so only the duration is asserted there.
#[tokio::test]
async fn a_reload_costs_a_draining_app_no_connections() {
    let during = reload_under_load("drainer", false).await;
    let failures = during.iter().filter(|attempt| attempt.failed()).count();
    // Printed on every platform, asserted on one.
    println!(
        "draining app, {} attempts across the reload, {failures} lost: {}",
        during.len(),
        tally(&during)
    );
    assert!(
        during.len() > 20,
        "the reload must last long enough to be measured: {}",
        tally(&during)
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        failures,
        0,
        "an app that drains inside its graceful timeout must lose nothing: {}",
        tally(&during)
    );
}

#[cfg(unix)]
/// An instance that will not stop accepting, finish what it has, and exit
/// inside `graceful_timeout` reaches the end of that window still holding
/// work, and `SIGKILL` takes the work with it. No supervisor can give that
/// app zero downtime.
///
/// The count is asserted on Linux only, for the sibling case's reason: there
/// the defiant instance is still being handed a share of every new connection
/// when `SIGKILL` lands, while on macOS it is killed empty.
#[tokio::test]
async fn a_reload_costs_a_defiant_app_the_work_it_will_not_finish() {
    let during = reload_under_load("defier", true).await;
    let failures = during.iter().filter(|attempt| attempt.failed()).count();
    println!(
        "defiant app, {} attempts across the reload, {failures} lost: {}",
        during.len(),
        tally(&during)
    );
    assert!(
        during.len() > 20,
        "the reload must last long enough to be measured: {}",
        tally(&during)
    );
    #[cfg(target_os = "linux")]
    assert!(
        failures > 0,
        "an app that will not drain must be seen to lose connections: {}",
        tally(&during)
    );
}

/// The reference smit, the one `shep-deploy` paints: a mark and a revision,
/// thirteen characters, and nothing shep understands.
const SMIT: &str = "\u{25b2} main@a1b2c3";

/// Starts one long-lived real sheep under `name` and answers with its id.
async fn start_sheep(client: &mut Client, name: &str) -> u32 {
    let app = forever_app(name);
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.expect("the sheep must start") else {
        panic!("expected started")
    };
    infos[0].id
}

/// `name`'s smit as `shep flock` would paint it, read over the socket rather
/// than out of the daemon's memory.
async fn smit_of(client: &mut Client, name: &str) -> Option<String> {
    let listed = client.request(Request::ListFlock).await;
    let Response::Flock(flock) = listed.result.expect("the flock must list") else {
        panic!("expected flock")
    };
    flock
        .into_iter()
        .find(|info| info.name == name)
        .expect("the sheep must still be registered")
        .smit
}

/// Waits for `name`'s smit to clear, answering `false` at [`RECV_TIMEOUT`].
///
/// Polls: the daemon learns of a closed socket asynchronously.
async fn await_smit_cleared(client: &mut Client, name: &str) -> bool {
    tokio::time::timeout(RECV_TIMEOUT, async {
        while smit_of(client, name).await.is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

/// What has to hold is that closing a real socket reaches the forget path,
/// through `handle_conn`'s tail, the actor's mailbox and `to_info`.
#[tokio::test]
async fn a_smit_dies_with_the_connection_that_painted_it() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    // The observer must not be the connection whose closing is under test.
    let mut looker = fixture.connect().await;
    start_sheep(&mut looker, "web").await;

    let mut painter = fixture.connect().await;
    let painted = painter
        .request(Request::SetSmit {
            sheep: "web".to_string(),
            smit: Some(SMIT.parse().expect("the reference smit must be valid")),
        })
        .await;
    assert!(
        matches!(painted.result, Ok(Response::SmitPainted(_))),
        "{painted:?}"
    );

    assert_eq!(
        smit_of(&mut looker, "web").await,
        Some(SMIT.to_string()),
        "a smit must be visible to every client, not only its painter"
    );

    drop(painter);

    assert!(
        await_smit_cleared(&mut looker, "web").await,
        "the smit outlived the connection that painted it"
    );

    fixture.shutdown().await;
}

/// Also fails if a dog can clear a smit it did not paint: connection scoping
/// is otherwise indistinguishable from "any disconnect wipes everything".
#[tokio::test]
async fn one_dogs_disconnect_leaves_another_dogs_smit_alone() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut looker = fixture.connect().await;
    start_sheep(&mut looker, "web").await;
    start_sheep(&mut looker, "api").await;

    let mut deployer = fixture.connect().await;
    let mut watcher = fixture.connect().await;
    for (client, sheep) in [(&mut deployer, "web"), (&mut watcher, "api")] {
        let painted = client
            .request(Request::SetSmit {
                sheep: sheep.to_string(),
                smit: Some(SMIT.parse().expect("the reference smit must be valid")),
            })
            .await;
        assert!(
            matches!(painted.result, Ok(Response::SmitPainted(_))),
            "{painted:?}"
        );
    }

    // A clear only takes effect from the connection that painted it.
    let ignored = watcher
        .request(Request::SetSmit {
            sheep: "web".to_string(),
            smit: None,
        })
        .await;
    assert!(
        matches!(ignored.result, Ok(Response::SmitPainted(_))),
        "{ignored:?}"
    );
    assert_eq!(
        smit_of(&mut looker, "web").await,
        Some(SMIT.to_string()),
        "one dog cleared a smit another dog painted"
    );

    drop(deployer);

    assert!(
        await_smit_cleared(&mut looker, "web").await,
        "the smit outlived the connection that painted it"
    );
    assert_eq!(
        smit_of(&mut looker, "api").await,
        Some(SMIT.to_string()),
        "one dog's disconnect cleared another dog's smit"
    );

    fixture.shutdown().await;
}

/// The renderer is not the guard: `output::width::sanitize_cell` keeps a
/// well-formed CSI sequence, since shep's own colouring is made of them.
///
/// The frame is built past the `Smit` parser, so this is the daemon's refusal
/// rather than the client's. A malformed body ends the connection with no
/// reply, so either answer is a refusal and neither is a stored smit.
#[tokio::test]
async fn a_smit_carrying_an_escape_is_refused_at_the_daemon() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut looker = fixture.connect().await;
    start_sheep(&mut looker, "web").await;

    let mut rogue = fixture.connect().await;
    let refused = rogue
        .request_raw(serde_json::json!({
            "kind": "set_smit",
            "sheep": "web",
            "smit": "\u{1b}[2Jgone",
        }))
        .await;
    assert!(
        refused.as_ref().is_none_or(|reply| reply.result.is_err()),
        "the daemon accepted a smit carrying an escape: {refused:?}"
    );
    assert_eq!(smit_of(&mut looker, "web").await, None);

    fixture.shutdown().await;
}

// --- Reload: a probed app's replacement answers for itself ---

/// The `AwaitReady` window a probed reload gets, as `listen_timeout`.
///
/// Both directions matter: a replacement that will serve has to bind inside
/// it, and one that will not costs the case its whole length before the
/// abandonment lands.
///
/// `cfg(unix)`: `reuse_port_sheep` has no Windows build under that name.
#[cfg(unix)]
const PROBED_READY_WINDOW: UpDuration = UpDuration::from_millis(800);

/// One `reuse_port_sheep` on `port`, gated on a TCP probe against the port it
/// binds: the arrangement in which "is the new instance ready" and "is
/// something listening" are the same question.
#[cfg(unix)]
fn probed_sheep(name: &str, port: u16, mute_file: &std::path::Path) -> AppConfig {
    let mut app = AppConfig::minimal(name, &reuse_port_sheep().display().to_string());
    app.interpreter = Some("none".to_string());
    app.env
        .insert("SHEEP_PORT_BASE".to_string(), port.to_string());
    app.env.insert("SHEEP_HOLD_MS".to_string(), "0".to_string());
    app.env.insert(
        "SHEEP_MUTE_FILE".to_string(),
        mute_file.display().to_string(),
    );
    app.readiness_probe = Some(ProbeConfig {
        kind: ProbeKind::Tcp,
        target: format!("127.0.0.1:{port}"),
        interval: UpDuration::from_millis(50),
        timeout: UpDuration::from_millis(200),
        failure_threshold: 3,
    });
    app.listen_timeout = PROBED_READY_WINDOW;
    app.graceful_timeout = DRAIN_WINDOW;
    app.kill_timeout = DRAIN_WINDOW;
    // Nothing may respawn behind the case: a restart would put a third process
    // on this port.
    app.autorestart = false;
    app
}

/// The control for the case below: without it, an implementation that
/// abandoned every probed reload would pass the failure case and look correct.
///
/// The replacement must land in the drainee's instance slot, since the fixture
/// derives its port from `SHEP_INSTANCE`.
#[cfg(unix)]
#[tokio::test]
async fn a_probed_reload_of_a_working_release_still_finishes() {
    let _port_guard = RELOAD_PORT_LOCK.lock().await;
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let mute = dir.path().join("mute");

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let started = client
        .request(Request::Start {
            apps: vec![probed_sheep("web", port, &mute)],
        })
        .await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let drainee_id = infos[0].id;
    let drainee_pid = infos[0].pid.expect("a real spawn reports a real pid");
    await_serving(port, drainee_pid).await;
    await_online(&mut client, drainee_id).await;

    // Subscribed after the app is up: this case reads the event stream in
    // emission order, and an earlier subscription would put `Start` in front.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let accepted = client
        .request(Request::Reload {
            selector: SelectorSpec::Name("web".to_string()),
        })
        .await;
    let Response::Reloading { accepted, .. } = accepted.result.unwrap() else {
        panic!("expected an accepted reload")
    };
    assert_eq!(accepted.len(), 1);

    // In order: the question is which of the two endings the reload reached,
    // and a search would find one whatever else had already been said.
    let (ending, replacement) = client
        .next_process_event_of(&[
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
        ])
        .await;
    assert_eq!(
        ending,
        ProcessEventKind::Reloaded,
        "a serial reload is slower than an overlapping one, not broken"
    );
    let replacement_pid = replacement.pid.expect("a replacement has a pid");
    assert_ne!(replacement_pid, drainee_pid);
    assert_eq!(replacement.status, ProcStatus::Online);
    assert_eq!(
        tally(&burst(port).await),
        format!("{BURST}x served by {replacement_pid}"),
        "the replacement owns the port once the swap is done"
    );

    let held = fixture.shutdown().await;
    drop(held);
    drop(dir);
}

/// Both instances run the same command; the second finds `SHEEP_MUTE_FILE` in
/// place and binds nothing, which is what a release whose listener moved to
/// the wrong port does. Its first probe is otherwise answered by the
/// instance still bound to that address.
///
/// Two independent mechanisms cover a single-instance probed app and each is
/// enough alone: the serial reload mode, and the post-drain probe. The flock
/// row's status is what a deploy tool reads, so it is asserted too.
#[cfg(unix)]
#[tokio::test]
async fn a_replacement_that_serves_nothing_is_refused_not_reported_reloaded() {
    let _port_guard = RELOAD_PORT_LOCK.lock().await;
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let mute = dir.path().join("mute");

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let started = client
        .request(Request::Start {
            apps: vec![probed_sheep("web", port, &mute)],
        })
        .await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let drainee_id = infos[0].id;
    let drainee_pid = infos[0].pid.expect("a real spawn reports a real pid");
    await_serving(port, drainee_pid).await;
    await_online(&mut client, drainee_id).await;

    // Subscribed after the app is up: this case reads the event stream in
    // emission order, and an earlier subscription would put `Start` in front.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    // The bad release, staged between the two spawns of one unchanged app.
    std::fs::write(&mute, b"").expect("the marker must be writable");

    let accepted = client
        .request(Request::Reload {
            selector: SelectorSpec::Name("web".to_string()),
        })
        .await;
    let Response::Reloading { accepted, .. } = accepted.result.unwrap() else {
        panic!("expected an accepted reload")
    };
    assert_eq!(accepted.len(), 1);

    // In order again, for the reason the control case gives.
    let (ending, info) = client
        .next_process_event_of(&[
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
        ])
        .await;
    assert_eq!(
        ending,
        ProcessEventKind::ReloadAbandoned,
        "a replacement that binds nothing has not proved it can take over"
    );
    assert_ne!(info.id, drainee_id, "the abandonment names the replacement");

    let listed = client.request(Request::ListFlock).await;
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock.len(), 1, "the app keeps the one instance it has left");
    assert_eq!(
        flock[0].status,
        ProcStatus::Starting,
        "a replacement that never answered its probe is never called online"
    );

    let held = fixture.shutdown().await;
    drop(held);
    drop(dir);
}

mod app_configs;
mod channel;
mod harness;
mod lifecycle;
mod protocol;

pub(crate) use app_configs::*;
pub(crate) use harness::*;
