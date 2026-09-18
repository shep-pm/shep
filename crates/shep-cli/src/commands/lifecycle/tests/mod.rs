//! Shared fixtures for the lifecycle test suite.
//!
//! Every child module reaches these through `super::*`; they exist once
//! here rather than once per concern because several concerns exercise the
//! same daemon fixture or wire helper.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use shep_client::testing::{
    fake_client_answering, fake_client_capturing_envelopes, fake_client_replying_err,
};
use shep_client::{Client, DEFAULT_DEADLINE, RELOAD_DEADLINE, START_DEADLINE};
use shep_core::config::{AppConfig, FlockFormat, Flockfile, ResetDepth};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{
    ProcessInfo, Request, Response, RpcErrorCode, SelectorSpec, SheepApplied, SheepRefusal,
};
use shep_core::selector::ProcessSelector;

use crate::cli::{Format, ResetMode, SelectorArgs, StartArgs, StockArgs};
use crate::commands::lifecycle::configure::{applied_line, mapped_interpreter};
use crate::commands::lifecycle::operational::restart_within;
use crate::commands::lifecycle::resolve::{TargetError, evaluate_js_flockfile, split_assignments};
use crate::commands::lifecycle::respawn::unique_names;
use crate::commands::lifecycle::selectors::{
    flock_matches, is_reachable_as_a_name, render_outcome, selector_miss,
};
use crate::commands::lifecycle::*;
use crate::exit::ExitCode;
use crate::output::{FlockRows, Streams};

mod add;
mod configure;
mod deadline;
mod dogs;
mod interpreters_stock;
mod operational;
mod resolve;
mod selectors;
mod start;

/// The first envelope that is not a `ListFlock`.
async fn next_start(
    envelopes: &mut tokio::sync::mpsc::Receiver<shep_core::protocol::Envelope>,
) -> shep_core::protocol::Envelope {
    loop {
        let envelope = tokio::time::timeout(Duration::from_secs(5), envelopes.recv())
            .await
            .expect("start must reach the wire; it hung instead of sending a request")
            .unwrap();
        if envelope.body != Request::ListFlock {
            return envelope;
        }
    }
}

fn start_args(target: &str) -> StartArgs {
    StartArgs {
        targets: vec![target.to_string()],
        name: None,
        fold: None,
        cwd: None,
        interpreter: None,
        flockfile: false,
        reset: None,
    }
}

/// Returns `true` when node is on PATH, so a machine without node does
/// not fail the suite
///
/// `SHEP_REQUIRE_NODE` turns a missing node into a failure. The
/// `eprintln!` alone is invisible under a passing test.
fn node_available() -> bool {
    let ok = std::process::Command::new("node")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok || std::env::var_os("SHEP_REQUIRE_NODE").is_none(),
        "SHEP_REQUIRE_NODE is set but node is not usable on PATH"
    );
    if !ok {
        eprintln!("SKIPPED: node is not on PATH; the .js Flockfile cases did not run");
    }
    ok
}

/// The flock a `render_outcome` test hands the fake: two sheep the verb
/// did not touch, one it did, and a dog
fn a_flock_with_a_dog() -> Vec<shep_core::protocol::ProcessInfo> {
    use shep_core::protocol::{DogSource, ProcessInfo};
    use shep_core::status::ProcStatus;
    vec![
        ProcessInfo::builder(0, "golbat", ProcStatus::Online).build(),
        ProcessInfo::builder(1, "koji", ProcStatus::Stopped).build(),
        ProcessInfo::builder(2, "rotom", ProcStatus::Online).build(),
        ProcessInfo::builder(3, "log-rotate", ProcStatus::Online)
            .dog(Some(DogSource::Adopted {
                path: "/usr/local/bin/shep-log-rotate".to_string(),
            }))
            .build(),
    ]
}

/// A flock in which every tier of `start`'s precedence finds something
/// different: `golbat` and `koji` are in the fold `backed`, `rotom` is in
/// no fold, `log-rotate` is a dog, and `backed` is a fold and not a
/// sheep.
fn a_foldable_flock() -> Vec<shep_core::protocol::ProcessInfo> {
    use shep_core::protocol::{DogSource, ProcessInfo};
    use shep_core::status::ProcStatus;
    vec![
        ProcessInfo::builder(0, "golbat", ProcStatus::Stopped)
            .fold(Some("backed".to_string()))
            .build(),
        ProcessInfo::builder(1, "koji", ProcStatus::Stopped)
            .fold(Some("backed".to_string()))
            .build(),
        ProcessInfo::builder(2, "rotom", ProcStatus::Stopped).build(),
        ProcessInfo::builder(3, "log-rotate", ProcStatus::Online)
            .dog(Some(DogSource::BuiltIn))
            .build(),
    ]
}

/// The names `flock_matches` picks for `target`.
fn matched_names(target: &str) -> Vec<String> {
    let selector = ProcessSelector::parse(target).expect("the fixture uses valid selectors");
    flock_matches(&selector, &a_foldable_flock())
        .into_iter()
        .map(|info| info.name)
        .collect()
}

/// Three instances of one clustered app, stopped unless named in `online`
///
/// The shape `a_foldable_flock` cannot show: there a name selector and a
/// row selector pick the same set.
fn a_clustered_flock(online: &[u32]) -> Vec<ProcessInfo> {
    use shep_core::status::ProcStatus;
    (0..3)
        .map(|id| {
            let status = if online.contains(&id) {
                ProcStatus::Online
            } else {
                ProcStatus::Stopped
            };
            ProcessInfo::builder(id, "zam", status).build()
        })
        .collect()
}

/// A fake that answers a `start` invocation end to end
///
/// `failing` names the ids to answer as `errored`, which is how a spawn
/// failure reaches this verb.
fn a_daemon_for(
    flock: Vec<ProcessInfo>,
    failing: &'static [u32],
) -> impl Fn(&Request) -> Response + Send + 'static {
    use shep_core::status::ProcStatus;
    move |request| match request {
        Request::ListFlock => Response::Flock(flock.clone()),
        // One entry per app named, as `Response::Applied` promises.
        // Every entry is a no-op: these fixtures are about respawn
        // selectors.
        Request::ApplyConfig { apps, .. } => Response::Applied(
            apps.iter()
                .map(|app| SheepApplied::new(app.config.name.clone(), Vec::new(), Vec::new(), None))
                .collect(),
        ),
        Request::Restart { selector } => {
            let SelectorSpec::Id(id) = selector else {
                // Never reached by a correct build. Answered rather than
                // panicked, so the assertion naming the bug is the one
                // that fails.
                return Response::Restarted {
                    accepted: Vec::new(),
                    refused: Vec::new(),
                };
            };
            let status = if failing.contains(id) {
                ProcStatus::Errored
            } else {
                ProcStatus::Online
            };
            Response::Restarted {
                accepted: vec![ProcessInfo::builder(*id, "zam", status).build()],
                refused: Vec::new(),
            }
        }
        _ => Response::Pong,
    }
}

/// Every selector `start` sent inside a `Request::Restart`, in order.
fn respawns(
    envelopes: &mut tokio::sync::mpsc::UnboundedReceiver<shep_core::protocol::Envelope>,
) -> Vec<SelectorSpec> {
    let mut sent = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        if let Request::Restart { selector } = envelope.body {
            sent.push(selector);
        }
    }
    sent
}

/// Runs `start` against `daemon` and hands back the code, stdout and
/// stderr, in that order.
async fn start_against(client: &Client, target: &str) -> (ExitCode, String, String) {
    start_against_with_args(client, &start_args(target)).await
}

async fn start_against_with_args(client: &Client, args: &StartArgs) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        start(client, &mut streams, args, None, &BTreeMap::new()).await
    };
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// Runs `shep restart <selector>` against `client`, handing back its code
/// and both streams.
async fn restart_against(client: &Client, selector: &str) -> (ExitCode, String, String) {
    restart_against_in(client, selector, Format::Table).await
}

/// [`restart_against`] under a caller-chosen `--format`, so the JSON
/// shape can be asserted on the bytes a consumer actually reads.
///
/// A `$SHEP_HOME` with nothing adopted in it, so
/// `warn_of_a_dog_a_restart_would_break` has no binary to probe and the
/// budget below is never spent. These cases are about the reply.
async fn restart_against_in(
    client: &Client,
    selector: &str,
    fmt: Format,
) -> (ExitCode, String, String) {
    let home = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, home.path());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt,
        };
        restart_within(
            client,
            &mut streams,
            &paths,
            &SelectorArgs {
                selectors: vec![selector.to_string()],
            },
            Duration::from_millis(1),
        )
        .await
    };
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// How many top-level JSON values `text` holds. Counted rather than
/// parsed whole: `serde_json::from_str` refuses trailing input, so it
/// reports two objects and forty as the same failure.
fn objects_in(text: &str) -> usize {
    serde_json::Deserializer::from_str(text)
        .into_iter::<serde_json::Value>()
        .count()
}

/// The one sheep the two reload tests reload.
fn reloaded_api() -> ProcessInfo {
    ProcessInfo::builder(1, "api", shep_core::status::ProcStatus::Online).build()
}

/// Runs `shep reload <selector>` against `client`, handing back its code
/// and both streams.
async fn reload_against(client: &Client, selector: &str) -> (ExitCode, String, String) {
    reload_against_in(client, selector, Format::Table).await
}

/// [`reload_against`] under a caller-chosen `--format`, so the JSON
/// shape can be asserted on the bytes a consumer actually reads.
async fn reload_against_in(
    client: &Client,
    selector: &str,
    fmt: Format,
) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt,
        };
        reload(
            client,
            &mut streams,
            &SelectorArgs {
                selectors: vec![selector.to_string()],
            },
        )
        .await
    };
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// Every `Request::ApplyConfig` `start` sent, in order.
fn applies(
    envelopes: &mut tokio::sync::mpsc::UnboundedReceiver<shep_core::protocol::Envelope>,
) -> Vec<Vec<String>> {
    let mut sent = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        if let Request::ApplyConfig { apps, .. } = envelope.body {
            sent.push(apps.into_iter().map(|app| app.config.name).collect());
        }
    }
    sent
}

/// A `$SHEP_HOME`, a dog binary answering `--version` with `protocol`,
/// and a `shep.toml` that has adopted it under `name`
///
/// Written through `ShepToml::adopt_dog`, so a test dog is recorded the
/// way `shep adopt` records a real one.
fn adopted_dog(dir: &Path, name: &str, answer: &str) -> ShepPaths {
    let paths = ShepPaths::resolve(&|_| None, dir);
    let binary = dir.join(format!("shep-{name}"));
    std::fs::write(&binary, format!("#!/bin/sh\n{answer}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::commands::shep_toml::ShepToml::edit(&paths.daemon_config, |cfg| {
        cfg.adopt_dog(name, &binary).unwrap();
    })
    .unwrap();
    paths
}

/// A daemon that registers what it is asked to: an `Add` answers one
/// `Stopped` row per app, a `Start` one `Online` row
///
/// A `Start` is answered rather than refused, so a build that sent one
/// from `shep add` fails on the assertion naming the request.
fn a_daemon_that_registers(
    flock: Vec<ProcessInfo>,
) -> impl Fn(&Request) -> Response + Send + 'static {
    use shep_core::status::ProcStatus;
    let rows = |apps: &[AppConfig], status: ProcStatus| -> Vec<ProcessInfo> {
        apps.iter()
            .enumerate()
            .map(|(i, app)| {
                ProcessInfo::builder(u32::try_from(i).unwrap(), &app.name, status).build()
            })
            .collect()
    };
    move |request| match request {
        Request::ListFlock => Response::Flock(flock.clone()),
        Request::ApplyConfig { apps, .. } => Response::Applied(
            apps.iter()
                .map(|app| SheepApplied::new(app.config.name.clone(), Vec::new(), Vec::new(), None))
                .collect(),
        ),
        Request::Add { apps } => Response::Added(rows(apps, ProcStatus::Stopped)),
        Request::Start { apps } => Response::Started(rows(apps, ProcStatus::Online)),
        Request::Restart { .. } => Response::Restarted {
            accepted: Vec::new(),
            refused: Vec::new(),
        },
        _ => Response::Pong,
    }
}

/// Every request body an invocation put on the wire, in order
///
/// One drain rather than a helper per request kind: a channel can only be
/// emptied once.
fn sent(
    envelopes: &mut tokio::sync::mpsc::UnboundedReceiver<shep_core::protocol::Envelope>,
) -> Vec<Request> {
    let mut bodies = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        bodies.push(envelope.body);
    }
    bodies
}

/// Runs `shep add` against `target` and hands back its code and streams.
async fn add_against(client: &Client, target: &str) -> (ExitCode, String, String) {
    let args = start_args(target);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        add(client, &mut streams, &args, None, &BTreeMap::new()).await
    };
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}
