//! The credentials a spawn asks the OS for: a foreign uid or gid is
//! refused, and as root a child runs as the user it was given.

use super::*;

/// Proves `TokioRunner::spawn`'s `command.uid`/`command.gid` calls are
/// actually invoked, without requiring root.
///
/// Spawning with your own uid/gid is a permitted no-op, and std silently
/// swallows the `EPERM` from its own privilege-drop `setgroups` call for a
/// non-root caller. A non-root `setuid`/`setgid` to an id the test process
/// does not own fails `EPERM`, so real calls make `spawn()` return `Err`.
#[tokio::test]
async fn credentials_are_actually_applied_a_foreign_id_is_refused_by_the_os() {
    if nix::unistd::geteuid().is_root() {
        // As root, setuid/setgid to an arbitrary id typically succeeds, so
        // this test's EPERM premise doesn't hold.
        return;
    }
    let own_uid = nix::unistd::geteuid().as_raw();
    let own_gid = nix::unistd::getegid().as_raw();
    // Neither id needs a real passwd/group entry: setuid(2)/setgid(2)
    // EPERMs on any unowned id regardless.
    let foreign_uid = if own_uid == 1 { 2 } else { 1 };
    let foreign_gid = if own_gid == 1 { 2 } else { 1 };
    let runner = TokioRunner::new();

    // Isolates `command.uid`: `gid: None` means `command.gid` is never
    // reached, so a failure here can only come from `.uid()`.
    let dir = tempfile::tempdir().unwrap();
    let mut uid_spec = spec_for(&dir, "id", &["-u"]);
    uid_spec.credentials = Some(Credentials {
        uid: foreign_uid,
        gid: None,
    });
    let uid_result = runner.spawn(&uid_spec);
    assert!(
        uid_result.is_err(),
        "spawning with a foreign uid must be refused by the OS if `command.uid` is really \
         called; an `Ok` here means the credentials were silently dropped on the floor"
    );

    // Isolates `command.gid`: `uid: own_uid` is a permitted no-op, so a
    // failure here can only come from `.gid()`.
    let dir = tempfile::tempdir().unwrap();
    let mut gid_spec = spec_for(&dir, "id", &["-g"]);
    gid_spec.credentials = Some(Credentials {
        uid: own_uid,
        gid: Some(foreign_gid),
    });
    let gid_result = runner.spawn(&gid_spec);
    assert!(
        gid_result.is_err(),
        "spawning with a foreign gid must be refused by the OS if `command.gid` is really \
         called; an `Ok` here means the credentials were silently dropped on the floor"
    );
}

#[tokio::test]
#[ignore = "needs root: run with `sudo -E cargo test -p shep-daemon --test real_runner -- --ignored`"]
async fn a_dropped_child_runs_as_the_requested_user() {
    assert!(
        nix::unistd::geteuid().is_root(),
        "this test only means anything as root"
    );
    let target = nix::unistd::User::from_name("nobody")
        .unwrap()
        .expect("every unix box has `nobody`");

    let dir = tempfile::tempdir().unwrap();
    // `id -G` after `id -u` proves the drop also cleared root's
    // supplementary groups instead of leaking them into the child.
    let mut spec = spec_for(&dir, "/bin/sh", &["-c", "id -u; id -G"]);
    spec.credentials = Some(Credentials {
        uid: target.uid.as_raw(),
        gid: Some(target.gid.as_raw()),
    });

    let runner = TokioRunner::new();
    let (mut proc, mut io) = runner.spawn(&spec).unwrap();
    let uid_line = recv_log(&mut io, LOG_WRITE_DEADLINE, "the child must print its uid").await;
    assert_eq!(uid_line.line.trim(), target.uid.as_raw().to_string());
    assert!(!uid_line.err);

    let groups_line = recv_log(
        &mut io,
        LOG_WRITE_DEADLINE,
        "the child must print its group list",
    )
    .await;
    let groups: Vec<&str> = groups_line.line.split_whitespace().collect();
    assert_eq!(
        groups,
        vec![target.gid.as_raw().to_string().as_str()],
        "supplementary groups must be cleared, leaving only the target gid"
    );
    assert!(!groups_line.err);

    assert_eq!(proc.wait().await.code, Some(0));
}
