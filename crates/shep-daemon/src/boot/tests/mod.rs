//! [`boot`](super::boot)'s own tests, split by what each one is about
//!
//! [`dogs`] covers where a dog runs relative to the muster restore, and
//! [`readiness`] covers both of the reports a boot makes. What stays here
//! belongs to neither: the cron-sleep default, and the one case that drives
//! `boot`'s own extras reporter over the whole production chain.

mod dogs;
mod readiness;

use crate::boot::*;
use crate::fake::{ProcScript, ScriptedRunner};
use crate::testing::test_paths;
use shep_core::config::{AppConfig, ProbeConfig, ProbeKind, normalize};
use shep_core::protocol::{BusEvent, ProcessEventKind};
use shep_core::values::UpDuration;
use std::time::Duration;

// `boot` is the one place `DEFAULT_MAX_CRON_SLEEP` is applied: the CLI
// keeps the knob an `Option` all the way down, so nothing else here would
// notice a different fallback. Whole `BootOptions` values rather than bare
// `Option`s, since that is what `boot` reads.
#[test]
fn an_unset_max_cron_sleep_falls_back_to_the_daemons_own_default() {
    assert_eq!(
        max_cron_sleep(&BootOptions::default()),
        DEFAULT_MAX_CRON_SLEEP,
        "unset means the default"
    );
    assert_eq!(
        max_cron_sleep(&BootOptions {
            max_cron_sleep: Some(Duration::from_secs(300)),
            ..BootOptions::default()
        }),
        Duration::from_secs(300),
        "a configured value must reach the workers unchanged"
    );
}

// The only case driving `boot`'s own spawn of the extras reporter, over
// the whole production chain: the actor arms the liveness loop at Online,
// the loop reports over `Extras::real`'s sender, the reporter reads it,
// and `extra_restart` lets it through. Real time, and a real `OsProber`.
#[tokio::test]
async fn a_booted_daemon_restarts_a_sheep_whose_liveness_probe_fails() {
    // Real time: binds a real socket, so it takes SIGNAL_TEST_LOCK.
    let _guard = SIGNAL_TEST_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(&dir);

    // Reserve a port, then release it: nothing listens there, so every
    // probe fails with a connection refusal and there is no race.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reserved.local_addr().unwrap();
    drop(reserved);

    let daemon = boot(
        ScriptedRunner::new(vec![ProcScript::never_exits(); 4]),
        paths.clone(),
        BootOptions::default(),
    )
    .await
    .unwrap();
    let ctx = daemon.context();
    let mut events = ctx.events.subscribe();
    let run = tokio::spawn(daemon.run());

    let mut app = AppConfig::minimal("web", "./srv");
    app.liveness_probe = Some(ProbeConfig {
        kind: ProbeKind::Tcp,
        target: addr.to_string(),
        // The loop floors anything shorter at one second, so a smaller
        // number here would be a lie about what this test waits for.
        interval: UpDuration::from_millis(1_000),
        timeout: UpDuration::from_millis(500),
        failure_threshold: 1,
    });
    ctx.supervisor
        .start(vec![normalize(app).unwrap()])
        .await
        .unwrap();

    let restarted = async {
        loop {
            match events.recv().await.map(|event| event.to_event()) {
                Ok(BusEvent::Process {
                    event: ProcessEventKind::Restart,
                    info,
                    ..
                }) => return info,
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(err) => panic!("the event stream closed before a restart: {err}"),
            }
        }
    };
    let info = tokio::time::timeout(Duration::from_secs(20), restarted)
        .await
        .expect("a failing liveness probe must restart its sheep");
    assert_eq!(info.id, 0);
    assert_eq!(info.restarts, 1);

    ctx.shutdown();
    tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
