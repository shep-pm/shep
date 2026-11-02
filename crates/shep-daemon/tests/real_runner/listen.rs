//! The descriptors a sheep with a `listen` table is born with: systemd's
//! layout from fd 3, the channel after it, and none of it for a sheep
//! without one.

use super::*;
use shep_daemon::listen::ListenSockets;
use std::collections::BTreeMap;
use tokio::io::AsyncReadExt as _;

/// The fixture that accepts on the sockets it is given, built as
/// `examples/listen_fds_sheep.rs`; located the way `reuse_port_sheep` is.
fn listen_fds_sheep() -> PathBuf {
    let test_binary = std::env::current_exe().expect("a running test knows its own path");
    let path = test_binary
        .parent()
        .and_then(Path::parent)
        .expect("a test binary lives at <profile>/deps/<name>")
        .join("examples")
        .join("listen_fds_sheep");
    assert!(
        path.is_file(),
        "{} must exist: a plain `cargo test` builds the package's examples",
        path.display()
    );
    path
}

fn table(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(name, address)| ((*name).to_string(), (*address).to_string()))
        .collect()
}

/// What the fixture answered one connection with, by field.
async fn ask(addr: std::net::SocketAddr) -> BTreeMap<String, String> {
    let answer = tokio::time::timeout(CHANNEL_DEADLINE, async {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).await.unwrap();
        text
    })
    .await
    .unwrap_or_else(|_| panic!("{addr} did not answer within {CHANNEL_DEADLINE:?}"));
    answer
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[tokio::test]
async fn a_sheep_with_listen_is_born_with_systemds_layout_and_its_channel_after_it() {
    let dir = tempfile::tempdir().unwrap();
    // Named out of order: the sockets must come in name order, not table order.
    let sockets = ListenSockets::bind(&table(&[("web", "127.0.0.1:0"), ("admin", "127.0.0.1:0")]))
        .expect("loopback must bind");
    let addrs = sockets.local_addrs().unwrap();
    let (admin, web) = (addrs[0], addrs[1]);
    let mut spec = spec_for(&dir, &listen_fds_sheep().display().to_string(), &[]);
    spec.channel = true;
    spec.listen = Some(Arc::new(sockets));

    let runner = TokioRunner::new();
    let (mut proc, mut io) = runner.spawn(&spec).unwrap();

    let ready = tokio::time::timeout(CHANNEL_DEADLINE, io.from_child.recv())
        .await
        .expect("the sheep greets the channel it was told to find")
        .expect("the channel closed before the greeting");
    assert_eq!(ready, ChildMessage::Ready);

    let pid = proc.pid().to_string();
    for (addr, fd) in [(admin, "3"), (web, "4")] {
        let reply = ask(addr).await;
        assert_eq!(reply["fd"], fd, "{addr} is on fd {fd}: {reply:?}");
        assert_eq!(reply["pid"], pid);
        assert_eq!(reply["listen_pid"], pid, "the child's own pid: {reply:?}");
        assert_eq!(reply["listen_fds"], "2");
        assert_eq!(reply["names"], "admin:web");
        assert_eq!(reply["channel"], "5", "the channel follows the sockets");
    }

    proc.kill_tree().unwrap();
    reaped(&mut proc, REAP_DEADLINE, "kill_tree should reap promptly").await;
}

#[tokio::test]
async fn two_instances_of_one_set_accept_on_the_same_sockets() {
    let dir = tempfile::tempdir().unwrap();
    let sockets = Arc::new(ListenSockets::bind(&table(&[("http", "127.0.0.1:0")])).unwrap());
    let addr = sockets.local_addrs().unwrap()[0];
    let runner = TokioRunner::new();
    let mut spawned = Vec::new();
    for _ in 0..2 {
        let mut spec = spec_for(&dir, &listen_fds_sheep().display().to_string(), &[]);
        spec.channel = true;
        spec.listen = Some(Arc::clone(&sockets));
        let (proc, mut io) = runner.spawn(&spec).unwrap();
        tokio::time::timeout(CHANNEL_DEADLINE, io.from_child.recv())
            .await
            .expect("each instance greets its channel")
            .expect("the channel closed before the greeting");
        spawned.push((proc, io));
    }

    let pids: Vec<String> = spawned.iter().map(|(p, _)| p.pid().to_string()).collect();
    let mut seen = std::collections::BTreeSet::new();
    // The kernel decides who accepts, so connect until both have: bounded by
    // the deadline inside `ask`, and by the count.
    for _ in 0..200 {
        let reply = ask(addr).await;
        assert_eq!(reply["listen_pid"], reply["pid"]);
        assert!(pids.contains(&reply["pid"]), "{reply:?}");
        seen.insert(reply["pid"].clone());
        if seen.len() == 2 {
            break;
        }
    }
    assert_eq!(seen.len(), 2, "both instances serve the one socket");

    for (mut proc, _io) in spawned {
        proc.kill_tree().unwrap();
        reaped(&mut proc, REAP_DEADLINE, "kill_tree should reap promptly").await;
    }
}

/// A sheep that listens on nothing keeps the channel on fd 3 and is told
/// nothing about socket activation.
#[tokio::test]
async fn a_sheep_without_listen_keeps_its_channel_on_fd_3_and_gets_no_listen_vars() {
    for listen in [
        None,
        Some(Arc::new(ListenSockets::bind(&BTreeMap::new()).unwrap())),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = sh_spec(
            r#"printf '[%s][%s][%s][%s]' "$LISTEN_FDS" "$LISTEN_FDNAMES" "$LISTEN_PID" "$SHEP_CHANNEL_FD"; printf '{"kind":"ready"}\n' >&3"#,
            true,
            dir.path().join("out.log"),
            dir.path().join("err.log"),
        );
        spec.listen = listen;

        let runner = TokioRunner::new();
        let (_proc, mut io) = runner.spawn(&spec).unwrap();

        let ready = tokio::time::timeout(CHANNEL_DEADLINE, io.from_child.recv())
            .await
            .expect("the greeting arrives on fd 3")
            .expect("the channel closed before the greeting");
        assert_eq!(ready, ChildMessage::Ready);
        await_file_contents(&dir.path().join("out.log"), "[][][][3]\n").await;
    }
}
