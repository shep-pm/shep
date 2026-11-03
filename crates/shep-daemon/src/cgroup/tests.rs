use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use shep_core::config::{AppConfig, CpuCores, MemoryEnforce, normalize};
use shep_core::protocol::{BusEvent, ProcessEventKind};
use shep_core::selector::ProcessSelector;
use shep_core::status::ProcStatus;
use shep_core::values::{MemSize, UpDuration};
use tokio::time::timeout;

use super::{
    Cgroups, Reason, cpu_quota, detect, detect_at, oom_kills, remove_if_empty, remove_stale,
    sheep_dir, write_cpu_max, write_memory_max,
};
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
    assert!(
        message.contains("No space left"),
        "the child's write failed, not a refusal to try: {message}"
    );
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
fn an_endless_cgroup_procs_is_not_read_to_its_end() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let cgroup = sheep_dir(dir.path(), 3, 0);
    fs::create_dir(&cgroup).expect("the sheep's cgroup");
    // Zeros without end, as `/dev/full` reads: not a listing, and not to be read whole.
    std::os::unix::fs::symlink("/dev/zero", cgroup.join("cgroup.procs")).expect("the symlink");

    // Not taken for a process, so the removal is tried and the symlink in the
    // directory makes the filesystem refuse it.
    assert!(remove_if_empty(&cgroup).is_err());
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

// --- memory_enforce = "kernel" ---

/// A sheep `name` running `program`, capped by the kernel at `limit` bytes.
fn kernel_app(name: &str, program: &str, limit: u64) -> AppConfig {
    let mut app = AppConfig::minimal(name, program);
    app.max_memory = Some(MemSize::from_bytes(limit));
    app.memory_enforce = MemoryEnforce::Kernel;
    app
}

/// What a sheep's cgroup file holds, trailing newline off.
fn read(dir: &Path, file: &str) -> String {
    fs::read_to_string(dir.join(file))
        .unwrap_or_else(|error| panic!("{file}: {error}"))
        .trim_end()
        .to_string()
}

#[test]
fn memory_max_is_written_in_bytes_and_lifted_with_max() {
    let dir = tempfile::tempdir().expect("a temp directory");

    write_memory_max(dir.path(), Some(67_108_864)).expect("a cap");
    assert_eq!(read(dir.path(), "memory.max"), "67108864");
    assert_eq!(
        read(dir.path(), "memory.swap.max"),
        "0",
        "a sheep over its cap is killed, not swapped"
    );

    write_memory_max(dir.path(), None).expect("no cap");
    assert_eq!(read(dir.path(), "memory.max"), "max");
    assert_eq!(read(dir.path(), "memory.swap.max"), "max");
}

#[test]
fn a_cgroup_without_a_swap_file_still_takes_its_cap() {
    let dir = tempfile::tempdir().expect("a temp directory");
    // A directory where the swap file would be: the write fails, as on a
    // kernel with no swap accounting.
    fs::create_dir(dir.path().join("memory.swap.max")).expect("a blocker");

    write_memory_max(dir.path(), Some(4096)).expect("the cap holds without swap accounting");

    assert_eq!(read(dir.path(), "memory.max"), "4096");
}

#[test]
fn oom_kills_reads_the_local_counter_and_is_zero_without_one() {
    let dir = tempfile::tempdir().expect("a temp directory");
    assert_eq!(oom_kills(dir.path()), 0, "a cgroup not made yet");

    fs::write(
        dir.path().join("memory.events.local"),
        "low 0\nhigh 0\nmax 12\noom 3\noom_kill 2\noom_group_kill 1\n",
    )
    .expect("the events file");
    assert_eq!(oom_kills(dir.path()), 2);

    fs::write(dir.path().join("memory.events.local"), "low 0\nmax 12\n").expect("no oom_kill line");
    assert_eq!(oom_kills(dir.path()), 0);
}

#[test]
fn a_cap_is_written_before_the_child_joins() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let cgroup = sheep_dir(dir.path(), 1, 0);
    fs::create_dir(&cgroup).expect("the sheep's cgroup");
    fs::write(cgroup.join("cgroup.procs"), "").expect("an empty cgroup.procs");
    let mut command = std::process::Command::new("/bin/true");

    super::join_on_exec(&mut command, &cgroup, Some(1 << 20), None).expect("the join");

    assert_eq!(read(&cgroup, "memory.max"), "1048576");
}

/// Starts `app` on a supervisor over a plain directory standing in for the
/// delegated base, so a file the test writes is what the kernel would.
async fn start_over(
    cgroups: Cgroups,
    runner: Arc<ScriptedRunner>,
    app: AppConfig,
) -> (
    crate::supervisor::SupervisorHandle,
    Result<Vec<shep_core::protocol::ProcessInfo>, crate::supervisor::SupervisorError>,
    tempfile::TempDir,
) {
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().expect("a temp directory");
    let handle = SupervisorBuilder::new(SharedRunner(runner), test_paths(&dir), events)
        .cgroups(cgroups)
        .spawn();
    let started = handle.start(vec![normalize(app).expect("a config")]).await;
    (handle, started, dir)
}

#[tokio::test]
async fn a_kernel_enforced_spawn_is_asked_for_its_memory_max_and_a_polled_one_is_not() {
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
        kernel_app("capped", "./srv", 64 << 20),
    )
    .await;
    started.expect("the start");
    let mut polled = AppConfig::minimal("polled", "./srv");
    polled.max_memory = Some(MemSize::from_bytes(64 << 20));
    let (_handle, started, _dir) = start_over(cgroups, Arc::clone(&runner), polled).await;
    started.expect("the start");

    assert_eq!(runner.spawned_memory_max(0), Some(64 << 20));
    assert_eq!(runner.spawned_memory_max(1), None, "poll writes no cap");
}

#[tokio::test]
async fn an_unavailable_host_refuses_a_kernel_enforced_sheep_and_names_why() {
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));

    let (_handle, started, _dir) = start_over(
        Cgroups::Unavailable(Reason::NotDelegated),
        Arc::clone(&runner),
        kernel_app("capped", "./srv", 64 << 20),
    )
    .await;

    let refusal = started.expect_err("no silent fall back to polling");
    assert!(
        refusal
            .to_string()
            .contains("the shepherd's cgroup is not delegated to it"),
        "the refusal carries the reason detect gave: {refusal}"
    );
    assert_eq!(runner.spawn_count(), 0, "nothing ran uncapped");
}

/// Runs a kernel-enforced sheep whose every life stays up a second and exits
/// 137, raising its cgroup's `oom_kill` counter in each life when `oom` is set.
/// Returns its row once it is `Errored`, or once `lives` lives have spawned.
async fn run_lives(oom: bool, max_restarts: u32, lives: usize) -> shep_core::protocol::ProcessInfo {
    let base = tempfile::tempdir().expect("a temp directory");
    let cgroup = sheep_dir(base.path(), 0, 0);
    fs::create_dir_all(&cgroup).expect("the sheep's cgroup");
    let events = cgroup.join("memory.events.local");
    fs::write(&events, "oom_kill 0\n").expect("the counter");
    let runner = Arc::new(ScriptedRunner::new(vec![
        ProcScript::stable_then_exit(
            1000, 137
        );
        lives
    ]));
    let mut app = kernel_app("leak", "./leak", 16 << 20);
    app.max_restarts = max_restarts;
    // Every exit below ran past it, so only the OOM charge can park the sheep.
    app.min_uptime = UpDuration::from_millis(100);
    let (handle, started, _dir) = start_over(
        Cgroups::Available {
            base: base.path().to_path_buf(),
        },
        Arc::clone(&runner),
        app,
    )
    .await;
    started.expect("the start");

    for life in 1..=lives {
        // Written as soon as the life exists, a second before it exits, and
        // after the baseline its spawn read.
        while runner.spawn_count() < life {
            let row = handle.list().await.remove(0);
            if row.status == ProcStatus::Errored {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if oom {
            fs::write(&events, format!("oom_kill {life}\n")).expect("the counter");
        }
    }
    handle.list().await.remove(0)
}

#[tokio::test(start_paused = true)]
async fn an_oom_kill_in_the_sheeps_cgroup_counts_against_max_restarts_even_after_a_long_run() {
    let row = run_lives(true, 3, 6).await;

    assert_eq!(row.status, ProcStatus::Errored, "a leak parks");
    assert_eq!(row.restarts, 2, "after the budget of three lives");
}

#[tokio::test(start_paused = true)]
async fn a_long_run_with_no_rise_in_the_counter_is_not_charged() {
    let row = run_lives(false, 3, 6).await;

    assert_ne!(
        row.status,
        ProcStatus::Errored,
        "stable exits reset the budget"
    );
    assert!(row.restarts >= 5, "restarted {} times", row.restarts);
}

/// The memory cap a sheep's cgroup holds, as a real kernel keeps it.
#[tokio::test]
async fn a_kernel_enforced_sheep_gets_memory_max_and_a_live_change_rewrites_it() {
    let Some(base) = delegated_base() else {
        return;
    };
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().expect("a temp directory");
    let handle = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .cgroups(Cgroups::Available { base: base.clone() })
        .spawn();
    let mut app = kernel_app("web", "/bin/sleep", 64 << 20);
    app.args = vec!["30".to_string()];
    let started = handle
        .start(vec![normalize(app).expect("a config")])
        .await
        .expect("the start");
    let cgroup = sheep_dir(&base, started[0].id, 0);

    assert_eq!(read(&cgroup, "memory.max"), (64u64 << 20).to_string());

    handle
        .set_sheep_field("web", "max_memory", serde_json::json!("128M"))
        .await
        .expect("a live change");
    assert_eq!(
        read(&cgroup, "memory.max"),
        (128u64 << 20).to_string(),
        "the running sheep's cap followed the change"
    );

    handle
        .set_sheep_field("web", "memory_enforce", serde_json::json!("poll"))
        .await
        .expect("back to polling");
    assert_eq!(
        read(&cgroup, "memory.max"),
        "max",
        "a sheep that left kernel enforcement is uncapped"
    );

    timeout(
        DEADLINE,
        handle.stop(ProcessSelector::Name("web".to_string())),
    )
    .await
    .expect("the stop answers")
    .expect("the stop");
}

/// A sheep that allocates well past a small `max_memory`, from a real kernel.
#[tokio::test]
async fn an_oom_inside_the_sheeps_own_cgroup_is_recorded_and_counts_against_max_restarts() {
    let Some(base) = delegated_base() else {
        return;
    };
    let (events, _rx) = crate::bus::test_bus(64);
    let dir = tempfile::tempdir().expect("a temp directory");
    let handle = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .cgroups(Cgroups::Available { base })
        .spawn();
    let mut app = kernel_app("leak", "/bin/sh", 32 << 20);
    // A second of life before the leak, so the exit is "stable": only the OOM
    // charge stops the restarts.
    app.args = vec![
        "-c".to_string(),
        "/bin/sleep 1; x=$(/usr/bin/head -c 200000000 /dev/zero | /usr/bin/tr '\\0' a)".to_string(),
    ];
    app.max_restarts = 2;
    app.min_uptime = UpDuration::from_millis(100);
    handle
        .start(vec![normalize(app).expect("a config")])
        .await
        .expect("the start");

    let parked = timeout(DEADLINE, async {
        loop {
            let row = handle.list().await.remove(0);
            if row.status == ProcStatus::Errored {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a leak parks rather than looping");

    assert_eq!(
        parked.restarts, 1,
        "one restart, then the budget of two ran out"
    );
    assert_eq!(
        parked.last_exit.expect("an exit").signal,
        Some(9),
        "the kernel's kill"
    );
}

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

        super::join_on_exec(
            &mut std::process::Command::new("/bin/true"),
            &cgroup,
            None,
            quota,
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
