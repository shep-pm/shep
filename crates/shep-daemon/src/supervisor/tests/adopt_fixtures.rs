//! Fixtures for the adoption cases: a runner that takes an inherited sheep
//! without a process behind it, the blob entries it adopts, and the real
//! children the slower cases take over.

use super::*;

/// Two descriptors standing in for the daemon's own, so a snapshot taken
/// in a test names numbers that are really open. The actor never learns
/// the real listener and pidfile, so they are an argument.
#[cfg(unix)]
pub(super) fn daemon_fds(dir: &tempfile::TempDir) -> (DaemonFds, [std::fs::File; 2]) {
    use std::os::fd::AsRawFd as _;

    let listener = std::fs::File::create(dir.path().join("listener.stand-in")).unwrap();
    let pidfile = std::fs::File::create(dir.path().join("pidfile.stand-in")).unwrap();
    let fds = DaemonFds {
        listener: listener.as_raw_fd(),
        pidfile: pidfile.as_raw_fd(),
    };
    // Returned alongside so the caller holds both files open: a closed
    // descriptor's number is free to be handed to the next open.
    (fds, [listener, pidfile])
}

/// A runner that takes an inherited sheep without a real process behind
/// it, so the install path can be driven under the paused clock.
///
/// `wait` never resolves: these cases assert on what an install puts in
/// the flock, and an exit that arrived on its own would race them.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(super) struct AdoptingRunner;

/// The pid [`AdoptingRunner`] gives anything it spawns fresh. Not a pid
/// any carried sheep in these cases holds.
#[cfg(unix)]
pub(super) const STAND_IN_SPAWN_PID: u32 = 7000;

/// A proc with a pid and no process: it reports what it was built with
/// and never exits.
#[cfg(unix)]
#[derive(Debug)]
pub(super) struct StandInProc {
    pid: u32,
}

#[cfg(unix)]
impl RunningProcess for StandInProc {
    fn pid(&self) -> u32 {
        self.pid
    }

    async fn wait(&mut self) -> ExitOutcome {
        core::future::pending().await
    }

    fn signal(&mut self, _sig: crate::runner::StopSignal) -> Result<(), RunnerError> {
        Ok(())
    }

    fn kill_tree(&mut self) -> Result<(), RunnerError> {
        Ok(())
    }
}

/// Four channels shaped the way an adopted sheep's are: logs and
/// shepherd traffic closed, since nothing here writes either.
#[cfg(unix)]
fn stand_in_io() -> ProcIo {
    let (_logs_tx, logs) = mpsc::channel(1);
    let (_from_child_tx, from_child) = mpsc::channel(1);
    ProcIo {
        logs,
        from_child,
        to_child: closed_tx(),
        log_ctl: closed_tx(),
        to_stdin: closed_tx(),
    }
}

/// A sender with nothing on the far end of it, so every send fails at once.
#[cfg(unix)]
fn closed_tx<T>() -> mpsc::Sender<T> {
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    tx
}

#[cfg(unix)]
impl ProcessRunner for AdoptingRunner {
    type Proc = StandInProc;

    fn spawn(&self, _spec: &SpawnSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        Ok((
            StandInProc {
                pid: STAND_IN_SPAWN_PID,
            },
            stand_in_io(),
        ))
    }

    fn adopt(&self, spec: crate::runner::AdoptSpec) -> Result<(Self::Proc, ProcIo), RunnerError> {
        Ok((StandInProc { pid: spec.pid }, stand_in_io()))
    }
}

/// One sheep as a blob describes it, with `mutate` free to give it a
/// history no fresh registration could have.
#[cfg(unix)]
pub(super) fn carried(
    name: &str,
    id: u32,
    pid: Option<u32>,
    mutate: impl FnOnce(&mut ProcessEntry),
) -> CarriedSheep {
    carried_marked(name, id, pid, false, None, false, mutate)
}

/// [`carried`], with the slot facts a blob carries beside the entry set
/// explicitly: a pending delete, the manual command that owns this
/// sheep's next exit, and an earlier reload's failed readiness verdict.
#[cfg(unix)]
pub(super) fn carried_marked(
    name: &str,
    id: u32,
    pid: Option<u32>,
    pending_delete: bool,
    manual: Option<PendingManual>,
    ready_failed: bool,
    mutate: impl FnOnce(&mut ProcessEntry),
) -> CarriedSheep {
    // Nothing here wants a respawn: an automatic restart would spawn a
    // second process behind the assertions.
    let spec = app_with(name, |app| app.autorestart = false);
    let mut entry = ProcessEntry {
        pid,
        ..plain_entry(id, spec)
    };
    mutate(&mut entry);
    CarriedSheep::from_entry(
        &entry,
        0,
        CarriedFds::none(),
        pending_delete,
        manual,
        ready_failed,
        None,
    )
}

/// A carried sheep with no descriptors to rebuild, for every case below that opens no pipes.
#[cfg(unix)]
pub(super) fn without_handles(carried: CarriedSheep) -> crate::handover::adopt::AdoptedSheep {
    crate::handover::adopt::AdoptedSheep {
        carried,
        out_pipe: None,
        err_pipe: None,
        out_log: None,
        err_log: None,
        stdin_pipe: None,
        channel: None,
        dog_link: None,
    }
}

/// Counters as a blob carries them, with `next_id` the one a case cares
/// about.
#[cfg(unix)]
pub(super) const fn counters(next_id: u32) -> Counters {
    Counters {
        next_id,
        next_deadline: 0,
        next_action_stamp: 0,
    }
}

/// A real child for the adoption cases below to take over, running `script`
/// under `/bin/sh`, and its pid.
///
/// Its own process group is load-bearing: `TokioProc::signal` and `kill_tree`
/// both address the group, so a child that inherited this test binary's group
/// is not a group leader, `killpg` answers `ESRCH`, and the ladder delivers
/// nothing. The `Child` handle is dropped, not waited on.
#[cfg(unix)]
pub(super) fn adoptable_child(script: &str) -> u32 {
    use std::os::unix::process::CommandExt as _;

    std::process::Command::new("/bin/sh")
        .args(["-c", script])
        .process_group(0)
        // Null, not inherited: under `cargo test ... | <anything>` the
        // harness's stdout is a pipe, and a child that outlives a failing
        // case holds it open, turning the assertion into a hang.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("a test host can spawn a shell")
        .id()
}

/// The app an adopted sheep in these cases runs, respawnable for real:
/// `mutate` gets it before it is normalized onto the entry.
#[cfg(unix)]
pub(super) fn respawnable(mutate: impl FnOnce(&mut AppConfig)) -> impl FnOnce(&mut ProcessEntry) {
    move |entry: &mut ProcessEntry| {
        entry.spec = app_with("web", |app| {
            // Real: one case lets the successor respawn the sheep and asserts
            // on the pid it comes back with. `./srv` would land in `Errored`.
            app.script = "/bin/sh".to_owned();
            app.args = vec!["-c".to_owned(), "sleep 30".to_owned()];
            mutate(app);
        });
    }
}

/// Polls the flock until `done` accepts it, or fails the case.
///
/// Real children and real signals, so most cases run on a real clock: nothing
/// here can advance a paused one on the child's behalf. A paused clock runs
/// the same bound, since the sleep between polls lets it auto-advance. The
/// bound sits past anything these cases ask for, so a stall fails rather
/// than hangs.
#[cfg(unix)]
pub(super) async fn flock_until(
    sup: &SupervisorHandle,
    done: impl Fn(&[ProcessInfo]) -> bool,
    what: &str,
) -> Vec<ProcessInfo> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let info = sup.list().await;
            if done(&info) {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"))
}
