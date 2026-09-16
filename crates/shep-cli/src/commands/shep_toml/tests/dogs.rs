//! Enabling, disabling, adopting and rehoming a dog, plus taking the legacy
//! `[dog.<name>]` sections a pre-migration file still carries.

use super::*;

/// fails if the writer round-trips through a plain `toml::Table`,
/// losing comments and key order.
#[test]
fn enabling_a_dog_leaves_the_rest_of_the_file_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    let original =
        "# the shepherd's own knobs\n[daemon]\nlog_level = \"info\"  # chatty\nlog_json = false\n";
    std::fs::write(&path, original).unwrap();

    ShepToml::edit(&path, |doc| doc.enable_dog("metrics")).unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("# the shepherd's own knobs"));
    assert!(written.contains("# chatty"));
    assert!(
        written.find("log_level").unwrap() < written.find("log_json").unwrap(),
        "key order survives"
    );

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
        doc.enable_dog("bark");
        doc.enable_dog("bark");
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
        doc.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"));
    })
    .unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(cfg.daemon.enabled_dogs, vec!["otel"]);
    assert_eq!(
        cfg.daemon
            .adopted_dogs
            .get("otel")
            .map(std::path::PathBuf::as_path),
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
        doc.enable_dog("metrics"); // built-in: no `adopted_dogs` entry at all
        doc.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"));

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
