//! `max_tasks`: a sheep's `pids.max`, written at spawn and on a live change.

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use shep_core::config::{AppConfig, normalize};
use shep_core::selector::ProcessSelector;
use tokio::time::timeout;

use super::tests::{DEADLINE, delegated_base, pids, read, start_over};
use super::{Cgroups, Limits, Reason, sheep_dir, write_pids_max};
use crate::fake::{ProcScript, ScriptedRunner};
use crate::supervisor::SupervisorBuilder;
use crate::testing::test_paths;
use crate::tokio_runner::TokioRunner;

/// A sheep `name` running `program`, limited to `tasks` processes.
fn task_app(name: &str, program: &str, tasks: u32) -> AppConfig {
    let mut app = AppConfig::minimal(name, program);
    app.max_tasks = Some(tasks);
    app
}

#[test]
fn max_tasks_writes_pids_max_before_the_child_joins() {
    for (tasks, written) in [(Some(64), Some("64")), (Some(1), Some("1")), (None, None)] {
        let base = tempfile::tempdir().expect("a temp directory");
        let cgroup = sheep_dir(base.path(), 1, 0);
        fs::create_dir(&cgroup).expect("the sheep's cgroup");
        fs::write(cgroup.join("cgroup.procs"), "").expect("an empty cgroup.procs");

        let limits = Limits {
            pids_max: tasks,
            ..Limits::default()
        };
        super::join_on_exec(
            &mut std::process::Command::new("/bin/true"),
            &cgroup,
            limits,
        )
        .expect("the join");

        let on_disk = fs::read_to_string(cgroup.join("pids.max")).ok();
        assert_eq!(on_disk.as_deref(), written, "{tasks:?} tasks");
    }
}

#[test]
fn pids_max_is_lifted_with_max() {
    let dir = tempfile::tempdir().expect("a temp directory");

    write_pids_max(dir.path(), Some(256)).expect("a limit");
    assert_eq!(read(dir.path(), "pids.max"), "256");

    write_pids_max(dir.path(), None).expect("no limit");
    assert_eq!(read(dir.path(), "pids.max"), "max");
}

#[tokio::test]
async fn a_limited_spawn_is_asked_for_its_pids_max_and_an_unlimited_one_is_not() {
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
        task_app("limited", "./srv", 256),
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

    assert_eq!(runner.spawned_pids_max(0), Some(256));
    assert_eq!(runner.spawned_pids_max(1), None, "no key writes no limit");
}

#[tokio::test]
async fn an_unavailable_host_refuses_a_limited_sheep_and_names_why() {
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));

    let (_handle, started, _dir) = start_over(
        Cgroups::Unavailable(Reason::NotDelegated),
        Arc::clone(&runner),
        task_app("limited", "./srv", 256),
    )
    .await;

    let refusal = started.expect_err("no silent start without the limit");
    let message = refusal.to_string();
    assert!(message.contains("`max_tasks`"), "{message}");
    assert!(
        message.contains("the shepherd's cgroup is not delegated to it"),
        "the refusal carries the reason detect gave: {message}"
    );
    assert_eq!(runner.spawn_count(), 0, "nothing ran unlimited");
}

#[tokio::test]
async fn a_sheep_cannot_fork_past_max_tasks() {
    let Some(base) = delegated_base() else {
        return;
    };
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().expect("a temp directory");
    let handle = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .cgroups(Cgroups::Available { base: base.clone() })
        .spawn();
    // The inner shell forks twenty sleeps and stops at the first refused fork
    // (dash) or keeps trying (bash); either way the outer shell then becomes
    // a sleep itself, so the sheep outlives the attempt and nothing is killed.
    let mut app = task_app("bomb", "/bin/sh", 5);
    app.args = vec![
        "-c".to_string(),
        "/bin/sh -c 'i=0; while [ $i -lt 20 ]; do /bin/sleep 30 & i=$((i+1)); done'; \
         exec /bin/sleep 30"
            .to_string(),
    ];
    let started = handle
        .start(vec![normalize(app).expect("a config")])
        .await
        .expect("the start");
    let cgroup = sheep_dir(&base, started[0].id, 0);
    let pid = started[0].pid.expect("a pid");

    assert_eq!(read(&cgroup, "pids.max"), "5");
    // Real clock: the kernel counts a refused fork in `pids.events` as the
    // inner shell makes it, so the file is polled and `timeout` bounds the wait.
    let refused = timeout(DEADLINE, async {
        loop {
            let events = read(&cgroup, "pids.events");
            let count = events
                .lines()
                .find_map(|line| line.strip_prefix("max "))
                .and_then(|count| count.parse::<u64>().ok())
                .unwrap_or(0);
            if count > 0 {
                return count;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the kernel refuses a fork past the limit");

    assert!(refused >= 1, "pids.events counted {refused} refused forks");
    let live: u32 = read(&cgroup, "pids.current").parse().expect("a count");
    assert!(live <= 5, "{live} processes ran under a limit of 5");
    assert!(
        pids(&cgroup.join("cgroup.procs")).contains(&pid),
        "the sheep itself was not killed"
    );

    let stop = handle.stop(ProcessSelector::Name("bomb".to_string()));
    timeout(DEADLINE, stop)
        .await
        .expect("in time")
        .expect("the stop");
}

#[tokio::test]
async fn a_live_change_rewrites_pids_max() {
    let Some(base) = delegated_base() else {
        return;
    };
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().expect("a temp directory");
    let handle = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .cgroups(Cgroups::Available { base: base.clone() })
        .spawn();
    let mut app = task_app("web", "/bin/sleep", 64);
    app.args = vec!["30".to_string()];
    let started = handle
        .start(vec![normalize(app).expect("a config")])
        .await
        .expect("the start");
    let cgroup = sheep_dir(&base, started[0].id, 0);

    assert_eq!(read(&cgroup, "pids.max"), "64");

    handle
        .set_sheep_field("web", "max_tasks", serde_json::json!(128))
        .await
        .expect("a live change");
    assert_eq!(
        read(&cgroup, "pids.max"),
        "128",
        "the running sheep's limit followed the change"
    );

    handle
        .set_sheep_field("web", "max_tasks", serde_json::json!(null))
        .await
        .expect("the key cleared");
    assert_eq!(
        read(&cgroup, "pids.max"),
        "max",
        "a sheep that dropped the key has no limit"
    );

    let stop = handle.stop(ProcessSelector::Name("web".to_string()));
    timeout(DEADLINE, stop)
        .await
        .expect("in time")
        .expect("the stop");
}
