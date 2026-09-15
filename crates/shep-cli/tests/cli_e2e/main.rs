//! End-to-end tier: drives the real `shep` binary via `assert_cmd` against a
//! real daemon, a real socket, and real spawned sheep, each on a fresh
//! `$SHEP_HOME` in its own [`tempfile::TempDir`].
//!
//! Two rules every case follows: `.timeout(CMD_TIMEOUT)` before `.output()`,
//! so a hang fails as a named assertion; and a [`DaemonGuard`] adopting the
//! `$SHEP_HOME` immediately after the `Output` that might have spawned a
//! daemon, before any assertion that could panic.
//!
//! Windows scripts are `.cmd` (see `script_header`); cases that cannot port
//! carry their own `#[cfg(unix)]`.

// The `#[cfg(unix)]` cases take their helpers and constants with them, so on
// Windows those items compile unused.
#![cfg_attr(windows, allow(dead_code))]

#[cfg(unix)]
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use assert_cmd::cargo::CommandCargoExt as _;
use tempfile::TempDir;

/// Rows [`a_flock_of_every_carried_kind_survives_a_daemon_reload`] expects:
/// four single-instance apps and two clustered ones at two instances each.
#[cfg(unix)]
const ROWS_IN_THE_MIXED_FLOCK: usize = 8;

/// Writes one line to `sheep`'s stdin and asserts shep accepted it. `sent`
/// says the bytes reached the pipe, never that the app read them, so the
/// caller still has to look in the sheep's own log for the echo.
#[cfg(unix)]
fn whisper(home: &Path, sheep: &str, line: &str) {
    let sent = shep(home)
        .arg("whisper")
        .arg(sheep)
        .arg(line)
        .output()
        .unwrap();
    assert_success(&sent);
}

/// Reads `path` until it holds a line equal to `want`, or
/// [`HANDOVER_DEADLINE`] expires. Equality rather than `contains`, so a prefix
/// of a longer line cannot answer for the line itself. Returns rather than
/// panicking, so the failure is the caller's own assertion.
#[cfg(unix)]
fn await_log_line(path: &Path, want: &str) -> bool {
    let start = Instant::now();
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text
            .lines()
            .any(|line| shep_core::logstamp::strip(line) == want)
        {
            return true;
        }
        if start.elapsed() >= HANDOVER_DEADLINE {
            return false;
        }
        std::thread::sleep(HANDOVER_POLL_INTERVAL);
    }
}

/// A `/bin/sh` sheep that waits for `gate` to appear, deletes it, and then
/// grows its resident set past [`BALLOON_BYTES`]. Growing only after the exec
/// makes the breach attributable to the successor's arming, and deleting the
/// gate on the way past makes it happen exactly once.
#[cfg(unix)]
fn write_gated_ballooning_script(dir: &TempDir, name: &str, gate: &Path) -> PathBuf {
    write_script(
        dir,
        name,
        &format!(
            "{header}{pid}while [ ! -f '{gate}' ]; do\n  sleep 0.1\ndone\nrm -f '{gate}'\n\
             s=x\nwhile [ ${{#s}} -lt {BALLOON_BYTES} ]; do s=\"$s$s\"; done\n{sleep}",
            header = script_header(),
            pid = record_pid_line(dir),
            gate = gate.display(),
            sleep = sleep_line(SLOW_SCRIPT_SLEEP_SECS),
        ),
    )
}

/// Every lifecycle extra is armed again by the successor, proved by behaviour
/// rather than by a handle existing. [`ExtrasRegistry::arm`] fans out to five
/// mechanisms across two scopes: sampling, the memory limit and the liveness
/// loop per instance, the cron worker and the filesystem watch per name.
///
/// Every trigger fires after the exec, and that ordering is the case: a watch
/// armed by the predecessor is indistinguishable from one armed by the
/// successor if the file is written before the reload. `control` configures no
/// extra at all, so its `restarts` of 0 says nothing here restarts sheep.
#[cfg(unix)]
#[test]
fn every_lifecycle_extra_is_re_armed_across_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    // Its own tempdir, never `$SHEP_HOME`: every fixture script appends its
    // pid to `<home>/`[`FIXTURE_PIDS`] on each spawn, so a watch rooted at
    // the home would restart on its own sheep's restart, forever.
    let watched = tempfile::tempdir().unwrap();
    let greedy_gate = dir.path().join("greedy.gate");
    let control_gate = dir.path().join("control.gate");
    // A file the probe requires, so the test trips it by deleting and heals it
    // by writing. The opposite polarity races the fixture: a script clearing
    // its own trigger on the way up can do so after `shep flock` already called
    // the sheep `online`.
    let healthy = dir.path().join("probe.healthy");
    std::fs::write(&healthy, "ok").unwrap();
    let sleeper = write_slow_script(&dir);
    let greedy = write_gated_ballooning_script(&dir, "greedy.sh", &greedy_gate);
    let control = write_gated_ballooning_script(&dir, "control.sh", &control_gate);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"watched\"\nscript = '{sleeper}'\ncwd = '{root}'\nwatch = true\n\n\
             [[app]]\nname = \"scheduled\"\nscript = '{sleeper}'\ncron_restart = \"* * * * *\"\n\n\
             [[app]]\nname = \"greedy\"\nscript = '{greedy}'\nmax_memory = \"{BREACH_LIMIT}\"\n\n\
             [[app]]\nname = \"probed\"\nscript = '{sleeper}'\n\
             liveness_probe = {{ kind = \"exec\", target = \"test -f {healthy}\", \
             interval = \"1s\", timeout = \"2s\", failure_threshold = 2 }}\n\n\
             [[app]]\nname = \"control\"\nscript = '{control}'\n",
            sleeper = sleeper.display(),
            root = watched.path().display(),
            greedy = greedy.display(),
            control = control.display(),
            healthy = healthy.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let before = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == 5
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let pids_before: BTreeMap<String, u64> = restart_counts(&before)
        .keys()
        .map(|name| {
            (
                name.clone(),
                sheep_named(&before, name)["pid"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        restart_counts(&before),
        BTreeMap::from([
            ("control".to_owned(), 0),
            ("greedy".to_owned(), 0),
            ("probed".to_owned(), 0),
            ("scheduled".to_owned(), 0),
            ("watched".to_owned(), 0),
        ]),
        "precondition: nothing has restarted yet: {before}"
    );

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);
    let after = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.len() == 5 && rows.iter().all(|row| row["status"] == "online"))
    });
    // The handover really happened, which every assertion below depends on:
    // a stop-and-start would re-arm everything from a fresh spawn and prove
    // nothing about `install_adopted`.
    for (name, pid) in &pids_before {
        assert_eq!(
            sheep_named(&after, name)["pid"].as_u64(),
            Some(*pid),
            "{name} was respawned, so this reload took the stop arm: {after}"
        );
    }
    // The sampling arm, the cheapest of the five to lose silently. A successor
    // that never called `stats.watch` leaves this null for the life of the
    // daemon while every other column looks right.
    for name in pids_before.keys() {
        assert!(
            !sheep_named(&after, name)["memory_bytes"].is_null(),
            "{name} is no longer sampled after the handover: {after}"
        );
    }

    // Every trigger, fired only now.
    std::fs::write(watched.path().join("app.txt"), "changed").unwrap();
    std::fs::write(&greedy_gate, "go").unwrap();
    std::fs::write(&control_gate, "go").unwrap();
    std::fs::remove_file(&healthy).unwrap();
    // One wait for the three fast arms. The enforcer's ticks are phased off
    // daemon boot rather than off the breach, so the memory limit's worst case
    // is a whole `MEMORY_POLL_INTERVAL` after the resident set moves.
    let fired = poll_flock_data(home, BREACH_DEADLINE, |data| {
        ["watched", "probed", "greedy"]
            .iter()
            .all(|name| sheep_named(data, name)["restarts"].as_u64().unwrap_or(0) >= 1)
    });
    assert!(
        sheep_named(&fired, "watched")["restarts"].as_u64().unwrap() >= 1,
        "a write under the watched tree after the exec must restart the sheep: {fired}"
    );
    assert!(
        sheep_named(&fired, "probed")["restarts"].as_u64().unwrap() >= 1,
        "a liveness probe failing after the exec must restart the sheep: {fired}"
    );
    assert!(
        sheep_named(&fired, "greedy")["restarts"].as_u64().unwrap() >= 1,
        "a resident set crossing max_memory after the exec must restart the sheep: {fired}"
    );
    // Healed now that the restart is on the books, so the probe stops
    // failing and `probed` cannot spend its `max_restarts` while the case
    // waits out the cron minute below.
    std::fs::write(&healthy, "ok").unwrap();

    // The cron worker, and the slow one. A `* * * * *` pattern armed at an
    // arbitrary moment lands uniformly in the minute, so the bound is a minute
    // plus the restart's round trip.
    let cronned = poll_flock_data(home, CRON_DEADLINE, |data| {
        sheep_named(data, "scheduled")["restarts"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    });
    assert!(
        sheep_named(&cronned, "scheduled")["restarts"]
            .as_u64()
            .unwrap()
            >= 1,
        "a cron occurrence after the exec must restart the sheep: {cronned}"
    );
    assert_eq!(
        sheep_named(&cronned, "control")["restarts"].as_u64(),
        Some(0),
        "the control ballooned through the same gate and configures no extra at all; \
         a restart it shares is this case restarting sheep rather than an extra firing: \
         {cronned}"
    );

    // The daemon's own log, the only place the observed resident set and the
    // ceiling it crossed are stated. Read rather than polled:
    // `spawn_extras_reporter` writes the record before it asks for the restart.
    let daemon_log = std::fs::read_to_string(home.join("logs").join("shepd.err.log")).unwrap();
    assert!(
        daemon_log.contains("exceeded its max_memory"),
        "the successor's own log must say why the sheep was restarted: {daemon_log:?}"
    );

    graceful_kill(home);
}

/// `restarts` per sheep name, for the two observations this case compares.
#[cfg(unix)]
fn restart_counts(data: &serde_json::Value) -> BTreeMap<String, u64> {
    data.as_array()
        .unwrap_or_else(|| panic!("flock data is an array: {data}"))
        .iter()
        .map(|row| {
            (
                row["name"].as_str().unwrap().to_owned(),
                row["restarts"].as_u64().unwrap_or(0),
            )
        })
        .collect()
}

/// A sheep already owed a respawn when the shepherd is replaced still gets it.
///
/// `Actor::schedule_restart` spawns a task that sleeps and then sends
/// `Msg::RestartDue`, and that task dies with the process image, while
/// `handle_restart_due` is the only thing that moves a sheep off
/// `WaitingRestart`. The precondition is asserted before and immediately after
/// the reload: if the delay elapsed during `daemon reload`, the predecessor
/// respawned the sheep and the final assertion proves nothing. `steady` never
/// exits, so a moved pid means the stop arm ran.
#[cfg(unix)]
#[test]
fn a_sheep_owed_a_restart_still_gets_one_after_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let steady = write_slow_script(&dir);
    let flapper = write_script(
        &dir,
        "flapper.sh",
        &format!("{}{}exit 1\n", script_header(), record_pid_line(&dir)),
    );
    let flockfile = write_flockfile(
        &dir,
        &format!(
            // Long enough that a loaded runner cannot let the wait expire
            // between the observation below and the reload after it, and short
            // enough that the case then waits it out once.
            "[[app]]\nname = \"flapper\"\nscript = '{flapper}'\nrestart_delay = \"8s\"\n\n\
             [[app]]\nname = \"steady\"\nscript = '{steady}'\n",
            flapper = flapper.display(),
            steady = steady.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    let before = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        sheep_named(data, "flapper")["status"] == "waiting-restart"
            && sheep_named(data, "steady")["status"] == "online"
    });
    assert_eq!(
        sheep_named(&before, "flapper")["status"],
        "waiting-restart",
        "precondition: the sheep must be owed a respawn when the shepherd is replaced: {before}"
    );
    let steady_pid = sheep_named(&before, "steady")["pid"].as_u64().unwrap();

    let reloaded = shep(home).arg("daemon").arg("reload").output().unwrap();
    assert_success(&reloaded);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&reloaded.stdout),
        String::from_utf8_lossy(&reloaded.stderr)
    );
    assert!(
        !text.contains("falls back to a stop-and-start"),
        "a sheep in its restart backoff is carried, not refused: {text}"
    );

    let carried = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        !sheep_named(data, "steady")["pid"].is_null()
    });
    assert_eq!(
        sheep_named(&carried, "steady")["pid"].as_u64(),
        Some(steady_pid),
        "a moved pid means the stop arm ran, which restarts everything: {carried}"
    );
    assert_eq!(
        sheep_named(&carried, "flapper")["status"],
        "waiting-restart",
        "the wait must still have been pending at the exec, or this case proves nothing: \
         {carried}"
    );
    assert_eq!(
        sheep_named(&carried, "flapper")["restarts"].as_u64(),
        Some(0),
        "ditto: the predecessor must not have respawned it first: {carried}"
    );

    let restarted = poll_flock_data(home, RESTARTED_DEADLINE, |data| {
        sheep_named(data, "flapper")["restarts"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    });
    assert!(
        sheep_named(&restarted, "flapper")["restarts"]
            .as_u64()
            .unwrap()
            >= 1,
        "the sheep was left waiting for a timer that died with the exec: {restarted}"
    );

    graceful_kill(home);
}

/// A `/bin/sh` sheep that echoes every line it is whispered, prefixed.
/// `stdin = true` is the only thing that gives a sheep a readable fd 0, and the
/// echo comes back in the sheep's own log, so a pipe that survived as a number
/// attached to the wrong end delivers nothing. No trailing `sleep`: the `read`
/// parks the script for as long as the daemon holds the write end open.
#[cfg(unix)]
fn write_echoing_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "echoer.sh",
        &format!(
            "{}{}while read -r line; do\n  echo \"heard $line\"\n done\n",
            script_header(),
            record_pid_line(dir),
        ),
    )
}

/// A `/bin/sh` sheep that writes down whatever the shepherd tells it and then
/// exits cleanly.
///
/// For `shutdown_with_message`: the daemon writes down the socket on the stop
/// path, with no reply to correlate. The line in the log is the evidence, and
/// the clean `exit 0` beside it says the message arrived rather than the kill
/// ladder.
#[cfg(unix)]
fn write_farewell_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "bye.sh",
        &format!(
            "{}{}while read -r line <&3; do\n  echo \"told $line\"\n  exit 0\ndone\n",
            script_header(),
            record_pid_line(dir),
        ),
    )
}

/// Every kind of sheep, in one flock, across one reload.
///
/// One flock rather than five: `handover::adopt::refuse_repeated_fds` refuses
/// the entire blob over one repeated number, and a mixed flock is the only
/// place six kinds of descriptor (two log files, two pipe read ends, a stdin
/// pipe and a socketpair, times eight sheep) are numbered together. Every
/// assertion below is one a pid check cannot make.
#[cfg(unix)]
#[test]
fn a_flock_of_every_carried_kind_survives_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let counter = write_counting_script(&dir);
    let slot = write_slot_script(&dir);
    let chatty = write_channel_script(&dir);
    let echoer = write_echoing_script(&dir);
    let bye = write_farewell_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"counter\"\nscript = '{counter}'\n\n\
             [[app]]\nname = \"echoer\"\nscript = '{echoer}'\nstdin = true\n\n\
             [[app]]\nname = \"chatty\"\nscript = '{chatty}'\nchannel = true\nwait_ready = true\n\n\
             [[app]]\nname = \"bye\"\nscript = '{bye}'\nshutdown_with_message = true\n\n\
             [[app]]\nname = \"split\"\nscript = '{slot}'\ninstances = 2\n\n\
             [[app]]\nname = \"merged\"\nscript = '{slot}'\ninstances = 2\nmerge_logs = true\n",
            counter = counter.display(),
            echoer = echoer.display(),
            chatty = chatty.display(),
            bye = bye.display(),
            slot = slot.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("start")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);

    // `online` for all seven, which for `chatty` is already an assertion:
    // `wait_ready` holds it at `starting` until the child writes up fd 3.
    let before = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == ROWS_IN_THE_MIXED_FLOCK
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let rows_before = rows_by_slot(&before);
    assert_eq!(
        rows_before.len(),
        ROWS_IN_THE_MIXED_FLOCK,
        "six apps, two of them clustered: {before}"
    );

    // Every feature is exercised before the reload too, so a whisper or a
    // trigger that never worked at all does not read as a handover defect.
    let out_file = |name: &str| rows_before[&(name.to_owned(), 0)].1.clone();
    let counter_log = out_file("counter");
    let echoer_log = out_file("echoer");
    let bye_log = out_file("bye");
    let seen_before = counting_lines(&counter_log, 3);
    assert!(
        seen_before.len() >= 3,
        "the counter must be logging before the reload: {seen_before:?}"
    );
    whisper(dir.path(), "echoer", "before");
    assert!(
        await_log_line(&echoer_log, "heard before"),
        "the whisper must reach the sheep before the reload, or this case proves nothing: {}",
        std::fs::read_to_string(&echoer_log).unwrap_or_default()
    );
    let answered = trigger_ping(dir.path());
    assert_eq!(
        answered["kind"], "replied",
        "the channel must work before the reload: {answered}"
    );
    let counts_before: HashMap<(String, u32), usize> = rows_before
        .iter()
        .filter(|((name, _), _)| name == "split" || name == "merged")
        .map(|(key, (_, out_file))| (key.clone(), slot_lines(out_file).len()))
        .collect();

    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .output()
        .unwrap();
    assert_success(&reloaded);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&reloaded.stdout),
        String::from_utf8_lossy(&reloaded.stderr)
    );
    // The exact sentence `handover::RefusedReason`'s `Display` ends with;
    // without it the case would pass on a stop-and-start.
    assert!(
        !text.contains("falls back to a stop-and-start"),
        "every kind in this flock is carried now, not refused: {text}"
    );

    let after = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == ROWS_IN_THE_MIXED_FLOCK
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let rows_after = rows_by_slot(&after);
    assert_eq!(
        rows_after, rows_before,
        "every sheep keeps its pid and its own log file: {after}"
    );

    // The log plane, on a plain sheep.
    let seen = counting_lines(&counter_log, seen_before.len() + 3);
    assert!(
        seen.len() > seen_before.len(),
        "the counter stopped logging across the handover: {seen:?}"
    );
    assert_unbroken_sequence(&seen, "the counter's log across a handover");

    // stdin, which nothing else in this file covers. A fresh line, so a
    // stale `heard before` in the file cannot answer for it.
    whisper(dir.path(), "echoer", "after");
    assert!(
        await_log_line(&echoer_log, "heard after"),
        "the carried stdin pipe must still reach the same child: {}",
        std::fs::read_to_string(&echoer_log).unwrap_or_default()
    );

    // The channel, both directions, against the child that has had fd 3
    // since before the exec.
    let still = trigger_ping(dir.path());
    assert_eq!(
        still["kind"], "replied",
        "the successor must reach the same fd 3 the child has had all along: {still}"
    );
    assert_eq!(still["body"], "pong", "{still}");

    // The clustered halves. Each row is asked for its own file, and every
    // line written into it after the reload has to agree with the row.
    for ((name, slot), (pid, out_file)) in &rows_after {
        if name != "split" && name != "merged" {
            continue;
        }
        let before = counts_before[&(name.clone(), *slot)];
        let want = before + 2;
        let lines = poll_slot_lines(out_file, want);
        assert!(
            lines.len() >= want,
            "{name}:{slot} stopped logging across the handover: {} lines, wanted {want}",
            lines.len()
        );
        for (line_slot, line_pid) in &lines[before..] {
            assert_eq!(
                rows_after[&(name.clone(), *line_slot)].0,
                *line_pid,
                "a {name} line's slot and pid disagree with the flock: {lines:?}"
            );
            if name == "split" {
                assert_eq!(
                    (*line_slot, *line_pid),
                    (*slot, *pid),
                    "{name}:{slot}'s own log holds another instance's output: {lines:?}"
                );
            }
        }
    }

    // `shutdown_with_message`, last because it ends its sheep. The message
    // goes down the carried socket, the child writes it to its own log and
    // exits 0.
    let stopped = shep(dir.path()).arg("stop").arg("bye").output().unwrap();
    assert_success(&stopped);
    // The row rides along in the message: a `bye` killed by the stop ladder
    // never got the message, and a `bye` that got it and had the line dropped
    // on the way to the file (what `tokio_runner`'s `FINAL_DRAIN` guards) both
    // leave the log empty.
    assert!(
        await_log_line(&bye_log, "told {\"kind\":\"shutdown\"}"),
        "the stop message must reach the child down the carried channel. \
         The log holds {:?}; the flock reads {}",
        std::fs::read_to_string(&bye_log).unwrap_or_default(),
        poll_flock_data(dir.path(), Duration::ZERO, |_| true),
    );

    graceful_kill(dir.path());
}

/// Gap between the dials [`the_control_socket_accepts_throughout_a_handover`]
/// makes at the control address. A dial is a `connect(2)` and a close, so
/// this decides how narrow an outage the case can see, not what it costs:
/// every real way the address goes away spans a daemon teardown or a fresh
/// bind, hundreds of milliseconds.
#[cfg(unix)]
const DIAL_INTERVAL: Duration = Duration::from_millis(5);

/// `ExitCode::DaemonUnreachable`, the one failing exit `shep ping` has,
/// whatever the reason. Any other failing exit is a usage error or a
/// refusal, which no handover produces, and the prober refuses it rather
/// than counting it as the one drop the exec is allowed.
#[cfg(unix)]
const PING_OFFLINE: i32 = 5;

/// The control socket answers throughout a handover.
///
/// The successor inherits the listening descriptor rather than binding the
/// address again, so a client that connects mid-replacement waits in the
/// kernel's backlog. Nothing may be refused, and the socket file may never
/// disappear: a rebind would race the predecessor's socket file.
///
/// Two probers and one file check. A `connect(2)` dialer sees an outage at
/// least `DIAL_INTERVAL` wide; the socket file's inode sees a rebind, which
/// is too brief for any poller; a ping loop sees a request still served.
/// Ping failures are counted, not read: `shep ping` prints nothing on
/// stderr and exits `DaemonUnreachable` for every reason it has.
#[cfg(unix)]
#[test]
fn the_control_socket_accepts_throughout_a_handover() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(dir.path())
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("sheep")
        .output()
        .unwrap();
    guard.adopt_home(dir.path());
    assert_success(&started);
    let _ = poll_flock(dir.path(), |info| info["status"] == "online");

    // One deadline for both threads, so the two answers describe the same
    // window.
    let deadline = Instant::now() + Duration::from_secs(8);
    let socket = dir.path().join("run").join("shep.sock");
    // The file's identity before anything happens to it. A successor that
    // binds fresh instead of adopting must unlink and recreate the file,
    // which changes the inode; that takes a microsecond no poller can see.
    let inode_before = std::fs::metadata(&socket)
        .expect("the control socket must exist before the handover")
        .ino();
    let dial_socket = socket.clone();
    let dialer = std::thread::spawn(move || {
        let mut refused = Vec::new();
        let mut dials = 0_usize;
        while Instant::now() < deadline {
            dials += 1;
            // Dropped where the `if let` ends: the daemon's accept loop meets
            // an EOF and logs it at `debug!`. The answer wanted is the
            // syscall's; anything more would be the bucket this case fixes.
            if let Err(err) = std::os::unix::net::UnixStream::connect(&dial_socket) {
                refused.push(format!("dial {dials}: {:?}: {err}", err.kind()));
            }
            std::thread::sleep(DIAL_INTERVAL);
        }
        (refused, dials)
    });

    let home = dir.path().to_path_buf();
    // The prober says when it is really probing, and the reload waits for
    // that. Without the handshake the reload could finish before the first
    // `ping` ran, and every probe would be served by the successor alone.
    let (probing, started_probing) = std::sync::mpsc::channel();
    let prober = std::thread::spawn(move || {
        let mut before_reload = Vec::new();
        let mut dropped = Vec::new();
        let mut pings = 0_usize;
        let mut announced = false;
        while Instant::now() < deadline {
            pings += 1;
            let out = shep(&home).arg("ping").output().unwrap();
            let served = out.status.success();
            if !served {
                // The failure has to be the one shape a handover can cause.
                // Its reason cannot be read (see the case's doc), but its
                // exit can: anything but `DaemonUnreachable` is a different
                // defect wearing the tolerated drop's clothes.
                assert_eq!(
                    out.status.code(),
                    Some(PING_OFFLINE),
                    "ping {pings} failed for a reason no handover produces: {}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            if !announced {
                // The reload waits for a ping the predecessor ANSWERED. A
                // failure before that has no exec to blame, since none has
                // been asked for, and it must not spend the one drop the
                // exec is allowed: it is kept apart and refused below.
                if served {
                    announced = true;
                    let _ = probing.send(());
                } else {
                    before_reload.push(pings);
                }
                continue;
            }
            if !served {
                dropped.push(pings);
            }
        }
        (before_reload, dropped, pings)
    });
    started_probing
        .recv_timeout(FLOCK_DEADLINE)
        .expect("the prober must reach the shepherd before the reload starts");

    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .output()
        .unwrap();
    assert_success(&reloaded);
    // The premise, checked. A reload that fell back to stopping and starting
    // really did unbind the address, and the dialer would report that as
    // the defect. Both fallback arms say so on stderr (`commands/daemon.rs`'s
    // two `aside("reload", ...)` calls).
    let reload_aside = String::from_utf8_lossy(&reloaded.stderr);
    assert!(
        !reload_aside.contains("starting one instead")
            && !reload_aside.contains("stopping and starting instead"),
        "this case is about the handover arm and the reload took the other one: {reload_aside}"
    );
    // Same file, same inode: the successor adopted the carried listener. A
    // rebind at the same path passed the dialer 10 of 10; the inode is the
    // deterministic reading of the same property.
    let inode_after = std::fs::metadata(&socket)
        .expect("the control socket must still exist after the handover")
        .ino();
    assert_eq!(
        inode_after, inode_before,
        "the successor bound a fresh listener instead of adopting the carried one: \
         the socket file's inode changed across the handover"
    );

    // The listener's descriptor is carried, so no client ever finds the
    // address unbound; an accepted connection is not, so the one reply in
    // flight at the exec may fail. One at most: the prober is sequential.
    let (refused, dials) = dialer.join().unwrap();
    let (before_reload, dropped, pings) = prober.join().unwrap();
    assert!(
        before_reload.is_empty(),
        "a ping failed before any reload was asked for, at {before_reload:?} of \
         {pings}: the predecessor was not answering, which is not the handover's \
         doing"
    );
    assert!(
        refused.is_empty(),
        "the control address must stay bound across the handover, \
         {} of {dials} dials refused: {refused:?}",
        refused.len()
    );
    assert!(
        dropped.len() <= 1,
        "at most the one request in flight at the exec may drop, got {} of {pings} \
         pings, at {dropped:?}",
        dropped.len()
    );

    graceful_kill(dir.path());
}

/// Reads `roll` until it records exactly `want` apps, or [`FLOCK_DEADLINE`]
/// expires, and returns the bytes it held on the last read.
///
/// The muster roll is written by a debounced task, so "the flock changed" and
/// "the roll on disk says so" are two events, and callers here need the second.
#[cfg(unix)]
fn roll_recording(roll: &Path, want: usize) -> Vec<u8> {
    let start = Instant::now();
    loop {
        let bytes = std::fs::read(roll).unwrap_or_default();
        let apps = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value["apps"].as_array().map(Vec::len));
        if apps == Some(want) || start.elapsed() >= FLOCK_DEADLINE {
            return bytes;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

/// A successor that inherited an empty flock must not fall back to the roll.
/// A boot either installs the flock it was handed or restores the roll, and
/// what decides is whether it was handed a flock at all, not how large.
///
/// SIGHUP directly, not `shep daemon reload`, which would start `ghost`
/// through `shep muster` whatever the boot decided. A failed exec leaves no
/// shepherd, so the poll fails on its own `assert_success` and the pid check
/// says a successor answered. The wait is for a sheep to appear, so asserting
/// none did cannot pass by looking too early; it goes through
/// [`poll_flock_data_across_a_handover`] since the raw signal can drop one reply.
#[cfg(unix)]
#[test]
fn a_successor_inheriting_an_empty_flock_does_not_restore_the_roll() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let mut guard = DaemonGuard::default();

    let started = shep(home)
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("ghost")
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&started);
    let _ = poll_flock(home, |info| info["status"] == "online");
    let shepherd = wait_for_daemon_pid(home).expect("the shepherd must record a pid");

    assert_success(&shep(home).arg("save").output().unwrap());
    let roll = home.join("flock.json");
    let stale = roll_recording(&roll, 1);
    assert!(
        !stale.is_empty(),
        "the roll must record `ghost` before it can go stale: {}",
        String::from_utf8_lossy(&stale)
    );

    assert_success(&shep(home).arg("delete").arg("ghost").output().unwrap());
    let emptied = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(Vec::is_empty)
    });
    assert_eq!(
        emptied.as_array().map(Vec::len),
        Some(0),
        "the delete must leave an idle shepherd: {emptied}"
    );
    // Waited out rather than assumed: the debounced writer is about to record
    // the empty flock, and a stale roll put back before that write lands would
    // be overwritten by it.
    let _ = roll_recording(&roll, 0);
    std::fs::write(&roll, &stale).unwrap();

    nix::sys::signal::kill(shepherd, nix::sys::signal::Signal::SIGHUP).unwrap();

    let after = poll_flock_data_across_a_handover(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| !rows.is_empty())
    });
    assert_eq!(
        after.as_array().map(Vec::len),
        Some(0),
        "a successor must install the flock it was handed and nothing else; \
         this one restored a stale roll: {after}"
    );
    assert_eq!(
        wait_for_daemon_pid(home),
        Some(shepherd),
        "the shepherd must have been replaced in place; a moved pid means \
         SIGHUP stopped it and the polling above started a fresh one, which \
         is not the boot this test is about"
    );

    graceful_kill(home);
}

/// `shep add` registers a sheep, starts nothing, and a later `shep start`
/// brings that same sheep up. Without it, `shep start Flockfile.toml` on a
/// template shipping `env = { DB_HOST = "", DB_PASSWORD = "" }` spawns against
/// an empty database URL and spends the restart budget before it can be
/// configured.
///
/// The listing is read once, not polled: `Request::Add` is answered after the
/// actor has registered, so a build routing `add` through the start path
/// reports `online` on this first read.
#[test]
fn add_registers_a_stopped_sheep_that_a_later_start_brings_up() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"pending\"\nscript = '{}'\n",
            script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let added = shep(home)
        .arg("--format")
        .arg("json")
        .arg("add")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&added);

    let envelope: serde_json::Value = serde_json::from_slice(&added.stdout)
        .unwrap_or_else(|e| panic!("add stdout was not JSON: {e}"));
    assert_eq!(
        envelope["command"], "add",
        "the envelope names the verb the operator typed: {envelope}"
    );
    assert_eq!(
        envelope["data"][0]["status"], "stopped",
        "registered, not started: {envelope}"
    );
    assert!(
        envelope["data"][0]["pid"].is_null(),
        "a sheep that was never spawned has no pid: {envelope}"
    );

    // The script appends its pid to this file on every run, so an empty one is
    // the child's own evidence that it never executed. Unix only, because
    // `record_pid_line` writes nothing on Windows.
    #[cfg(unix)]
    assert!(
        !dir.path().join(FIXTURE_PIDS).exists(),
        "the script never ran, so it never recorded a pid"
    );

    // Registered rather than merely reported: a row `shep flock` cannot see is
    // not a flock member.
    let listed = shep(home)
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&listed);
    let flock: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(flock["data"][0]["name"], "pending", "it is in the flock");
    assert_eq!(flock["data"][0]["status"], "stopped", "and still at rest");

    // By name: a name reads no file, so this can only reach a sheep the flock
    // already holds.
    let started = shep(home).arg("start").arg("pending").output().unwrap();
    assert_success(&started);
    let running = poll_flock(home, |info| info["status"] == "online");
    assert_eq!(
        running["status"], "online",
        "the registered sheep came up: {running}"
    );

    graceful_kill(home);
}

/// `shep add` with no target and no Flockfile in the current directory is a
/// usage error, where bare `shep start` brings a shepherd up: `start`'s
/// empty-directory case means "give me a shepherd with nothing running yet",
/// and `add` produces a shepherd holding nothing either way. The temporary
/// directory is the working directory as well as the home, so there is no
/// Flockfile to discover.
#[test]
fn add_with_nothing_to_add_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    let output = shep(home).arg("add").current_dir(home).output().unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "clap's own code for bad arguments: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no target and no Flockfile"),
        "the refusal says what was missing: {stderr}"
    );
    assert!(
        !home.join("run").join("shep.sock").exists(),
        "and no shepherd was started to answer a request nobody made"
    );
}

/// `$SUDO_USER` names `nobody` (passwd home `/var/empty` on macOS,
/// `/nonexistent` on Linux) and `$HOME` is a throwaway standing in for
/// root's. The refusal must name nobody's `~/.shep`, not `$HOME/.shep`.
/// Skipped as root: a broken gate would really install a unit.
#[cfg(unix)]
#[test]
fn a_sudo_startup_without_home_carries_the_target_users_home_not_this_processes() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipping: as root this would really install a unit if the gate were broken");
        return;
    }
    let Ok(Some(nobody)) = nix::unistd::User::from_name("nobody") else {
        eprintln!("skipping: no `nobody` user to stand in for $SUDO_USER");
        return;
    };
    let fake_root_home = TempDir::new().unwrap();

    let output = Command::cargo_bin("shep")
        .unwrap()
        .env("HOME", fake_root_home.path())
        .env("SUDO_USER", "nobody")
        .env_remove("SHEP_HOME")
        .arg("startup")
        .arg("--init")
        .arg("systemd")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let target_home = nobody.dir.join(".shep");
    let refusal = format!(
        "error[usage]: no directory at {}; create it first (any shep verb run as nobody \
         creates that user's own ~/.shep), or pass --home with the $SHEP_HOME this unit \
         should carry",
        target_home.display()
    );
    assert!(
        stderr.lines().any(|line| line == refusal),
        "the refusal names nobody's own home and both ways out: {stderr}"
    );
    assert!(
        !stderr.contains(fake_root_home.path().to_str().unwrap()),
        "and never this process's $HOME: {stderr}"
    );
    assert!(
        !fake_root_home.path().join(".shep").exists(),
        "nothing is created under a $HOME that is not the target user's"
    );
}

/// A three-app Flockfile drawing one chain: `db`, then `api`, then `web`.
///
/// Three stages is the point. Each gated stage is held for its own
/// `listen_timeout` before the next one spawns, so this is what a staged
/// start's wall clock is made of.
fn write_chained_flockfile(dir: &TempDir, script: &Path) -> PathBuf {
    write_flockfile(
        dir,
        &format!(
            "[[app]]\nname = \"db\"\nscript = '{script}'\n\
             [[app]]\nname = \"api\"\nscript = '{script}'\ndepends_on = [\"db\"]\n\
             [[app]]\nname = \"web\"\nscript = '{script}'\ndepends_on = [\"api\"]\n",
            script = script.display(),
        ),
    )
}

#[test]
fn starting_a_flockfile_with_a_cycle_refuses_and_names_it() {
    // fails if the cycle starts an arbitrary half of the flock anyway, or if
    // the refusal says only that a cycle exists without naming the path an
    // operator has to break
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"a\"\nscript = '{script}'\ndepends_on = [\"b\"]\n\
             [[app]]\nname = \"b\"\nscript = '{script}'\ndepends_on = [\"a\"]\n",
            script = script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let out = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);

    assert_eq!(out.status.code(), Some(4), "invalid_config");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(" -> "),
        "the cycle must be named as a path: {stderr}"
    );
    graceful_kill(home);
}

#[test]
fn a_three_stage_start_brings_every_stage_online() {
    // fails if a staged start does not survive the round trip: the reply
    // lands only after the last stage, so a deadline sized for one spawn
    // abandons a flock the shepherd is still bringing up correctly
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    let flockfile = write_chained_flockfile(&dir, &script);
    let mut guard = DaemonGuard::default();

    let out = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&out);

    let data = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.len() == 3 && rows.iter().all(|row| row["status"] == "online"))
    });
    let names: Vec<&str> = data
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 3, "every stage came up: {data}");
    graceful_kill(home);
}

mod adopt;
mod assertions;
mod available_dogs;
mod constants;
mod daemon_log;
mod dev;
mod dispatch;
mod dogs_kv;
mod fixtures;
mod flockfile_load;
mod guard;
mod handover_basic;
mod handover_clustered;
mod home_and_watch;
mod import_env;
mod init_verb;
mod lifecycle;
mod logs;
mod lookout_whistle;
mod polling;
mod real_clock;
mod rendering;
mod runtime;
mod serve;
mod spawn_failures;

pub(crate) use assertions::*;
pub(crate) use constants::*;
pub(crate) use fixtures::*;
pub(crate) use guard::*;
pub(crate) use handover_basic::*;
pub(crate) use handover_clustered::*;
pub(crate) use polling::*;
