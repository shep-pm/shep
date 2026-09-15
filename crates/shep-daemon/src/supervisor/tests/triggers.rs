//! Tests for a trigger across a whole flock.
//!
//! A trigger answers only once every matched sheep has, and reports each one in
//! its own row: delivered, refused for want of a channel, or skipped as a
//! reload drainee.

use super::*;

/// Registers one more `Online` sheep under `name`, holding `to_child` as
/// its shepherd-channel sender, and hands back its id. Direct because the
/// fake runner wires a live channel for every spawn whatever `channel` says.
fn register_sheep(
    actor: &mut Actor<ScriptedRunner>,
    dir: &tempfile::TempDir,
    name: &str,
    to_child: Option<mpsc::Sender<ShepherdMessage>>,
) -> u32 {
    let id = actor.next_id;
    actor.next_id += 1;
    let paths = test_paths(dir);
    let app = normalize(AppConfig::minimal(name, "./srv")).unwrap();
    actor.sheep.insert(
        id,
        SheepSlot {
            to_child,
            ..SheepSlot::new(armed_entry(id, 0, 2000 + id, app, &paths))
        },
    );
    id
}

/// Puts one action on every sheep matching `selector` and hands back the
/// receiver the whole answer will arrive on.
fn trigger_flock(
    actor: &mut Actor<ScriptedRunner>,
    selector: ProcessSelector,
    action: &str,
) -> oneshot::Receiver<Result<Vec<ActionReply>, SupervisorError>> {
    let (reply, answer) = oneshot::channel();
    actor.handle_command(Command::Trigger {
        selector,
        action: action.to_string(),
        params: None,
        reply,
    });
    answer
}

/// Fails if a trigger answers before every sheep it matched has been heard
/// from, drops any of them, or returns the rows in settle order.
///
/// The names are chosen so no two candidate orders agree. Ids run 0, 1, 2
/// in registration order; the rows come out in name order, `[0, 2, 1]`,
/// where settle order would be `[1, 0, 2]` and id order `[0, 1, 2]`.
/// `spawn_trigger_task` sorts by `(name, id)`.
#[tokio::test(start_paused = true)]
async fn a_trigger_answers_every_sheep_it_matched_before_it_answers_at_all() {
    // Must sort after `worker`, or name order and settle order coincide.
    let third = "zone";
    assert!(third > "worker", "`{third}` must sort after `worker`");

    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox, mut child_rx) = actor_with_an_open_channel(&dir);
    register_sheep(&mut actor, &dir, third, None);
    let (silent_tx, mut silent_rx) = mpsc::channel(16);
    register_sheep(&mut actor, &dir, "worker", Some(silent_tx));

    let mut answer = trigger_flock(&mut actor, ProcessSelector::All, "gc");
    sent_action(&mut child_rx).await;
    sent_action(&mut silent_rx).await;

    actor.handle_action_reply(0, "gc", "swept 3".to_string(), None);
    assert_eq!(
        settle_action(&mut actor, &mut mailbox).await,
        ActionOutcome::Replied {
            body: "swept 3".to_string()
        }
    );
    // Yielded first: `settle_action` returns without the collecting task
    // having been polled, so an unyielded `try_recv` reads `Empty` anyway.
    tokio::task::yield_now().await;
    assert!(
        matches!(answer.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
        "a trigger answered while a sheep it matched was still being waited on"
    );

    assert_eq!(
        settle_action(&mut actor, &mut mailbox).await,
        ActionOutcome::TimedOut
    );
    assert_eq!(
        triggered(answer).await,
        Ok(vec![
            row(
                0,
                "web",
                ActionOutcome::Replied {
                    body: "swept 3".to_string()
                }
            ),
            row(2, "worker", ActionOutcome::TimedOut),
            row(1, third, ActionOutcome::NoChannel),
        ])
    );
}

/// Fails if a sheep with no live channel is waited out instead of refused
/// on the spot, or if that refusal takes the rest of the selector's matches
/// with it. Refusing takes no wait, and the mailbox carrying exactly one
/// result is what says so.
#[tokio::test(start_paused = true)]
async fn a_sheep_with_no_channel_is_refused_in_its_own_row() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox, mut child_rx) = actor_with_an_open_channel(&dir);
    register_sheep(&mut actor, &dir, "api", None);

    let answer = trigger_flock(&mut actor, ProcessSelector::All, "gc");
    sent_action(&mut child_rx).await;
    actor.handle_action_reply(0, "gc", "swept 3".to_string(), None);
    settle_action(&mut actor, &mut mailbox).await;

    assert_eq!(
        triggered(answer).await,
        Ok(vec![
            row(1, "api", ActionOutcome::NoChannel),
            row(
                0,
                "web",
                ActionOutcome::Replied {
                    body: "swept 3".to_string()
                }
            ),
        ])
    );
    assert!(
        matches!(mailbox.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "a sheep with no channel armed a wait anyway"
    );
}

/// Fails if "can this sheep be triggered" is answered off the presence of a
/// sender rather than off whether anything is still receiving on it. An app
/// configured without a channel has its receiving end dropped at spawn.
#[tokio::test(start_paused = true)]
async fn a_sheep_whose_channel_has_no_far_end_is_refused_too() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox) = actor_with_one_online_sheep(&dir, vec![]);
    let (to_child, receiver) = mpsc::channel(16);
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers id 0")
        .to_child = Some(to_child);
    drop(receiver);

    assert_eq!(
        triggered(trigger_flock(&mut actor, ProcessSelector::All, "gc")).await,
        Ok(vec![row(0, "web", ActionOutcome::NoChannel)])
    );
    assert!(
        matches!(mailbox.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "a sheep whose channel has no far end armed a wait anyway"
    );
}

/// Fails if a flock where nothing can be reached is answered as though the
/// action had been delivered. It is a success, and the rows are what stop
/// it being a silent one.
#[tokio::test(start_paused = true)]
async fn a_trigger_no_sheep_can_take_is_a_success_that_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox) = actor_with_one_online_sheep(&dir, vec![]);
    register_sheep(&mut actor, &dir, "api", None);

    assert_eq!(
        triggered(trigger_flock(&mut actor, ProcessSelector::All, "gc")).await,
        Ok(vec![
            row(0, "web", ActionOutcome::NoChannel),
            row(1, "api", ActionOutcome::NoChannel),
        ])
    );
    assert!(
        matches!(mailbox.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "a flock with nothing to deliver to armed a wait anyway"
    );
}

/// Fails if a reload drainee is sent the action rather than skipped. Both
/// halves answer to the app's name and the drainee still holds a live
/// channel, so an operator asking `web` gets two rows for one instance.
/// Built by hand: the skip is decided by the crate-internal
/// `ProcessEntry::reload`.
#[tokio::test(start_paused = true)]
async fn a_reload_drainee_is_skipped_and_its_replacement_answers() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox, mut child_rx) = actor_with_an_open_channel(&dir);
    let (replacement_tx, mut replacement_rx) = mpsc::channel(16);
    let new_id = register_sheep(&mut actor, &dir, "web", Some(replacement_tx));
    let drainee = actor.sheep.get_mut(&0).expect("the fixture registers id 0");
    drainee.entry.status = ProcStatus::Stopping;
    drainee.entry.reload = ReloadState::Drainee {
        new_id: Some(new_id),
    };
    actor
        .sheep
        .get_mut(&new_id)
        .expect("the replacement was just registered")
        .entry
        .reload = ReloadState::Replacement;

    let answer = trigger_flock(&mut actor, ProcessSelector::Name("web".to_string()), "gc");
    sent_action(&mut replacement_rx).await;
    actor.handle_action_reply(new_id, "gc", "swept 3".to_string(), None);
    settle_action(&mut actor, &mut mailbox).await;

    assert_eq!(
        triggered(answer).await,
        Ok(vec![
            row(0, "web", ActionOutcome::Skipped),
            row(
                new_id,
                "web",
                ActionOutcome::Replied {
                    body: "swept 3".to_string()
                }
            ),
        ])
    );
    assert!(
        matches!(child_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "an action was delivered to a process that is on its way out"
    );
}

/// Fails if a selector matching nothing is answered with rows rather than
/// an error. Every [`ActionOutcome`] is a statement about a sheep.
#[tokio::test(start_paused = true)]
async fn a_trigger_matching_no_sheep_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![]);

    assert_eq!(
        triggered(trigger_flock(
            &mut actor,
            ProcessSelector::Name("ghost".to_string()),
            "gc"
        ))
        .await,
        Err(SupervisorError::NotFound)
    );
}

/// Fails if a wait armed against a process that then exits is left for its
/// own deadline to end, or dropped without an answer. The debts go with the
/// waits: a replacement under this id has written none of the replies the
/// dead process owed.
#[tokio::test(start_paused = true)]
async fn a_sheep_exiting_answers_every_action_waiting_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, mut mailbox, mut child_rx) = actor_with_an_open_channel(&dir);

    // One wait that will be left waiting, and one debt for a wait that
    // already gave up: the two halves the exit has to clear.
    let timed_out = trigger_action(&mut actor, "gc");
    sent_action(&mut child_rx).await;
    settle_action(&mut actor, &mut mailbox).await;
    assert_eq!(timed_out.await.unwrap(), ActionOutcome::TimedOut);

    let waiting = trigger_action(&mut actor, "stats");
    sent_action(&mut child_rx).await;

    actor.handle_exited(
        0,
        ExitOutcome {
            code: Some(0),
            signal: None,
        },
    );
    // Bounded: nothing else will answer a wait the exit failed to.
    let answered = tokio::time::timeout(ACTION_WINDOW, waiting)
        .await
        .expect("a wait outlived the process it was waiting on")
        .unwrap();
    assert_eq!(
        answered,
        ActionOutcome::NoChannel,
        "a wait outlived the process it was waiting on"
    );
    assert!(
        actor.sheep[&0].actions.abandoned.is_empty(),
        "a debt owed by a process that has exited outlived it, and would \
         have swallowed a reply from whatever runs under this id next"
    );
}
