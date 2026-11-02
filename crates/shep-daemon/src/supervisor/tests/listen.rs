//! Tests for the sockets a sheep's `listen` table has the shepherd hold.
//!
//! One set per app, bound before the first spawn and shared by every spawn
//! after it. A set whose address cannot be bound fails the spawn that wanted
//! it.

use super::*;
#[cfg(unix)]
use crate::listen::ListenSockets;

fn listening(name: &str, entries: &[(&str, &str)]) -> AppConfig {
    let mut app = AppConfig::minimal(name, "./srv");
    app.listen = entries
        .iter()
        .map(|(key, address)| ((*key).to_string(), (*address).to_string()))
        .collect();
    app
}

#[test]
fn a_probed_sheep_with_listen_overlaps_its_reload() {
    let mut app = listening("web", &[("http", "127.0.0.1:0")]);
    app.readiness_probe = Some(probe_config(ProbeKind::Tcp, "127.0.0.1:9"));
    let source = crate::supervisor::ReadinessSource::of(&app).expect("the target parses");

    assert_eq!(ReloadMode::of(&app, &source), ReloadMode::Overlap);

    app.listen.clear();
    assert_eq!(
        ReloadMode::of(&app, &source),
        ReloadMode::Serial,
        "the probe alone would serialise it, so `listen` is what overlaps the first"
    );
}

// `normalize` refuses `listen` on Windows, so these load it through one.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_taken_address_fails_the_start_naming_it() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = held.local_addr().unwrap().to_string();
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![ProcScript::never_exits()]);
    let app = normalize(listening("web", &[("http", &address)])).unwrap();

    let info = actor.register_at_rest(&app);
    let after = actor.respawn(info.id, true);

    assert_eq!(after.status, ProcStatus::Errored);
    assert_eq!(
        actor.runner.spawn_count(),
        0,
        "nothing runs without its socket"
    );
    let failure = actor.sheep[&info.id].entry.spawn_failure.clone().unwrap();
    assert!(
        failure.contains(&address) && failure.contains("`http`"),
        "{failure}"
    );
}

// `normalize` refuses `listen` on Windows, so these load it through one.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn every_instance_of_a_scaled_sheep_gets_the_same_sockets() {
    let dir = tempfile::tempdir().unwrap();
    let app = listening("web", &[("http", "127.0.0.1:0")]);
    let (handle, runner, _events) = started(
        &dir,
        app,
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;

    handle.scale("web", 2).await.unwrap();

    let first = runner
        .spawned_listen(0)
        .expect("the first instance has them");
    let second = runner
        .spawned_listen(1)
        .expect("the second instance has them");
    assert!(
        Arc::ptr_eq(&first, &second),
        "one bind serves every instance"
    );
}

#[tokio::test(start_paused = true)]
async fn a_sheep_without_listen_is_handed_no_sockets() {
    let dir = tempfile::tempdir().unwrap();
    let (_handle, runner, _events) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits()],
    )
    .await;

    assert!(runner.spawned_listen(0).is_none());
}

// `normalize` refuses `listen` on Windows, so these load it through one.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_reload_that_leaves_listen_alone_keeps_the_set() {
    let dir = tempfile::tempdir().unwrap();
    let app = listening("web", &[("http", "127.0.0.1:0")]);
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(
        &dir,
        app.clone(),
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    );

    let before = actor.listen_for(&app).unwrap().expect("a set is bound");
    let after = actor.listen_for(&app).unwrap().expect("and kept");

    assert!(
        Arc::ptr_eq(&before, &after),
        "the same open sockets, not a rebind"
    );
}

#[tokio::test(start_paused = true)]
async fn a_reload_that_adds_an_address_keeps_the_sockets_it_still_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![]);
    let first = listening("web", &[("http", "127.0.0.1:0")]);
    let held = actor.listen_for(&first).unwrap().unwrap();
    let port = held.local_addrs().unwrap()[0].port();
    drop(held);
    actor.listeners.clear();
    let fixed = format!("127.0.0.1:{port}");
    let first = listening("web", &[("http", &fixed)]);
    let held = actor.listen_for(&first).unwrap().unwrap();

    let grown = listening("web", &[("admin", "127.0.0.1:0"), ("http", &fixed)]);
    let next = actor
        .listen_for(&grown)
        .expect("the held address is not bound a second time")
        .unwrap();

    assert!(!Arc::ptr_eq(&held, &next));
    assert_eq!(next.names().collect::<Vec<_>>(), ["admin", "http"]);
    assert_eq!(next.local_addrs().unwrap()[1].port(), port);
}

#[tokio::test(start_paused = true)]
async fn a_reload_that_cannot_bind_its_new_set_keeps_the_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut actor = actor_with_an_empty_flock(&dir, vec![]);
    let app = listening("web", &[("http", "127.0.0.1:0")]);
    let held = actor.listen_for(&app).unwrap().unwrap();

    let broken = listening("web", &[("http", "127.0.0.1:0"), ("web", "not-an-address")]);
    actor.listen_for(&broken).unwrap_err();

    let still = actor.listeners.get("web").expect("the old set stays");
    assert!(Arc::ptr_eq(still, &held));
}

// Pipes stand in for sockets: the cases below ask only whether a set is held,
// so they need no bind.
#[cfg(unix)]
fn stand_in_set() -> Arc<ListenSockets> {
    let (read, _write) = std::io::pipe().unwrap();
    Arc::new(ListenSockets::from_descriptors(vec![("http", read.into())]))
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn the_sockets_of_a_sheep_with_no_instance_left_are_closed() {
    let dir = tempfile::tempdir().unwrap();
    let app = listening("web", &[("http", "127.0.0.1:0")]);
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(&dir, app, vec![]);
    actor.listeners.insert("web".to_string(), stand_in_set());
    actor.listeners.insert("gone".to_string(), stand_in_set());

    actor.prune_listeners();

    assert!(
        actor.listeners.contains_key("web"),
        "an online sheep keeps its set"
    );
    assert!(
        !actor.listeners.contains_key("gone"),
        "no sheep named it, so the sockets close and the next start binds afresh"
    );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_stopped_sheeps_sockets_are_closed() {
    let dir = tempfile::tempdir().unwrap();
    let app = listening("web", &[("http", "127.0.0.1:0")]);
    let (mut actor, _mailbox) = actor_with_one_online_sheep_of(&dir, app, vec![]);
    actor.listeners.insert("web".to_string(), stand_in_set());
    actor.sheep.get_mut(&0).unwrap().entry.status = ProcStatus::Stopped;

    actor.prune_listeners();

    assert!(actor.listeners.is_empty());
}
