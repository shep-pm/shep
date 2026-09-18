//! Loading a Flockfile: what one refused app does to the rest, and what a
//! spawn no check could have caught still reports.

use super::*;

/// A Flockfile edit reaches a registered sheep only where the first load
/// established nothing, and what it reaches is reported by name, never by
/// value. A key the first load established belongs to whoever set it. A key
/// nobody has established is appended, applied where it can be and parked
/// where it cannot, and the line naming a parked field names `shep reload`.
#[test]
fn a_flockfile_edit_reaches_a_sheep_only_where_the_first_load_established_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let elsewhere = tempfile::tempdir().unwrap();
    let script = write_test_script(&dir);
    // `env` carries a value that must never be printed, and the third file
    // changes two fields at once, so a report that stopped at the first
    // difference fails here too.
    let body = |cwd: &Path, env: &str, extra: &str| {
        format!(
            "[[app]]\nname = \"edited\"\nscript = '{}'\ncwd = '{}'\n{extra}env = {{ {env} }}\n",
            script.display(),
            cwd.display(),
        )
    };
    let flockfile = write_flockfile(&dir, &body(home, "API_TOKEN = \"hunter2-before\"", ""));
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);
    poll_flock(home, |info| info["status"] == "online");

    // The edit, over the same path the daemon was told about. The first load
    // established both keys, so this load may do nothing at all.
    write_flockfile(
        &dir,
        &body(elsewhere.path(), "API_TOKEN = \"hunter2-after\"", ""),
    );
    let again = shep(home).arg("start").arg(&flockfile).output().unwrap();
    // A load reports, it does not fail.
    assert_success(&again);
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(
        !stderr.contains("cwd") && !stderr.contains("env"),
        "an established key is not the file's to change, so there is nothing \
         to report: {stderr}"
    );
    let info = poll_flock(home, |info| info["status"] == "online");
    assert!(
        info["pending"].is_null(),
        "an established key must not be parked either: {info}"
    );

    // A third load, adding a key nobody has established: one that reaches the
    // running child and one that cannot.
    write_flockfile(
        &dir,
        &body(
            elsewhere.path(),
            "API_TOKEN = \"hunter2-after\", MODE = \"blue\"",
            "max_memory = \"512M\"\n",
        ),
    );
    let third = shep(home).arg("start").arg(&flockfile).output().unwrap();
    assert_success(&third);

    let stderr = String::from_utf8_lossy(&third.stderr);
    assert!(
        stderr.contains("edited"),
        "the report must name the sheep: {stderr}"
    );
    assert!(
        stderr.contains("max_memory") && stderr.contains("env"),
        "the report must name every field that changed: {stderr}"
    );
    assert!(
        !stderr.contains("cwd"),
        "the established key is still nobody's to change: {stderr}"
    );
    assert!(
        !stderr.contains("hunter2") && !stderr.contains("blue"),
        "a field's VALUE must never reach an operator's terminal (IR-41): {stderr}"
    );
    // An env change is baked into a running child, so the report has to say
    // what brings it into effect, not only which field moved.
    assert!(
        stderr.contains("shep reload edited"),
        "a pending field travels with the verb that promotes it: {stderr}"
    );

    graceful_kill(home);
}

/// A load that refused one app exits non-zero, and still reports the app it
/// applied. The refusal is a real one: a plain load never reshapes a flock, so
/// a file that grows an `instances` line is refused that one field by name and
/// told which flag would take it.
#[test]
fn a_load_that_refused_one_app_exits_non_zero_and_still_reports_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    // `steady` carries the field that lands, `stocky` the one that cannot.
    // `max_restarts` is read when a sheep exits, so it is in force the moment
    // it reaches the stored spec and reports as applied.
    let body = |steady: &str, stocky: &str| {
        format!(
            "[[app]]\nname = \"steady\"\nscript = '{}'\n{steady}\
             [[app]]\nname = \"stocky\"\nscript = '{}'\n{stocky}",
            script.display(),
            script.display(),
        )
    };
    let flockfile = write_flockfile(&dir, &body("", ""));
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);
    poll_flock(home, |info| info["status"] == "online");

    // One field each: one the daemon applies, one it refuses.
    write_flockfile(&dir, &body("max_restarts = 9\n", "instances = 2\n"));
    let again = shep(home).arg("start").arg(&flockfile).output().unwrap();

    let stderr = String::from_utf8_lossy(&again.stderr);
    // Pinned at 4: `InvalidConfig` is the code the rest of this CLI uses for a
    // configuration the daemon would not accept.
    assert_eq!(
        again.status.code(),
        Some(4),
        "a refused load is a failed load, and an invalid-config one: {stderr}"
    );
    assert!(
        stderr.contains("stocky") && stderr.contains("reshapes a flock"),
        "the refusal names the app and what was refused: {stderr}"
    );
    assert!(
        stderr.contains("applied max_restarts"),
        "and the app that DID apply is still reported beside it: {stderr}"
    );

    graceful_kill(home);
}

/// One app whose script does not exist refuses the whole Flockfile, before
/// anything is registered, and names every app that failed. Without the check,
/// an app that fails to spawn partway through leaves the apps before it
/// registered and the apps after it unreached.
#[test]
fn one_absent_script_refuses_the_whole_flockfile_and_registers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    // `good` first, so a check that runs per app as it registers would have
    // registered it by the time it reached `unbuilt`.
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"good\"\nscript = '{}'\n\n\
             [[app]]\nname = \"unbuilt\"\nscript = '{}/never-built'\n",
            script.display(),
            dir.path().display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);

    assert_eq!(
        output.status.code(),
        Some(7),
        "the spawn-failed exit code: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unbuilt"),
        "the refusal must name the app: {stderr}"
    );
    assert!(
        stderr.contains("never-built"),
        "the refusal must name the path it looked at: {stderr}"
    );

    let flock = shep(home)
        .arg("--format")
        .arg("json")
        .arg("flock")
        .output()
        .unwrap();
    assert_success(&flock);
    let envelope: serde_json::Value = serde_json::from_slice(&flock.stdout).unwrap();
    assert_eq!(
        envelope["data"].as_array().map(Vec::len),
        Some(0),
        "a Flockfile refused as a whole must leave NOTHING registered: {}",
        envelope
    );

    graceful_kill(home);
}

/// A spawn that fails for a reason no preflight could see still names the
/// sheep, the path it tried, and the `cwd` it tried it in. A bare
/// `SpawnFailed` names none of them.
///
/// The script exists and cannot be exec'd, so this reaches the real `spawn`
/// and its real `EACCES` rather than the batch existence check.
#[test]
fn a_spawn_that_no_check_could_have_caught_still_names_the_sheep_and_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let unrunnable = dir.path().join("unrunnable.sh");
    std::fs::write(&unrunnable, "#!/bin/sh\nsleep 60\n").unwrap();
    // Present, so the batch check passes it; no execute bit anywhere, which
    // even a root-owned run cannot exec.
    #[cfg(unix)]
    {
        std::fs::set_permissions(&unrunnable, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"locked-out\"\nscript = '{}'\n",
            unrunnable.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);

    assert_eq!(
        output.status.code(),
        Some(7),
        "the spawn-failed exit code: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("locked-out"),
        "the error must name the sheep: {stderr}"
    );
    assert!(
        stderr.contains("unrunnable.sh"),
        "the error must name the script it tried: {stderr}"
    );
    // Canonicalized: `start` fills an app's absent `cwd` from the
    // Flockfile's own directory through `canonicalize`, and on macOS a
    // tempdir's `/var/...` resolves to `/private/var/...`.
    let flockfile_dir = as_shep_spells_it(dir.path());
    assert!(
        stderr.contains(&format!("in {flockfile_dir}")),
        "the error must name the cwd it tried it in: {stderr}"
    );

    graceful_kill(home);
}

/// A bare command not on the shepherd's PATH is reported, fails to spawn, and
/// takes no other app in the Flockfile down with it. A `script` with a `/` in
/// it is a filesystem claim the daemon can settle, so that one is refused as a
/// batch instead. The `PATH` deciding a bare command is the daemon's: under a
/// `shep startup` unit, whatever launchd or systemd hands it, with
/// `assemble`'s fallback of `/usr/local/bin:/usr/bin:/bin`.
#[test]
fn a_bare_command_off_the_path_takes_only_its_own_app_down() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let script = write_test_script(&dir);
    // `resolvable` first: it is the app that must survive, and a refusal of
    // the whole batch would leave it unregistered rather than online.
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"resolvable\"\nscript = '{}'\n\n\
             [[app]]\nname = \"no-interpreter\"\nscript = \"shep-no-such-interpreter-xyz\"\n",
            script.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);

    // Exit 7 all the same: one app really did fail.
    assert_eq!(
        output.status.code(),
        Some(7),
        "the one app that cannot run still fails the command: {output:?}"
    );
    // The useful sentence reaches the operator's terminal, not only the
    // shepherd's log; `SpawnFailed` carries free-form text.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is not on the shepherd's PATH"),
        "the reply must explain WHY the program was not found, not only that \
         it was not: {stderr}"
    );
    assert!(
        stderr.contains("shep-no-such-interpreter-xyz") && stderr.contains("no-interpreter"),
        "naming the program and the sheep: {stderr}"
    );

    let data = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["name"] == "resolvable" && row["status"] == "online")
        })
    });
    // Found by hand rather than through `sheep_named`, which panics with its
    // own message: the regression makes the row absent, and a red run has to
    // say that.
    let survivor = data
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == "resolvable"));
    assert_eq!(
        survivor.map(|row| &row["status"]).map(ToString::to_string),
        Some("\"online\"".to_string()),
        "an app whose own script resolves must come up regardless of a \
         sibling's unresolvable interpreter, and must not be refused \
         registration over it: {data}"
    );

    let log = std::fs::read_to_string(home.join("logs").join("shepd.err.log")).unwrap();
    assert!(
        log.contains("shep-no-such-interpreter-xyz") && log.contains("PATH"),
        "the shepherd must still say which program it could not find: {log}"
    );
    assert!(
        log.contains("no-interpreter"),
        "and which sheep wanted it: {log}"
    );

    graceful_kill(home);
}

/// A real multi-instance flock through the real binary: distinct slots, a
/// grouped `shep flock` table, and a `merge_logs` app whose backlog prints each
/// line exactly once. A `shep bleats` that reads a file per matched row doubles
/// every line, which counting occurrences catches and a `contains` check does
/// not. A `sh`/`.cmd` script rather than node, so the case runs on Windows too.
#[test]
fn a_multi_instance_flock_gets_distinct_slots_a_grouped_table_and_undoubled_bleats() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let web = write_instance_logging_script(&dir, "web-instances", "web-slot");
    let merged = write_instance_logging_script(&dir, "merged-instances", "merged-slot");
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"web\"\nscript = '{}'\ninstances = 3\n\n\
             [[app]]\nname = \"merged\"\nscript = '{}'\ninstances = 2\nmerge_logs = true\n",
            web.display(),
            merged.display(),
        ),
    );
    let mut guard = DaemonGuard::default();

    let boot = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&boot);

    // 1. Five processes, and every one of them reports the slot it occupies.
    let data = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.iter().filter(|row| row["status"] == "online").count() == 5)
    });
    let slots_of = |name: &str| {
        let mut slots: Vec<u64> = data
            .as_array()
            .expect("flock data is an array")
            .iter()
            .filter(|row| row["name"] == name)
            .map(|row| {
                row["instance"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("every instance reports a slot: {data}"))
            })
            .collect();
        slots.sort_unstable();
        slots
    };
    assert_eq!(slots_of("web"), vec![0, 1, 2], "distinct slots: {data}");
    assert_eq!(slots_of("merged"), vec![0, 1], "distinct slots: {data}");

    // 2. The table names each row by its own slot. The flat shape, not the
    // boxed one: `must_render_bare` drops any run whose stdout is not a
    // terminal to `StyleLevel::Bare`, and `--style plain` does not override it.
    let table = shep(home).arg("flock").output().unwrap();
    assert_success(&table);
    let rendered = String::from_utf8_lossy(&table.stdout);
    for slot in 0..3 {
        assert!(
            rendered.contains(&format!("web:{slot}")),
            "a row named for slot {slot}: {rendered}"
        );
    }
    assert!(
        rendered.contains("merged:0") && rendered.contains("merged:1"),
        "and the same for the merged app: {rendered}"
    );

    // 3. The regression guard. First, that `merge_logs` really did collapse
    // both instances onto one path: without that the count below is vacuous.
    let out_files: Vec<&str> = data
        .as_array()
        .expect("flock data is an array")
        .iter()
        .filter(|row| row["name"] == "merged")
        .map(|row| {
            row["out_file"]
                .as_str()
                .unwrap_or_else(|| panic!("a running sheep reports its out file: {data}"))
        })
        .collect();
    assert_eq!(out_files.len(), 2, "{data}");
    assert_eq!(
        out_files[0], out_files[1],
        "merge_logs points both instances at one file: {data}"
    );

    // Then that the shared file is read once, not once per matched row.
    let backlog =
        bleats_no_follow_until_contains(home, &["merged"], &["merged-slot-0", "merged-slot-1"]);
    assert_eq!(
        backlog.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&backlog.stderr)
    );
    let stdout = String::from_utf8_lossy(&backlog.stdout);
    for slot in 0..2 {
        let needle = format!("merged-slot-{slot}");
        assert_eq!(
            stdout.matches(&needle).count(),
            1,
            "a shared log file is read once, not once per instance: {stdout}"
        );
    }

    graceful_kill(home);
}
