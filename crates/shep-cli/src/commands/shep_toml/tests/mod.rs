//! unix only: asserts a `0600` mode and an inode preserved across an atomic
//! rename. Windows differs; `tests/cli_e2e.rs` covers that tier.

use std::os::unix::fs::PermissionsExt as _;

use shep_core::config::DaemonConfig;

use super::*;

/// `path`'s permission bits, masked to the nine that matter.
fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_file_that_will_not_parse_is_refused_rather_than_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon\nlog_json = true\n").unwrap();
    assert!(matches!(
        ShepToml::edit(&path, |doc| doc.enable_dog("metrics")),
        Err(ShepTomlError::Parse { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[daemon\nlog_json = true\n"
    );
}
#[test]
fn a_missing_file_opens_empty_and_edit_creates_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("shep.toml");
    ShepToml::edit(&path, |doc| doc.enable_dog("metrics")).unwrap();
    assert!(path.exists());
}
#[test]
fn a_missing_file_reads_as_an_empty_document_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");

    let cfg = ShepToml::read_only(&path).unwrap();
    assert_eq!(cfg.daemon_log_level(), None);
    assert!(!path.exists(), "a read must never create the file");
}
/// Both modes are asserted on a path where neither the directory nor
/// the file existed beforehand: that is the case the ambient umask
/// would decide.
#[test]
fn a_first_edit_creates_the_home_and_the_file_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("cold");
    let path = home.join("shep.toml");

    ShepToml::edit(&path, |doc| doc.enable_dog("bark")).unwrap();

    assert_eq!(
        mode_of(&home),
        0o700,
        "$SHEP_HOME is readable by other local users until the first boot"
    );
    assert_eq!(
        mode_of(&path),
        0o600,
        "the file a webhook token goes in, and the mode a `tar` of it keeps"
    );
}
/// The rename installs the staging file's inode, mode included:
/// narrowing is a property of the write path, not a separate chmod.
#[test]
fn editing_a_world_readable_config_leaves_it_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    std::fs::write(&path, "[daemon]\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    ShepToml::edit(&path, |doc| doc.enable_dog("bark")).unwrap();

    assert_eq!(mode_of(&path), 0o600);
}
/// `Debug` carries only the path and the parser's short message, never
/// the document `Display` quotes: a webhook in `[dog.bark]` must not
/// reach `{:?}` output.
#[test]
fn parse_error_debug_never_prints_the_document() {
    let path = PathBuf::from("/home/ada/.shep/shep.toml");
    let secret = "https://hooks.example.com/services/T00/B00/super-secret-token";
    let broken = format!("[dog.bark]\nwebhook = \"{secret}\"\n[daemon\n");
    let source = broken.parse::<DocumentMut>().unwrap_err();
    let err = ShepTomlError::Parse { path, source };

    let debug = format!("{err:?}");
    assert!(
        !debug.contains(secret),
        "the document must never reach Debug: {debug}"
    );
    assert!(!debug.contains("webhook"), "{debug}");
    assert!(!debug.contains("hooks.example.com"), "{debug}");
    assert_eq!(
        debug,
        "Parse { path: \"/home/ada/.shep/shep.toml\", message: \"invalid table header\\n\
             expected `.`, `]`\" }"
    );

    // `Display` is what an operator reads for a typo; it still shows
    // the offending line.
    let display = err.to_string();
    assert!(display.contains("invalid table header"));
}
/// Env var naming the `shep.toml` the re-executed child should edit.
/// Its presence is also what tells the child it is a child.
const CHILD_PATH_VAR: &str = "SHEP_CONFIG_RACE_PATH";
/// Env var carrying the child's tag, which decides both which verb's
/// edit it makes and what it names the dogs it writes.
const CHILD_TAG_VAR: &str = "SHEP_CONFIG_RACE_TAG";
/// How many edits each of the two writers makes. One apiece would race
/// only in the instant the two overlap; this many makes an unlocked
/// read-modify-write lose an edit on essentially every run.
const EDITS_PER_WRITER: usize = 100;
/// The tag whose child adopts (`[daemon] adopted_dogs` plus
/// `enabled_dogs`); the other enables (`enabled_dogs` alone). Two
/// different edits, so a survivor of one cannot stand in for the other.
const ADOPTING_TAG: &str = "alpha";

/// Child half of [`two_writer_processes_do_not_lose_each_other_s_edits`],
/// re-executed with `--ignored --exact`. Hammers [`ShepToml::edit`] from
/// a second OS process; asserts nothing itself, the parent judges.
#[test]
#[ignore = "child process of two_writer_processes_do_not_lose_each_other_s_edits"]
fn config_race_child() {
    let Ok(path) = std::env::var(CHILD_PATH_VAR) else {
        panic!("{CHILD_PATH_VAR} unset — this test is only run as a child process");
    };
    let tag = std::env::var(CHILD_TAG_VAR).expect("child needs a tag");
    let path = PathBuf::from(path);

    for i in 0..EDITS_PER_WRITER {
        let name = format!("{tag}-{i}");
        ShepToml::edit(&path, |doc| {
            if tag == ADOPTING_TAG {
                doc.adopt_dog(&name, Path::new("/usr/local/bin/shep-otel"));
            } else {
                doc.enable_dog(&name);
            }
        })
        .expect("child edit");
    }
}

/// Two OS processes, not threads: the race is a read-modify-write
/// across a `rename` with no lock between address spaces, which
/// in-process serialisation cannot reproduce.
#[test]
fn two_writer_processes_do_not_lose_each_other_s_edits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    let exe = std::env::current_exe().expect("test binary path");

    let children: Vec<_> = [ADOPTING_TAG, "beta"]
        .iter()
        .map(|tag| {
            std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "--ignored",
                    "commands::shep_toml::tests::config_race_child",
                ])
                .env(CHILD_PATH_VAR, &path)
                .env(CHILD_TAG_VAR, tag)
                // Piped, not inherited: a passing run should not
                // interleave two child harnesses' output into this
                // one's, and a failing child's harness output is
                // exactly what the assertion below needs to show.
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("spawn writer")
        })
        .collect();

    for child in children {
        let out = child.wait_with_output().expect("wait for writer");
        assert!(
            out.status.success(),
            "a writer process failed: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout)
        );
        // `--exact` against a path this binary no longer has matches
        // nothing, and a harness that ran zero tests still exits 0. Without
        // this, the failure above is a NotFound on shep.toml, which points
        // at the wrong file entirely.
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("running 1 test"),
            "the child ran no test, so the `--exact` path is stale: {stdout}"
        );
    }

    let written = std::fs::read_to_string(&path).unwrap();
    let cfg = DaemonConfig::load(Some(&written), &|_| None).unwrap();
    for i in 0..EDITS_PER_WRITER {
        let adopted = format!("{ADOPTING_TAG}-{i}");
        let enabled = format!("beta-{i}");
        assert!(
            cfg.daemon.adopted_dogs.contains_key(&adopted),
            "{adopted}: an adopt was overwritten by the other writer"
        );
        assert!(
            cfg.daemon.enabled_dogs.contains(&adopted),
            "{adopted}: the adopt's own enable was overwritten"
        );
        assert!(
            cfg.daemon.enabled_dogs.contains(&enabled),
            "{enabled}: an enable was overwritten by the other writer"
        );
    }
    assert_eq!(
        cfg.daemon.enabled_dogs.len(),
        2 * EDITS_PER_WRITER,
        "the config enables dogs nobody asked for"
    );
}

mod dogs;
mod scalars;
mod style;
