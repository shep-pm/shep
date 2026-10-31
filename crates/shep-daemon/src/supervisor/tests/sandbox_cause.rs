//! `ProcessInfo::sandbox_refused`: why an `errored` row stopped.

use super::*;

async fn wait_errored(handle: &SupervisorHandle) -> ProcessInfo {
    let waits = async {
        loop {
            let row = handle.list().await.remove(0);
            if row.status == ProcStatus::Errored {
                return row;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), waits)
        .await
        .expect("the sheep must reach errored")
}

#[cfg(unix)] // normalize refuses sandbox on Windows.
#[tokio::test(start_paused = true)]
async fn a_refused_sandboxed_sheep_reports_the_refusal() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(125)]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("jailed", "./srv");
    app.sandbox = Some("sandbox.json".to_owned());
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(wait_errored(&handle).await.sandbox_refused, Some(true));
}

#[tokio::test(start_paused = true)]
async fn an_errored_sheep_out_of_restarts_reports_no_refusal() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(1), ProcScript::const_exit(1)]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let mut app = AppConfig::minimal("svc", "./svc");
    app.max_restarts = 2;
    app.exp_backoff_restart_delay = None;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(wait_errored(&handle).await.sandbox_refused, Some(false));
}

#[tokio::test(start_paused = true)]
async fn a_running_sheep_has_no_refusal_to_report() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::never_exits()]);
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    let app = AppConfig::minimal("svc", "./svc");
    handle.start(vec![normalize(app).unwrap()]).await.unwrap();

    assert_eq!(handle.list().await[0].sandbox_refused, None);
}
