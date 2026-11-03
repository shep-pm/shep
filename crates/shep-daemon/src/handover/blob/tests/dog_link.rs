//! A dog's own connection in the blob: which sheep it is named against, what
//! rides with it, and that an older blob without it still loads.

use std::os::fd::{AsRawFd as _, OwnedFd};

use super::sample_handover;
use crate::handover::{DogConnection, Handover};

/// A connection for the sheep `sample_handover` carries (`web`, pid 100).
fn connection(
    pid: u32,
    topics: Option<Vec<String>>,
) -> (DogConnection, std::os::unix::net::UnixStream) {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    (
        DogConnection {
            name: "web".to_owned(),
            launch_for: None,
            pid,
            fd: OwnedFd::from(ours),
            topics,
        },
        theirs,
    )
}

#[test]
fn a_launch_link_is_carried_on_the_sheep_it_was_opened_for() {
    let mut blob = sample_handover();
    let (mut link, _sheep_end) = connection(100, Some(vec!["config.sheep.sandbox".to_owned()]));
    link.name = "sandbox".to_owned();
    link.launch_for = Some("web".to_owned());
    let number = link.fd.as_raw_fd();

    blob.carry_dog_links(&[link]);

    assert_eq!(blob.sheep[0].fds.dog_link, Some(number));
    assert_eq!(
        blob.sheep[0].dog_topics(),
        Some(&["config.sheep.sandbox".to_owned()][..])
    );
}

#[test]
fn a_launch_link_is_not_carried_on_the_dog_that_launches_it() {
    let mut blob = sample_handover();
    let (mut link, _sheep_end) = connection(100, None);
    link.launch_for = Some("elsewhere".to_owned());

    blob.carry_dog_links(&[link]);

    assert_eq!(blob.sheep[0].fds.dog_link, None);
}

#[test]
fn a_connection_is_named_against_the_sheep_with_its_name_and_pid() {
    let mut blob = sample_handover();
    let (link, _dog_end) = connection(100, Some(vec!["process.*".to_owned()]));
    let number = link.fd.as_raw_fd();

    blob.carry_dog_links(&[link]);

    assert_eq!(blob.sheep[0].fds.dog_link, Some(number));
    assert_eq!(
        blob.sheep[0].dog_topics(),
        Some(&["process.*".to_owned()][..])
    );
}

#[test]
fn the_number_is_one_the_exec_will_carry() {
    let mut blob = sample_handover();
    let (link, _dog_end) = connection(100, None);
    let number = link.fd.as_raw_fd();

    blob.carry_dog_links(&[link]);

    assert!(
        blob.named_fds().any(|fd| fd == number),
        "a descriptor named in a sheep's row is carried"
    );
}

#[test]
fn a_connection_from_before_a_respawn_is_not_handed_to_the_new_process() {
    let mut blob = sample_handover();
    let (stale, _dog_end) = connection(99, Some(vec![]));

    blob.carry_dog_links(&[stale]);

    assert_eq!(blob.sheep[0].fds.dog_link, None);
    assert_eq!(blob.sheep[0].dog_topics(), None);
}

#[test]
fn a_connection_whose_dog_has_not_said_hello_carries_no_topics() {
    let mut blob = sample_handover();
    let (link, _dog_end) = connection(100, None);

    blob.carry_dog_links(&[link]);

    assert!(blob.sheep[0].fds.dog_link.is_some());
    assert_eq!(blob.sheep[0].dog_topics(), None);
}

#[test]
fn a_connection_and_its_topics_survive_the_blob() {
    let mut blob = sample_handover();
    let (link, _dog_end) = connection(100, Some(vec!["log.*".to_owned(), "process.*".to_owned()]));
    blob.carry_dog_links(&[link]);

    let back: Handover = serde_json::from_str(&serde_json::to_string(&blob).unwrap()).unwrap();

    assert_eq!(back, blob);
}

#[test]
fn a_blob_written_before_dog_connections_were_carried_still_loads() {
    let mut value = serde_json::to_value(sample_handover()).unwrap();
    let row = value["sheep"][0]
        .as_object_mut()
        .expect("a carried sheep is an object");
    row["fds"]
        .as_object_mut()
        .expect("a carried sheep names its descriptors")
        .remove("dog_link")
        .expect("the field this case removes must be there to remove");
    row.remove("dog_topics")
        .expect("the field this case removes must be there to remove");

    let loaded = Handover::load_value(value).expect("an older blob must still load");

    assert_eq!(loaded.sheep[0].fds.dog_link, None);
    assert_eq!(loaded.sheep[0].dog_topics(), None);
}
