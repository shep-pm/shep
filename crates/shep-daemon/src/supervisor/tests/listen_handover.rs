//! Tests for the `listen` sockets a handover carries.
//!
//! The sockets here are stand-in descriptors from
//! `ListenSockets::from_descriptors`, because a sandbox cannot bind: these
//! cases are about where the sockets travel, not about a bind.

use super::adopt_fixtures::daemon_fds;
use super::*;
use crate::handover::adopt::AdoptedListeners;
use crate::listen::ListenSockets;
use std::os::fd::{AsRawFd as _, OwnedFd};

/// A descriptor that opens and closes like a socket would, as far as the
/// handover is concerned.
fn stand_in() -> OwnedFd {
    OwnedFd::from(tempfile::tempfile().unwrap())
}

/// A supervisor that inherited `web`'s `http` socket.
fn inheriting(dir: &tempfile::TempDir, runner: &Arc<ScriptedRunner>) -> SupervisorHandle {
    let (events, _rx) = crate::bus::test_bus(64);
    SupervisorBuilder::new(SharedRunner(Arc::clone(runner)), test_paths(dir), events)
        .adopted_listeners(vec![AdoptedListeners {
            sheep: "web".to_owned(),
            sockets: ListenSockets::from_descriptors(vec![("http", stand_in())]),
        }])
        .spawn()
}

fn web_listening_on_any_port() -> ResolvedApp {
    let mut app = AppConfig::minimal("web", "./srv");
    app.listen.insert("http".to_owned(), "0.0.0.0:0".to_owned());
    normalize(app).unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_snapshot_carries_the_sockets_a_sheep_holds() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let handle = inheriting(&dir, &runner);
    let (fds, _held) = daemon_fds(&dir);

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    let [carried] = blob.listeners() else {
        panic!("one listener was held: {:?}", blob.listeners());
    };
    assert_eq!((carried.sheep(), carried.name()), ("web", "http"));
    assert!(
        nix::fcntl::fcntl(carried.fd(), nix::fcntl::FcntlArg::F_GETFD).is_ok(),
        "the blob names a closed descriptor: {}",
        carried.fd()
    );
}

#[tokio::test(start_paused = true)]
async fn a_snapshot_of_a_flock_that_holds_none_carries_none() {
    let dir = tempfile::tempdir().unwrap();
    let (events, _rx) = crate::bus::test_bus(64);
    let handle = spawn_supervisor(
        ScriptedRunner::new(vec![ProcScript::never_exits()]),
        test_paths(&dir),
        events,
    );
    let (fds, _held) = daemon_fds(&dir);

    let (_candidates, blob, _parked) = handle.handover_snapshot(fds).await.unwrap();

    assert!(blob.listeners().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_sockets_a_snapshot_duplicated_outlive_the_supervisors_set() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let handle = inheriting(&dir, &runner);
    let (fds, _held) = daemon_fds(&dir);
    let (_candidates, blob, parked) = handle.handover_snapshot(fds).await.unwrap();
    let number = blob.listeners()[0].fd();

    handle.shutdown().await;

    assert!(
        nix::fcntl::fcntl(number, nix::fcntl::FcntlArg::F_GETFD).is_ok(),
        "the duplicate the blob names must stay open until the exec"
    );
    drop(parked);
    assert!(
        nix::fcntl::fcntl(number, nix::fcntl::FcntlArg::F_GETFD).is_err(),
        "an abandoned handover must not leak the duplicate"
    );
}

#[tokio::test(start_paused = true)]
async fn the_next_spawn_passes_the_inherited_set_instead_of_binding() {
    let dir = tempfile::tempdir().unwrap();
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    let inherited = stand_in();
    let identity = nix::sys::stat::fstat(inherited.as_raw_fd()).unwrap().st_ino;
    let (events, _rx) = crate::bus::test_bus(64);
    let handle =
        SupervisorBuilder::new(SharedRunner(Arc::clone(&runner)), test_paths(&dir), events)
            .adopted_listeners(vec![AdoptedListeners {
                sheep: "web".to_owned(),
                sockets: ListenSockets::from_descriptors(vec![("http", inherited)]),
            }])
            .spawn();

    handle
        .start(vec![web_listening_on_any_port()])
        .await
        .unwrap();

    let passed = runner
        .spawned_listen(0)
        .expect("the sheep is handed the inherited set");
    let duplicates = passed.duplicates().unwrap();
    assert_eq!(duplicates.len(), 1);
    assert_eq!(
        nix::sys::stat::fstat(duplicates[0].as_raw_fd())
            .unwrap()
            .st_ino,
        identity,
        "the spawn got the socket the predecessor carried, not a fresh bind"
    );
}
