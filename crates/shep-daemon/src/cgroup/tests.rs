use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{Cgroups, Reason, detect, detect_at};

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
