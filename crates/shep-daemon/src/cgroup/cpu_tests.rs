//! `max_cpu_cores`: a sheep's `cpu.max`, written at spawn and on a live change.

use std::fs;
use std::sync::Arc;

use shep_core::config::{AppConfig, CpuCores, normalize};
use shep_core::selector::ProcessSelector;
use tokio::time::timeout;

use super::tests::{DEADLINE, delegated_base, read, start_over};
use super::{Cgroups, Limits, Reason, cpu_quota, sheep_dir, write_cpu_max};
use crate::fake::{ProcScript, ScriptedRunner};
use crate::supervisor::SupervisorBuilder;
use crate::testing::test_paths;
use crate::tokio_runner::TokioRunner;

/// A sheep `name` running `program`, throttled to `cores`.
fn cpu_app(name: &str, program: &str, cores: f64) -> AppConfig {
    let mut app = AppConfig::minimal(name, program);
    app.max_cpu_cores = Some(CpuCores::from_cores(cores));
    app
}

#[test]
fn max_cpu_cores_writes_quota_and_period() {
    for (cores, written) in [
        (Some(0.5), Some("50000 100000")),
        (Some(1.0), Some("100000 100000")),
        (Some(2.5), Some("250000 100000")),
        (None, None),
    ] {
        let base = tempfile::tempdir().expect("a temp directory");
        let cgroup = sheep_dir(base.path(), 1, 0);
        fs::create_dir(&cgroup).expect("the sheep's cgroup");
        fs::write(cgroup.join("cgroup.procs"), "").expect("an empty cgroup.procs");
        let quota = cores.map(|c| cpu_quota(CpuCores::from_cores(c)));

        let limits = Limits {
            cpu_quota: quota,
            ..Limits::default()
        };
        super::join_on_exec(
            &mut std::process::Command::new("/bin/true"),
            &cgroup,
            limits,
        )
        .expect("the join");

        let on_disk = fs::read_to_string(cgroup.join("cpu.max")).ok();
        assert_eq!(on_disk.as_deref(), written, "{cores:?} cores");
    }
}

#[test]
fn cpu_max_is_lifted_with_max_and_keeps_the_period() {
    let dir = tempfile::tempdir().expect("a temp directory");

    write_cpu_max(dir.path(), Some(150_000)).expect("a quota");
    assert_eq!(read(dir.path(), "cpu.max"), "150000 100000");

    write_cpu_max(dir.path(), None).expect("no quota");
    assert_eq!(read(dir.path(), "cpu.max"), "max 100000");
}

#[tokio::test]
async fn a_throttled_spawn_is_asked_for_its_cpu_quota_and_an_unthrottled_one_is_not() {
    let base = tempfile::tempdir().expect("a temp directory");
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let cgroups = Cgroups::Available {
        base: base.path().to_path_buf(),
    };

    let (_handle, started, _dir) = start_over(
        cgroups.clone(),
        Arc::clone(&runner),
        cpu_app("throttled", "./srv", 1.5),
    )
    .await;
    started.expect("the start");
    let (_handle, started, _dir) = start_over(
        cgroups,
        Arc::clone(&runner),
        AppConfig::minimal("free", "./srv"),
    )
    .await;
    started.expect("the start");

    assert_eq!(runner.spawned_cpu_quota(0), Some(150_000));
    assert_eq!(runner.spawned_cpu_quota(1), None, "no key writes no quota");
}

#[tokio::test]
async fn an_unavailable_host_refuses_a_throttled_sheep_and_names_why() {
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));

    let (_handle, started, _dir) = start_over(
        Cgroups::Unavailable(Reason::NotDelegated),
        Arc::clone(&runner),
        cpu_app("throttled", "./srv", 1.5),
    )
    .await;

    let refusal = started.expect_err("no silent start without the cap");
    let message = refusal.to_string();
    assert!(message.contains("`max_cpu_cores`"), "{message}");
    assert!(
        message.contains("the shepherd's cgroup is not delegated to it"),
        "the refusal carries the reason detect gave: {message}"
    );
    assert_eq!(runner.spawn_count(), 0, "nothing ran unthrottled");
}

#[tokio::test]
async fn a_live_change_rewrites_cpu_max() {
    let Some(base) = delegated_base() else {
        return;
    };
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().expect("a temp directory");
    let handle = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .cgroups(Cgroups::Available { base: base.clone() })
        .spawn();
    let mut app = cpu_app("web", "/bin/sleep", 0.5);
    app.args = vec!["30".to_string()];
    let started = handle
        .start(vec![normalize(app).expect("a config")])
        .await
        .expect("the start");
    let cgroup = sheep_dir(&base, started[0].id, 0);

    assert_eq!(read(&cgroup, "cpu.max"), "50000 100000");

    handle
        .set_sheep_field("web", "max_cpu_cores", serde_json::json!(2.5))
        .await
        .expect("a live change");
    assert_eq!(
        read(&cgroup, "cpu.max"),
        "250000 100000",
        "the running sheep's quota followed the change"
    );

    handle
        .set_sheep_field("web", "max_cpu_cores", serde_json::json!(null))
        .await
        .expect("the key cleared");
    assert_eq!(
        read(&cgroup, "cpu.max"),
        "max 100000",
        "a sheep that dropped the key is unthrottled"
    );

    let stop = handle.stop(ProcessSelector::Name("web".to_string()));
    timeout(DEADLINE, stop)
        .await
        .expect("in time")
        .expect("the stop");
}
