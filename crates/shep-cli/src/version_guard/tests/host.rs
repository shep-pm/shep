//! The `--host` half: only a protocol difference refuses, the fix is spelled
//! for whichever side of the gap is behind, and a host that agrees on both
//! says nothing at all.
//!
//! Every test here dials a stand-in `ssh` written into a temp dir, so the
//! handshake is real and the peer's `HelloAck` is whatever the test announced.

use super::*;

/// A remedy without `ssh <dest>` would reload this machine's shepherd, which
/// is not the one in skew.
#[cfg(unix)]
#[tokio::test]
async fn a_protocol_skew_over_a_host_names_the_remote_reload_command() {
    let dir = tempfile::tempdir().unwrap();
    let link = Link::Host {
        dest: "amp2",
        options: shep_client::SshOptions::default().program(ssh_stand_in(
            dir.path(),
            "9.9.9",
            PROTOCOL_VERSION - 1,
        )),
    };

    for fmt in [Format::Table, Format::Json] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = buffered_streams(&mut out, &mut err);
            streams.fmt = fmt;
            flock_command(&mut streams, &link, VersionGuard::Enforce, &flock_args()).await
        };

        assert_eq!(code, ExitCode::VersionSkew, "{fmt:?}");
        let text = String::from_utf8(err).unwrap();
        assert!(text.contains("the shepherd on amp2 is 9.9.9"), "{text}");
        assert!(text.contains("ssh amp2 shep daemon reload"), "{text}");
        assert!(
            !text.contains("`cargo install shep`"),
            "the local cause does not explain a remote skew: {text}"
        );
        assert_eq!(
            text.matches("shep daemon reload").count(),
            1,
            "the remedy is named once: {text}"
        );
        assert!(
            !text.contains("Run `shep daemon reload`") && !text.contains("Run:\n    shep daemon"),
            "the bare command would reload this machine's shepherd: {text}"
        );
        assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
    }
}

/// The fleet case this issue is about: two hosts on two patch releases, the
/// wire unchanged, and a verb that writes.
///
/// The write goes ahead, and the difference is one notice on stderr in the
/// words a read verb already uses.
#[cfg(unix)]
#[tokio::test]
async fn a_host_on_another_version_but_the_same_protocol_is_written_to_and_warns() {
    let dir = tempfile::tempdir().unwrap();
    let client = client_over_host(dir.path(), "9.9.9", PROTOCOL_VERSION).await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = buffered_streams(&mut out, &mut err);
        refuse_version_skew(&mut streams, &client, VersionGuard::Enforce)
            .expect("a host that speaks this protocol is writable, whatever it is versioned");
    }
    assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
    let text = String::from_utf8(err).unwrap();
    assert_eq!(
        text,
        format!("notice[version_skew]: {HOST_SKEWED_NOTICE}\n"),
        "{text}"
    );
    assert!(
        !text.contains("error[version_skew]"),
        "a tolerated difference is a notice, not a refusal: {text}"
    );
}

/// Under `--format json` the notice is one envelope on stderr and stdout
/// belongs to the verb alone.
#[cfg(unix)]
#[tokio::test]
async fn under_json_a_tolerated_host_skew_leaves_stdout_to_the_verb() {
    let dir = tempfile::tempdir().unwrap();
    let client = client_over_host(dir.path(), "9.9.9", PROTOCOL_VERSION).await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = buffered_streams(&mut out, &mut err);
        streams.fmt = Format::Json;
        refuse_version_skew(&mut streams, &client, VersionGuard::Enforce).unwrap();
    }
    assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
    let text = String::from_utf8(err).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    let notice: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(notice["notice"]["code"], "version_skew");
    assert_eq!(notice["notice"]["message"], HOST_SKEWED_NOTICE);
}

/// The half of the table that still refuses, and the numbers an operator
/// needs from it: this build's and that host's, so the two installs can be
/// named before anything is upgraded.
#[cfg(unix)]
#[tokio::test]
async fn a_host_on_another_protocol_is_refused_naming_both_protocol_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let peer = PROTOCOL_VERSION - 1;
    let client = client_over_host(dir.path(), "0.16.0", peer).await;

    for fmt in [Format::Table, Format::Json] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = buffered_streams(&mut out, &mut err);
            streams.fmt = fmt;
            refuse_version_skew(&mut streams, &client, VersionGuard::Enforce)
                .expect_err("a shepherd on another protocol is not writable")
        };

        assert_eq!(code, ExitCode::VersionSkew, "{fmt:?}");
        let text = String::from_utf8(err).unwrap();
        let this_build = format!(
            "this shep is {} speaking protocol {PROTOCOL_VERSION}",
            env!("CARGO_PKG_VERSION")
        );
        assert!(text.contains(&this_build), "{text}");
        assert!(
            text.contains(&format!(
                "the shepherd on amp2 is 0.16.0 speaking protocol {peer}"
            )),
            "{text}"
        );
        assert!(text.contains("ssh amp2 shep daemon reload"), "{text}");
        assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
    }
}

/// A host that agrees on both says nothing at all: a notice nobody asked for
/// on every `--host` run is noise an operator learns to ignore.
#[cfg(unix)]
#[tokio::test]
async fn a_host_that_agrees_on_version_and_protocol_says_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let client = client_over_host(dir.path(), env!("CARGO_PKG_VERSION"), PROTOCOL_VERSION).await;

    for guard in [VersionGuard::Enforce, VersionGuard::Warn] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = buffered_streams(&mut out, &mut err);
            refuse_version_skew(&mut streams, &client, guard)
                .expect("an agreeing host is writable under every guard");
        }
        assert!(out.is_empty() && err.is_empty(), "{guard:?}");
    }
}

/// A fleet caller flattens the refusal to one line, so the two protocol
/// numbers have to survive without the table's layout.
#[cfg(unix)]
#[tokio::test]
async fn a_host_skew_under_an_enforcing_guard_is_one_line_naming_both_protocols() {
    let dir = tempfile::tempdir().unwrap();
    let peer = PROTOCOL_VERSION - 1;
    let client = client_over_host(dir.path(), "0.16.0", peer).await;

    let message = host_skew(&client, VersionGuard::Enforce)
        .expect_err("a host on another protocol is a failure of its own");
    assert_eq!(message.lines().count(), 1, "{message}");
    assert!(
        message.contains(&format!("speaking protocol {PROTOCOL_VERSION}")),
        "{message}"
    );
    assert!(
        message.contains(&format!("speaking protocol {peer}")),
        "{message}"
    );
    assert!(message.contains("ssh amp2 shep daemon reload"), "{message}");
}

/// The other side of the gap, which is what a fleet looks like halfway
/// through a host-by-host upgrade: `amp2` is already on the newer shep and
/// this machine is not. A reload there runs the newer install that is already
/// running, so the refusal has to send the operator the other way.
#[cfg(unix)]
#[tokio::test]
async fn a_host_on_a_newer_protocol_is_refused_naming_the_local_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let peer = PROTOCOL_VERSION + 1;
    let client = client_over_host(dir.path(), "0.99.0", peer).await;

    for fmt in [Format::Table, Format::Json] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = buffered_streams(&mut out, &mut err);
            streams.fmt = fmt;
            refuse_version_skew(&mut streams, &client, VersionGuard::Enforce)
                .expect_err("a shepherd on another protocol is not writable")
        };

        assert_eq!(code, ExitCode::VersionSkew, "{fmt:?}");
        let text = String::from_utf8(err).unwrap();
        assert!(
            text.contains(&format!(
                "the shepherd on amp2 is 0.99.0 speaking protocol {peer}"
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!("speaking protocol {PROTOCOL_VERSION}")),
            "{text}"
        );
        assert!(text.contains("newer wire protocol"), "{text}");
        assert!(text.contains("cargo install shep"), "{text}");
        assert!(
            !text.contains("ssh amp2 shep daemon reload"),
            "a reload on a host that is already ahead changes nothing: {text}"
        );
        assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
    }
}

/// A fleet caller flattens the refusal to one line, so the fix has to survive
/// on it too: the one line for a host this build is behind names the upgrade
/// that runs here and no ssh command at all.
#[cfg(unix)]
#[tokio::test]
async fn a_host_ahead_of_this_build_is_one_line_naming_the_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let peer = PROTOCOL_VERSION + 1;
    let client = client_over_host(dir.path(), "0.99.0", peer).await;

    let message = host_skew(&client, VersionGuard::Enforce)
        .expect_err("a host on another protocol is a failure of its own");
    assert_eq!(message.lines().count(), 1, "{message}");
    assert!(message.contains("cargo install shep"), "{message}");
    assert!(
        !message.contains("ssh amp2"),
        "the ssh reload is the fix for the other direction: {message}"
    );
    // A read verb over the same host is told the same fix, so a listing and
    // a write cannot point an operator at two different machines.
    let notice = host_skew(&client, VersionGuard::Warn)
        .unwrap()
        .expect("a refused protocol is still worth naming to a read verb");
    assert!(notice.contains("cargo install shep"), "{notice}");
    assert!(!notice.contains("ssh amp2"), "{notice}");
}

/// The roll is this machine's `flock.json`. A dead host that printed it
/// would list the wrong flock under the right exit code.
#[tokio::test]
async fn flock_over_a_host_never_falls_back_to_the_local_roll() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.run).unwrap();
    let roll =
        shep_daemon::snapshot::FlockSnapshot::with_apps(vec![shep_daemon::snapshot::SavedApp {
            app: shep_core::config::AppConfig::minimal("local-sheep", "./srv"),
            instances_running: 1,
            disabled: false,
        }]);
    std::fs::write(&paths.snapshot, serde_json::to_vec(&roll).unwrap()).unwrap();
    let link = Link::Host {
        dest: "amp2",
        options: shep_client::SshOptions::default().program(dir.path().join("no-such-ssh")),
    };

    for fmt in [Format::Table, Format::Json] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = buffered_streams(&mut out, &mut err);
            streams.fmt = fmt;
            flock_command(&mut streams, &link, VersionGuard::Enforce, &flock_args()).await
        };

        assert_eq!(code, ExitCode::DaemonUnreachable, "{fmt:?}");
        assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
        let text = String::from_utf8(err).unwrap();
        assert!(!text.contains("local-sheep"), "{text}");
        assert!(!text.contains("saved roll"), "{text}");
    }
}

/// `--follow` reaches a host, so what stops it here is the terminal. The
/// host's own refusal text would send the operator after the wrong fix.
#[tokio::test]
async fn a_host_follow_without_a_terminal_refuses_for_the_terminal_not_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let link = Link::Host {
        dest: "amp2",
        options: shep_client::SshOptions::default().program(dir.path().join("no-such-ssh")),
    };
    let args = cli::FlockArgs {
        follow: true,
        interval: 1,
    };

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = buffered_streams(&mut out, &mut err);
        flock_command_on(&mut streams, &link, VersionGuard::Enforce, &args, false).await
    };

    assert_eq!(code, ExitCode::Usage);
    let text = String::from_utf8(err).unwrap();
    assert!(text.contains("needs a terminal"), "{text}");
    assert!(!text.contains(crate::remote::HOST_REFUSAL), "{text}");
}

/// The notice a tolerated host skew prints, in full: the sentence says the
/// write went ahead, which is what `--host` now does on a matching protocol.
const HOST_SKEWED_NOTICE: &str = concat!(
    "this shep is ",
    env!("CARGO_PKG_VERSION"),
    ", the shepherd on amp2 is 9.9.9. The wire protocol matches, so verbs go ",
    "ahead; install the same version there if you want them to match. Run ",
    "`ssh amp2 shep daemon reload`.",
);

#[cfg(unix)]
#[tokio::test]
async fn the_notice_for_a_host_names_that_host_and_an_ssh_remedy() {
    let dir = tempfile::tempdir().unwrap();
    let client = client_over_host(dir.path(), "9.9.9", PROTOCOL_VERSION).await;

    assert_eq!(
        host_skew(&client, VersionGuard::Warn).unwrap().as_deref(),
        Some(HOST_SKEWED_NOTICE)
    );
    // The same line a verb that writes gets, so a fleet listing and a single
    // `--host` write cannot drift apart in what they admit.
    assert_eq!(
        host_skew(&client, VersionGuard::Enforce)
            .unwrap()
            .as_deref(),
        Some(HOST_SKEWED_NOTICE)
    );
    assert_eq!(host_skew(&client, VersionGuard::Exempt).unwrap(), None);
}
