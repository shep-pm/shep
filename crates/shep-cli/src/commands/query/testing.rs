//! Fixtures and helpers shared by this module's tests.

use super::flock_display::flock;
use super::secret_inspection::describe;
use crate::cli::{Format, SelectorArgs};
use crate::exit::ExitCode;
use crate::output::Streams;
use shep_client::Client;
use shep_client::testing::{fake_client_answering, fake_client_with_ack, sample_ack, sample_info};
use shep_core::paths::ShepPaths;
use shep_core::protocol::DogSource;
use shep_core::protocol::{HostUsage, ProcessInfo, Request, Response};
use shep_core::secrets::{self};
use shep_core::status::ProcStatus;
use shep_daemon::snapshot::FlockSnapshot;
use std::time::Duration;

/// Bounds every `envelopes.recv()` here: a verb that never reaches the
/// wire must fail by assertion, not by hanging the job.
pub(super) const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// A reading with every rate present, so a strip built from it names
/// all four segments.
pub(super) fn sample_host() -> HostUsage {
    HostUsage {
        cpu_percent: Some(11.459_433),
        memory_used_bytes: 39_963_869_184,
        memory_total_bytes: 51_539_607_552,
        disk_bytes_per_second: Some((1_258_291, 491_520)),
        network_bytes_per_second: Some((24_594, 9_260)),
    }
}

/// A shepherd answering both halves of what a listing asks for, with
/// `host` as its answer to `Request::HostUsage`.
///
/// The envelope receiver comes back with the client and must be held:
/// `fake_client_answering`'s loop stops the moment nothing is listening,
/// which would leave the second request of a listing unanswered.
pub(super) async fn shepherd_answering(
    path: &std::path::Path,
    host: Option<HostUsage>,
) -> (
    Client,
    tokio::sync::mpsc::UnboundedReceiver<shep_core::protocol::Envelope>,
) {
    fake_client_answering(path, move |request| match request {
        Request::HostUsage => Response::HostUsage(host),
        _ => Response::Flock(vec![sample_info()]),
    })
    .await
}

/// One `shep flock` rendered into a string, at `fmt`.
pub(super) async fn listing(client: &Client, fmt: Format) -> (ExitCode, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt,
        };
        flock(client, &mut streams).await
    };
    (code, String::from_utf8(out).unwrap())
}

/// A roll entry, an operator store entry and no provider cache at all:
/// the three verdicts `secret_inspection::gather_secrets` can produce from local files
/// alone, exercised through the real `secret_inspection::describe` verb rather than
/// `secret_inspection::render_describe_secrets` in isolation.
pub(super) async fn describe_with_a_seeded_web(
    fmt: Format,
) -> (ExitCode, shep_core::paths::ShepPaths, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_with_ack(&path, sample_ack()).await;
    daemon.reply_to_describe(vec![
        ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
    ]);

    // `SHEP_HOME` pinned to `dir` itself, not its `.shep` default: this
    // test writes `paths.snapshot`/`paths.secrets` directly, and the
    // default subdirectory is never created outside a real boot.
    let home = dir.path().display().to_string();
    let paths = ShepPaths::resolve(
        &move |key| (key == "SHEP_HOME").then(|| home.clone()),
        dir.path(),
    );

    let mut config = shep_core::config::AppConfig::minimal("web", "./srv");
    config
        .env
        .insert("A".into(), "{{secret:DB_PASSWORD}}".into());
    config
        .env
        .insert("B".into(), "{{secret:vercel/API_KEY}}".into());
    let roll = FlockSnapshot {
        version: 1,
        saved_at_ms: 0,
        apps: vec![shep_daemon::snapshot::SavedApp {
            app: config,
            instances_running: 1,
        }],
    };
    std::fs::write(&paths.snapshot, serde_json::to_vec(&roll).unwrap()).unwrap();
    secrets::set(&paths.secrets, "DB_PASSWORD", "production", "hunter2").unwrap();
    // No provider cache written: `vercel` has never pushed anything.

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt,
        };
        describe(
            &client,
            &mut streams,
            &paths,
            &SelectorArgs {
                selectors: vec!["all".into()],
            },
        )
        .await
    };
    (code, paths, out)
}

/// Whether `hunter2` shows up anywhere in `value`'s own JSON text, not
/// just at the top level: a value smuggled in nested one level deeper
/// would still be a leak.
pub(super) fn out_contains(value: &serde_json::Value, needle: &str) -> bool {
    value.to_string().contains(needle)
}

// --- sheep_flourish ---

/// A sheep with `status` pinned and nothing else.
pub(super) fn sheep(id: u32, status: ProcStatus) -> ProcessInfo {
    ProcessInfo::builder(id, format!("s{id}"), status).build()
}

/// A registered dog, which `flock_display::sheep_flourish` must never count as a sheep.
pub(super) fn dog(id: u32) -> ProcessInfo {
    ProcessInfo::builder(id, format!("d{id}"), ProcStatus::Online)
        .dog(Some(DogSource::BuiltIn))
        .build()
}
