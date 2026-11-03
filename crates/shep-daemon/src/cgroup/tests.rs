use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use shep_core::config::{AppConfig, normalize};
use shep_core::protocol::{BusEvent, ProcessEventKind};
use shep_core::selector::ProcessSelector;
use tokio::time::timeout;

use super::{Cgroups, Reason, detect, detect_at, remove_if_empty, remove_stale, sheep_dir};
use crate::fake::{ProcScript, ScriptedRunner};
use crate::runner::{ProcessRunner, RunnerError, RunningProcess, SpawnSpec};
use crate::supervisor::SupervisorBuilder;
use crate::testing::{SharedRunner, test_paths};
use crate::tokio_runner::TokioRunner;

const FAKE_PID: u32 = 4242;
const CONTROLLERS: [&str; 3] = ["memory", "cpu", "pids"];

/// A temp directory standing in for the cgroup2 mount, plus a fake
/// `/proc/self/cgroup` holding `proc_cgroup`.
struct Host {
    _dir: tempfile::TempDir,
    mount: PathBuf,
    proc_cgroup: PathBuf,
}

fn host(proc_cgroup: &str) -> Host {
    let dir = tempfile::tempdir().expect("a temp directory");
    let mount = dir.path().join("mount");
    fs::create_dir(&mount).expect("the mount directory");
    // The file only a cgroup2 root has.
    fs::write(mount.join("cgroup.controllers"), "cpu memory pids\n").expect("cgroup.controllers");
    let proc_cgroup_file = dir.path().join("proc-self-cgroup");
    fs::write(&proc_cgroup_file, proc_cgroup).expect("the fake /proc/self/cgroup");
    Host {
        _dir: dir,
        mount,
        proc_cgroup: proc_cgroup_file,
    }
}

fn detect_on(host: &Host) -> Cgroups {
    detect_at(&host.proc_cgroup, &host.mount, FAKE_PID)
}

#[test]
fn a_v2_line_on_a_writable_cgroup_is_available() {
    let host = host("0::/shep.slice\n");
    let own = host.mount.join("shep.slice");
    fs::create_dir(&own).expect("the shepherd's own cgroup");

    let found = detect_on(&host);

    assert_eq!(found, Cgroups::Available { base: own.clone() });
    let leaf = own.join("shepherd");
    assert!(leaf.is_dir(), "the leaf is created under the base");
    assert_eq!(
        fs::read_to_string(leaf.join("cgroup.procs")).expect("the leaf's cgroup.procs"),
        FAKE_PID.to_string(),
        "the shepherd's pid is written into the leaf"
    );
    let enabled = fs::read_to_string(own.join("cgroup.subtree_control"))
        .expect("the base's cgroup.subtree_control");
    for controller in CONTROLLERS {
        assert!(
            enabled.contains(&format!("+{controller}")),
            "{controller} is enabled, got {enabled:?}"
        );
    }
}

#[test]
fn a_v1_or_hybrid_host_is_not_v2() {
    let v1 = "12:memory:/user.slice\n11:cpu,cpuacct:/user.slice\n1:name=systemd:/user.slice\n";
    let hybrid = "1:name=systemd:/user.slice\n0::/user.slice\n";
    for proc_cgroup in [v1, hybrid, ""] {
        let host = host(proc_cgroup);
        fs::create_dir(host.mount.join("user.slice")).expect("a cgroup directory");

        assert_eq!(
            detect_on(&host),
            Cgroups::Unavailable(Reason::NotV2),
            "for {proc_cgroup:?}"
        );
    }
}

#[test]
fn a_v2_line_without_a_cgroup2_mount_is_not_v2() {
    let host = host("0::/shep.slice\n");
    fs::remove_file(host.mount.join("cgroup.controllers")).expect("remove the cgroup2 marker");

    assert_eq!(detect_on(&host), Cgroups::Unavailable(Reason::NotV2));
}

#[test]
fn a_read_only_cgroup_is_not_delegated() {
    let host = host("0::/shep.slice\n");
    let own = host.mount.join("shep.slice");
    fs::create_dir(&own).expect("the shepherd's own cgroup");
    fs::set_permissions(&own, fs::Permissions::from_mode(0o555)).expect("make it read-only");
    // Root writes anywhere, which is the delegated case this test is not about.
    if fs::create_dir(own.join("probe")).is_ok() {
        println!("skipped: this user can write a 0555 directory, so it is root or the like");
        return;
    }

    assert_eq!(detect_on(&host), Cgroups::Unavailable(Reason::NotDelegated));
    assert!(!own.join("shepherd").exists(), "no leaf is left behind");
}

#[test]
fn a_cgroup_holding_another_process_is_busy_and_the_shepherd_stays_put() {
    let host = host("0::/shep.slice\n");
    let own = host.mount.join("shep.slice");
    fs::create_dir(&own).expect("the shepherd's own cgroup");
    // The shepherd and the CLI that started it, as in `shep start`.
    fs::write(own.join("cgroup.procs"), format!("{FAKE_PID}\n999\n")).expect("cgroup.procs");

    assert_eq!(detect_on(&host), Cgroups::Unavailable(Reason::Busy));
    assert!(!own.join("shepherd").exists(), "the shepherd is not moved");
}

#[test]
fn a_cgroup_holding_only_the_shepherd_is_not_busy() {
    let host = host("0::/shep.slice\n");
    let own = host.mount.join("shep.slice");
    fs::create_dir(&own).expect("the shepherd's own cgroup");
    fs::write(own.join("cgroup.procs"), format!("{FAKE_PID}\n")).expect("cgroup.procs");

    assert_eq!(detect_on(&host), Cgroups::Available { base: own });
}

#[test]
fn a_leaf_that_was_already_there_is_not_removed_when_the_move_fails() {
    let host = host("0::/shep.slice\n");
    let own = host.mount.join("shep.slice");
    let leaf = own.join("shepherd");
    fs::create_dir_all(&leaf).expect("an existing leaf");
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o555)).expect("make it read-only");
    if fs::write(leaf.join("probe"), "").is_ok() {
        println!("skipped: this user can write a 0555 directory, so it is root or the like");
        return;
    }

    assert_eq!(detect_on(&host), Cgroups::Unavailable(Reason::NotDelegated));
    assert!(leaf.is_dir(), "a leaf this call did not create stays");
}

#[test]
fn a_shepherd_already_in_its_leaf_finds_the_same_base() {
    let host = host("0::/shep.slice\n");
    let own = host.mount.join("shep.slice");
    fs::create_dir(&own).expect("the shepherd's own cgroup");
    let first = detect_on(&host);

    // After `shep daemon reload`'s `execve` the same pid reads its leaf back.
    fs::write(&host.proc_cgroup, "0::/shep.slice/shepherd\n").expect("the leaf line");
    let second = detect_on(&host);

    assert_eq!(first, Cgroups::Available { base: own });
    assert_eq!(second, first);
}

#[test]
fn a_shepherd_in_the_root_cgroup_takes_the_mount_as_its_base() {
    let host = host("0::/\n");

    assert_eq!(
        detect_on(&host),
        Cgroups::Available {
            base: host.mount.clone()
        }
    );
}

/// Moves this test process, so it runs only when `SHEP_TEST_CGROUPS` is set,
/// which the `cgroups` CI job does under a delegated scope.
#[test]
fn a_real_delegated_subtree_moves_the_shepherd_into_its_leaf() {
    if std::env::var_os("SHEP_TEST_CGROUPS").is_none() {
        println!("skipped: SHEP_TEST_CGROUPS is not set, so this would move the test process");
        return;
    }
    let before = fs::read_to_string("/proc/self/cgroup").expect("/proc/self/cgroup");
    let found = detect();
    let Cgroups::Available { base } = found else {
        println!("skipped: no delegated cgroup v2 subtree here: {found}");
        return;
    };

    let after = fs::read_to_string("/proc/self/cgroup").expect("/proc/self/cgroup");
    let line = after.trim_end().strip_prefix("0::").expect("a v2 line");
    let leaf = Path::new("/sys/fs/cgroup").join(line.trim_start_matches('/'));
    assert_eq!(leaf, base.join("shepherd"), "after: {after:?}");
    let enabled = fs::read_to_string(base.join("cgroup.subtree_control"))
        .expect("the base's cgroup.subtree_control");
    for controller in CONTROLLERS {
        assert!(enabled.split_whitespace().any(|c| c == controller));
    }

    // Leave the process where it started, so a later test sees the same cgroup.
    let _ = fs::write(base.join("cgroup.subtree_control"), "-memory -cpu -pids");
    let origin = before.trim_end().strip_prefix("0::").unwrap_or("/");
    let origin = Path::new("/sys/fs/cgroup").join(origin.trim_start_matches('/'));
    let _ = fs::write(origin.join("cgroup.procs"), std::process::id().to_string());
    let _ = fs::remove_dir(&leaf);
}

/// How long a real child gets to do what the test waits for: slack for a
/// loaded runner, not an expected duration.
const DEADLINE: Duration = Duration::from_secs(10);

/// A spawn of `sh -c script` logging under `dir`, joining `cgroup`.
fn sh(dir: &Path, script: &str, cgroup: Option<PathBuf>) -> SpawnSpec {
    SpawnSpec {
        name: "web".to_string(),
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        env: BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        out_file: dir.join("out.log"),
        err_file: dir.join("err.log"),
        cgroup,
        ..SpawnSpec::default()
    }
}

/// The base of a real delegated subtree, or `None` after printing why not.
///
/// Gated like `a_real_delegated_subtree_moves_the_shepherd_into_its_leaf`,
/// whose reason holds here: `detect` moves the test process.
fn delegated_base() -> Option<PathBuf> {
    if std::env::var_os("SHEP_TEST_CGROUPS").is_none() {
        println!("skipped: SHEP_TEST_CGROUPS is not set, so this would move the test process");
        return None;
    }
    match detect() {
        Cgroups::Available { base } => Some(base),
        other => {
            println!("skipped: no delegated cgroup v2 subtree here: {other}");
            None
        }
    }
}

fn pids(procs: &Path) -> Vec<u32> {
    fs::read_to_string(procs)
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

#[tokio::test]
async fn a_joining_child_writes_zero_to_its_cgroup_procs_before_it_runs() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let cgroup = sheep_dir(dir.path(), 1, 0);
    fs::create_dir(&cgroup).expect("the sheep's cgroup");
    fs::write(cgroup.join("cgroup.procs"), "").expect("an empty cgroup.procs");
    let marker = dir.path().join("ran");
    let script = format!(
        "test \"$(cat '{}')\" = 0 && touch '{}'",
        cgroup.join("cgroup.procs").display(),
        marker.display()
    );

    let (mut proc, _io) = TokioRunner::new()
        .spawn(&sh(dir.path(), &script, Some(cgroup)))
        .expect("the spawn");
    let exit = timeout(DEADLINE, proc.wait())
        .await
        .expect("the child exits");

    assert_eq!(
        exit.code,
        Some(0),
        "the write had landed when the script read it"
    );
    assert!(marker.exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("sheep-1-0/memory.oom.group")).expect("oom.group"),
        "1"
    );
}

#[tokio::test]
async fn a_failed_join_fails_the_spawn_and_runs_nothing() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let cgroup = sheep_dir(dir.path(), 1, 0);
    fs::create_dir(&cgroup).expect("the sheep's cgroup");
    // Opens, then refuses every write with ENOSPC, as a cgroup that cannot take the process.
    std::os::unix::fs::symlink("/dev/full", cgroup.join("cgroup.procs")).expect("the symlink");
    let marker = dir.path().join("ran");
    let script = format!("touch '{}'", marker.display());

    let failed = TokioRunner::new()
        .spawn(&sh(dir.path(), &script, Some(cgroup)))
        .expect_err("the join fails");

    let RunnerError::SpawnFailed(message) = failed else {
        panic!("expected SpawnFailed, got {failed:?}");
    };
    assert!(message.contains("web"), "names the sheep: {message}");
    assert!(!marker.exists(), "the program never ran");
}

#[tokio::test]
async fn a_cgroup_that_already_holds_a_process_is_not_joined() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let cgroup = sheep_dir(dir.path(), 1, 0);
    fs::create_dir(&cgroup).expect("the sheep's cgroup");
    // A sheep that outlived the shepherd which started it.
    fs::write(cgroup.join("cgroup.procs"), "4242\n").expect("a surviving process");
    let marker = dir.path().join("ran");
    let script = format!("touch '{}'", marker.display());

    let failed = TokioRunner::new()
        .spawn(&sh(dir.path(), &script, Some(cgroup.clone())))
        .expect_err("the cgroup is taken");

    let RunnerError::SpawnFailed(message) = failed else {
        panic!("expected SpawnFailed, got {failed:?}");
    };
    assert!(message.contains("web"), "names the sheep: {message}");
    assert!(!marker.exists(), "the program never ran");
    assert!(cgroup.is_dir(), "the survivor keeps its cgroup");
}

#[tokio::test]
async fn an_unavailable_host_spawns_as_before() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let dir = tempfile::tempdir().expect("a temp directory");
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events)
            .cgroups(Cgroups::Unavailable(Reason::NotV2))
            .spawn();

    handle
        .start(vec![
            normalize(AppConfig::minimal("web", "./srv")).expect("a config"),
        ])
        .await
        .expect("the start");

    assert_eq!(runner.spawned_cgroup(0), None);
}

#[tokio::test]
async fn an_available_host_names_each_sheeps_cgroup_by_id_and_instance() {
    let (events, _rx) = crate::bus::test_bus(64);
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::never_exits(),
        ProcScript::never_exits(),
    ]));
    let dir = tempfile::tempdir().expect("a temp directory");
    let base = dir.path().join("base");
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events)
            .cgroups(Cgroups::Available { base: base.clone() })
            .spawn();
    let mut app = AppConfig::minimal("web", "./srv");
    app.instances = 2;

    handle
        .start(vec![normalize(app).expect("a config")])
        .await
        .expect("the start");

    assert_eq!(runner.spawned_cgroup(0), Some(base.join("sheep-0-0")));
    assert_eq!(runner.spawned_cgroup(1), Some(base.join("sheep-1-1")));
}

#[tokio::test(start_paused = true)]
async fn a_sheep_that_exits_has_its_cgroup_removed() {
    let (events, mut rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(vec![ProcScript::const_exit(0)]);
    let dir = tempfile::tempdir().expect("a temp directory");
    let base = dir.path().join("base");
    let cgroup = sheep_dir(&base, 0, 0);
    fs::create_dir_all(&cgroup).expect("the sheep's cgroup");
    let handle = SupervisorBuilder::new(runner, test_paths(&dir), events)
        .cgroups(Cgroups::Available { base })
        .spawn();
    let mut app = AppConfig::minimal("job", "./job");
    app.stop_exit_codes = vec![0];

    handle
        .start(vec![normalize(app).expect("a config")])
        .await
        .expect("the start");
    while let Ok(event) = rx.recv().await {
        if matches!(
            event.to_event(),
            BusEvent::Process {
                event: ProcessEventKind::Stop,
                ..
            }
        ) {
            break;
        }
    }

    assert!(!cgroup.exists(), "removed once the sheep was reaped");
}

#[test]
fn a_cgroup_holding_a_process_is_not_removed() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let cgroup = sheep_dir(dir.path(), 3, 0);
    fs::create_dir(&cgroup).expect("the sheep's cgroup");
    fs::write(cgroup.join("cgroup.procs"), "4242\n").expect("cgroup.procs");

    remove_if_empty(&cgroup).expect("a busy cgroup is left, not an error");

    assert!(cgroup.is_dir());
}

#[test]
fn a_cgroup_procs_that_never_ends_is_read_as_holding_a_process() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let cgroup = sheep_dir(dir.path(), 3, 0);
    fs::create_dir(&cgroup).expect("the sheep's cgroup");
    // Endless, as `/dev/full` is: the check must not read it to the end.
    std::os::unix::fs::symlink("/dev/zero", cgroup.join("cgroup.procs")).expect("the symlink");

    remove_if_empty(&cgroup).expect("left, not an error");

    assert!(cgroup.is_dir());
}

#[test]
fn boot_removes_only_the_sheep_cgroups_a_crash_left_empty() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let base = dir.path();
    let stale = sheep_dir(base, 1, 0);
    let live = sheep_dir(base, 2, 1);
    let leaf = base.join("shepherd");
    let stranger = base.join("sheep-x-1");
    for cgroup in [&stale, &live, &leaf, &stranger] {
        fs::create_dir(cgroup).expect("a cgroup directory");
    }
    fs::write(live.join("cgroup.procs"), "4242\n").expect("a live sheep's process");

    remove_stale(base);

    assert!(!stale.exists(), "an empty sheep cgroup goes");
    assert!(
        live.is_dir(),
        "a sheep that outlived the crash keeps its cgroup"
    );
    assert!(leaf.is_dir(), "the shepherd's own leaf is not a sheep's");
    assert!(
        stranger.is_dir(),
        "a name sheep_dir never makes is left alone"
    );
}

#[tokio::test]
async fn a_sheep_that_forks_before_exec_has_every_pid_in_its_cgroup() {
    let Some(base) = delegated_base() else {
        return;
    };
    let dir = tempfile::tempdir().expect("a temp directory");
    let cgroup = sheep_dir(&base, 9001, 0);
    let spec = sh(dir.path(), "sleep 30 & exec sleep 30", Some(cgroup.clone()));

    let (mut proc, _io) = TokioRunner::new().spawn(&spec).expect("the spawn");
    let procs = cgroup.join("cgroup.procs");
    // Real clock: the background `sleep` forks a moment after the spawn, so the
    // listing is polled until it holds both, and `timeout` bounds the wait.
    let listed = timeout(DEADLINE, async {
        loop {
            let listed = pids(&procs);
            if listed.len() >= 2 {
                return listed;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both processes appear in the cgroup");

    assert!(
        listed.contains(&proc.pid()),
        "the leader is in it: {listed:?}"
    );
    assert_eq!(listed.len(), 2, "the forked sleep is in it too: {listed:?}");

    proc.kill_tree().expect("kill the group");
    timeout(DEADLINE, proc.wait())
        .await
        .expect("the leader exits");
    let _ = timeout(DEADLINE, async {
        while remove_if_empty(&cgroup).is_ok() && cgroup.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn the_cgroup_is_removed_after_the_sheep_is_reaped() {
    let Some(base) = delegated_base() else {
        return;
    };
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().expect("a temp directory");
    let handle = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .cgroups(Cgroups::Available { base: base.clone() })
        .spawn();
    let mut app = AppConfig::minimal("web", "/bin/sleep");
    app.args = vec!["30".to_string()];
    let started = handle
        .start(vec![normalize(app).expect("a config")])
        .await
        .expect("the start");
    let cgroup = sheep_dir(&base, started[0].id, 0);
    assert!(
        pids(&cgroup.join("cgroup.procs")).contains(&started[0].pid.expect("a pid")),
        "the sheep is in its cgroup while it runs"
    );

    timeout(
        DEADLINE,
        handle.stop(ProcessSelector::Name("web".to_string())),
    )
    .await
    .expect("the stop answers")
    .expect("the stop");

    assert!(
        !cgroup.exists(),
        "gone once the sheep was reaped and the group empty"
    );
}
