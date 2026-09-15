//! Tests for triggering an action and matching its reply.
//!
//! An app answers on its own schedule, so a reply has to find the wait that
//! asked for it. These cover stamped and unstamped replies, timeouts, debts
//! owed by an action that already gave up, and a second reply to an answered
//! one.

use super::*;

/// Fails if a reply stops reaching the wait that asked for it: the whole
/// path, from `SupervisorHandle::trigger` through the actor, the writer's
/// end of the channel, `run_sheep`'s relay of a `ChildMessage` and back.
/// `params` are asserted on the wire; nothing in the daemon reads them.
#[tokio::test(start_paused = true)]
async fn a_triggered_action_answers_with_the_apps_reply() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.channel = true;
    let (handle, runner, _events) = started(&dir, app, vec![ProcScript::never_exits()]).await;
    let mut io = runner.io_handles(0);

    // Spawned rather than awaited: the reply below is what ends this wait.
    let triggered = tokio::spawn(async move {
        handle
            .trigger(
                ProcessSelector::Name("web".to_string()),
                "gc".to_string(),
                Some("--full".to_string()),
            )
            .await
    });

    assert_eq!(
        sent_action(&mut io.to_child_rx).await,
        ShepherdMessage::Action {
            name: "gc".to_string(),
            params: Some("--full".to_string()),
            // `next_action_stamp` is read before it is incremented, so a
            // freshly-built actor's first dispatch is always stamp 0.
            id: 0,
        },
        "the action reaches the child's end of the channel as it was asked for"
    );

    io.from_child_tx
        .send(ChildMessage::ActionReply {
            action: "gc".to_string(),
            body: "swept 3".to_string(),
            // Echoing the dispatch's own stamp makes this a real round trip.
            id: Some(0),
        })
        .await
        .unwrap();

    assert_eq!(
        triggered.await.unwrap(),
        Ok(vec![ActionReply {
            id: 0,
            name: "web".to_string(),
            outcome: ActionOutcome::Replied {
                body: "swept 3".to_string()
            },
        }]),
        "the app's reply body is what the caller is answered with"
    );
}

/// fails if a `ready` on fd 3 reaches only the readiness machinery and
/// never the bus. Forwarding is a second thing the arm does.
#[tokio::test(start_paused = true)]
async fn a_ready_on_the_channel_reaches_both_the_bus_and_the_readiness_wait() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.channel = true;
    app.wait_ready = true;
    // `started` subscribes the bus receiver before the start.
    let (handle, runner, mut events) = started(&dir, app, vec![ProcScript::never_exits()]).await;
    let io = runner.io_handles(0);

    io.from_child_tx.send(ChildMessage::Ready).await.unwrap();

    // Bounded: a bus that never receives would park this case.
    let seen = tokio::time::timeout(ACTION_WINDOW, async {
        loop {
            if let BusEvent::Channel { id, message } = events.recv().await.unwrap().to_event() {
                break (id, message);
            }
        }
    })
    .await
    .expect("no channel event within the window");

    assert_eq!(seen, (0, ChildMessage::Ready));

    // The readiness half still works: the sheep goes Online off this message.
    let listed = handle.list().await;
    assert_eq!(listed[0].status, ProcStatus::Online);
}

/// fails if a metric is still only a `tracing::debug!`, which no
/// subscriber can read.
#[tokio::test(start_paused = true)]
async fn a_metric_on_the_channel_reaches_the_bus_with_its_name_and_value() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.channel = true;
    let (_handle, runner, mut events) = started(&dir, app, vec![ProcScript::never_exits()]).await;
    let io = runner.io_handles(0);

    io.from_child_tx
        .send(ChildMessage::Metric {
            name: "rps".to_string(),
            value: 42.0,
        })
        .await
        .unwrap();

    let seen = tokio::time::timeout(ACTION_WINDOW, async {
        loop {
            if let BusEvent::Channel { id, message } = events.recv().await.unwrap().to_event() {
                break (id, message);
            }
        }
    })
    .await
    .expect("no channel event within the window");

    assert_eq!(
        seen,
        (
            0,
            ChildMessage::Metric {
                name: "rps".to_string(),
                value: 42.0,
            }
        )
    );
}

/// fails if an `action-reply` nobody is waiting for is dropped before the
/// bus sees it. `handle_action_reply` finds no wait and discards it.
#[tokio::test(start_paused = true)]
async fn an_action_reply_no_trigger_is_waiting_for_still_reaches_the_bus() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("web", "./srv");
    app.channel = true;
    let (_handle, runner, mut events) = started(&dir, app, vec![ProcScript::never_exits()]).await;
    let io = runner.io_handles(0);

    io.from_child_tx
        .send(ChildMessage::ActionReply {
            action: "gc".to_string(),
            body: "unprompted".to_string(),
            id: None,
        })
        .await
        .unwrap();

    let seen = tokio::time::timeout(ACTION_WINDOW, async {
        loop {
            if let BusEvent::Channel { message, .. } = events.recv().await.unwrap().to_event() {
                break message;
            }
        }
    })
    .await
    .expect("no channel event within the window");

    let ChildMessage::ActionReply { body, .. } = seen else {
        panic!("expected an action reply, got {seen:?}");
    };
    assert_eq!(body, "unprompted");
}

/// Fails if a wait for an app that never answers does not end on its own.
///
/// The action is read off the channel after the answer: a build that never
/// sent it would time out too, and look identical without that read.
#[tokio::test(start_paused = true)]
async fn a_triggered_action_times_out_when_the_app_never_answers() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = AppConfig::minimal("api", "./api");
    app.channel = true;
    let (handle, runner, _events) = started(&dir, app, vec![ProcScript::never_exits()]).await;
    let mut io = runner.io_handles(0);

    assert_eq!(
        handle
            .trigger(
                ProcessSelector::Name("api".to_string()),
                "stats".to_string(),
                None,
            )
            .await,
        Ok(vec![ActionReply {
            id: 0,
            name: "api".to_string(),
            outcome: ActionOutcome::TimedOut,
        }]),
        "an app that says nothing is reported as saying nothing, not waited on forever"
    );
    assert_eq!(
        sent_action(&mut io.to_child_rx).await,
        ShepherdMessage::Action {
            name: "stats".to_string(),
            params: None,
            // The actor's first and only dispatch carries stamp 0.
            id: 0,
        },
        "the timeout is an app that did not answer, not an action that was never sent"
    );
}

/// T1 times out and leaves a `gc` debt, T2 is triggered and is live, and
/// the app's next `gc` reply, carrying T2's stamp, must reach T2.
#[test]
fn a_stamped_reply_wakes_its_own_wait_even_with_a_debt_outstanding() {
    let mut waits = ActionWaits::default();

    // T1: armed, then resolved without its reply: the timeout path.
    let (t1_reply, _t1_out) = oneshot::channel();
    let (t1_waiter, _t1_body) = oneshot::channel();
    waits.arm(PendingAction {
        stamp: 1,
        action: "gc".to_string(),
        waiter: Some(t1_waiter),
        reply: t1_reply,
    });
    assert!(waits.resolve(1).is_some(), "T1 must have been live");

    // T2: armed and still live.
    let (t2_reply, _t2_out) = oneshot::channel();
    let (t2_waiter, t2_body) = oneshot::channel();
    waits.arm(PendingAction {
        stamp: 2,
        action: "gc".to_string(),
        waiter: Some(t2_waiter),
        reply: t2_reply,
    });

    let woken = waits
        .answer("gc", Some(2))
        .expect("a reply stamped with the live wait's own stamp must reach it");
    woken.send("collected".to_string()).unwrap();
    assert_eq!(t2_body.blocking_recv().unwrap(), "collected");
}

/// An app that does not echo the stamp must see the debt paid first and the
/// live wait left alone.
#[test]
fn an_unstamped_reply_still_settles_the_oldest_debt_first() {
    let mut waits = ActionWaits::default();

    let (t1_reply, _t1_out) = oneshot::channel();
    let (t1_waiter, _t1_body) = oneshot::channel();
    waits.arm(PendingAction {
        stamp: 1,
        action: "gc".to_string(),
        waiter: Some(t1_waiter),
        reply: t1_reply,
    });
    waits.resolve(1);

    let (t2_reply, _t2_out) = oneshot::channel();
    let (t2_waiter, _t2_body) = oneshot::channel();
    waits.arm(PendingAction {
        stamp: 2,
        action: "gc".to_string(),
        waiter: Some(t2_waiter),
        reply: t2_reply,
    });

    assert!(
        waits.answer("gc", None).is_none(),
        "an unstamped reply pays the debt, exactly as it did before stamping"
    );
    assert!(
        waits.answer("gc", None).is_some(),
        "and the next one reaches the live wait, exactly as it did before"
    );
}

/// The stamped path has to settle its own debt, not just skip the queue.
#[test]
fn a_stamped_reply_for_a_dead_wait_does_not_reach_a_live_one() {
    let mut waits = ActionWaits::default();

    let (t1_reply, _t1_out) = oneshot::channel();
    let (t1_waiter, _t1_body) = oneshot::channel();
    waits.arm(PendingAction {
        stamp: 1,
        action: "gc".to_string(),
        waiter: Some(t1_waiter),
        reply: t1_reply,
    });
    waits.resolve(1);

    let (t2_reply, _t2_out) = oneshot::channel();
    let (t2_waiter, _t2_body) = oneshot::channel();
    waits.arm(PendingAction {
        stamp: 2,
        action: "gc".to_string(),
        waiter: Some(t2_waiter),
        reply: t2_reply,
    });

    assert!(
        waits.answer("gc", Some(1)).is_none(),
        "T1's own late reply belongs to T1's debt, not to T2"
    );
    assert!(
        waits.answer("gc", Some(2)).is_some(),
        "and T2 is still waiting for its own"
    );
}

/// Fails if a reply owed by a wait that already timed out answers a later
/// wait for the same action. The one failure that produces a wrong answer
/// rather than an error: an app's reply names the action and nothing else.
/// Delete the `abandoned` bookkeeping in `ActionWaits::resolve` and the
/// second trigger is answered `Replied` with the first trigger's body.
#[tokio::test(start_paused = true)]
async fn a_reply_owed_by_a_timed_out_action_never_answers_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox, mut child_rx) = actor_with_an_open_channel(&dir);

    let first = trigger_action(&mut actor, "gc");
    sent_action(&mut child_rx).await;
    assert_eq!(
        settle_action(&mut actor, &mut mailbox).await,
        ActionOutcome::TimedOut
    );
    assert_eq!(first.await.unwrap(), ActionOutcome::TimedOut);

    let second = trigger_action(&mut actor, "gc");
    sent_action(&mut child_rx).await;
    // The app finally answers the first `gc`, having no way to say so.
    actor.handle_action_reply(0, "gc", "swept 3".to_string(), None);
    assert_eq!(
        settle_action(&mut actor, &mut mailbox).await,
        ActionOutcome::TimedOut,
        "a reply the first `gc` was owed was handed to the second one"
    );
    assert_eq!(second.await.unwrap(), ActionOutcome::TimedOut);

    // One reply per debt: two `gc` waits have given up and the reply above
    // settled the first.
    let third = trigger_action(&mut actor, "gc");
    sent_action(&mut child_rx).await;
    actor.handle_action_reply(0, "gc", "swept 7".to_string(), None);
    actor.handle_action_reply(0, "gc", "swept 11".to_string(), None);
    assert_eq!(
        settle_action(&mut actor, &mut mailbox).await,
        ActionOutcome::Replied {
            body: "swept 11".to_string()
        },
        "the debts outlived the replies that settled them"
    );
    assert_eq!(
        third.await.unwrap(),
        ActionOutcome::Replied {
            body: "swept 11".to_string()
        }
    );
}

/// Fails if a second reply to an already-answered action is kept for
/// anything. An app is free to write two, and the second is neither an
/// error nor a debt. Proved by a wait armed after it still timing out.
#[tokio::test(start_paused = true)]
async fn a_second_reply_to_an_answered_action_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox, mut child_rx) = actor_with_an_open_channel(&dir);

    let answered = trigger_action(&mut actor, "gc");
    sent_action(&mut child_rx).await;
    actor.handle_action_reply(0, "gc", "swept 3".to_string(), None);
    assert_eq!(
        settle_action(&mut actor, &mut mailbox).await,
        ActionOutcome::Replied {
            body: "swept 3".to_string()
        }
    );
    assert_eq!(
        answered.await.unwrap(),
        ActionOutcome::Replied {
            body: "swept 3".to_string()
        }
    );

    actor.handle_action_reply(0, "gc", "spent".to_string(), None);

    let next = trigger_action(&mut actor, "gc");
    sent_action(&mut child_rx).await;
    assert_eq!(
        settle_action(&mut actor, &mut mailbox).await,
        ActionOutcome::TimedOut,
        "a spare reply was kept and used to answer a wait that came after it"
    );
    assert_eq!(next.await.unwrap(), ActionOutcome::TimedOut);
}

/// Fails if two waits for the same action on one sheep are not answered in
/// the order they were asked. Neither reply says which is which; order is a
/// property of the channel rather than of anything the daemon records.
#[tokio::test(start_paused = true)]
async fn two_waits_for_one_action_are_answered_in_the_order_they_were_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox, mut child_rx) = actor_with_an_open_channel(&dir);

    let first = trigger_action(&mut actor, "gc");
    sent_action(&mut child_rx).await;
    let second = trigger_action(&mut actor, "gc");
    sent_action(&mut child_rx).await;

    actor.handle_action_reply(0, "gc", "swept 3".to_string(), None);
    actor.handle_action_reply(0, "gc", "swept 7".to_string(), None);
    settle_action(&mut actor, &mut mailbox).await;
    settle_action(&mut actor, &mut mailbox).await;

    assert_eq!(
        first.await.unwrap(),
        ActionOutcome::Replied {
            body: "swept 3".to_string()
        },
        "the earlier trigger was answered with the later reply"
    );
    assert_eq!(
        second.await.unwrap(),
        ActionOutcome::Replied {
            body: "swept 7".to_string()
        },
        "the second reply was dropped and its wait left waiting"
    );
}

/// Fails if the deadline stops covering the delivery of an action and
/// covers only the reply to it. A child that has stopped reading fd 3 backs
/// its socket up, and a send onto a full channel waits for room that is not
/// coming. The channel here is built full and left unread.
#[tokio::test(start_paused = true)]
async fn an_action_that_cannot_even_be_delivered_still_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox) = actor_with_one_online_sheep(&dir, vec![]);

    // Held, never read: dropping it would make the send fail outright.
    let (to_child, _wedged) = mpsc::channel(1);
    to_child.try_send(ShepherdMessage::Shutdown).unwrap();
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers id 0")
        .to_child = Some(to_child);

    let answer = trigger_action(&mut actor, "gc");
    assert_eq!(
        settle_action(&mut actor, &mut mailbox).await,
        ActionOutcome::TimedOut,
        "an action that never got onto the channel left its wait parked"
    );
    assert_eq!(answer.await.unwrap(), ActionOutcome::TimedOut);
}
