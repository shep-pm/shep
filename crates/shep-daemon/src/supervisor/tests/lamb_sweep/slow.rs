//! The lamb sweep on real processes: a real runner, the real extras, and a
//! lamb that left its sheep's session and ignores `SIGTERM`.
//!
//! Real children answer real signals on the wall clock, so every case runs
//! on a real one, bounded by [`BOUND`]. A case skips with a message on a
//! host with no `sleep`, or no perl, python3 or `setsid` to detach a lamb.

use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use super::*;
use crate::extras::{Extras, LivenessReport};
use crate::limits::sample::{MemorySampler as _, SysinfoSampler, TreeIndex};
use crate::limits::stats::StatsState;
use crate::proc_table::read_pids;

/// Past anything a case waits for, so a stall fails rather than hangs.
const BOUND: Duration = Duration::from_secs(20);

/// The sheep's `kill_timeout`, which is also the sweep's grace: short, since
/// every lamb here ignores `SIGTERM` and waits out the whole of it.
const KILL_TIMEOUT: Duration = Duration::from_millis(500);

/// Seconds a lamb or a sheep sleeps before exiting on its own, so a case
/// that fails before its drop guard runs still leaves nothing for long.
const LIFETIME_SECS: u32 = 30;

/// `name` resolved against this test's own `PATH`: the runner clears a
/// sheep's environment, so a sheep's script names every program in full.
fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// A shell command that starts a lamb in a session of its own, ignoring
/// `SIGTERM`, which writes its pid to `pid_file` and sleeps.
///
/// The pid lands by rename, so a reader never sees half of it, and only once
/// the lamb has left the session and ignores `SIGTERM`. `None` when this host
/// has nothing to call `setsid` with.
fn detached_lamb(pid_file: &Path) -> Option<String> {
    let pid_file = pid_file.display();
    if let Some(perl) = which("perl") {
        return Some(format!(
            "{perl} -e 'use POSIX (); POSIX::setsid() or die; $SIG{{TERM}} = \"IGNORE\"; \
             open(my $f, \">\", \"$ARGV[0].tmp\") or die; print $f $$; close($f); \
             rename(\"$ARGV[0].tmp\", $ARGV[0]) or die; sleep {LIFETIME_SECS}' \"{pid_file}\"",
            perl = perl.display()
        ));
    }
    if let Some(python) = which("python3") {
        return Some(format!(
            "{python} -c 'import os, signal, sys, time; os.setsid(); \
             signal.signal(signal.SIGTERM, signal.SIG_IGN); p = sys.argv[1]; \
             f = open(p + \".tmp\", \"w\"); f.write(str(os.getpid())); f.close(); \
             os.rename(p + \".tmp\", p); time.sleep({LIFETIME_SECS})' \"{pid_file}\"",
            python = python.display()
        ));
    }
    // Not a group leader as a background job, so `setsid` execs in place and
    // the pid the shell writes is the lamb's own.
    let setsid = which("setsid")?;
    let sleep = which("sleep")?;
    Some(format!(
        "{setsid} /bin/sh -c 'trap \"\" TERM; echo $$ > \"$0.tmp\"; mv \"$0.tmp\" \"$0\"; \
         exec {sleep} {LIFETIME_SECS}' \"{pid_file}\"",
        setsid = setsid.display(),
        sleep = sleep.display()
    ))
}

/// A perl lamb like [`detached_lamb`]'s, except that its first `SIGTERM`
/// starts a child that ignores `SIGTERM` and writes its pid to
/// `child.pid` beside `pid_file`. Perl only.
fn lamb_that_forks_on_term(pid_file: &Path) -> Option<String> {
    let perl = which("perl")?;
    let child_file = pid_file.with_file_name("child.pid");
    Some(format!(
        "{perl} -e 'use POSIX (); POSIX::setsid() or die; \
         sub put {{ open(my $f, \">\", \"$_[0].tmp\") or die; print $f $$; close($f); \
         rename(\"$_[0].tmp\", $_[0]) or die }} \
         my $born = 0; $SIG{{TERM}} = sub {{ return if $born++; my $c = fork(); \
         if (defined $c && $c == 0) {{ $SIG{{TERM}} = \"IGNORE\"; put($ARGV[1]); \
         sleep {LIFETIME_SECS}; exit 0 }} }}; \
         put($ARGV[0]); my $end = time + {LIFETIME_SECS}; sleep 1 while time < $end' \
         \"{pid_file}\" \"{child_file}\"",
        perl = perl.display(),
        pid_file = pid_file.display(),
        child_file = child_file.display(),
    ))
}

/// SIGKILLs every process it holds on drop, so a red case leaves nothing
/// behind.
///
/// Each pid is held with the start second the table gave it, read while the
/// case knew the process was its own. A pid is signalled only while it is
/// live, unreaped and still shows that start, which a recycled pid does not.
#[derive(Default)]
struct KillOnDrop {
    held: Vec<(u32, u64)>,
}

impl KillOnDrop {
    /// Holds `pid`, which must be running and this case's own right now.
    fn hold(&mut self, pid: u32) {
        let started = read_pids(&[pid])
            .get(&pid)
            .and_then(|reading| reading.started_secs)
            .expect("fixture check: a process this case just started has a start time");
        self.held.push((pid, started));
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let own = std::process::id();
        let pids: Vec<u32> = self.held.iter().map(|&(pid, _)| pid).collect();
        let readings = read_pids(&pids);
        for &(pid, started) in &self.held {
            let ours = readings
                .get(&pid)
                .is_some_and(|reading| !reading.zombie && reading.started_secs == Some(started));
            if ours
                && pid > 1
                && pid != own
                && let Ok(raw) = i32::try_from(pid)
            {
                let _ = kill(Pid::from_raw(raw), Signal::SIGKILL);
            }
        }
    }
}

/// One case's sheep, the lamb it detached, and the supervisor over both.
struct Rig {
    sup: SupervisorHandle,
    stats: Arc<StatsState>,
    sheep_pid: u32,
    lamb_pid: u32,
    // Held: the sheep's logs and the lamb's pid file live here.
    dir: tempfile::TempDir,
    _reports: (
        mpsc::Receiver<crate::limits::LimitBreach>,
        mpsc::Receiver<LivenessReport>,
    ),
}

/// Starts a sheep that detaches a lamb, then runs `rest` (given the case's
/// directory and the full path to `sleep`), under a real runner and the real
/// extras. Waits for the sheep to be watched and the lamb's pid.
///
/// `None`, after saying why, on a host with no way to detach a lamb.
async fn start_rig(
    lamb: fn(&Path) -> Option<String>,
    rest: impl FnOnce(&Path, &Path) -> String,
    guard: &mut KillOnDrop,
) -> Option<Rig> {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("lamb.pid");
    let (Some(lamb), Some(sleep)) = (lamb(&pid_file), which("sleep")) else {
        eprintln!("skipped: no sleep, or nothing on PATH to detach this case's lamb with");
        return None;
    };
    let script = format!(
        "{lamb} </dev/null >/dev/null 2>&1 & {}",
        rest(dir.path(), &sleep)
    );

    let (events, _events_rx) = crate::bus::test_bus(64);
    let (breaches_tx, breaches) = mpsc::channel(8);
    let (liveness_tx, liveness) = mpsc::channel(8);
    let extras = Extras::real(
        ExtrasReports {
            breaches: breaches_tx,
            liveness: liveness_tx,
        },
        DEFAULT_MAX_CRON_SLEEP,
    );
    let stats = Arc::clone(&extras.stats);
    let sup = SupervisorBuilder::new(TokioRunner::new(), test_paths(&dir), events)
        .extras(extras)
        .spawn();

    let mut app = AppConfig::minimal("lamb-keeper", "/bin/sh");
    app.args = vec!["-c".to_owned(), script];
    app.autorestart = false;
    app.kill_timeout = UpDuration::from_millis(500);
    assert_eq!(app.kill_timeout.as_duration(), KILL_TIMEOUT);
    sup.start(vec![normalize(app).unwrap()]).await.unwrap();

    let info = flock_until(
        &sup,
        |info| info[0].pid.is_some(),
        "the sheep must come up with a pid",
    )
    .await;
    let sheep_pid = info[0].pid.unwrap();
    guard.hold(sheep_pid);

    let lamb_pid = tokio::time::timeout(BOUND, async {
        loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the lamb must write its pid once it has detached");
    guard.hold(lamb_pid);

    tokio::time::timeout(BOUND, async {
        while !stats
            .watched_for_test()
            .iter()
            .any(|&(_, root)| root == sheep_pid)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the extras must watch the sheep once it is online");

    assert!(
        is_running(lamb_pid),
        "fixture check: the lamb must be running before anything ends it"
    );
    assert_ne!(
        nix::unistd::getsid(Some(Pid::from_raw(i32::try_from(lamb_pid).unwrap()))).unwrap(),
        nix::unistd::getsid(Some(Pid::from_raw(i32::try_from(sheep_pid).unwrap()))).unwrap(),
        "fixture check: the lamb must have left its sheep's session"
    );
    Some(Rig {
        sup,
        stats,
        sheep_pid,
        lamb_pid,
        dir,
        _reports: (breaches, liveness),
    })
}

/// Whether `pid` is a live process rather than gone or a zombie.
fn is_running(pid: u32) -> bool {
    read_pids(&[pid])
        .get(&pid)
        .is_some_and(|reading| !reading.zombie)
}

/// Waits for `pid` to be gone from the table: an exited lamb lingers as a
/// zombie until init reaps it.
async fn reaped_within_bound(pid: u32) -> bool {
    let pid = Pid::from_raw(i32::try_from(pid).unwrap());
    tokio::time::timeout(BOUND, async {
        while kill(pid, None) != Err(Errno::ESRCH) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

/// Asserts the sweep ended `lamb_pid` before the exit was reported, `waited`
/// after the exit began.
///
/// Only a sweep waits out the grace, since the leader obeys `SIGTERM` at
/// once and the lamb never does. A killed lamb may linger as a zombie until
/// init reaps it, so "ended" is not running, then gone within the bound.
async fn assert_swept_before_the_report(lamb_pid: u32, waited: Duration) {
    assert!(
        !is_running(lamb_pid),
        "the lamb {lamb_pid} was still running when the exit was reported"
    );
    assert!(
        waited >= KILL_TIMEOUT,
        "the exit was reported after {waited:?}, inside the sweep's grace: nothing waited \
         out the lamb {lamb_pid}"
    );
    assert!(
        waited < BOUND / 2,
        "the exit took {waited:?} over a lamb that ignores SIGTERM"
    );
    assert!(
        reaped_within_bound(lamb_pid).await,
        "the lamb {lamb_pid} outlived its sheep"
    );
}

// Real time: real children answer the signals.
#[tokio::test]
async fn a_stop_ends_a_lamb_that_left_the_session_and_ignores_term() {
    let mut guard = KillOnDrop::default();
    let Some(rig) = start_rig(
        detached_lamb,
        |_, sleep| format!("exec {} {LIFETIME_SECS}", sleep.display()),
        &mut guard,
    )
    .await
    else {
        return;
    };
    let started = std::time::Instant::now();

    tokio::time::timeout(
        BOUND,
        rig.sup
            .stop(ProcessSelector::Name("lamb-keeper".to_owned())),
    )
    .await
    .expect("the stop must reply within the bound")
    .unwrap();

    assert_swept_before_the_report(rig.lamb_pid, started.elapsed()).await;
}

// Real time: real children answer the signals.
#[tokio::test]
async fn a_natural_exit_ends_the_lamb_the_last_tick_saw() {
    let mut guard = KillOnDrop::default();
    let Some(rig) = start_rig(
        detached_lamb,
        |dir, sleep| {
            format!(
                "i=0; while [ ! -e \"{go}\" ] && [ $i -lt {polls} ]; do {sleep} 0.05; \
                 i=$((i+1)); done; exit 3",
                go = dir.join("go").display(),
                sleep = sleep.display(),
                polls = LIFETIME_SECS * 20,
            )
        },
        &mut guard,
    )
    .await
    else {
        return;
    };
    // The tick the extras run every `MEMORY_POLL_INTERVAL`, taken by hand.
    let taken_at = crate::now_ms() / 1000;
    let table = SysinfoSampler::new().sample();
    rig.stats
        .record_lamb_snapshots(&TreeIndex::build(&table), taken_at, std::process::id());
    assert!(
        rig.stats
            .last_lamb_snapshot(rig.sheep_pid)
            .is_some_and(|snapshot| snapshot.pids().any(|pid| pid == rig.lamb_pid)),
        "fixture check: the tick must have seen the lamb"
    );

    let started = std::time::Instant::now();
    std::fs::write(rig.dir.path().join("go"), b"").unwrap();
    let info = flock_until(
        &rig.sup,
        |info| info[0].pid.is_none() && info[0].last_exit.is_some(),
        "the sheep must exit on its own and be reported",
    )
    .await;

    assert_eq!(
        info[0].last_exit,
        Some(ExitInfo {
            code: Some(3),
            signal: None
        }),
        "the sheep ended on its own, not by any signal"
    );
    assert_swept_before_the_report(rig.lamb_pid, started.elapsed()).await;
}

// Real time: real children answer the signals.
#[tokio::test]
async fn a_shepherd_shutdown_ends_a_lamb_that_left_the_session() {
    let mut guard = KillOnDrop::default();
    let Some(rig) = start_rig(
        detached_lamb,
        |_, sleep| format!("exec {} {LIFETIME_SECS}", sleep.display()),
        &mut guard,
    )
    .await
    else {
        return;
    };
    let started = std::time::Instant::now();

    tokio::time::timeout(BOUND, rig.sup.shutdown())
        .await
        .expect("the shutdown must finish within the bound");

    assert_swept_before_the_report(rig.lamb_pid, started.elapsed()).await;
}

// Real time: real children answer the signals.
#[tokio::test]
async fn a_stop_ends_a_child_a_lamb_started_after_the_sweep_began() {
    let mut guard = KillOnDrop::default();
    let Some(rig) = start_rig(
        lamb_that_forks_on_term,
        |_, sleep| format!("exec {} {LIFETIME_SECS}", sleep.display()),
        &mut guard,
    )
    .await
    else {
        return;
    };
    let child_file = rig.dir.path().join("child.pid");
    let started = std::time::Instant::now();

    tokio::time::timeout(
        BOUND,
        rig.sup
            .stop(ProcessSelector::Name("lamb-keeper".to_owned())),
    )
    .await
    .expect("the stop must reply within the bound")
    .unwrap();
    let waited = started.elapsed();

    let child_pid = std::fs::read_to_string(&child_file)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .expect("the lamb must have started its child on the sweep's SIGTERM");
    if is_running(child_pid) {
        guard.hold(child_pid);
    }
    assert!(
        !is_running(child_pid),
        "the child {child_pid}, born after the sweep's walk, outlived the stop"
    );
    assert_swept_before_the_report(rig.lamb_pid, waited).await;
}
