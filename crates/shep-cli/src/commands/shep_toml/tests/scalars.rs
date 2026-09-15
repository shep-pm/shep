//! Reading, setting and unsetting the daemon section's plain scalar keys:
//! sockets, cron sleep, whistle control, and the shape refusals around them.

use super::*;

#[test]
fn a_document_with_no_daemon_section_reads_every_scalar_as_absent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[interpreters]\njs = \"node\"\n").unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.daemon_log_json(), None);
    assert_eq!(cfg.daemon_log_level(), None);
    assert_eq!(cfg.daemon_socket(), None);
    assert_eq!(cfg.daemon_max_cron_sleep(), None);
    assert_eq!(cfg.whistle_allow_control(), None);
}
/// The distinction the screen rests on: a key written to its own default is
/// not the same fact as a key nobody wrote, and `DaemonConfig::load` cannot
/// tell them apart because every section is `serde(default)`.
#[test]
fn a_scalar_written_to_its_default_still_reads_as_present() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon]\nlog_level = \"warn\"\nlog_json = false\n").unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.daemon_log_level().as_deref(), Some("warn"));
    assert_eq!(cfg.daemon_log_json(), Some(false));
}
#[test]
fn setting_a_scalar_keeps_the_comments_and_the_keys_around_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "# keep me\n[daemon]\nenabled_dogs = [\"metrics\"]\n\n[style]\nlevel = \"full\"\n",
    )
    .unwrap();

    ShepToml::try_edit(&path, |cfg| cfg.set_daemon_log_level("debug")).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# keep me\n"), "got: {text}");
    assert!(text.contains("enabled_dogs = [\"metrics\"]"), "got: {text}");
    assert!(text.contains("level = \"full\""), "got: {text}");
    assert!(text.contains("log_level = \"debug\""), "got: {text}");

    // Substring checks can't tell which section `log_level = "debug"`
    // landed under; `daemon_log_level` reads `[daemon]` specifically.
    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.daemon_log_level().as_deref(), Some("debug"));
}
#[test]
fn unsetting_removes_the_key_and_leaves_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "[daemon]\nlog_level = \"debug\"\nmax_cron_sleep = \"30s\"\n",
    )
    .unwrap();

    ShepToml::edit(&path, ShepToml::unset_daemon_max_cron_sleep).unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.daemon_max_cron_sleep(), None);
    assert_eq!(cfg.daemon_log_level().as_deref(), Some("debug"));
}
#[test]
fn a_daemon_key_of_the_wrong_shape_is_refused_rather_than_clobbered() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "daemon = \"loud\"\n").unwrap();

    // `try_edit`, not `edit`: `edit` always saves, which would stage a
    // byte-identical copy despite the refusal.
    let refusal: Result<(), ShepTomlError> =
        ShepToml::try_edit(&path, |cfg| cfg.set_daemon_log_json(true));

    assert!(matches!(
        refusal,
        Err(ShepTomlError::WrongShape { key: "daemon", .. })
    ));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "daemon = \"loud\"\n"
    );
}
/// `set_daemon_socket` has no caller until the settings screen lands,
/// so this is the only thing that exercises it before then. Reads back
/// through `daemon_socket`, which is what actually pins that the value
/// landed under `[daemon] socket` rather than merely appearing
/// somewhere in the file (the gap `setting_a_scalar_keeps_the_comments_
/// and_the_keys_around_it` had before this same fix round).
#[test]
fn setting_the_socket_reads_back_through_daemon_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    ShepToml::try_edit(&path, |cfg| {
        cfg.set_daemon_socket(Path::new("/tmp/shep.sock"))
    })
    .unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.daemon_socket(), Some(PathBuf::from("/tmp/shep.sock")));
    assert_eq!(cfg.daemon_log_level().as_deref(), Some("debug"));
}
/// `unset_daemon_socket`'s own sibling test, pinning the same thing
/// `unsetting_removes_the_key_and_leaves_its_neighbours` pins for
/// `max_cron_sleep`: the key goes, its neighbours stay.
#[test]
fn unsetting_the_socket_removes_the_key_and_leaves_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(
        &path,
        "[daemon]\nlog_level = \"debug\"\nsocket = \"/tmp/shep.sock\"\n",
    )
    .unwrap();

    ShepToml::edit(&path, ShepToml::unset_daemon_socket).unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.daemon_socket(), None);
    assert_eq!(cfg.daemon_log_level().as_deref(), Some("debug"));
}
/// `set_daemon_max_cron_sleep` has no caller until the settings screen
/// lands. Reads back through `daemon_max_cron_sleep`, the raw string as
/// written, not a parsed `UpDuration`.
#[test]
fn setting_max_cron_sleep_reads_back_through_daemon_max_cron_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    ShepToml::try_edit(&path, |cfg| cfg.set_daemon_max_cron_sleep("45s")).unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.daemon_max_cron_sleep().as_deref(), Some("45s"));
    assert_eq!(cfg.daemon_log_level().as_deref(), Some("debug"));
}
/// `set_whistle_allow_control` has no caller until the settings screen
/// lands. Reads back through `whistle_allow_control`, which is what
/// pins the value under `[whistle]` rather than `[daemon]`.
#[test]
fn setting_whistle_allow_control_reads_back_through_whistle_allow_control() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    ShepToml::try_edit(&path, |cfg| cfg.set_whistle_allow_control(true)).unwrap();

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.whistle_allow_control(), Some(true));
    assert_eq!(cfg.daemon_log_level().as_deref(), Some("debug"));
}
