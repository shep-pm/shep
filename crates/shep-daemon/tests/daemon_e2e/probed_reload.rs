//! A reload gated on a readiness probe: the replacement that serves is let
//! through, the one that serves nothing is refused.

use super::*;

// --- Reload: a probed app's replacement answers for itself ---

/// The `AwaitReady` window a probed reload gets, as `listen_timeout`.
///
/// Both directions matter: a replacement that will serve has to bind inside
/// it, and one that will not costs the case its whole length before the
/// abandonment lands.
///
/// `cfg(unix)`: `reuse_port_sheep` has no Windows build under that name.
#[cfg(unix)]
const PROBED_READY_WINDOW: UpDuration = UpDuration::from_millis(800);

/// One `reuse_port_sheep` on `port`, gated on a TCP probe against the port it
/// binds: the arrangement in which "is the new instance ready" and "is
/// something listening" are the same question.
#[cfg(unix)]
fn probed_sheep(name: &str, port: u16, mute_file: &std::path::Path) -> AppConfig {
    let mut app = AppConfig::minimal(name, &reuse_port_sheep().display().to_string());
    app.interpreter = Some("none".to_string());
    app.env
        .insert("SHEEP_PORT_BASE".to_string(), port.to_string());
    app.env.insert("SHEEP_HOLD_MS".to_string(), "0".to_string());
    app.env.insert(
        "SHEEP_MUTE_FILE".to_string(),
        mute_file.display().to_string(),
    );
    app.readiness_probe = Some(ProbeConfig {
        kind: ProbeKind::Tcp,
        target: format!("127.0.0.1:{port}"),
        interval: UpDuration::from_millis(50),
        timeout: UpDuration::from_millis(200),
        failure_threshold: 3,
    });
    app.listen_timeout = PROBED_READY_WINDOW;
    app.graceful_timeout = DRAIN_WINDOW;
    app.kill_timeout = DRAIN_WINDOW;
    // Nothing may respawn behind the case: a restart would put a third process
    // on this port.
    app.autorestart = false;
    app
}

/// The control for the case below: without it, an implementation that
/// abandoned every probed reload would pass the failure case and look correct.
///
/// The replacement must land in the drainee's instance slot, since the fixture
/// derives its port from `SHEP_INSTANCE`.
#[cfg(unix)]
#[tokio::test]
async fn a_probed_reload_of_a_working_release_still_finishes() {
    let _port_guard = RELOAD_PORT_LOCK.lock().await;
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let mute = dir.path().join("mute");

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let started = client
        .request(Request::Start {
            apps: vec![probed_sheep("web", port, &mute)],
        })
        .await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let drainee_id = infos[0].id;
    let drainee_pid = infos[0].pid.expect("a real spawn reports a real pid");
    await_serving(port, drainee_pid).await;
    await_online(&mut client, drainee_id).await;

    // Subscribed after the app is up: this case reads the event stream in
    // emission order, and an earlier subscription would put `Start` in front.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let accepted = client
        .request(Request::Reload {
            selector: SelectorSpec::Name("web".to_string()),
        })
        .await;
    let Response::Reloading { accepted, .. } = accepted.result.unwrap() else {
        panic!("expected an accepted reload")
    };
    assert_eq!(accepted.len(), 1);

    // In order: the question is which of the two endings the reload reached,
    // and a search would find one whatever else had already been said.
    let (ending, replacement) = client
        .next_process_event_of(&[
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
        ])
        .await;
    assert_eq!(
        ending,
        ProcessEventKind::Reloaded,
        "a serial reload is slower than an overlapping one, not broken"
    );
    let replacement_pid = replacement.pid.expect("a replacement has a pid");
    assert_ne!(replacement_pid, drainee_pid);
    assert_eq!(replacement.status, ProcStatus::Online);
    assert_eq!(
        tally(&burst(port).await),
        format!("{BURST}x served by {replacement_pid}"),
        "the replacement owns the port once the swap is done"
    );

    let held = fixture.shutdown().await;
    drop(held);
    drop(dir);
}

/// Both instances run the same command; the second finds `SHEEP_MUTE_FILE` in
/// place and binds nothing, which is what a release whose listener moved to
/// the wrong port does. Its first probe is otherwise answered by the
/// instance still bound to that address.
///
/// Two independent mechanisms cover a single-instance probed app and each is
/// enough alone: the serial reload mode, and the post-drain probe. The flock
/// row's status is what a deploy tool reads, so it is asserted too.
#[cfg(unix)]
#[tokio::test]
async fn a_replacement_that_serves_nothing_is_refused_not_reported_reloaded() {
    let _port_guard = RELOAD_PORT_LOCK.lock().await;
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let mute = dir.path().join("mute");

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let started = client
        .request(Request::Start {
            apps: vec![probed_sheep("web", port, &mute)],
        })
        .await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let drainee_id = infos[0].id;
    let drainee_pid = infos[0].pid.expect("a real spawn reports a real pid");
    await_serving(port, drainee_pid).await;
    await_online(&mut client, drainee_id).await;

    // Subscribed after the app is up: this case reads the event stream in
    // emission order, and an earlier subscription would put `Start` in front.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    // The bad release, staged between the two spawns of one unchanged app.
    std::fs::write(&mute, b"").expect("the marker must be writable");

    let accepted = client
        .request(Request::Reload {
            selector: SelectorSpec::Name("web".to_string()),
        })
        .await;
    let Response::Reloading { accepted, .. } = accepted.result.unwrap() else {
        panic!("expected an accepted reload")
    };
    assert_eq!(accepted.len(), 1);

    // In order again, for the reason the control case gives.
    let (ending, info) = client
        .next_process_event_of(&[
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
        ])
        .await;
    assert_eq!(
        ending,
        ProcessEventKind::ReloadAbandoned,
        "a replacement that binds nothing has not proved it can take over"
    );
    assert_ne!(info.id, drainee_id, "the abandonment names the replacement");

    let listed = client.request(Request::ListFlock).await;
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock.len(), 1, "the app keeps the one instance it has left");
    assert_eq!(
        flock[0].status,
        ProcStatus::Starting,
        "a replacement that never answered its probe is never called online"
    );

    let held = fixture.shutdown().await;
    drop(held);
    drop(dir);
}
