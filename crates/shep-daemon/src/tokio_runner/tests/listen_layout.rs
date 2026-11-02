//! Where a child finds the descriptors a `listen` table gave it, checked with
//! pipes standing in for sockets so no test needs a bind.
//!
//! The child writes a letter down each descriptor it expects, so a pipe's
//! reader says which descriptor reached which number.

use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::Duration;

use crate::channel::ChildMessage;
use crate::listen::ListenSockets;
use crate::runner::{ProcessRunner, RunningProcess, SpawnSpec};
use crate::tokio_runner::TokioRunner;

const DEADLINE: Duration = Duration::from_secs(10);

fn pipe() -> (std::fs::File, OwnedFd) {
    let (read, write) = std::io::pipe().unwrap();
    (std::fs::File::from(OwnedFd::from(read)), OwnedFd::from(write))
}

#[tokio::test]
async fn listeners_take_fd_3_up_in_name_order_and_the_channel_follows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut admin_read, admin) = pipe();
    let (mut web_read, web) = pipe();
    // Handed over web first: the layout follows names, not the order given.
    let sockets = ListenSockets::from_descriptors(vec![("web", web), ("admin", admin)]);
    let spec = SpawnSpec {
        program: "/bin/sh".to_string(),
        args: vec![
            "-c".to_string(),
            r#"printf A >&3; printf W >&4
printf '{"kind":"ready"}\n' >&5
printf '%s|%s|%s|%s|%s' "$LISTEN_FDS" "$LISTEN_FDNAMES" "$SHEP_CHANNEL_FD" "$$" "$LISTEN_PID""#
                .to_string(),
        ],
        out_file: dir.path().join("out.log"),
        err_file: dir.path().join("err.log"),
        channel: true,
        listen: Some(Arc::new(sockets)),
        ..Default::default()
    };

    let runner = TokioRunner::new();
    let (proc, mut io) = runner.spawn(&spec).unwrap();
    // The spec held the only other copies of the write ends.
    drop(spec);

    let message = tokio::time::timeout(DEADLINE, io.from_child.recv())
        .await
        .expect("the greeting arrives on fd 5")
        .expect("the channel closed before the greeting");
    assert_eq!(message, ChildMessage::Ready);

    let mut letter = String::new();
    admin_read.read_to_string(&mut letter).unwrap();
    assert_eq!(letter, "A", "name order puts `admin` on fd 3");
    letter.clear();
    web_read.read_to_string(&mut letter).unwrap();
    assert_eq!(letter, "W", "and `web` on fd 4");

    let line = tokio::time::timeout(DEADLINE, async {
        loop {
            let text = std::fs::read_to_string(dir.path().join("out.log")).unwrap_or_default();
            if let Some(line) = text.lines().next() {
                break shep_core::logstamp::strip(line).to_string();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the child logs its environment");
    let parts: Vec<&str> = line.split('|').collect();
    assert_eq!(&parts[..3], ["2", "admin:web", "5"], "{line}");
    assert_eq!(parts[3], parts[4], "LISTEN_PID is the child's own pid: {line}");
    assert_eq!(parts[3], proc.pid().to_string());
}

#[tokio::test]
async fn listen_variables_set_by_the_operator_are_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (_read, only) = pipe();
    let mut spec = SpawnSpec {
        program: "/bin/sh".to_string(),
        args: vec![
            "-c".to_string(),
            r#"printf '%s|%s|%s' "$LISTEN_FDS" "$LISTEN_FDNAMES" "$KEPT""#.to_string(),
        ],
        out_file: dir.path().join("out.log"),
        err_file: dir.path().join("err.log"),
        listen: Some(Arc::new(ListenSockets::from_descriptors(vec![("http", only)]))),
        ..Default::default()
    };
    spec.env.insert("LISTEN_FDS".to_string(), "9".to_string());
    spec.env.insert("LISTEN_FDNAMES".to_string(), "stale".to_string());
    spec.env.insert("KEPT".to_string(), "yes".to_string());

    let runner = TokioRunner::new();
    let (mut proc, _io) = runner.spawn(&spec).unwrap();

    let text = tokio::time::timeout(DEADLINE, async {
        loop {
            let text = std::fs::read_to_string(dir.path().join("out.log")).unwrap_or_default();
            if !text.is_empty() {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the child logs its environment");
    assert_eq!(shep_core::logstamp::strip(text.lines().next().unwrap()), "1|http|yes");
    let _ = proc.kill_tree();
}
