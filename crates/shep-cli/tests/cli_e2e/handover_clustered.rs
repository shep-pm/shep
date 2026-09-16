//! A reload under a clustered flock: every slot keeps its pid and its
//! instance, and a channel sheep still answers a trigger afterwards.

use super::*;

/// A script that says which slot it is and which process it is, on every line.
/// `$SHEP_INSTANCE` is injected at the spawn and fixed for the life of the
/// process, so a line naming a slot is the child's claim rather than the
/// shepherd's, which is what makes a slot swap visible.
#[cfg(unix)]
pub(crate) fn write_slot_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "slot.sh",
        &format!(
            "{}{}while :; do\n  echo \"slot=$SHEP_INSTANCE pid=$$\"\n  sleep 0.2\ndone\n",
            script_header(),
            record_pid_line(dir),
        ),
    )
}

/// Every line of `path` that names a slot, as `(slot, pid)` pairs. Panics on a
/// line it cannot parse: a torn line is the failure these cases look for, and
/// dropping it would turn a lost write into a shorter list.
#[cfg(unix)]
pub(crate) fn slot_lines(path: &Path) -> Vec<(u32, u32)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(shep_core::logstamp::strip)
        .map(|line| {
            let (slot, pid) = line
                .split_once(' ')
                .unwrap_or_else(|| panic!("a torn line in {}: {line:?}", path.display()));
            let parse = |field: &str, prefix: &str| {
                field
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.parse::<u32>().ok())
                    .unwrap_or_else(|| panic!("a torn line in {}: {line:?}", path.display()))
            };
            (parse(slot, "slot="), parse(pid, "pid="))
        })
        .collect()
}

/// Waits until `path` holds at least `want` slot lines, or
/// [`HANDOVER_DEADLINE`] expires, and returns what it held on the last read.
#[cfg(unix)]
pub(crate) fn poll_slot_lines(path: &Path, want: usize) -> Vec<(u32, u32)> {
    let start = Instant::now();
    loop {
        let lines = slot_lines(path);
        if lines.len() >= want || start.elapsed() >= HANDOVER_DEADLINE {
            return lines;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

/// Polls `path` until the lines written after the first `before` of them
/// satisfy `ready`, or the handover deadline passes.
///
/// A line count is the wrong wait for a `merge_logs` app: both instances write
/// to one file, so "two more lines" is satisfied by either of them writing
/// twice. The caller therefore says what it is waiting for.
#[cfg(unix)]
fn poll_fresh_lines(
    path: &Path,
    before: usize,
    ready: impl Fn(&[(u32, u32)]) -> bool,
) -> Vec<(u32, u32)> {
    let start = Instant::now();
    loop {
        let lines = slot_lines(path);
        if lines.len() > before && ready(&lines[before..]) {
            return lines;
        }
        if start.elapsed() >= HANDOVER_DEADLINE {
            return lines;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

/// A clustered app is carried, and every instance comes back in its own slot.
///
/// Two apps, because `merge_logs` points every instance at one log file and
/// `handover::adopt::refuse_repeated_fds` refuses the entire blob when a
/// descriptor number appears twice. A slot swap leaves two live processes
/// adopted under each other's names, so each row's `out_file` is read back and
/// its lines have to agree with the row about slot and pid.
#[cfg(unix)]
#[test]
fn a_clustered_flock_keeps_every_pid_and_every_slot_across_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_slot_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"split\"\nscript = '{}'\ninstances = 2\n\n\
             [[app]]\nname = \"merged\"\nscript = '{}'\ninstances = 2\nmerge_logs = true\n",
            script.display(),
            script.display(),
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

    let before = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == 4
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let rows_before = rows_by_slot(&before);
    assert_eq!(
        rows_before.len(),
        4,
        "two apps at two instances each: {before}"
    );
    // The fixture check: `merge_logs` collapsing both instances onto one path
    // is the premise of half this case.
    assert_eq!(
        rows_before[&("merged".to_owned(), 0)].1,
        rows_before[&("merged".to_owned(), 1)].1,
        "merge_logs must really point both instances at one file"
    );
    assert_ne!(
        rows_before[&("split".to_owned(), 0)].1,
        rows_before[&("split".to_owned(), 1)].1,
        "without merge_logs each instance must have its own file"
    );
    for ((name, slot), (_, out_file)) in &rows_before {
        assert!(
            !poll_slot_lines(out_file, 1).is_empty(),
            "{name}:{slot} must be logging before the reload"
        );
    }
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
    // The exact sentence `handover::RefusedReason`'s `Display` ends with. A
    // looser probe would pass whether or not the reload was refused.
    assert!(
        !text.contains("falls back to a stop-and-start"),
        "a clustered flock is carried now, not refused: {text}"
    );

    let after = poll_flock_data(dir.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.len() == 4
                && rows
                    .iter()
                    .all(|row| row["status"] == "online" && !row["pid"].is_null())
        })
    });
    let rows_after = rows_by_slot(&after);
    assert_eq!(
        rows_after, rows_before,
        "every instance keeps its pid and its own log file: {after}"
    );

    // The mark is taken after the reload returned, not before it was issued. A
    // pid is carried across a handover, so an early mark lets a pre-reload line
    // satisfy "this instance wrote again".
    let counts_before: HashMap<(String, u32), usize> = rows_after
        .iter()
        .map(|(key, (_, out_file))| (key.clone(), slot_lines(out_file).len()))
        .collect();

    // The slot assertion, the one a pid check cannot make. Each row is asked
    // for its own file, and every line written into that file after the
    // reload has to name that row's slot and that row's pid.
    for ((name, slot), (pid, out_file)) in &rows_after {
        let before = counts_before[&(name.clone(), *slot)];
        // A split app's file holds nobody else, so two more lines in it are
        // two more lines from this row. A merged app's file holds both
        // instances, so the wait is this row's own pid turning up.
        let lines = if *name == "merged" {
            poll_fresh_lines(out_file, before, |fresh| {
                fresh.iter().any(|(_, line_pid)| line_pid == pid)
            })
        } else {
            poll_fresh_lines(out_file, before, |fresh| fresh.len() >= 2)
        };
        let fresh = lines.get(before..).unwrap_or(&[]);
        assert!(
            !fresh.is_empty(),
            "{name}:{slot} stopped logging across the handover: {} lines, none of them new",
            lines.len()
        );
        if *name == "merged" {
            // One file for both slots, so the row's own lines are the ones
            // carrying its pid. Both must be present, or a handle was lost.
            assert!(
                fresh.iter().any(|(_, line_pid)| line_pid == pid),
                "merged:{slot} wrote nothing after the reload: {fresh:?}"
            );
            for (line_slot, line_pid) in fresh {
                assert_eq!(
                    rows_after[&("merged".to_owned(), *line_slot)].0,
                    *line_pid,
                    "a merged line's slot and pid disagree with the flock: {fresh:?}"
                );
            }
        } else {
            assert!(
                fresh.len() >= 2,
                "{name}:{slot} stopped logging across the handover: {} fresh lines, wanted 2",
                fresh.len()
            );
            for (line_slot, line_pid) in fresh {
                assert_eq!(
                    (*line_slot, *line_pid),
                    (*slot, *pid),
                    "{name}:{slot}'s own log holds another instance's output: {fresh:?}"
                );
            }
        }
    }

    graceful_kill(dir.path());
}

/// `shep flock`'s JSON rows as `(name, instance) -> (pid, out_file)`. Keyed on
/// the pair, since a name matches as many rows as the app has instances.
#[cfg(unix)]
pub(crate) fn rows_by_slot(data: &serde_json::Value) -> BTreeMap<(String, u32), (u32, PathBuf)> {
    data.as_array()
        .unwrap_or_else(|| panic!("flock data is an array: {data}"))
        .iter()
        .map(|row| {
            let name = row["name"]
                .as_str()
                .unwrap_or_else(|| panic!("a row names its app: {row}"))
                .to_owned();
            let instance = u32::try_from(
                row["instance"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("a row names its slot: {row}")),
            )
            .unwrap();
            let pid = u32::try_from(
                row["pid"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("an online row names its pid: {row}")),
            )
            .unwrap();
            let out_file = PathBuf::from(
                row["out_file"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a row names its out file: {row}")),
            );
            ((name, instance), (pid, out_file))
        })
        .collect()
}

/// A `/bin/sh` sheep that signals readiness on fd 3 and answers every shepherd
/// message with the same reply. The `ready` line is what `wait_ready` holds the
/// sheep at `starting` for, the loop is what `shep trigger` gets an answer
/// from, and `read -r line <&3` is a plain blocking read, which a channel that
/// came back non-blocking would break. The reply names the action verbatim:
/// `ActionWaits` correlates on the action name when the app echoes no id.
#[cfg(unix)]
pub(crate) fn write_channel_script(dir: &TempDir) -> PathBuf {
    write_script(
        dir,
        "chatty.sh",
        &format!(
            "{}{}printf '{{\"kind\":\"ready\"}}\\n' >&3\nwhile read -r line <&3; do\n  \
             printf '{{\"kind\":\"action-reply\",\"action\":\"ping\",\"body\":\"pong\"}}\\n' \
             >&3\ndone\n",
            script_header(),
            record_pid_line(dir),
        ),
    )
}

/// Runs `shep trigger chatty ping` and returns the one row's outcome. Asked
/// identically either side of the reload, and the point is that the two
/// answers are the same.
#[cfg(unix)]
pub(crate) fn trigger_ping(home: &Path) -> serde_json::Value {
    let triggered = shep(home)
        .arg("--format")
        .arg("json")
        .arg("trigger")
        .arg("chatty")
        .arg("ping")
        .output()
        .unwrap();
    assert_success(&triggered);
    let envelope: serde_json::Value = serde_json::from_slice(&triggered.stdout)
        .unwrap_or_else(|e| panic!("trigger stdout was not JSON: {e}"));
    envelope["data"][0]["outcome"].clone()
}

/// A sheep's shepherd channel survives `shep daemon reload`, in both
/// directions and against a real app. A socketpair can survive as a number
/// attached to the wrong end, or be adopted with only one of its two pumps
/// rebuilt, and both leave the flock healthy and the pid unmoved. `wait_ready`
/// is on as well as `channel`, so `online` before the reload is itself proof
/// the child's `{"kind":"ready"}` came up the channel.
#[cfg(unix)]
#[test]
fn a_channel_sheep_still_answers_a_trigger_across_a_daemon_reload() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_channel_script(&dir);
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"chatty\"\nscript = '{}'\nchannel = true\nwait_ready = true\n",
            script.display()
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

    // `online` rather than `starting`, which only the child's own readiness
    // line over fd 3 can produce.
    let before = poll_flock(dir.path(), |info| {
        info["status"] == "online" && !info["pid"].is_null()
    });
    let pid_before = before["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("an online sheep reports a pid: {before}"));
    let answered = trigger_ping(dir.path());
    assert_eq!(
        answered["kind"], "replied",
        "the channel must work before the reload, or this case proves nothing: {answered}"
    );

    let reloaded = shep(dir.path())
        .arg("daemon")
        .arg("reload")
        .output()
        .unwrap();
    assert_success(&reloaded);

    let after = poll_flock(dir.path(), |info| {
        info["status"] == "online" && !info["pid"].is_null()
    });
    assert_eq!(
        after["pid"].as_u64(),
        Some(pid_before),
        "a moved pid means the sheep was respawned, which is the stop arm: {after}"
    );

    let still = trigger_ping(dir.path());
    assert_eq!(
        still["kind"], "replied",
        "the successor must reach the same fd 3 the child has had all along: {still}"
    );
    assert_eq!(still["body"], "pong", "{still}");

    graceful_kill(dir.path());
}
