//! Enabling, disabling, adopting and rehoming a dog, plus taking the legacy
//! `[dog.<name>]` sections a pre-migration file still carries.

use super::*;

/// fails if the writer round-trips through a plain `toml::Table`,
/// losing comments and key order.
#[test]
fn enabling_a_dog_leaves_the_rest_of_the_file_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, PRESERVATION_SEED).unwrap();

    ShepToml::try_edit(&path, |doc| doc.enable_dog("metrics")).unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert_preserves_comments_and_key_order(&written);

    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(cfg.daemon.enabled_dogs, vec!["metrics"]);
    assert!(
        cfg.dog.is_empty(),
        "enable writes no dog section at all; the next boot refuses a \
             name held in both files: {written}"
    );
}
#[test]
fn enable_is_idempotent_and_disable_keeps_the_config_it_did_not_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[dog.bark]\ndebounce = \"30s\"\n").unwrap();

    ShepToml::edit(&path, |doc| {
        doc.enable_dog("bark").unwrap();
        doc.enable_dog("bark").unwrap();
    })
    .unwrap();
    let cfg =
        DaemonConfig::load(Some(&std::fs::read_to_string(&path).unwrap()), &|_| None).unwrap();
    assert_eq!(cfg.daemon.enabled_dogs, vec!["bark"]);

    ShepToml::edit(&path, |doc| doc.disable_dog("bark")).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert!(cfg.daemon.enabled_dogs.is_empty());
    assert!(
        written.contains("30s"),
        "disable stops a dog; it never touches what the operator wrote"
    );
}
/// Two places have to be empty afterwards, `[daemon] adopted_dogs` and
/// `enabled_dogs`, and one has to be untouched: a `[dog.<name>]` an
/// un-migrated file still carries.
#[test]
fn rehoming_a_dog_forgets_its_adoption_and_keeps_its_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    // Seeded by hand: no writer here creates a `[dog.<name>]` any more,
    // but an un-migrated file carries one.
    std::fs::write(&path, "[dog.otel]\ndebounce = \"30s\"\n").unwrap();
    ShepToml::edit(&path, |doc| {
        doc.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), false)
            .unwrap();
    })
    .unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(cfg.daemon.enabled_dogs, vec!["otel"]);
    assert_eq!(
        cfg.daemon
            .adopted_dogs
            .get("otel")
            .map(|entry| entry.path.as_path()),
        Some(Path::new("/usr/local/bin/shep-otel"))
    );
    assert!(cfg.dog.contains_key("otel"));

    ShepToml::edit(&path, |doc| doc.rehome_dog("otel")).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert!(cfg.daemon.enabled_dogs.is_empty());
    assert!(!cfg.daemon.adopted_dogs.contains_key("otel"));
    assert!(
        cfg.dog.contains_key("otel"),
        "the settings an operator wrote survive a rehome: {written}"
    );
}
#[test]
fn adopted_dog_path_reads_what_adopt_dog_wrote_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    ShepToml::edit(&path, |doc| {
        doc.enable_dog("metrics").unwrap(); // built-in: no `adopted_dogs` entry at all
        doc.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), false)
            .unwrap();

        assert_eq!(
            doc.adopted_dog_path("otel"),
            Some(PathBuf::from("/usr/local/bin/shep-otel"))
        );
        assert_eq!(doc.adopted_dog_path("metrics"), None);
        assert_eq!(doc.adopted_dog_path("ghost"), None);
    })
    .unwrap();
}
/// `enabled_dog_names` is `adopted_dog_names`'s sibling and was left
/// untouched by the brief's own six tests. A dog can be adopted and not
/// enabled, or (for a built-in) enabled without ever being adopted, so
/// this reads `[daemon] enabled_dogs` specifically, in file order.
#[test]
fn enabled_dog_names_reads_the_array_in_file_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon]\nenabled_dogs = [\"metrics\", \"bark\"]\n").unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.enabled_dog_names(), vec!["metrics", "bark"]);
}
/// A document with no `[daemon] enabled_dogs` at all reads as empty,
/// never a panic or a default entry invented on its behalf.
#[test]
fn enabled_dog_names_is_empty_when_the_document_never_wrote_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.enabled_dog_names(), Vec::<String>::new());
}
#[test]
fn taking_dog_sections_returns_them_keyed_by_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("shep.toml");
    std::fs::write(
            &path,
            "[daemon]\nenabled_dogs = [\"metrics\"]\n\n[dog.metrics]\nbind = \"127.0.0.1:9615\"\n\n[dog.bark.sinks]\noncall = { kind = \"discord\" }\n",
        )
        .expect("write");

    let taken = ShepToml::edit(&path, ShepToml::take_dog_sections).expect("edit");

    assert_eq!(taken.keys().collect::<Vec<_>>(), vec!["bark", "metrics"]);
    assert_eq!(taken["metrics"]["bind"].as_str(), Some("127.0.0.1:9615"));
}
#[test]
fn taking_dog_sections_leaves_every_other_section_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("shep.toml");
    std::fs::write(
            &path,
            "# keep me\n[daemon]\nenabled_dogs = [\"metrics\"]\n\n[dog.metrics]\nbind = \"127.0.0.1:9615\"\n\n[style]\nlevel = \"full\"\n",
        )
        .expect("write");

    ShepToml::edit(&path, ShepToml::take_dog_sections).expect("edit");

    // Exact string: a `toml::Table` round-trip would drop the comment
    // or reorder the key.
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "# keep me\n[daemon]\nenabled_dogs = [\"metrics\"]\n\n[style]\nlevel = \"full\"\n"
    );
}
#[test]
fn taking_from_a_file_with_no_dog_sections_changes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("shep.toml");
    let before = "[daemon]\nlog_level = \"info\"\n";
    std::fs::write(&path, before).expect("write");

    let taken = ShepToml::edit(&path, ShepToml::take_dog_sections).expect("edit");

    assert!(taken.is_empty());
    // Content identity, not proof that nothing was written: `edit` always
    // stages and renames, so the file has a new inode either way. Not
    // writing at all is the migration's job, and its own early return is
    // where that is tested.
    assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
}
#[test]
fn taking_dog_sections_keeps_nested_tables_and_arrays_of_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("shep.toml");
    std::fs::write(
            &path,
            "[dog.bark.sinks]\noncall = { kind = \"discord\", url = \"https://discord.com/api/webhooks/x\" }\n\n[[dog.bark.rules]]\non = \"gave_up\"\nsinks = [\"oncall\"]\n",
        )
        .expect("write");

    let taken = ShepToml::edit(&path, ShepToml::take_dog_sections).expect("edit");

    let bark = &taken["bark"];
    assert_eq!(
        bark["sinks"]["oncall"]["url"].as_str(),
        Some("https://discord.com/api/webhooks/x"),
        "a nested sub-table's own values must survive the take"
    );
    // `as_array_of_tables`, not `as_array`: `[[dog.bark.rules]]` is a
    // `toml_edit::ArrayOfTables`, a document construct, where `sinks =
    // ["oncall"]` below is a `Value::Array`.
    let rules = bark["rules"]
        .as_array_of_tables()
        .expect("rules is an array of tables");
    assert_eq!(rules.len(), 1);
    let rule = rules.get(0).expect("one rule");
    assert_eq!(rule["on"].as_str(), Some("gave_up"));
    assert_eq!(
        rule["sinks"]
            .as_array()
            .and_then(|sinks| sinks.get(0))
            .and_then(toml_edit::Value::as_str),
        Some("oncall"),
        "the array-of-tables entry keeps its own array field"
    );
}
#[test]
fn taking_dog_sections_keeps_an_inline_table_dog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[dog]\nmetrics = { bind = \"127.0.0.1:9615\" }\n").expect("write");

    let taken = ShepToml::edit(&path, ShepToml::take_dog_sections).expect("edit");

    assert_eq!(
        taken["metrics"]["bind"].as_str(),
        Some("127.0.0.1:9615"),
        "an inline-table dog under [dog] must not be dropped"
    );
}

/// Three keys under `[daemon]` are written by the dog verbs and hand-edited
/// by operators, and all three refuse a shape they did not write instead of
/// `.expect()` on it.
///
/// One case each, and each asserts the file is left alone.
#[test]
fn a_hand_edited_daemon_key_is_refused_rather_than_panicked_on() {
    let cases = [
        ("daemon = \"loud\"\n", "daemon", "a table"),
        (
            "[daemon]\nenabled_dogs = \"metrics\"\n",
            "enabled_dogs",
            "an array",
        ),
        (
            "[daemon]\nadopted_dogs = \"nope\"\n",
            "adopted_dogs",
            "a table",
        ),
    ];
    for (original, expected_key, expected_shape) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shep.toml");
        std::fs::write(&path, original).unwrap();

        // `adopt_dog` reaches all three keys: `[daemon]`, then
        // `adopted_dogs`, then `enabled_dogs` through `enable_dog`.
        let refusal = ShepToml::try_edit(&path, |cfg| {
            cfg.adopt_dog("metrics", Path::new("/usr/local/bin/shep-metrics"), false)
        });

        match refusal {
            Err(ref err @ ShepTomlError::WrongShape { key, found, .. }) => {
                assert_eq!(key, expected_key, "names the key an operator has to fix");
                assert!(!found.is_empty(), "says what it found instead");
                // `enabled_dogs` is an array and the other two are tables, so
                // a fixed word here would tell an operator to write the wrong
                // thing before refusing the edit.
                assert!(
                    err.to_string().contains(expected_shape),
                    "{key} must be {expected_shape}: {err}"
                );
            }
            other => panic!("expected WrongShape on {original:?}, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "a refusal writes nothing at all"
        );
    }
}

/// The refusal is the unhappy path, so pin that the happy one still writes.
/// A `section_table_mut` that refused everything would pass the test above.
#[test]
fn a_daemon_table_shep_wrote_itself_still_takes_a_dog() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon]\nlog_level = \"info\"\n").unwrap();

    ShepToml::try_edit(&path, |cfg| {
        cfg.adopt_dog("metrics", Path::new("/usr/local/bin/shep-metrics"), false)
    })
    .unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(cfg.daemon.enabled_dogs, vec!["metrics"]);
    assert!(
        written.contains("shep-metrics"),
        "the binary landed: {written}"
    );
}

/// A re-adopt reads the binary as it is now, so a dog whose new build stops
/// asking must lose the channel rather than keep a stale grant.
#[test]
fn a_channel_ask_is_recorded_and_a_readopt_that_does_not_ask_forgets_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    let binary = Path::new("/usr/local/bin/shep-otel");

    ShepToml::try_edit(&path, |cfg| cfg.adopt_dog("otel", binary, true)).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(cfg.daemon.channel_dogs, vec!["otel"]);
    assert!(
        ShepToml::read_only(&path)
            .unwrap()
            .asked_for_channel("otel")
    );

    ShepToml::try_edit(&path, |cfg| cfg.adopt_dog("otel", binary, false)).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        !ShepToml::read_only(&path)
            .unwrap()
            .asked_for_channel("otel")
    );
    assert!(
        !written.contains("channel_dogs"),
        "an emptied key goes, so an older shep still loads the file: {written}"
    );
}

/// The ordinary adopt writes no key an older shep would refuse to load.
#[test]
fn adopting_a_dog_that_did_not_ask_writes_no_channel_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");

    ShepToml::try_edit(&path, |cfg| {
        cfg.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), false)
    })
    .unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("channel_dogs"), "{written}");
}

#[test]
fn rehoming_a_dog_forgets_its_channel_ask_and_keeps_another_dogs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    ShepToml::try_edit(&path, |cfg| {
        cfg.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), true)?;
        cfg.adopt_dog("jobs", Path::new("/usr/local/bin/shep-jobs"), true)
    })
    .unwrap();

    ShepToml::edit(&path, |cfg| cfg.rehome_dog("otel")).unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert!(!cfg.asked_for_channel("otel"));
    assert!(cfg.asked_for_channel("jobs"));
}

#[test]
fn a_hand_edited_channel_dogs_is_refused_rather_than_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    let original = "[daemon]\nchannel_dogs = \"otel\"\n";
    std::fs::write(&path, original).unwrap();

    let refusal = ShepToml::try_edit(&path, |cfg| {
        cfg.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), true)
    });

    assert!(
        matches!(
            refusal,
            Err(ShepTomlError::WrongShape {
                key: "channel_dogs",
                ..
            })
        ),
        "{refusal:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

/// The entry is the operator's, and the path is the one `shep adopt` wrote.
/// A bare path string gains its `env` half rather than losing the path, and
/// a re-adopt keeps the `env` the operator set.
#[test]
fn setting_a_dogs_env_rewrites_its_entry_and_keeps_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "[daemon]\nenabled_dogs = [\"otel\"]\n\n[daemon.adopted_dogs]\notel = \"/usr/local/bin/shep-otel\"\n",
    )
    .unwrap();

    ShepToml::edit(&path, |cfg| {
        cfg.set_adopted_dog_env("otel", "PADDOCK_KEY", Some("{{secret:paddock/key}}"))
            .unwrap();
        cfg.set_adopted_dog_env("otel", "RUST_LOG", Some("warn"))
            .unwrap();
    })
    .unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(
        cfg.adopted_dog_path("otel"),
        Some(PathBuf::from("/usr/local/bin/shep-otel")),
        "{written}"
    );
    assert_eq!(
        cfg.adopted_dog_env("otel"),
        BTreeMap::from([
            (
                "PADDOCK_KEY".to_owned(),
                "{{secret:paddock/key}}".to_owned()
            ),
            ("RUST_LOG".to_owned(), "warn".to_owned()),
        ]),
        "{written}"
    );
    DaemonConfig::load(Some(&written), &|_| None)
        .expect("the file the writer produced is one the daemon reads");

    ShepToml::edit(&path, |cfg| {
        cfg.adopt_dog("otel", Path::new("/opt/new/shep-otel"), false)
            .unwrap();
    })
    .unwrap();
    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(
        (
            cfg.adopted_dog_path("otel"),
            cfg.adopted_dog_env("otel")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        ),
        (
            Some(PathBuf::from("/opt/new/shep-otel")),
            vec!["PADDOCK_KEY", "RUST_LOG"],
        ),
        "a re-adopt replaces the binary, not what the operator set beside it"
    );
}

/// Removing the last key leaves no `env` behind, so the entry goes back to
/// the shape every `shep.toml` had before this key existed.
#[test]
fn removing_a_dogs_last_env_key_leaves_no_empty_env_behind_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "[daemon.adopted_dogs.otel]\npath = \"/usr/local/bin/shep-otel\"\nenv = { RUST_LOG = \"warn\" }\n",
    )
    .unwrap();

    ShepToml::edit(&path, |cfg| {
        cfg.set_adopted_dog_env("otel", "RUST_LOG", None).unwrap();
    })
    .unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("env"), "{written}");
    assert_eq!(
        ShepToml::read_only(&path).unwrap().adopted_dog_env("otel"),
        BTreeMap::new(),
    );
}

/// A built-in dog has no entry to put an `env` key in, and the refusal says
/// which dog it means rather than naming a path the operator never wrote.
#[test]
fn setting_an_env_on_a_built_in_dog_refuses_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    let original = "[daemon]\nenabled_dogs = [\"metrics\"]\n";
    std::fs::write(&path, original).unwrap();

    let refusal = ShepToml::try_edit(&path, |cfg| {
        cfg.set_adopted_dog_env("metrics", "RUST_LOG", Some("warn"))
    });

    assert!(
        matches!(
            refusal,
            Err(ShepTomlError::NoDogEntry {
                dog: _,
                found: "nothing",
                ..
            })
        ),
        "{refusal:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original,
        "a refused write leaves the file as it was"
    );
}

/// `otel = { path = "...", env = { ... } }` is the same entry
/// [`DaemonConfig::load`] reads and [`ShepToml::adopted_dog_env`] reads, so a
/// writer that refused it would leave a valid `shep.toml` uneditable through
/// the door that edits it. It also stays inline: the shape is the operator's.
#[test]
fn setting_an_env_key_on_an_inline_dog_entry_keeps_it_inline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "[daemon.adopted_dogs]\notel = { path = \"/usr/local/bin/shep-otel\", env = { RUST_LOG = \"warn\" } }\n",
    )
    .unwrap();

    ShepToml::edit(&path, |cfg| {
        cfg.set_adopted_dog_env("otel", "PADDOCK_KEY", Some("{{secret:paddock/key}}"))
            .unwrap();
    })
    .unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.contains("otel = {"),
        "the entry stays inline: {written}"
    );
    assert_eq!(
        ShepToml::read_only(&path).unwrap().adopted_dog_path("otel"),
        Some(PathBuf::from("/usr/local/bin/shep-otel")),
        "and it keeps its path: {written}"
    );
    let keys: Vec<String> = ShepToml::read_only(&path)
        .unwrap()
        .adopted_dog_env("otel")
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, vec!["PADDOCK_KEY", "RUST_LOG"], "{written}");
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(
        cfg.daemon.adopted_dogs.get("otel").unwrap().env["PADDOCK_KEY"],
        "{{secret:paddock/key}}",
        "the reference is stored as written, as a sheep's `env` stores one"
    );
}
