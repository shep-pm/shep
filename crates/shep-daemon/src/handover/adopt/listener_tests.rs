//! A sheep's `listen` sockets through the blob and the adoption.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};

use super::testing::*;
use super::{adopt, dry_run};
use crate::handover::{CarriedListener, Handover};
use crate::listen::ListenSockets;
use crate::testing::capture_logs;

fn table(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(name, address)| ((*name).to_string(), (*address).to_string()))
        .collect()
}

/// An address nothing listens on: the OS picked it and it was then released.
fn free_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn carried_listener(name: &str, address: SocketAddr, fd: i32) -> CarriedListener {
    CarriedListener {
        sheep: "web".to_owned(),
        name: name.to_owned(),
        address: address.to_string(),
        fd,
    }
}

#[tokio::test]
async fn a_sheeps_listener_keeps_its_socket_and_the_client_waiting_on_it() {
    let predecessor = Predecessor::new();
    // A named port, not `:0`: a set remembers the address it was bound from, so
    // `is_bound_from` below can only agree with the table that bound it.
    let address = free_address();
    let sockets = ListenSockets::bind(&table(&[("http", &address.to_string())])).unwrap();
    assert_eq!(sockets.local_addrs().unwrap(), [address]);
    let mut waiting = TcpStream::connect(address).unwrap();
    waiting.write_all(b"ping").unwrap();
    let held = sockets.hold("web").unwrap();
    let mut blob = predecessor.blob();
    blob.carry_listeners(&held);

    dry_run(&blob).expect("the fixture is adoptable");
    let adopted = adopt_a_copy_keeping(&blob).unwrap();
    // The predecessor's own handles close at the exec.
    drop(held);
    drop(sockets);

    let [web] = adopted.listeners.as_slice() else {
        panic!("one sheep held sockets: {:?}", adopted.listeners);
    };
    assert_eq!(web.sheep, "web");
    assert_eq!(web.sockets.names().collect::<Vec<_>>(), ["http"]);
    assert_eq!(web.sockets.local_addrs().unwrap(), [address]);
    assert!(
        web.sockets
            .is_bound_from(&table(&[("http", &address.to_string())]))
    );
    // A rebind would have a fresh backlog, with no client in it.
    let serving = TcpListener::from(web.sockets.duplicates().unwrap().remove(0));
    let (mut accepted, _) = serving.accept().unwrap();
    let mut said = [0_u8; 4];
    accepted.read_exact(&mut said).unwrap();
    assert_eq!(
        &said, b"ping",
        "the client connected before the handover is served"
    );
}

#[tokio::test]
async fn a_blob_without_listeners_adopts_cleanly() {
    let predecessor = Predecessor::new();
    let blob = predecessor.blob();
    let written = serde_json::to_value(&blob).unwrap();
    assert!(
        written.get("listeners").is_none(),
        "a flock with no `listen` sheep writes the blob a shepherd before `listen` did"
    );

    let read = Handover::load_value(written).expect("a blob without the key loads");
    assert!(read.listeners().is_empty());
    let adopted = adopt_a_copy_keeping(&read).unwrap();

    assert!(adopted.listeners.is_empty());
    assert_eq!(adopted.sheep.len(), 1, "the rest of the flock is adopted");
}

#[tokio::test]
async fn a_carried_descriptor_that_is_not_a_listening_socket_is_replaced_and_warned_about() {
    let predecessor = Predecessor::new();
    let address = free_address();
    // A socket, and not listening: the kind most easily mistaken for one.
    let (not_listening, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut blob = predecessor.blob();
    blob.listeners = Some(vec![carried_listener(
        "http",
        address,
        std::os::fd::AsRawFd::as_raw_fd(&not_listening),
    )]);

    let mut adopted = None;
    let logs = capture_logs(|| adopted = Some(adopt_a_copy_keeping(&blob).unwrap()));
    let adopted = adopted.unwrap();

    let [web] = adopted.listeners.as_slice() else {
        panic!("the sheep keeps its set: {:?}", adopted.listeners);
    };
    assert_eq!(web.sockets.local_addrs().unwrap(), [address]);
    TcpStream::connect(address).expect("the replacement is listening");
    assert_eq!(logs.matches("WARN").count(), 1, "{logs}");
    assert!(
        logs.contains("web") && logs.contains(&address.to_string()),
        "the warning names the sheep and the address: {logs}"
    );
}

#[tokio::test]
async fn a_carried_descriptor_that_is_not_open_is_replaced_and_warned_about() {
    let dir = tempfile::tempdir().unwrap();
    let address = free_address();
    let mut blob = blob_with(&dir.path().join("shep.sock"), Vec::new());
    blob.listeners = Some(vec![carried_listener("http", address, NEVER_OPEN)]);

    let mut adopted = None;
    let logs = capture_logs(|| adopted = Some(adopt(&blob).unwrap()));
    let adopted = adopted.unwrap();

    assert_eq!(
        adopted.listeners[0].sockets.local_addrs().unwrap(),
        [address]
    );
    assert_eq!(logs.matches("WARN").count(), 1, "{logs}");
}

#[tokio::test]
async fn a_replacement_that_cannot_bind_is_left_out_with_the_warning() {
    let predecessor = Predecessor::new();
    // Held by someone else, as it is when a running instance still has it.
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap();
    let (not_listening, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut blob = predecessor.blob();
    blob.listeners = Some(vec![carried_listener(
        "http",
        address,
        std::os::fd::AsRawFd::as_raw_fd(&not_listening),
    )]);

    let mut adopted = None;
    let logs = capture_logs(|| adopted = Some(adopt_a_copy_keeping(&blob).unwrap()));

    assert!(
        adopted.unwrap().listeners.is_empty(),
        "the next start binds it or fails naming it"
    );
    assert_eq!(logs.matches("WARN").count(), 1, "{logs}");
    assert!(logs.contains(&address.to_string()), "{logs}");
}

#[tokio::test]
async fn a_carried_address_that_does_not_parse_is_left_out_with_the_warning() {
    let dir = tempfile::tempdir().unwrap();
    let mut blob = blob_with(&dir.path().join("shep.sock"), Vec::new());
    blob.listeners = Some(vec![CarriedListener {
        sheep: "web".to_owned(),
        name: "http".to_owned(),
        address: "not-an-address".to_owned(),
        fd: NEVER_OPEN,
    }]);

    let mut adopted = None;
    let logs = capture_logs(|| adopted = Some(adopt(&blob).unwrap()));

    assert!(adopted.unwrap().listeners.is_empty());
    assert_eq!(logs.matches("WARN").count(), 1, "{logs}");
    assert!(logs.contains("not-an-address"), "{logs}");
}
