//! Running the real binary, and [`DaemonGuard`], which owns every
//! `$SHEP_HOME` a case touched and sweeps the flock when it drops.

use super::*;

/// A `shep --home <home>` invocation, timeout already attached. Every case
/// below appends its own verb and flags, then `.output()`s it.
pub(crate) fn shep(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("shep").unwrap();
    cmd.arg("--home").arg(home).timeout(CMD_TIMEOUT);
    cmd
}

/// Asserts `output` exited `Success`, printing stderr on failure so a
/// red run names the actual cause instead of just "assertion failed".
pub(crate) fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "expected success, got {:?}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Best-effort graceful shutdown, called at the end of a test's success path.
///
/// [`DaemonGuard`]'s sweep is gated on `std::thread::panicking()`, so on a
/// success path nothing but this reaps the sheep. Without it a run leaks one
/// orphaned `sleep` per sheep started.
pub(crate) fn graceful_kill(home: &Path) {
    let _ = shep(home).arg("kill").output();
}

#[cfg(unix)]
/// Boots a daemon on `dir`'s `$SHEP_HOME` with `env` set on the `shep start`
/// that autostarts it, waits for [`write_never_ready_flockfile`]'s sheep to
/// give up, and hands back the daemon's own log.
///
/// `launch::launch_command` does not `.env_clear()` the re-exec, so `env`
/// reaches the child that installs the subscriber. Waiting for `online` orders
/// the read: `handle_ready_result` writes [`READINESS_RECORD`] before it sets
/// the status. The daemon is killed before the log is returned, so a caller's
/// assertion can panic without leaking a supervisor.
pub(crate) fn daemon_log_after_a_missed_handshake(dir: &TempDir, env: &[(&str, &str)]) -> String {
    let home = dir.path();
    let flockfile = write_never_ready_flockfile(dir);
    let mut guard = DaemonGuard::default();

    let mut start = shep(home);
    for (key, value) in env {
        start.env(key, value);
    }
    let boot = start.arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let online = poll_flock(home, |info| info["status"] == "online");
    assert_eq!(
        online["status"], "online",
        "a wait_ready sheep that never signals must still be taken online once \
         its listen_timeout elapses, which is the record's own trigger: {online}"
    );

    let log = std::fs::read_to_string(home.join("logs").join("shepd.err.log")).unwrap();
    graceful_kill(home);
    log
}

/// A `$SHEP_HOME` whose daemon and whole flock this test is responsible for,
/// reaped on `Drop` even if the test panics first.
///
/// Every sheep has its own process group, so killing the daemon does not reach
/// one; [`record_pid_line`] has each fixture script record its pid so this
/// guard can reach a flock it cannot enumerate over RPC.
///
/// Two orderings are load-bearing. The daemon dies first, or the restart brain
/// brings each killed sheep back. The sweep runs only while panicking: on a
/// success path [`graceful_kill`] has proven these pids gone, and the OS may
/// have recycled them. `Drop` must not panic, so an unreachable daemon is
/// reported with `eprintln!`.
#[derive(Debug, Default)]
pub(crate) struct DaemonGuard {
    pub(crate) homes: Vec<PathBuf>,
    /// Dogs adopted by pid, reaped individually because they are in no home's
    /// flock. Unix only: the Windows arm reaps through `shep kill` and its job
    /// objects.
    #[cfg(unix)]
    pub(crate) dog_pids: Vec<nix::unistd::Pid>,
}

impl DaemonGuard {
    /// Register a `$SHEP_HOME` whose daemon this test is responsible for.
    ///
    /// Call it immediately after `.output()` and before the assertion on
    /// `output.status`: registering after it leaks the daemon in exactly the
    /// failed-autostart case where one is most likely.
    pub(crate) fn adopt_home(&mut self, home: &Path) {
        self.homes.push(home.to_path_buf());
    }

    /// Register a dog's own pid, a grandchild whose process group sits outside
    /// the daemon's and so survives `kill_group_of(daemon_pid)` untouched.
    /// Call it as soon as the pid is known, by [`Self::adopt_home`]'s ordering.
    #[cfg(unix)]
    pub(crate) fn adopt_dog_pid(&mut self, pid: nix::unistd::Pid) {
        self.dog_pids.push(pid);
    }
}

impl Drop for DaemonGuard {
    /// On Windows the whole sweep collapses into `shep kill`: a job object
    /// takes the flock with the daemon. The unix arm exists because a sheep
    /// that outlives its daemon is an orphan only `kill(-pgid)` reaps.
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            for home in &self.homes {
                // Best-effort: a guard runs on a panic path where the daemon
                // may already be gone.
                let _ = std::process::Command::new(assert_cmd::cargo::cargo_bin("shep"))
                    .arg("--home")
                    .arg(home)
                    .arg("kill")
                    .output();
            }
        }

        #[cfg(unix)]
        {
            let panicking = std::thread::panicking();
            for home in &self.homes {
                match daemon_pid(home) {
                    Some(pid) => kill_group_of(pid),
                    // A success path unlinks the pidfile as its last act, so
                    // this is "already gone", not "never wrote one".
                    None if !panicking => {}
                    // On the panic path the case may have died inside the
                    // empty-pidfile window GUARD_PID_DEADLINE covers.
                    None => match wait_for_daemon_pid(home) {
                        Some(pid) => kill_group_of(pid),
                        None => eprintln!(
                            "DaemonGuard: no parseable daemon pid at {} after {GUARD_PID_DEADLINE:?}; \
                         if a daemon is still up it was NOT reaped",
                            home.display()
                        ),
                    },
                }

                if !panicking {
                    continue;
                }
                sweep_flock(home);
            }

            for pid in &self.dog_pids {
                kill_group_of(*pid);
            }
        }
    }
}

#[cfg(unix)]
/// SIGKILLs every process group named in `home`'s [`FIXTURE_PIDS`], resweeping
/// until [`GUARD_SWEEP_WINDOW`] expires.
///
/// A sheep records its pid as its script's first line, but `shep start`
/// reports `Online` off the spawn, so a case that panics straight after it
/// reaches here with the pid file still empty. Bounded rather than convergent:
/// no case tells this guard how many sheep to expect.
///
/// The daemon must already be dead: a sheep killed under a live supervisor is
/// one the restart brain brings straight back.
pub(crate) fn sweep_flock(home: &Path) {
    let start = Instant::now();
    loop {
        for pid in recorded_fixture_pids(home) {
            // `-pid`: every recorded pid leads its own group, so this
            // reaches its lambs. Re-signalling a dead one is an ESRCH no-op.
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-pid.as_raw()),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        if start.elapsed() >= GUARD_SWEEP_WINDOW {
            return;
        }
        std::thread::sleep(GUARD_PID_POLL_INTERVAL);
    }
}

#[cfg(unix)]
/// One non-blocking attempt at the daemon pid recorded at `home`.
pub(crate) fn daemon_pid(home: &Path) -> Option<nix::unistd::Pid> {
    let text = std::fs::read_to_string(home.join("pids").join("shepd.pid")).ok()?;
    let raw: i32 = text.trim().parse().ok()?;
    Some(nix::unistd::Pid::from_raw(raw))
}

#[cfg(unix)]
/// [`daemon_pid`], retried until it answers or [`GUARD_PID_DEADLINE`] expires.
/// A live daemon fills the pidfile in `PidfileLock::record`; one that never
/// fills it has already exited.
pub(crate) fn wait_for_daemon_pid(home: &Path) -> Option<nix::unistd::Pid> {
    let start = Instant::now();
    loop {
        if let Some(pid) = daemon_pid(home) {
            return Some(pid);
        }
        if start.elapsed() >= GUARD_PID_DEADLINE {
            return None;
        }
        std::thread::sleep(GUARD_PID_POLL_INTERVAL);
    }
}

#[cfg(unix)]
/// SIGKILLs `pid`'s process group, or `pid` alone if it does not lead one.
///
/// Leadership is checked, not assumed: `-pid` against a non-leader reaches
/// somebody else's group, which in a test runner holds the harness. `ESRCH`
/// from `getpgid` means already reaped, and the leader-only fallback is then a
/// no-op.
pub(crate) fn kill_group_of(pid: nix::unistd::Pid) {
    let target = match nix::unistd::getpgid(Some(pid)) {
        Ok(pgid) if pgid == pid => nix::unistd::Pid::from_raw(-pid.as_raw()),
        _ => pid,
    };
    // ESRCH on an already-reaped daemon is the expected happy path.
    let _ = nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGKILL);
}

#[cfg(unix)]
/// Every pid a fixture script recorded under `home`, in spawn order.
///
/// A missing file means the case started no sheep. An unparseable line is
/// skipped: this runs on a path that is already failing.
pub(crate) fn recorded_fixture_pids(home: &Path) -> Vec<nix::unistd::Pid> {
    let Ok(text) = std::fs::read_to_string(home.join(FIXTURE_PIDS)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| line.trim().parse::<i32>().ok())
        .map(nix::unistd::Pid::from_raw)
        .collect()
}

#[cfg(unix)]
/// Reads the daemon pid recorded at `home`'s pidfile, the same path
/// `shep_daemon::boot::pidfile` builds.
pub(crate) fn read_daemon_pid(home: &Path) -> nix::unistd::Pid {
    let path = home.join("pids").join("shepd.pid");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("no pidfile at {}: {e}", path.display()));
    let raw: i32 = text
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("bad pidfile contents {text:?}: {e}"));
    nix::unistd::Pid::from_raw(raw)
}

#[cfg(unix)]
/// Asserts `pid` is the leader of its own process group, the
/// `Command::process_group(0)` contract `launch.rs` relies on to detach the
/// daemon from the parent's group and terminal. `std::process::Command`
/// exposes no getter for this, so a real spawn is the only check.
pub(crate) fn assert_group_leader(pid: nix::unistd::Pid) {
    assert_eq!(
        nix::unistd::getpgid(Some(pid)).unwrap(),
        pid,
        "the daemon must be its own process-group leader"
    );
}

/// Copies `source` to nowhere, on a background thread. An undrained pipe
/// fills and blocks the child.
pub(crate) fn discard_in_background<R: Read + Send + 'static>(mut source: R) {
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut source, &mut std::io::sink());
    });
}
