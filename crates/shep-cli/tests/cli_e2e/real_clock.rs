//! The two cases that cost real wall-clock seconds: a cron occurrence and
//! a memory breach.

use super::*;

#[cfg(unix)]
/// The only place the cron subsystem runs on `SystemClock`; every other cron
/// test drives `TestClock` over a paused runtime.
///
/// `unscheduled` is the control: same script, same daemon, no `cron_restart`,
/// so a restart from the script exiting would move both counters. Its
/// [`SLOW_SCRIPT_SLEEP_SECS`] sleep outlasts [`CRON_DEADLINE`] twice over.
/// Costs 26s to 61s, a uniform draw on the minute the arming lands in.
#[test]
fn a_cron_occurrence_restarts_a_sheep_on_the_real_clock() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_slow_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"minutely\"\nscript = '{script}'\ncron_restart = \"* * * * *\"\n\n\
             [[app]]\nname = \"unscheduled\"\nscript = '{script}'\n",
            script = script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let before = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        sheep_named(data, "minutely")["status"] == "online"
            && sheep_named(data, "unscheduled")["status"] == "online"
    });
    assert_eq!(
        sheep_named(&before, "minutely")["restarts"],
        0,
        "precondition: {before}"
    );
    assert_eq!(
        sheep_named(&before, "unscheduled")["restarts"],
        0,
        "precondition: {before}"
    );

    let after = poll_flock_data(home, CRON_DEADLINE, |data| {
        sheep_named(data, "minutely")["restarts"] == 1
    });
    assert_eq!(
        sheep_named(&after, "minutely")["restarts"],
        1,
        "a `* * * * *` occurrence must restart the sheep within one real minute: {after}"
    );
    assert_eq!(
        sheep_named(&after, "unscheduled")["restarts"],
        0,
        "the same script with no cron_restart must not have moved: a restart both sheep \
         share is the script exiting, not an occurrence firing: {after}"
    );

    graceful_kill(home);
}

// --- Case 15 -------------------------------------------------------------

#[cfg(unix)]
/// The only place `PollingEnforcer` and `SysinfoSampler` run together on real
/// time against a real spawned process.
///
/// `unlimited` is the control: same ballooning script, same daemon, no
/// `max_memory`, so a restart caused by the shell dying under its own
/// allocation would move both counters. Its [`SLOW_SCRIPT_SLEEP_SECS`] sleep
/// outlasts [`BREACH_DEADLINE`] five times over. Costs about 16s, one
/// `MEMORY_POLL_INTERVAL` plus a restart.
#[test]
fn a_real_memory_breach_restarts_a_sheep() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_ballooning_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"greedy\"\nscript = '{script}'\nmax_memory = \"{BREACH_LIMIT}\"\n\n\
             [[app]]\nname = \"unlimited\"\nscript = '{script}'\n",
            script = script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let before = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        sheep_named(data, "greedy")["status"] == "online"
            && sheep_named(data, "unlimited")["status"] == "online"
    });
    assert_eq!(
        sheep_named(&before, "greedy")["restarts"],
        0,
        "precondition: {before}"
    );
    assert_eq!(
        sheep_named(&before, "unlimited")["restarts"],
        0,
        "precondition: {before}"
    );

    let after = poll_flock_data(home, BREACH_DEADLINE, |data| {
        sheep_named(data, "greedy")["restarts"] == 1
    });
    assert_eq!(
        sheep_named(&after, "greedy")["restarts"],
        1,
        "a process tree over its max_memory must be restarted by the real enforcer: {after}"
    );
    assert_eq!(
        sheep_named(&after, "unlimited")["restarts"],
        0,
        "the same script with no max_memory must not have moved: a restart both sheep \
         share is the script dying, not its ceiling being enforced: {after}"
    );

    // `launch.rs` redirects the daemon's stderr into this file, and the breach
    // record is the only place the observed RSS and its ceiling are stated.
    // Read rather than polled: `spawn_extras_reporter` writes it before asking
    // for the restart the counter above already saw.
    let daemon_log = std::fs::read_to_string(home.join("logs").join("shepd.err.log")).unwrap();
    assert!(
        daemon_log.contains("exceeded its max_memory"),
        "the daemon's own log must say why the sheep was restarted: {daemon_log:?}"
    );
    assert!(
        daemon_log.contains("limit="),
        "the record must carry the ceiling that was crossed: {daemon_log:?}"
    );

    graceful_kill(home);
}
