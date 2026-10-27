//! The settings reader and writer, through [`load_settings`] and
//! [`apply_setting`] over real files.

use super::*;
use crate::lookout::field::shep_toml_shape;

/// The style pair every test that does not care about `[style]` uses.
fn style_fixture() -> (StyleLevel, StyleSource) {
    (StyleLevel::Full, StyleSource::Config)
}

fn socket_default_fixture() -> PathBuf {
    PathBuf::from("/var/run/shep-settings-fixture.sock")
}

/// A `shep.toml` an operator has been in: comments on a section, on a key
/// and on a table, keys in an order no sort would give.
const LIVED_IN: &str = "\
# the shepherd's own knobs
hosts = [\"ada@box\"]  # who lookout reaches
[daemon]
log_level = \"info\"  # chatty
environment = \"production\"  # which tier
log_json = false

[secrets]
allow_read = false  # may a sheep read values

# scripts
[interpreters]
py = \"python3\"
js = \"node\"  # the usual
";

fn write(text: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, text).unwrap();
    (dir, path)
}

fn path_of(key: &str) -> Vec<String> {
    shep_toml_shape()
        .paths
        .get(key)
        .unwrap_or_else(|| panic!("{key} is a row"))
        .clone()
}

/// A write of the typed `value` at the dotted `key`, shaped by the row.
fn set(key: &str, value: &str) -> SettingEdit {
    let field = shep_toml_shape().fields.by_key(key).unwrap();
    SettingEdit::Set {
        path: path_of(key),
        shape: Shape::of(&field.kind).unwrap(),
        value: value.to_owned(),
    }
}

fn snapshot(path: &Path) -> SettingsSnapshot {
    load_settings(path, &socket_default_fixture(), style_fixture()).unwrap()
}

fn view(snap: &SettingsSnapshot, key: &str) -> ScalarView {
    snap.view(shep_toml_shape().fields.by_key(key).unwrap())
}

#[test]
fn a_fresh_home_reads_every_key_as_its_default() {
    let (_dir, path) = write("");
    let snap = snapshot(&path);

    for (key, value) in [
        ("daemon.log_level", "warn"),
        ("daemon.log_json", "false"),
        ("daemon.environment", "production"),
        ("whistle.allow_control", "false"),
        ("secrets.allow_read", "false"),
        ("daemon.boot_first_dogs", "none"),
        ("daemon.max_cron_sleep", "not set"),
    ] {
        let shown = view(&snap, key);
        assert_eq!(shown.value, value, "{key}");
        assert_eq!(shown.source, StyleSource::Default, "{key}");
    }
    assert_eq!(
        view(&snap, "daemon.socket").value,
        "/var/run/shep-settings-fixture.sock",
        "an absent socket shows the one lookout is connected over"
    );
}

#[test]
fn a_key_the_file_wrote_reads_as_config_even_at_its_default_value() {
    let (_dir, path) = write("hosts = [\"a@x\", \"b@y\"]\n\n[daemon]\nlog_level = \"warn\"\n");
    let snap = snapshot(&path);

    let level = view(&snap, "daemon.log_level");
    assert_eq!(level.value, "warn");
    assert_eq!(
        level.source,
        StyleSource::Config,
        "a key written to its own default is still a key someone wrote"
    );
    let hosts = view(&snap, "hosts");
    assert_eq!(
        (hosts.value.as_str(), hosts.source),
        ("a@x, b@y", StyleSource::Config)
    );
}

#[test]
fn style_level_shows_the_level_in_force_while_the_file_keeps_its_own_say() {
    let (_dir, path) = write("[style]\nlevel = \"bare\"\n");
    let snap = load_settings(
        &path,
        &socket_default_fixture(),
        (StyleLevel::Plain, StyleSource::Env),
    )
    .unwrap();

    let shown = view(&snap, "style.level");
    assert_eq!(
        (shown.value.as_str(), shown.source),
        ("plain", StyleSource::Env)
    );
    assert_eq!(
        snap.file_value("style.level").and_then(Value::as_str),
        Some("bare"),
        "cycling from the level in force would propose a no-op write"
    );
}

#[test]
fn a_map_reads_as_a_count_of_its_entries() {
    for (text, shown) in [
        ("", "none"),
        ("[interpreters]\npy = \"python3\"\n", "1 entry"),
        (
            "[interpreters]\npy = \"python3\"\njs = \"node\"\n",
            "2 entries",
        ),
    ] {
        let (_dir, path) = write(text);
        assert_eq!(
            view(&snapshot(&path), "interpreters").value,
            shown,
            "{text}"
        );
    }
}

#[test]
fn the_entries_of_a_map_are_read_in_key_order() {
    let (_dir, path) = write(LIVED_IN);
    assert_eq!(
        snapshot(&path).entries("interpreters"),
        [
            ("js".to_owned(), "node".to_owned()),
            ("py".to_owned(), "python3".to_owned())
        ]
    );
}

#[cfg(unix)]
mod preserving {
    use std::os::unix::fs::MetadataExt as _;

    use super::*;

    /// `LIVED_IN` with `from` swapped for `to`, which is exactly what a
    /// write that keeps every comment and the key order leaves behind.
    fn lived_in_with(from: &str, to: &str) -> String {
        assert!(LIVED_IN.contains(from), "{from} is in the seed");
        LIVED_IN.replace(from, to)
    }

    #[test]
    fn editing_daemon_environment_keeps_comments_and_key_order() {
        let (_dir, path) = write(LIVED_IN);

        apply_setting(&path, &set("daemon.environment", "staging")).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            lived_in_with("\"production\"  # which tier", "\"staging\"  # which tier")
        );
    }

    #[test]
    fn editing_secrets_allow_read_keeps_comments_and_key_order() {
        let (_dir, path) = write(LIVED_IN);

        apply_setting(&path, &set("secrets.allow_read", "true")).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            lived_in_with(
                "allow_read = false  # may a sheep read values",
                "allow_read = true  # may a sheep read values"
            )
        );
    }

    #[test]
    fn editing_a_list_keeps_its_comment_and_writes_the_items_typed() {
        let (_dir, path) = write(LIVED_IN);

        apply_setting(&path, &set("hosts", "ada@box, bob@host")).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            lived_in_with(
                "[\"ada@box\"]  # who lookout reaches",
                "[\"ada@box\", \"bob@host\"]  # who lookout reaches"
            )
        );
    }

    #[test]
    fn editing_one_interpreter_entry_keeps_comments_and_key_order() {
        let (_dir, path) = write(LIVED_IN);
        let edit = SettingEdit::Set {
            path: vec!["interpreters".into(), "js".into()],
            shape: Shape::Text,
            value: "bun".into(),
        };

        apply_setting(&path, &edit).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            lived_in_with("js = \"node\"  # the usual", "js = \"bun\"  # the usual")
        );
    }

    #[test]
    fn adding_an_interpreter_entry_lands_after_the_ones_there() {
        let (_dir, path) = write(LIVED_IN);
        let edit = SettingEdit::Set {
            path: vec!["interpreters".into(), "rb".into()],
            shape: Shape::Text,
            value: "ruby".into(),
        };

        apply_setting(&path, &edit).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{LIVED_IN}rb = \"ruby\"\n")
        );
    }

    #[test]
    fn removing_an_interpreter_entry_drops_only_its_line() {
        let (_dir, path) = write(LIVED_IN);

        apply_setting(
            &path,
            &SettingEdit::Unset {
                path: vec!["interpreters".into(), "py".into()],
            },
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            lived_in_with("py = \"python3\"\n", "")
        );
    }

    #[test]
    fn a_table_the_file_never_wrote_is_created_for_the_write() {
        let (_dir, path) = write("# only a comment\n");

        apply_setting(&path, &set("whistle.allow_control", "true")).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("# only a comment\n"), "{written}");
        assert_eq!(
            snapshot(&path).file_value("whistle.allow_control"),
            Some(&Value::Bool(true))
        );
    }

    /// The refusals the loader makes, each leaving the file as it was down
    /// to its inode: a refusal must not stage and rename, which is what
    /// `try_edit` buys.
    #[test]
    fn a_value_the_loader_refuses_is_not_written() {
        for (key, value) in [
            ("daemon.environment", "all"),
            ("daemon.environment", "not a name"),
            ("daemon.max_cron_sleep", "500ms"),
        ] {
            let (_dir, path) = write(LIVED_IN);
            let inode = std::fs::metadata(&path).unwrap().ino();

            let refusal = apply_setting(&path, &set(key, value));

            assert!(
                matches!(refusal, Err(SettingError::Invalid(_))),
                "{key} = {value}: {refusal:?}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), LIVED_IN);
            assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        }
    }

    #[test]
    fn the_refusal_names_the_key_the_loader_objected_to() {
        let (_dir, path) = write("");

        let Err(SettingError::Invalid(message)) =
            apply_setting(&path, &set("daemon.max_cron_sleep", "500ms"))
        else {
            panic!("a value under the floor must be refused");
        };

        assert!(message.contains("max_cron_sleep"), "{message}");
    }

    #[test]
    fn a_value_outside_a_closed_set_is_refused_even_where_the_loader_would_not() {
        let (_dir, path) = write("[style]\nlevel = \"full\"\n");

        let Err(SettingError::Invalid(message)) = apply_setting(&path, &set("style.level", "loud"))
        else {
            panic!("`loud` is no style level");
        };

        assert!(
            message.contains("loud") && message.contains("full, plain, bare"),
            "{message}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[style]\nlevel = \"full\"\n"
        );
    }

    #[test]
    fn a_key_under_something_that_is_not_a_table_is_refused_and_not_written() {
        let before = "interpreters = \"node\"\n";
        let (_dir, path) = write(before);
        let edit = SettingEdit::Set {
            path: vec!["interpreters".into(), "py".into()],
            shape: Shape::Text,
            value: "python3".into(),
        };

        let Err(SettingError::Invalid(message)) = apply_setting(&path, &edit) else {
            panic!("nowhere to put the entry");
        };

        assert!(
            message.contains("interpreters") && message.contains("not a table"),
            "{message}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
}

#[test]
fn a_boolean_takes_the_daemons_spellings_and_refuses_the_rest() {
    let (_dir, path) = write("");
    apply_setting(&path, &set("daemon.log_json", "1")).unwrap();
    assert_eq!(
        snapshot(&path).file_value("daemon.log_json"),
        Some(&Value::Bool(true))
    );

    let before = std::fs::read_to_string(&path).unwrap();
    let Err(SettingError::Invalid(message)) =
        apply_setting(&path, &set("daemon.log_json", "maybe"))
    else {
        panic!("`maybe` is no boolean");
    };
    assert!(message.contains("maybe"), "{message}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn a_list_is_typed_comma_separated_and_an_empty_item_is_dropped() {
    let (_dir, path) = write("");
    apply_setting(&path, &set("hosts", " ada@box ,, bob@host,")).unwrap();
    assert_eq!(
        snapshot(&path).file_value("hosts"),
        Some(&serde_json::json!(["ada@box", "bob@host"]))
    );
}

#[test]
fn a_number_shape_parses_what_the_toml_value_holds() {
    assert_eq!(Shape::Integer.parse(" 42 ").unwrap().as_integer(), Some(42));
    assert_eq!(Shape::Number.parse("1.5").unwrap().as_float(), Some(1.5));
    assert!(matches!(
        Shape::Integer.parse("4.5"),
        Err(SettingError::Invalid(_))
    ));
}

#[test]
fn unsetting_a_key_returns_it_to_the_default() {
    let (_dir, path) = write("[daemon]\nlog_level = \"debug\"\nmax_cron_sleep = \"30s\"\n");

    for key in ["daemon.log_level", "daemon.max_cron_sleep"] {
        apply_setting(&path, &SettingEdit::Unset { path: path_of(key) }).unwrap();
        assert_eq!(
            view(&snapshot(&path), key).source,
            StyleSource::Default,
            "{key}"
        );
    }
}

/// `DaemonConfig::load` takes `daemon = { ... }` and `interpreters = { ... }`
/// as it takes the headed tables, and the screen reads them, so it writes them.
mod inline_tables {
    use super::*;

    const INLINE: &str = "\
daemon = { log_level = \"info\", max_cron_sleep = \"30s\" }
interpreters = { py = \"python3\" }
";

    #[test]
    fn a_key_in_an_inline_table_is_edited_in_place() {
        let (_dir, path) = write(INLINE);

        apply_setting(&path, &set("daemon.log_level", "debug")).unwrap();
        apply_setting(
            &path,
            &SettingEdit::Set {
                path: vec!["interpreters".into(), "py".into()],
                shape: Shape::Text,
                value: "python3.13".into(),
            },
        )
        .unwrap();

        let snap = snapshot(&path);
        assert_eq!(view(&snap, "daemon.log_level").value, "debug");
        assert_eq!(
            snap.entries("interpreters"),
            vec![("py".to_owned(), "python3.13".to_owned())]
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("max_cron_sleep = \"30s\""),
            "the sibling key stays"
        );
    }

    #[test]
    fn a_key_in_an_inline_table_is_unset_and_reads_as_the_default() {
        let (_dir, path) = write(INLINE);

        apply_setting(
            &path,
            &SettingEdit::Unset {
                path: path_of("daemon.max_cron_sleep"),
            },
        )
        .unwrap();

        let shown = view(&snapshot(&path), "daemon.max_cron_sleep");
        assert_eq!(shown.source, StyleSource::Default);
        assert_eq!(
            view(&snapshot(&path), "daemon.log_level").value,
            "info",
            "only the named key goes"
        );
    }
}

#[test]
fn every_built_in_dog_is_a_candidate_even_when_nothing_is_enabled() {
    let (_dir, path) = write("");
    let snap = snapshot(&path);
    let names: Vec<&str> = snap.dogs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["bark", "metrics"]);
    assert!(snap.dogs.iter().all(|d| !d.enabled));
}

#[test]
fn an_adopted_dog_joins_the_candidates_and_carries_its_path() {
    let (_dir, path) = write(
        "[daemon]\nenabled_dogs = [\"otel\"]\n\n[daemon.adopted_dogs]\notel = \"/usr/local/bin/shep-otel\"\n",
    );
    let snap = snapshot(&path);
    let otel = snap.dogs.iter().find(|d| d.name == "otel").unwrap();
    assert!(otel.enabled);
    assert_eq!(
        otel.adopted_path.as_deref(),
        Some(Path::new("/usr/local/bin/shep-otel"))
    );
    let metrics = snap.dogs.iter().find(|d| d.name == "metrics").unwrap();
    assert_eq!(metrics.adopted_path, None, "a built-in dog has no path");
}
