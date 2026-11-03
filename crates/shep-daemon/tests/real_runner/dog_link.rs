//! The connection a dog is spawned holding, on a real child's fd 4.

use super::*;
use tokio::io::AsyncReadExt as _;

/// The next link the runner hands over, or a named panic.
async fn next_link(
    links: &mut tokio::sync::mpsc::UnboundedReceiver<shep_daemon::runner::DogLink>,
) -> shep_daemon::runner::DogLink {
    tokio::time::timeout(LONG_DEADLINE, links.recv())
        .await
        .expect("a link should arrive once the child has a pid")
        .expect("the runner dropped its links")
}

/// fails if a dog's fd 4 is not a live socket to the shepherd's end, or the
/// link does not name the dog and the pid it was spawned under.
#[tokio::test]
async fn a_dog_is_spawned_holding_a_connection_to_the_shepherd() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec_for(
        &dir,
        "/bin/sh",
        &["-c", r#"echo "$SHEP_DOG_FD"; printf hello >&4"#],
    );
    spec.name = "metrics".to_string();
    spec.dog_link = true;
    let runner = TokioRunner::new();
    let mut links = runner.dog_links().expect("the first call takes the links");

    let (proc, _io) = runner.spawn(&spec).unwrap();
    let mut link = next_link(&mut links).await;

    assert_eq!(link.name, "metrics");
    assert_eq!(link.pid, proc.pid());
    let mut heard = [0u8; 5];
    tokio::time::timeout(LONG_DEADLINE, link.stream.read_exact(&mut heard))
        .await
        .expect("the dog wrote to its fd 4")
        .unwrap();
    assert_eq!(&heard, b"hello");
    await_file_contents(&dir.path().join("out.log"), "4\n").await;
}

/// fails if a launched sheep's fd 4 is not a live socket to the shepherd, or the
/// link does not name the dog that launches it, the sheep and the pid.
#[tokio::test]
async fn a_launched_sheep_is_spawned_holding_a_link_named_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec_for(
        &dir,
        "/bin/sh",
        &["-c", r#"echo "$SHEP_DOG_FD"; printf hello >&4"#],
    );
    spec.name = "web".to_string();
    spec.launch_link = Some("sandbox".to_string());
    let runner = TokioRunner::new();
    let mut links = runner.dog_links().expect("the first call takes the links");

    let (proc, _io) = runner.spawn(&spec).unwrap();
    let mut link = next_link(&mut links).await;

    assert_eq!(link.name, "sandbox");
    assert_eq!(link.launch_for.as_deref(), Some("web"));
    assert_eq!(link.pid, proc.pid());
    let mut heard = [0u8; 5];
    tokio::time::timeout(LONG_DEADLINE, link.stream.read_exact(&mut heard))
        .await
        .expect("the sheep wrote to its fd 4")
        .unwrap();
    assert_eq!(&heard, b"hello");
    await_file_contents(&dir.path().join("out.log"), "4\n").await;
}

/// fails if a dog's own link is mistaken for a launch link.
#[tokio::test]
async fn a_dogs_own_link_is_not_scoped_to_a_sheep() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec_for(&dir, "/bin/sh", &["-c", "sleep 5"]);
    spec.name = "metrics".to_string();
    spec.dog_link = true;
    spec.launch_link = Some("sandbox".to_string());
    let runner = TokioRunner::new();
    let mut links = runner.dog_links().unwrap();

    let (_proc, _io) = runner.spawn(&spec).unwrap();
    let link = next_link(&mut links).await;

    assert_eq!((link.name.as_str(), link.launch_for), ("metrics", None));
}

/// fails if the links can be taken twice, which would give one runner two
/// servers.
#[tokio::test]
async fn the_links_are_taken_once() {
    let runner = TokioRunner::new();
    assert!(runner.dog_links().is_some());
    assert!(runner.dog_links().is_none());
}

/// fails if a sheep that asked for no link is told it has one.
#[tokio::test]
async fn a_sheep_is_given_no_connection() {
    let dir = tempfile::tempdir().unwrap();
    let spec = spec_for(&dir, "/bin/sh", &["-c", r#"echo "[$SHEP_DOG_FD]""#]);
    assert!(!spec.dog_link);
    let runner = TokioRunner::new();
    let mut links = runner.dog_links().unwrap();

    let (_proc, _io) = runner.spawn(&spec).unwrap();

    await_file_contents(&dir.path().join("out.log"), "[]\n").await;
    assert!(links.try_recv().is_err(), "no link for a sheep");
}

/// fails if asking for both mappings loses one: `fd_mappings` installs a
/// pre-exec hook per call, and the runner must call it once.
#[tokio::test]
async fn a_dog_with_a_channel_holds_both_fd_3_and_fd_4() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec_for(
        &dir,
        "/bin/sh",
        &[
            "-c",
            r#"printf '{"kind":"ready"}\n' >&3; printf hi >&4; sleep 5"#,
        ],
    );
    spec.channel = true;
    spec.dog_link = true;
    let runner = TokioRunner::new();
    let mut links = runner.dog_links().unwrap();

    let (mut proc, mut io) = runner.spawn(&spec).unwrap();
    let mut link = next_link(&mut links).await;

    let msg = tokio::time::timeout(LONG_DEADLINE, io.from_child.recv())
        .await
        .expect("Ready should arrive on fd 3")
        .expect("from_child closed before Ready arrived");
    assert_eq!(msg, ChildMessage::Ready);
    let mut heard = [0u8; 2];
    tokio::time::timeout(LONG_DEADLINE, link.stream.read_exact(&mut heard))
        .await
        .expect("the dog wrote to its fd 4")
        .unwrap();
    assert_eq!(&heard, b"hi");

    tokio::time::sleep(Duration::from_millis(100)).await;
    proc.kill_tree().unwrap();
    reaped(&mut proc, REAP_DEADLINE, "kill_tree should reap promptly").await;
}
