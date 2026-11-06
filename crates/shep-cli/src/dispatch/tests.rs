use super::*;
use crate::config::style_from_config;

use shep_core::paths::ShepPaths;
/// The two verbs are told apart by `--home`: `startup` refuses a
/// `$SHEP_HOME` that is not there, and `unstartup` ignores `--home`, since
/// a removal is addressed by the unit's path and label.
///
/// Skipped as root: `unstartup` would reach a real `systemctl` or
/// `launchctl` against whatever this machine has installed.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn startup_and_unstartup_reach_their_own_verbs() {
    use clap::Parser;

    if nix::unistd::geteuid().is_root() {
        eprintln!("skipping: as root these verbs really install and remove a system unit");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("never-created");
    let missing = missing.to_str().unwrap();

    let cli = Cli::try_parse_from(["shep", "--home", missing, "startup"]).unwrap();
    assert_eq!(
        run(
            cli,
            style::Presentation::BARE,
            (style::StyleLevel::Full, style::StyleSource::Default),
        )
        .await,
        ExitCode::Usage,
        "startup must refuse a $SHEP_HOME that is not there"
    );

    let cli = Cli::try_parse_from(["shep", "--home", missing, "unstartup"]).unwrap();
    assert_ne!(
        run(
            cli,
            style::Presentation::BARE,
            (style::StyleLevel::Full, style::StyleSource::Default),
        )
        .await,
        ExitCode::Usage,
        "unstartup removes a unit and never reads the home a --home names"
    );
}

/// What `shep style <level>` writes must be what `style_from_config`, the
/// reader `resolve_style` uses, reads back.
///
/// `#[cfg(unix)]`: the write goes through `commands::shep_toml`'s
/// `ConfigLock`, whose `cfg(windows)` arm nothing in this crate executes.
#[cfg(unix)]
#[tokio::test]
async fn style_with_a_level_writes_shep_toml_and_the_config_reads_it_back() {
    use clap::Parser;

    for (raw, expected) in [
        ("full", style::StyleLevel::Full),
        ("plain", style::StyleLevel::Plain),
        ("bare", style::StyleLevel::Bare),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let cli = Cli::try_parse_from(["shep", "--home", home, "style", raw]).unwrap();
        assert_eq!(
            run(
                cli,
                style::Presentation::BARE,
                (style::StyleLevel::Full, style::StyleSource::Default),
            )
            .await,
            ExitCode::Success,
            "style {raw}"
        );

        let written = std::fs::read_to_string(dir.path().join("shep.toml")).unwrap();
        assert_eq!(
            style_from_config(Some(&written)),
            Some(expected),
            "style {raw}"
        );
    }
}

/// The no-arg form is a report and only a report. `#[cfg(unix)]` to stay
/// paired with the test above.
#[cfg(unix)]
#[tokio::test]
async fn style_with_no_level_reports_and_writes_nothing() {
    use clap::Parser;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_str().unwrap();
    let cli = Cli::try_parse_from(["shep", "--home", home, "style"]).unwrap();
    assert_eq!(
        run(
            cli,
            style::Presentation::BARE,
            (style::StyleLevel::Full, style::StyleSource::Default),
        )
        .await,
        ExitCode::Success
    );

    assert!(
        !dir.path().join("shep.toml").exists(),
        "the no-arg form must not create a shep.toml that was not there"
    );
}

/// Pins the structural rule: `completions` never reaches `resolve_paths`.
///
/// It cannot pin the behaviour. With `$HOME` set, a reinstated
/// `resolve_paths` would succeed and fall through to the same call, so
/// `run` returns `Success` either way, and unsetting `$HOME` here is
/// `unsafe` in edition 2024. The e2e tier spawns the real binary with
/// `$HOME` cleared instead.
#[tokio::test]
async fn completions_never_resolves_paths() {
    use clap::Parser;
    let argv = ["shep", "completions", "bash"];
    let cli = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{argv:?} failed: {e}"));
    assert_eq!(
        run(
            cli,
            style::Presentation::BARE,
            (style::StyleLevel::Full, style::StyleSource::Default),
        )
        .await,
        ExitCode::Success
    );
}

/// A named home that is missing is refused as a usage error whatever the
/// verb, so that case alone cannot tell a `--host` refusal from the home
/// gate's. The second case can: with a home that is there, a verb that
/// ignored `--host` would dial this machine and exit 5.
#[tokio::test]
async fn a_verb_that_cannot_reach_a_host_refuses_before_creating_the_home() {
    use clap::Parser;

    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("never-created");
    let present = dir.path().join("present");
    std::fs::create_dir(&present).unwrap();

    for home in [&missing, &present] {
        let cli = Cli::try_parse_from([
            "shep",
            "--home",
            home.to_str().unwrap(),
            "--host",
            "amp2",
            "flush",
            "web",
        ])
        .unwrap();
        assert_eq!(
            run(
                cli,
                style::Presentation::BARE,
                (style::StyleLevel::Full, style::StyleSource::Default),
            )
            .await,
            ExitCode::Usage,
            "{}",
            home.display()
        );
    }
    assert!(!missing.exists(), "a refused --host must create no home");
    assert_eq!(
        std::fs::read_dir(&present).unwrap().count(),
        0,
        "a refused --host must write nothing into a home that is there"
    );
}

/// Drives `kill`'s dispatch arm, the one with its own connect, against a
/// home no shepherd owns.
#[cfg(unix)]
#[tokio::test]
async fn run_dispatches_kill_to_the_socket_free_path() {
    use clap::Parser;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    // `pids/` holds the lock `kill`'s socket-free path reads, `run/` the
    // control socket it tries first.
    let paths = ShepPaths::resolve(
        &|key| (key == "SHEP_HOME").then(|| home.to_string_lossy().into_owned()),
        std::path::Path::new("/nonexistent"),
    );
    std::fs::create_dir_all(&paths.pids).unwrap();
    std::fs::create_dir_all(&paths.run).unwrap();
    let argv = ["shep", "--home", home.to_str().unwrap(), "kill"];
    let cli = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{argv:?} failed: {e}"));

    assert_eq!(
        run(
            cli,
            style::Presentation::BARE,
            (style::StyleLevel::Full, style::StyleSource::Default),
        )
        .await,
        ExitCode::DaemonUnreachable,
        "`kill` against an unowned home must reach its own socket-free path"
    );
}
