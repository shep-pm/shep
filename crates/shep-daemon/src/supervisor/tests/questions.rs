//! Tests for a sheep's questions: asking and withdrawing on the channel,
//! and what an open question becomes when its process goes. The fixtures
//! here are shared with `answers.rs`.

use shep_core::protocol::{OpenQuestion, QuestionId, QuestionText, Settled, Takes};

use super::*;

/// A sheep named `name` whose shepherd channel is open.
pub(super) fn channelled(name: &str) -> AppConfig {
    let mut app = AppConfig::minimal(name, "./srv");
    app.channel = true;
    app
}

pub(super) fn question_id(raw: &str) -> QuestionId {
    QuestionId::new(raw).unwrap()
}

pub(super) fn ask(question: &str, text: &str, takes: Takes) -> ChildMessage {
    ChildMessage::Ask {
        question: question_id(question),
        text: QuestionText::new(text).unwrap(),
        takes,
    }
}

/// The ids of `id`'s open questions, in the order first asked.
pub(super) async fn open_ids(handle: &SupervisorHandle, id: u32) -> Vec<String> {
    let listing = handle.list().await;
    let row = listing
        .iter()
        .find(|info| info.id == id)
        .expect("the sheep is registered");
    row.questions
        .iter()
        .flatten()
        .map(|q| q.question.as_str().to_string())
        .collect()
}

/// Waits for `id`'s open questions to read `want`, failing after
/// [`ACTION_WINDOW`]. A message from the channel is two task hops from the
/// listing, and the paused clock advances on each idle sleep.
pub(super) async fn open_ids_become(handle: &SupervisorHandle, id: u32, want: &[&str]) {
    let reached = tokio::time::timeout(ACTION_WINDOW, async {
        loop {
            if open_ids(handle, id).await == want {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        reached.is_ok(),
        "sheep {id}'s open questions never became {want:?}; last seen {:?}",
        open_ids(handle, id).await
    );
}

/// The next `question.settled` on the bus, failing after [`ACTION_WINDOW`].
pub(super) async fn next_settled(
    events: &mut tokio::sync::broadcast::Receiver<SharedEvent>,
) -> (u32, String, QuestionId, Settled) {
    tokio::time::timeout(ACTION_WINDOW, async {
        loop {
            if let BusEvent::QuestionSettled {
                id,
                name,
                question,
                settled,
                ..
            } = events.recv().await.unwrap().to_event()
            {
                break (id, name, question, settled);
            }
        }
    })
    .await
    .expect("no question.settled within the window")
}

/// Answers `question` on `selector` with `answer`, carrying no note, `via`
/// or `who`.
pub(super) async fn answer_to(
    handle: &SupervisorHandle,
    selector: ProcessSelector,
    question: &str,
    answer: &str,
) -> Result<(u32, String), SupervisorError> {
    handle
        .answer(
            selector,
            question.to_string(),
            answer.to_string(),
            None,
            None,
            None,
        )
        .await
}

/// [`answer_to`] the sheep named `web`, as `shep answer web` does.
pub(super) async fn answer_web(
    handle: &SupervisorHandle,
    question: &str,
    answer: &str,
) -> Result<(u32, String), SupervisorError> {
    answer_to(
        handle,
        ProcessSelector::Name("web".to_string()),
        question,
        answer,
    )
    .await
}

/// `web` started with its channel open, having asked `question`, which the
/// listing already shows.
pub(super) async fn web_asking(
    dir: &tempfile::TempDir,
    question: &str,
    takes: Takes,
) -> (
    SupervisorHandle,
    crate::fake::FakeIo,
    tokio::sync::broadcast::Receiver<SharedEvent>,
) {
    let (handle, runner, events) =
        started(dir, channelled("web"), vec![ProcScript::never_exits()]).await;
    let io = runner.io_handles(0);
    io.from_child_tx
        .send(ask(question, "Proceed?", takes))
        .await
        .unwrap();
    open_ids_become(&handle, 0, &[question]).await;
    (handle, io, events)
}

#[tokio::test(start_paused = true)]
async fn an_ask_on_the_channel_reaches_the_listing_stamped_when_it_arrived() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, runner, _events) =
        started(&dir, channelled("web"), vec![ProcScript::never_exits()]).await;
    let io = runner.io_handles(0);

    let before = crate::now_ms();
    io.from_child_tx
        .send(ask("deploy", "Ship it?", Takes::YesNo))
        .await
        .unwrap();
    open_ids_become(&handle, 0, &["deploy"]).await;
    let after = crate::now_ms();

    let listing = handle.list().await;
    let open = &listing[0].questions.as_ref().unwrap()[0];
    assert_eq!(open.text.as_str(), "Ship it?");
    assert_eq!(open.takes, Takes::YesNo);
    assert!(
        (before..=after).contains(&open.asked_at_ms),
        "asked_at_ms {} is not the moment the ask arrived ({before}..={after})",
        open.asked_at_ms
    );
}

#[tokio::test(start_paused = true)]
async fn a_withdraw_empties_the_listing_and_publishes_withdrawn() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, io, mut events) = web_asking(&dir, "cleanup", Takes::YesNo).await;

    io.from_child_tx
        .send(ChildMessage::Withdraw {
            question: question_id("cleanup"),
        })
        .await
        .unwrap();

    assert_eq!(
        next_settled(&mut events).await,
        (
            0,
            "web".to_string(),
            question_id("cleanup"),
            Settled::Withdrawn
        )
    );
    assert!(open_ids(&handle, 0).await.is_empty());
    assert_eq!(
        answer_web(&handle, "cleanup", "yes").await,
        Err(SupervisorError::QuestionNotOpen(
            "web withdrew question cleanup".to_string()
        ))
    );
}

/// The question must leave the listing, publish `gone`, and refuse a late
/// answer rather than write to a dead channel.
#[tokio::test(start_paused = true)]
async fn a_process_that_exits_settles_its_open_questions_as_gone() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, runner, mut events) =
        started(&dir, channelled("web"), vec![ProcScript::never_exits()]).await;
    let io = runner.io_handles(0);
    io.from_child_tx
        .send(ask("first", "One?", Takes::YesNo))
        .await
        .unwrap();
    io.from_child_tx
        .send(ask("second", "Two?", Takes::Text))
        .await
        .unwrap();
    open_ids_become(&handle, 0, &["first", "second"]).await;

    tokio::time::timeout(
        ACTION_WINDOW,
        handle.stop(ProcessSelector::Name("web".to_string())),
    )
    .await
    .expect("the stop never answered")
    .unwrap();

    let gone = (
        next_settled(&mut events).await,
        next_settled(&mut events).await,
    );
    assert_eq!(
        gone,
        (
            (0, "web".to_string(), question_id("first"), Settled::Gone),
            (0, "web".to_string(), question_id("second"), Settled::Gone),
        )
    );
    assert!(open_ids(&handle, 0).await.is_empty());
    assert_eq!(
        answer_web(&handle, "first", "yes").await,
        Err(SupervisorError::QuestionNotOpen(
            "web is not running, so it has no open questions".to_string()
        ))
    );
}

/// An `ask` the previous process sent, reaching the actor after the slot
/// respawned, belongs to nobody now running.
#[tokio::test(start_paused = true)]
async fn an_ask_from_a_process_the_slot_has_replaced_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, runner, _events) = started(
        &dir,
        channelled("web"),
        vec![ProcScript::const_exit(1), ProcScript::never_exits()],
    )
    .await;
    let respawned = tokio::time::timeout(ACTION_WINDOW, async {
        loop {
            if handle.list().await[0].pid == Some(crate::fake::FIRST_SCRIPTED_PID + 1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(respawned.is_ok(), "the sheep never respawned");
    let io = runner.io_handles(1);

    handle
        .tx
        .send(Msg::Ask {
            id: 0,
            root_pid: crate::fake::FIRST_SCRIPTED_PID,
            question: OpenQuestion::new(
                question_id("stale"),
                QuestionText::new("From the old process?").unwrap(),
                Takes::YesNo,
                0,
            ),
        })
        .await
        .unwrap();
    // Sent after the stale one and two hops further away, so once it is
    // listed the stale one has been handled.
    io.from_child_tx
        .send(ask("fresh", "From the new one?", Takes::YesNo))
        .await
        .unwrap();

    open_ids_become(&handle, 0, &["fresh"]).await;
}
