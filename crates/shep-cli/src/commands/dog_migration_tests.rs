//! `dog_migration`'s tests: the one-time move of `[dog.<name>]` out of
//! `shep.toml` into `dogs.toml`, and the two refusals a boot can make.
//!
//! Split out of `dog_migration.rs` under IR-48. Reached as
//! `dog_migration::tests` through the `#[path]` declaration at the end of
//! that file, so every test keeps the path a caller cites it by.

use super::*;

fn home_with(shep_toml: &str) -> (tempfile::TempDir, ShepPaths) {
    let dir = tempfile::tempdir().expect("tempdir");
    // `ShepPaths` has no temp-directory constructor and does not grow
    // one for a test's convenience: `resolve` with an empty environment
    // puts the home under `dir` and every other test in this crate
    // builds one the same way.
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).expect("home");
    std::fs::write(&paths.daemon_config, shep_toml).expect("write");
    (dir, paths)
}

#[test]
fn sections_move_and_the_original_loses_them() {
    let (_dir, paths) = home_with(
        "# mine\n[daemon]\nenabled_dogs = [\"metrics\"]\n\n[dog.metrics]\nbind = \"127.0.0.1:9615\"\n",
    );

    let moved = migrate_dog_sections(&paths).expect("migrate");

    assert_eq!(moved, vec!["metrics".to_string()]);
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        "# mine\n[daemon]\nenabled_dogs = [\"metrics\"]\n"
    );
    let written = std::fs::read_to_string(&paths.dogs_config).expect("read");
    let parsed = DogsConfig::load(Some(&written)).expect("valid");
    assert_eq!(
        parsed.dog["metrics"]["bind"].as_str(),
        Some("127.0.0.1:9615")
    );
}

#[test]
fn a_file_with_no_dog_sections_is_not_rewritten() {
    let before = "[daemon]\nlog_level = \"info\"\n";
    let (_dir, paths) = home_with(before);

    let moved = migrate_dog_sections(&paths).expect("migrate");

    assert!(moved.is_empty());
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        before
    );
    assert!(!paths.dogs_config.exists(), "no sections means no file");
}

#[test]
fn a_second_boot_writes_nothing() {
    let (_dir, paths) = home_with("[dog.metrics]\nbind = \"127.0.0.1:9615\"\n");
    migrate_dog_sections(&paths).expect("first");
    let after_first = std::fs::read_to_string(&paths.dogs_config).expect("read");

    let moved = migrate_dog_sections(&paths).expect("second");

    assert!(moved.is_empty());
    assert_eq!(
        std::fs::read_to_string(&paths.dogs_config).expect("read"),
        after_first
    );
}

// `metrics = 5` is legal TOML: a non-table value under `[dog]`, dropped
// by `declared_dog_names`'s own filter.
#[test]
fn entries_under_dog_that_come_back_as_nothing_make_the_migration_refuse() {
    let (_dir, paths) = home_with("[dog]\nmetrics = 5\n");

    let err = migrate_dog_sections(&paths).expect_err("a section that will not travel");

    assert!(matches!(err, DogMigrationError::SectionsUnreadable { .. }));
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        "[dog]\nmetrics = 5\n",
        "a refused migration strikes nothing"
    );
    assert!(!paths.dogs_config.exists(), "nor writes anything");
}

// A `[dog]` header with nothing under it has no section to move or
// lose, so it is struck rather than refused.
#[test]
fn an_empty_dog_table_is_struck_without_a_refusal() {
    let (_dir, paths) = home_with("[daemon]\nlog_level = \"info\"\n\n[dog]\n");

    let moved = migrate_dog_sections(&paths).expect("migrate");

    assert!(moved.is_empty());
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        "[daemon]\nlog_level = \"info\"\n"
    );
    assert!(!paths.dogs_config.exists(), "no sections means no file");
}

// `stray` comes back as an entry, not a section, so it would be
// silently dropped rather than moved; refused instead, naming it.
#[test]
fn an_entry_that_comes_back_beside_a_section_but_not_as_one_makes_the_migration_refuse() {
    let (_dir, paths) = home_with("[dog]\nstray = 5\n\n[dog.metrics]\nbind = \"127.0.0.1:9615\"\n");

    let err = migrate_dog_sections(&paths).expect_err("stray would be dropped");

    let DogMigrationError::SectionsUnreadable { names } = &err else {
        panic!("expected a refusal naming what would be lost, got {err:?}");
    };
    assert_eq!(names, &vec!["stray".to_string()]);
    assert!(err.to_string().contains("stray"), "{err}");
    assert!(
        std::fs::read_to_string(&paths.daemon_config)
            .expect("read")
            .contains("stray = 5"),
        "a refused migration strikes nothing"
    );
    assert!(!paths.dogs_config.exists(), "nor writes anything");
}

/// `dogs.toml` is created at `0600`, at the `open` rather than by a
/// later `chmod`. Fails if the migration goes back to `std::fs::write`,
/// which would leave it at the ambient umask.
#[cfg(unix)]
#[test]
fn the_written_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let (_dir, paths) = home_with("[dog.metrics]\nbind = \"127.0.0.1:9615\"\n");

    migrate_dog_sections(&paths).expect("migrate");

    let mode = std::fs::metadata(&paths.dogs_config)
        .expect("stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "mode was {mode:o}");
}

/// Exact string: it pins that the moved section arrives carrying its
/// own `shep.toml` comments, which a `toml::to_string` of a parsed map
/// would drop, and that it lands after everything already there.
#[test]
fn migrating_into_a_hand_edited_file_keeps_both_files_comments() {
    let (_dir, paths) = home_with(
        "[daemon]\nlog_level = \"info\"\n\n# scrape target\n[dog.metrics]\nbind = \"127.0.0.1:9615\" # loopback only\n",
    );
    std::fs::write(
        &paths.dogs_config,
        "# hand-written, do not clobber\n[otel]\nendpoint = \"127.0.0.1:4317\"\nheaders = { auth = \"x\" }\n",
    )
    .expect("write");

    let moved = migrate_dog_sections(&paths).expect("migrate");

    assert_eq!(moved, vec!["metrics".to_string()]);
    assert_eq!(
        std::fs::read_to_string(&paths.dogs_config).expect("read"),
        "# hand-written, do not clobber\n[otel]\nendpoint = \"127.0.0.1:4317\"\nheaders = { auth = \"x\" }\n\n# scrape target\n[metrics]\nbind = \"127.0.0.1:9615\" # loopback only\n"
    );
}

/// Fails if a moved dog with sub-tables and an array of tables is
/// split across the destination's own sections.
///
/// `toml_edit` renders every table by its own position, not its
/// parent's, so `renumber_tables` must descend into `[bark.sinks]` and
/// `[[bark.rules]]` too, or they stay at the indices they held in
/// `shep.toml` and interleave with `[a]`, `[b]` and `[c]` here.
#[test]
fn a_moved_dog_with_sub_tables_lands_in_one_piece_at_the_end() {
    let (_dir, paths) = home_with(
        "[dog.bark.sinks]\noncall = { url = \"u\" }\n\n[[dog.bark.rules]]\non = \"gave_up\"\n",
    );
    std::fs::write(
        &paths.dogs_config,
        "[a]\nk = 1\n\n[b]\nk = 2\n\n[c]\nk = 3\n",
    )
    .expect("write");

    migrate_dog_sections(&paths).expect("migrate");

    let written = std::fs::read_to_string(&paths.dogs_config).expect("read");
    let headers: Vec<&str> = written
        .lines()
        .filter(|line| line.starts_with('['))
        .collect();
    assert_eq!(
        headers,
        vec!["[a]", "[b]", "[c]", "[bark.sinks]", "[[bark.rules]]"],
        "the migration appends; it does not interleave: {written}"
    );
}

/// Fails if a destination with nested or array-of-tables sections puts
/// the moved section at the front instead of the end.
///
/// `last_position` must count `[bark.sinks]` and `[[bark.rules]]`, not
/// just top-level `Item::Table`s, or it answers 0 and `[metrics]`
/// renders first. Exact string: a reparse agrees with a wrong rendering.
#[test]
fn a_moved_dog_lands_after_a_destination_of_nested_and_array_tables() {
    let (_dir, paths) =
        home_with("[daemon]\nlog_level = \"info\"\n\n[dog.metrics]\nbind = \"127.0.0.1:19615\"\n");
    let before = "[bark.sinks]\noncall = { url = \"u\" }\n\n[[bark.rules]]\non = \"gave_up\"\n\n[[bark.rules]]\non = \"restarted\"\n";
    std::fs::write(&paths.dogs_config, before).expect("write");

    migrate_dog_sections(&paths).expect("migrate");

    assert_eq!(
        std::fs::read_to_string(&paths.dogs_config).expect("read"),
        format!("{before}\n[metrics]\nbind = \"127.0.0.1:19615\"\n")
    );
}

/// Fails if an empty `[dog.<name>]` can stop a shepherd booting.
///
/// An older `shep enable` scaffolds an empty `[dog.metrics]`, and a
/// mixed-version host is ordinary.
///
/// The configured `dogs.toml` wins untouched, and `shep.toml` loses its
/// stray header.
#[test]
fn an_empty_section_colliding_with_a_configured_one_is_not_a_refusal() {
    let (_dir, paths) = home_with("[daemon]\nenabled_dogs = [\"metrics\"]\n\n[dog.metrics]\n");
    let dogs = "[metrics]\nbind = \"127.0.0.1:19616\"\n";
    std::fs::write(&paths.dogs_config, dogs).expect("write");

    let moved = migrate_dog_sections(&paths).expect("an empty table is not a second value");

    assert!(moved.is_empty());
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        "[daemon]\nenabled_dogs = [\"metrics\"]\n"
    );
    assert_eq!(
        std::fs::read_to_string(&paths.dogs_config).expect("read"),
        dogs
    );
}

/// `[[dog.metrics]]` with nothing under it: `take_dog_sections` hands
/// back no array for it, so the name guard must not call this
/// `SectionsUnreadable`, or a header holding nothing refuses the boot.
#[test]
fn an_empty_array_of_tables_under_dog_is_skipped_rather_than_refused() {
    let (_dir, paths) = home_with("[[dog.metrics]]\n");

    let moved = migrate_dog_sections(&paths).expect("nothing declared, nothing to lose");

    assert!(moved.is_empty());
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        ""
    );
}

/// An array of tables that carries values has no one section to
/// become, so the name guard refuses rather than dropping it silently.
#[test]
fn an_array_of_tables_with_values_still_makes_the_migration_refuse() {
    let before = "[[dog.metrics]]\nbind = \"127.0.0.1:9615\"\n";
    let (_dir, paths) = home_with(before);

    let err = migrate_dog_sections(&paths).expect_err("values with nowhere to go");

    assert!(matches!(
        err,
        DogMigrationError::SectionsUnreadable { ref names } if names == &["metrics".to_string()]
    ));
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        before
    );
}

/// An empty section beside a real one still lets the real one move, and
/// the empty header goes with it: `take_dog_sections` strikes the whole
/// `[dog]` table, and an empty scaffold is not something to preserve.
#[test]
fn an_empty_section_beside_a_real_one_does_not_block_it() {
    let (_dir, paths) = home_with("[dog.metrics]\n\n[dog.bark.sinks]\noncall = { url = \"u\" }\n");

    let moved = migrate_dog_sections(&paths).expect("migrate");

    assert_eq!(moved, vec!["bark".to_string()]);
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        ""
    );
    let written = std::fs::read_to_string(&paths.dogs_config).expect("read");
    assert!(
        !written.contains("metrics"),
        "an empty scaffold is not carried across: {written}"
    );
}

/// Fails if a bare header in `dogs.toml` can refuse a section that
/// carries values.
///
/// `WouldOverwrite` exists because two values for one key is a
/// question shep cannot answer; a bare `[metrics]` holds no value, so
/// there is no second answer to guess between.
#[test]
fn a_bare_header_in_dogs_toml_does_not_refuse_a_configured_section() {
    let (_dir, paths) = home_with("[dog.metrics]\nbind = \"127.0.0.1:19615\"\n");
    std::fs::write(&paths.dogs_config, "[metrics]\n").expect("write");

    let moved = migrate_dog_sections(&paths).expect("an empty destination is not a value");

    assert_eq!(moved, vec!["metrics".to_string()]);
    let written = std::fs::read_to_string(&paths.dogs_config).expect("read");
    let parsed = DogsConfig::load(Some(&written)).expect("valid");
    assert_eq!(
        parsed.dog["metrics"]["bind"].as_str(),
        Some("127.0.0.1:19615"),
        "the section that had values is the one left standing: {written}"
    );
}

/// Fails if a header spelled with spaces inside the brackets strands
/// its section in `shep.toml`.
///
/// `[ dog.metrics ]` is ordinary TOML and means exactly what
/// `[dog.metrics]` means, so it must parse the same as the tight form
/// rather than by a `"[dog."` substring match.
#[test]
fn a_header_spelled_with_whitespace_still_migrates() {
    let (_dir, paths) = home_with(
        "[daemon]\nenabled_dogs = [\"metrics\"]\n\n[ dog.metrics ]\nbind = \"127.0.0.1:19615\"\n",
    );

    let moved = migrate_dog_sections(&paths).expect("migrate");

    assert_eq!(moved, vec!["metrics".to_string()]);
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        "[daemon]\nenabled_dogs = [\"metrics\"]\n",
        "the section leaves shep.toml"
    );
    let written = std::fs::read_to_string(&paths.dogs_config).expect("read");
    let parsed = DogsConfig::load(Some(&written)).expect("valid");
    assert_eq!(
        parsed.dog["metrics"]["bind"].as_str(),
        Some("127.0.0.1:19615")
    );
}

/// Fails if a boot with nothing to migrate opens `shep.toml` for
/// editing anyway.
///
/// The inode is the assertion, not the bytes, which are identical
/// either way: `try_edit` renames a temp file over the original
/// whenever `save` runs.
#[cfg(unix)]
#[test]
fn a_boot_with_nothing_to_move_never_reopens_the_file() {
    use std::os::unix::fs::MetadataExt as _;

    let (_dir, paths) = home_with("[daemon]\nlog_level = \"info\"\n");
    let before = std::fs::metadata(&paths.daemon_config).expect("stat").ino();

    assert!(migrate_dog_sections(&paths).expect("migrate").is_empty());

    let after = std::fs::metadata(&paths.daemon_config).expect("stat").ino();
    assert_eq!(before, after, "an idle boot must not rewrite shep.toml");
}

// Two values for one key: picking either would be shep guessing, so
// this refuses instead of merging.
#[test]
fn an_existing_dogs_file_makes_the_migration_refuse() {
    let (_dir, paths) = home_with("[dog.metrics]\nbind = \"127.0.0.1:9615\"\n");
    std::fs::write(&paths.dogs_config, "[metrics]\nbind = \"0.0.0.0:9615\"\n").expect("write");

    let err = migrate_dog_sections(&paths).expect_err("both files hold metrics");

    assert!(matches!(
        err,
        DogMigrationError::WouldOverwrite { ref name, .. } if name == "metrics"
    ));
    assert!(
        std::fs::read_to_string(&paths.daemon_config)
            .expect("read")
            .contains("[dog.metrics]"),
        "a refused migration strikes nothing"
    );
}

/// Fails if the both-files refusal leaves an operator with nowhere to put
/// the `env` they were writing.
///
/// `env` is the one setting a dog never reads from its own section, and
/// `shep.toml` is where an operator reaches for it, so this is the reading
/// the refusal most often has to give. Exact string, and no value from the
/// `env`: the message names a place, never a credential.
#[test]
fn a_section_holding_env_is_told_where_the_env_goes() {
    let shep_toml = "[dog.kelpie]\nworker = \"5\"\nenv = { PADDOCK_KEY = \"hunter2\" }\n";
    let dogs_toml = "[kelpie]\nworker = \"3\"\n";
    let (_dir, paths) = home_with(shep_toml);
    std::fs::write(&paths.dogs_config, dogs_toml).expect("write");

    let err = migrate_dog_sections(&paths).expect_err("both files hold kelpie");

    let DogMigrationError::WouldOverwrite { name, holds_env } = &err else {
        panic!("expected the env reading of the refusal, got {err:?}");
    };
    assert_eq!(name, "kelpie");
    assert!(holds_env, "{err}");
    assert_eq!(
        err.to_string(),
        "dog 'kelpie' is configured in both shep.toml and dogs.toml; \
         a dog's env goes beside its path in [daemon.adopted_dogs], as \
         kelpie = { path = \"...\", env = { KEY = \"{{secret:name}}\" } }; move env there, \
         choose the remaining settings to keep in [kelpie] in dogs.toml, then delete \
         [dog.kelpie] from shep.toml and start the daemon again"
    );
    assert!(
        !err.to_string().contains("hunter2") && !err.to_string().contains("PADDOCK_KEY"),
        "the refusal names a place, never the value it was written for: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        shep_toml,
        "a refused migration preserves env and the other settings"
    );
    assert_eq!(
        std::fs::read_to_string(&paths.dogs_config).expect("read"),
        dogs_toml,
        "the operator chooses which settings to keep"
    );
}

/// Fails if the env guidance arrives where there is no `env` to guide: a
/// collision between two ordinary settings sections keeps the text it has
/// always had.
#[test]
fn a_section_holding_no_env_keeps_the_plain_refusal() {
    let (_dir, paths) = home_with("[dog.ledger]\nworker = \"5\"\n");
    std::fs::write(&paths.dogs_config, "[ledger]\nworker = \"3\"\n").expect("write");

    let err = migrate_dog_sections(&paths).expect_err("both files hold ledger");

    let DogMigrationError::WouldOverwrite { name, holds_env } = &err else {
        panic!("expected the plain reading of the refusal, got {err:?}");
    };
    assert_eq!(name, "ledger");
    assert!(!holds_env, "{err}");
    assert_eq!(
        err.to_string(),
        "dog 'ledger' is configured in both shep.toml and dogs.toml; \
         delete one of the two sections and start the daemon again"
    );
}

// --- dogs.toml's lock, contended -----------------------------------

/// How many times [`a_boot_migration_and_a_dog_config_write_at_once_both_land`]
/// re-runs its race.
///
/// A lost update needs both threads to read `dogs.toml` before either
/// renames, and on this write path that window is wide. Twenty makes
/// the failure a certainty rather than a likelihood.
const ROUNDS_OF_CONTENTION: u32 = 20;

/// The two `dogs.toml` writers this test races: [`migrate_dog_sections`]
/// at boot and reload, and [`shep_daemon::dogs::set_dog_section`] behind
/// `Request::SetDogConfig`. Each rewrites the whole file.
///
/// `ConfigLock` is what makes the read, the merge and the rename one
/// transaction, proven here by forcing the interleaving with a barrier.
/// Pointing `_dogs_lock` at a path unique to each thread turns this red.
#[test]
fn a_boot_migration_and_a_dog_config_write_at_once_both_land() {
    use std::sync::{Arc, Barrier};

    for round in 0..ROUNDS_OF_CONTENTION {
        let (_dir, paths) = home_with("[dog.metrics]\nbind = \"127.0.0.1:9615\"\n");

        let gate = Arc::new(Barrier::new(2));
        let migrating = {
            let gate = Arc::clone(&gate);
            let paths = paths.clone();
            std::thread::spawn(move || {
                gate.wait();
                migrate_dog_sections(&paths)
            })
        };
        let setting = {
            let gate = Arc::clone(&gate);
            let dogs_config = paths.dogs_config.clone();
            std::thread::spawn(move || {
                gate.wait();
                shep_daemon::dogs::set_dog_section(&dogs_config, "watchdog", "every = \"30s\"\n")
            })
        };
        let moved = migrating.join().expect("thread").expect("migrate");
        let set = setting.join().expect("thread");
        assert!(
            set.is_ok(),
            "round {round}: set_dog_section failed: {set:?}"
        );

        let written = std::fs::read_to_string(&paths.dogs_config).expect("read");
        let parsed = DogsConfig::load(Some(&written)).expect("valid");
        assert!(
            parsed.dog.contains_key("metrics"),
            "round {round}: the migrated section must survive, got {written:?} (moved {moved:?})"
        );
        assert!(
            parsed.dog.contains_key("watchdog"),
            "round {round}: the config-pane write must survive, got {written:?}"
        );
    }
}

/// Fails if a header is left behind once its values are in `dogs.toml`:
/// an empty `[dog.metrics]` reads as the place a dog's settings go.
#[test]
fn a_migrated_section_leaves_no_header_in_shep_toml() {
    let (_dir, paths) = home_with("[dog.metrics]\nbind = \"127.0.0.1:9615\"\n");

    let moved = migrate_dog_sections(&paths).expect("migrate");

    assert_eq!(moved, vec!["metrics".to_string()]);
    let left = std::fs::read_to_string(&paths.daemon_config).expect("read");
    assert!(!left.contains("dog"), "no [dog] or [dog.metrics]: {left}");
    let written = std::fs::read_to_string(&paths.dogs_config).expect("read");
    assert!(written.contains("bind = \"127.0.0.1:9615\""), "{written}");
}

/// A file an older shep migrated holds only empty headers: they go at the
/// next boot, and nothing else in the file moves.
#[test]
fn empty_dog_headers_are_removed_and_the_rest_of_the_file_stays() {
    let (_dir, paths) = home_with(
        "# mine\n[daemon]\nlog_level = \"info\" # loud\n\n[dog]\n\n[dog.whistle]\n\n[style]\n# keep\nlevel = \"full\"\n",
    );

    let moved = migrate_dog_sections(&paths).expect("migrate");

    assert!(moved.is_empty());
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        "# mine\n[daemon]\nlog_level = \"info\" # loud\n\n[style]\n# keep\nlevel = \"full\"\n"
    );
    assert!(!paths.dogs_config.exists(), "nothing to move, no file");
}

/// Fails if a comment written above an empty header goes with it.
#[test]
fn a_comment_above_an_empty_header_stays_above_where_it_was() {
    let (_dir, paths) = home_with(
        "[daemon]\nlog_level = \"info\"\n\n# whistle settings\n[dog.whistle]\n\n[style]\nlevel = \"full\"\n",
    );

    migrate_dog_sections(&paths).expect("migrate");

    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        "[daemon]\nlog_level = \"info\"\n\n# whistle settings\n\n[style]\nlevel = \"full\"\n"
    );
}

/// With nothing after the header, the comment ends the file.
#[test]
fn a_comment_above_a_last_empty_header_ends_the_file() {
    let (_dir, paths) =
        home_with("[daemon]\nlog_level = \"info\"\n\n# whistle settings\n[dog.whistle]\n");

    migrate_dog_sections(&paths).expect("migrate");

    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        "[daemon]\nlog_level = \"info\"\n\n# whistle settings\n"
    );
}

/// The cleaned file has no `[dog]` left, so a second boot returns before
/// opening it.
#[test]
fn a_second_boot_over_cleaned_headers_changes_nothing() {
    let (_dir, paths) = home_with("# note\n[dog]\n[dog.whistle]\n\n[style]\nlevel = \"full\"\n");
    migrate_dog_sections(&paths).expect("first");
    let after_first = std::fs::read_to_string(&paths.daemon_config).expect("read");

    let moved = migrate_dog_sections(&paths).expect("second");

    assert!(moved.is_empty());
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).expect("read"),
        after_first
    );
}
