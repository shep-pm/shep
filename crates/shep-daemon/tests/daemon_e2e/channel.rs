//! The shepherd channel on fd 3: readiness, triggered actions, stdin and
//! stdout, and a child's own metrics.

use super::*;

/// How long this test waits for the gated sheep's `Online`. A small fraction
/// of the `listen_timeout` below: the gap between the two is the assertion.
const READY_DEADLINE: Duration = Duration::from_secs(5);

/// The only case that drives a real child's fd 3 through `run_sheep`'s
/// `ChildMessage::Ready -> Msg::Ready` forward.
///
/// `listen_timeout` is two orders of magnitude past [`READY_DEADLINE`]: an
/// elapsed readiness deadline brings the sheep online rather than failing it,
/// so only an early `Online` tells a forwarded ready message from an expired
/// one.
#[tokio::test]
async fn a_wait_ready_sheep_goes_online_on_its_own_channel_message() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    // Subscribe before starting: the bus delivers from the moment you join.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let mut app = ready_app("greeter", &fixture.paths.home);
    // `wait_ready` both arms the gate and makes `assemble` open the channel.
    app.wait_ready = true;
    app.listen_timeout = UpDuration::from_millis(600_000);

    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;
    assert_eq!(
        infos[0].status,
        ProcStatus::Starting,
        "a gated sheep is `starting` when Start replies, never `online`"
    );

    let online = tokio::time::timeout(
        READY_DEADLINE,
        client.await_process_event(id, ProcessEventKind::Online),
    )
    .await
    .expect("the child's own ready message must bring the sheep online");
    assert_eq!(online.status, ProcStatus::Online);

    let listed = client.request(Request::ListFlock).await;
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock[0].status, ProcStatus::Online);

    fixture.shutdown().await;
}

/// Two round trips, not one: a single reply can land by winning a
/// spawn-timing race. The child echoes a counter, so the two are
/// distinguishable.
///
/// A successful `to_child.send()` is not delivery: the first send after a
/// child has died is accepted and discarded. The `Replied` row is the proof.
// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn a_triggered_action_reaches_a_real_child_and_answers_it_twice() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let mut app = AppConfig::minimal("responder", "/bin/sh");
    app.interpreter = Some("none".to_string());
    // `channel` is what opens fd 3 here; this app gates no readiness on it.
    app.channel = true;
    app.args = vec![
        "-c".to_string(),
        r#"i=0; while IFS= read -r _line <&3; do i=$((i + 1)); printf '{"kind":"action-reply","action":"gc","body":"round-%d"}\n' "$i" >&3; done"#
            .to_string(),
    ];
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;

    for round in 1..=2 {
        let triggered = client
            .request(Request::Trigger {
                selector: SelectorSpec::Id(id),
                action: "gc".to_string(),
                params: None,
            })
            .await;
        let Response::Triggered(rows) = triggered.result.unwrap() else {
            panic!("expected triggered")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(
            rows[0].outcome,
            ActionOutcome::Replied {
                body: format!("round-{round}"),
            },
            "round {round} must carry its own reply, not a leftover from the other one"
        );
    }

    fixture.shutdown().await;
}

// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn a_line_written_to_a_real_sheeps_stdin_comes_back_on_its_stdout() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    // Subscribe before starting: the bus delivers from the moment you join.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["log.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let mut app = AppConfig::minimal("echoer", "/bin/sh");
    app.interpreter = Some("none".to_string());
    app.args = vec![
        "-c".to_string(),
        "while IFS= read -r line; do echo \"got $line\"; done".to_string(),
    ];
    app.stdin = true;

    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;

    let reply = client
        .request(Request::SendLine {
            selector: SelectorSpec::Name("echoer".to_string()),
            line: "ping".to_string(),
        })
        .await;
    let Response::SentLine(rows) = reply.result.unwrap() else {
        panic!("expected sent line")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, LineOutcome::Sent);

    // `Sent` only claims the bytes reached the pipe; this proves a read.
    let echoed = client.await_log_line(id).await;
    assert_eq!(echoed, "got ping");

    fixture.shutdown().await;
}

// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn a_childs_metric_reaches_a_channel_subscriber_over_the_socket() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    // Subscribe before starting: the bus delivers from the moment you join.
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["channel.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let mut app = AppConfig::minimal("chatty", "/bin/sh");
    app.interpreter = Some("none".to_string());
    app.channel = true;
    // Sleeps after the metric: the sheep must outlive the assertion.
    app.args = vec![
        "-c".to_string(),
        r#"printf '{"kind":"metric","name":"rps","value":42}\n' >&3; sleep 30"#.to_string(),
    ];
    client.request(Request::Start { apps: vec![app] }).await;

    // Bounded: a subscriber that never receives must fail, not hang.
    let frame = tokio::time::timeout(RECV_TIMEOUT, async {
        loop {
            if let ServerFrame::Event(BusEvent::Channel { message, .. }) = client.next_frame().await
            {
                break message;
            }
        }
    })
    .await
    .expect("no channel.* frame within the timeout");

    match frame {
        ChildMessage::Metric { name, value } => {
            assert_eq!(name, "rps");
            assert!((value - 42.0).abs() < f64::EPSILON, "{value}");
        }
        other => panic!("subscribed to channel.*, received {other:?}"),
    }

    fixture.shutdown().await;
}

/// `AppConfig::minimal` leaves `channel`, `wait_ready` and
/// `shutdown_with_message` false, the three `assemble()` ors together to
/// decide whether a sheep gets fd 3.
#[tokio::test]
async fn a_trigger_against_a_channelless_sheep_names_the_missing_channel() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let app = forever_app("mute");
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;

    let triggered = client
        .request(Request::Trigger {
            selector: SelectorSpec::Id(id),
            action: "gc".to_string(),
            params: None,
        })
        .await;
    let Response::Triggered(rows) = triggered.result.unwrap() else {
        panic!("expected triggered")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(
        rows[0].outcome,
        ActionOutcome::NoChannel,
        "a sheep spawned with no channel must be refused by name, not waited out"
    );

    fixture.shutdown().await;
}

/// `action_timeout` is set under both [`RECV_TIMEOUT`] and `rpc.rs`'s
/// `DEFAULT_DEADLINE_MS` (5s), the budget every `Client::request` here gets.
///
/// The child reads the action before falling silent: a fixture that never
/// reads leaves the message in the kernel buffer, which times out the same
/// way for a different reason.
// `cfg(unix)` because its fixture is a `/bin/sh` script.
#[cfg(unix)]
#[tokio::test]
async fn a_trigger_against_a_silent_child_times_out_rather_than_hitting_the_rpc_deadline() {
    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;

    let mut app = AppConfig::minimal("silent", "/bin/sh");
    app.interpreter = Some("none".to_string());
    app.channel = true;
    app.action_timeout = UpDuration::from_millis(500);
    app.args = vec![
        "-c".to_string(),
        "read -r _line <&3; while :; do sleep 1; done".to_string(),
    ];
    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let id = infos[0].id;

    let triggered = client
        .request(Request::Trigger {
            selector: SelectorSpec::Id(id),
            action: "gc".to_string(),
            params: None,
        })
        .await;
    let Response::Triggered(rows) = triggered.result.unwrap() else {
        panic!("expected triggered")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);
    assert_eq!(
        rows[0].outcome,
        ActionOutcome::TimedOut,
        "an app that never replies must produce a named TimedOut row, not a bare RPC error"
    );

    fixture.shutdown().await;
}
