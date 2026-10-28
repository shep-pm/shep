//! The shepherd channel on a real child's fd 3: readiness, the version
//! it is told, and a child parked on `read <&3`.

use super::*;

#[tokio::test]
async fn shepherd_channel_delivers_ready() {
    let dir = tempfile::tempdir().unwrap();
    let runner = TokioRunner::new();
    let spec = sh_spec(
        r#"printf '{"kind":"ready"}\n' >&3; sleep 5"#,
        true,
        dir.path().join("out.log"),
        dir.path().join("err.log"),
    );

    let (mut proc, mut io) = runner.spawn(&spec).unwrap();

    let msg = tokio::time::timeout(CHANNEL_DEADLINE, io.from_child.recv())
        .await
        .expect("shepherd-channel Ready should arrive promptly")
        .expect("from_child closed before Ready arrived");
    assert_eq!(msg, ChildMessage::Ready);

    // Gives the shell time to fork its `sleep 5` child before kill_tree's
    // SIGKILL, or the fork can miss the group signal entirely.
    tokio::time::sleep(Duration::from_millis(100)).await;
    proc.kill_tree().unwrap();
    let outcome = reaped(&mut proc, REAP_DEADLINE, "kill_tree should reap promptly").await;
    assert_eq!(outcome.signal, Some(9));
}

/// fails if a real child with a channel does not see `SHEP_CHANNEL_VERSION`
/// in its environment, from the `Command` through the exec to a plain
/// shell read.
///
/// Bounded by `await_file_contents`'s `LOG_WRITE_DEADLINE`: a version that
/// never arrives fails in seconds rather than hanging the whole binary.
#[tokio::test]
async fn a_child_with_a_channel_is_told_which_channel_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec_for(&dir, "/bin/sh", &["-c", "echo \"$SHEP_CHANNEL_VERSION\""]);
    // The variable rides with the channel, not with every spawn: an app with
    // no fd 3 has no channel to be told the version of.
    spec.channel = true;

    let runner = TokioRunner::new();
    let (_proc, _io) = runner.spawn(&spec).unwrap();

    await_file_contents(&dir.path().join("out.log"), "1\n").await;
}

/// fails if `SHEP_CHANNEL_VERSION` leaks into a child given no channel: an
/// app with no fd 3 must not be told it has one.
#[tokio::test]
async fn a_child_without_a_channel_is_told_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let spec = spec_for(&dir, "/bin/sh", &["-c", "echo \"[$SHEP_CHANNEL_VERSION]\""]);
    assert!(!spec.channel, "`spec_for` opens no channel by default");

    let runner = TokioRunner::new();
    let (_proc, _io) = runner.spawn(&spec).unwrap();

    await_file_contents(&dir.path().join("out.log"), "[]\n").await;
}

/// A `/bin/sh` child that answers exactly `rounds` shepherd-channel
/// messages with a reply naming which round it is, then exits 0.
///
/// `read -r line <&3` blocks; a failed read exits with a distinct status
/// instead of looping, so a broken channel ends the case promptly. The round
/// number is the child's own count of completed reads, not anything parsed
/// from the message.
pub(crate) fn channel_echo_script(rounds: u32) -> String {
    format!(
        r#"n=0
while [ "$n" -lt {rounds} ]; do
  read -r line <&3 || exit 3
  n=$((n + 1))
  printf '{{"kind":"action-reply","action":"round-%s","body":"ok"}}\n' "$n" >&3
done"#
    )
}

/// Sends one message and returns the child's reply.
///
/// Panics with the child's stderr rather than a bare timeout: a shell `read`
/// on a non-blocking descriptor prints `read error: 0: Resource temporarily
/// unavailable` there and nowhere else. A send that fails is folded into the
/// same report: it means the writer task already saw the child's end close.
async fn channel_round_trip(io: &mut ProcIo, round: u32, err_file: &Path) -> ChildMessage {
    let name = format!("round-{round}");
    let delivered = io
        .to_child
        .send(ShepherdMessage::Action {
            name,
            params: None,
            // Derived from `round`: a distinct id per call keeps each
            // round's dispatch its own.
            id: u64::from(round),
        })
        .await
        .is_ok();
    let reply = if delivered {
        tokio::time::timeout(CHANNEL_DEADLINE, io.from_child.recv())
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    reply.unwrap_or_else(|| {
        panic!(
            "round {round}: no reply on the shepherd channel; child stderr: {:?}",
            fs::read_to_string(err_file).unwrap_or_default()
        )
    })
}

/// Fails if fd 3 reaches the child non-blocking: a plain `read <&3` gets
/// `EAGAIN` instead of parking, so a child waiting for the shepherd never
/// hears it.
///
/// Two round trips: the first can pass even against a non-blocking fd 3,
/// since the message is often already buffered before the child's first
/// `read`. Only the second puts the child at the read first.
#[tokio::test]
async fn a_child_can_block_reading_the_shepherd_channel() {
    let dir = tempfile::tempdir().unwrap();
    let err_file = dir.path().join("err.log");
    let runner = TokioRunner::new();
    let spec = sh_spec(
        &channel_echo_script(2),
        true,
        dir.path().join("out.log"),
        err_file.clone(),
    );

    let (mut proc, mut io) = runner.spawn(&spec).unwrap();
    let _reaper = Reaper::of(&proc);

    let first = channel_round_trip(&mut io, 1, &err_file).await;
    assert_eq!(
        first,
        ChildMessage::ActionReply {
            action: "round-1".to_string(),
            body: "ok".to_string(),
            // The echo script never writes an `id` key.
            id: None,
        }
    );

    let second = channel_round_trip(&mut io, 2, &err_file).await;
    assert_eq!(
        second,
        ChildMessage::ActionReply {
            action: "round-2".to_string(),
            body: "ok".to_string(),
            id: None,
        }
    );

    let outcome = reaped(
        &mut proc,
        REAP_DEADLINE,
        "the child exits once it has answered both rounds",
    )
    .await;
    assert_eq!(
        outcome.code,
        Some(0),
        "status 3 is the script's own `read` failure"
    );
}
