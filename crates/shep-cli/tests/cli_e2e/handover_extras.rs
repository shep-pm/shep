//! What a reload has to re-arm: cron, memory limits, owed restarts, and a
//! flock carrying one of every kind at once.

use super::*;

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
