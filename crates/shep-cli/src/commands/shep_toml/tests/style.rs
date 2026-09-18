//! Setting the presentation style level and writing the starter
//! interpreters a fresh `$SHEP_HOME` needs to run its first sheep.

use super::*;

#[test]
fn setting_a_style_level_round_trips_through_daemon_config() {
    for level in [StyleLevel::Full, StyleLevel::Plain, StyleLevel::Bare] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shep.toml");
        ShepToml::try_edit(&path, |doc| doc.set_style_level(level)).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
        assert_eq!(cfg.style.level.as_deref(), Some(level.to_string().as_str()));
    }
}
#[test]
fn setting_a_style_level_leaves_the_rest_of_the_file_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    let original =
        "# the shepherd's own knobs\n[daemon]\nlog_level = \"info\"  # chatty\nlog_json = false\n";
    std::fs::write(&path, original).unwrap();

    ShepToml::try_edit(&path, |doc| doc.set_style_level(StyleLevel::Plain)).unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("# the shepherd's own knobs"));
    assert!(written.contains("# chatty"));
    assert!(
        written.find("log_level").unwrap() < written.find("log_json").unwrap(),
        "key order survives"
    );

    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(cfg.style.level.as_deref(), Some("plain"));
}
#[test]
fn setting_a_style_level_twice_replaces_rather_than_appends() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    ShepToml::try_edit(&path, |doc| doc.set_style_level(StyleLevel::Full)).unwrap();
    ShepToml::try_edit(&path, |doc| doc.set_style_level(StyleLevel::Bare)).unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written.matches("level").count(), 1, "one key, not appended");
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(cfg.style.level.as_deref(), Some("bare"));
}
#[test]
fn setting_a_style_level_into_a_home_with_no_shep_toml_creates_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    assert!(!path.exists());

    ShepToml::try_edit(&path, |doc| doc.set_style_level(StyleLevel::Bare)).unwrap();

    assert!(path.exists());
    let cfg =
        DaemonConfig::load(Some(&std::fs::read_to_string(&path).unwrap()), &|_| None).unwrap();
    assert_eq!(cfg.style.level.as_deref(), Some("bare"));
}
/// Active, not commented out: a fresh `$SHEP_HOME` has to run
/// `shep start server.js` with no further setup.
#[test]
fn the_starter_interpreters_are_written_active() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");

    ShepToml::edit(&path, |doc| doc.write_starter_interpreters()).unwrap();

    let cfg =
        DaemonConfig::load(Some(&std::fs::read_to_string(&path).unwrap()), &|_| None).unwrap();
    assert_eq!(cfg.interpreters.get("js").map(String::as_str), Some("node"));
    assert_eq!(
        cfg.interpreters.get("mjs").map(String::as_str),
        Some("node")
    );
    assert_eq!(
        cfg.interpreters.get("cjs").map(String::as_str),
        Some("node")
    );
    assert_eq!(
        cfg.interpreters.get("py").map(String::as_str),
        Some("python3")
    );
    assert_eq!(cfg.interpreters.get("rb").map(String::as_str), Some("ruby"));
    assert_eq!(cfg.interpreters.get("sh").map(String::as_str), Some("sh"));
}
#[test]
fn the_starter_interpreters_carry_an_explanatory_comment() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");

    ShepToml::edit(&path, |doc| doc.write_starter_interpreters()).unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.contains("# Extension -> interpreter mapping"),
        "no explanatory comment above [interpreters]:\n{written}"
    );
    assert!(
        written.find("# Extension -> interpreter mapping").unwrap()
            < written.find("[interpreters]").unwrap(),
        "the comment must precede the table it explains:\n{written}"
    );
    assert!(
        !written.contains('\u{2014}') && !written.contains('\u{2013}'),
        "no em or en dashes in copy an operator reads:\n{written}"
    );
}
#[test]
fn writing_the_starter_interpreters_twice_does_not_duplicate_or_clobber() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");

    ShepToml::edit(&path, |doc| doc.write_starter_interpreters()).unwrap();
    // An operator's own edit to the mapping this scaffold wrote.
    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace("js = \"node\"", "js = \"bun\"");
    std::fs::write(&path, &edited).unwrap();

    ShepToml::edit(&path, |doc| doc.write_starter_interpreters()).unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        written.matches("[interpreters]").count(),
        1,
        "one table, not appended:\n{written}"
    );
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert_eq!(
        cfg.interpreters.get("js").map(String::as_str),
        Some("bun"),
        "the operator's own edit must survive a second scaffold call"
    );
}
/// The inode and mode checks are the point: [`ShepToml::edit`] would
/// stage and rename a byte-identical copy on a refusal, which content
/// equality alone hides. [`ShepToml::try_edit`] never reaches `save`
/// when the closure returns `Err`.
#[test]
fn a_style_key_that_is_not_a_table_is_reported_and_the_file_is_never_rewritten() {
    use std::os::unix::fs::MetadataExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    let original = "style = \"full\"\n";
    std::fs::write(&path, original).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let before = std::fs::metadata(&path).unwrap();

    let err = ShepToml::try_edit(&path, |doc| doc.set_style_level(StyleLevel::Bare))
        .expect_err("style is a string here, not a table");
    assert!(
        matches!(
            &err,
            ShepTomlError::WrongShape { key, found, .. }
                if *key == "style" && *found == "string"
        ),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        format!(
            "{}: [style] must be a table, found a string",
            path.display()
        )
    );

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original,
        "a refused write must leave the operator's file exactly as it was"
    );
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(
        before.ino(),
        after.ino(),
        "a refused write must not replace the file -- same inode, not just same bytes"
    );
    assert_eq!(
        before.mode() & 0o777,
        after.mode() & 0o777,
        "a refused write must not touch the file's mode"
    );
}
