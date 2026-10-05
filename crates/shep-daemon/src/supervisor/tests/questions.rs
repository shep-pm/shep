//! Tests for a sheep's questions: asking, answering, withdrawing, and what
//! an open question becomes when its process goes.

use shep_core::protocol::{
    Answer, OpenQuestion, QuestionError, QuestionId, QuestionText, SelectorSpec, Settled, Takes,
};

use super::*;

/// A sheep named `name` whose shepherd channel is open.
fn channelled(name: &str) -> AppConfig {
    let mut app = AppConfig::minimal(name, "./srv");
    app.channel = true;
    app
}

fn question_id(raw: &str) -> QuestionId {
    QuestionId::new(raw).unwrap()
}

fn ask(question: &str, text: &str, takes: Takes) -> ChildMessage {
    ChildMessage::Ask {
        question: question_id(question),
        text: QuestionText::new(text).unwrap(),
        takes,
    }
}

/// The ids of `id`'s open questions, in the order first asked.
async fn open_ids(handle: &SupervisorHandle, id: u32) -> Vec<String> {
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
async fn open_ids_become(handle: &SupervisorHandle, id: u32, want: &[&str]) {
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
async fn next_settled(
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
async fn answer_to(
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
async fn answer_web(
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
async fn web_asking(
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
async fn an_answer_reaches_the_childs_channel_and_publishes_answered() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, mut io, mut events) = web_asking(&dir, "merge-12", Takes::YesNo).await;

    let answered = handle
        .answer(
            ProcessSelector::Name("web".to_string()),
            "merge-12".to_string(),
            "no".to_string(),
            Some("rebase first".to_string()),
            Some("discord".to_string()),
            Some("<@81234>".to_string()),
        )
        .await;

    assert_eq!(answered, Ok((0, "web".to_string())));
    assert_eq!(
        sent_action(&mut io.to_child_rx).await,
        ShepherdMessage::Answer(
            Answer::new(question_id("merge-12"), "no")
                .with_note("rebase first")
                .with_via("discord")
                .with_who("<@81234>")
        )
    );
    assert_eq!(
        next_settled(&mut events).await,
        (
            0,
            "web".to_string(),
            question_id("merge-12"),
            Settled::Answered {
                via: Some("discord".to_string()),
                who: Some("<@81234>".to_string()),
            }
        )
    );
    assert!(open_ids(&handle, 0).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_second_answer_is_not_found_and_names_how_the_first_came() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _io, _events) = web_asking(&dir, "rotate", Takes::YesNo).await;
    handle
        .answer(
            ProcessSelector::Name("web".to_string()),
            "rotate".to_string(),
            "yes".to_string(),
            None,
            Some("ntfy".to_string()),
            Some("ada".to_string()),
        )
        .await
        .unwrap();

    assert_eq!(
        answer_web(&handle, "rotate", "no").await,
        Err(SupervisorError::QuestionNotOpen(
            "question rotate on web was already answered via ntfy by ada".to_string()
        ))
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
            "web has no shepherd channel, so it has no questions".to_string()
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

/// Review Focus 3: two instances holding one id refuse an answer by name and
/// name both ids, never answering the first one found.
#[tokio::test(start_paused = true)]
async fn two_instances_holding_one_question_refuse_by_name_and_answer_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = channelled("web");
    app.instances = 2;
    let (handle, runner, _events) = started(
        &dir,
        app,
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;
    let first = runner.io_handles(0);
    let mut second = runner.io_handles(1);
    for io in [&first, &second] {
        io.from_child_tx
            .send(ask("q1", "Proceed?", Takes::YesNo))
            .await
            .unwrap();
    }
    open_ids_become(&handle, 0, &["q1"]).await;
    open_ids_become(&handle, 1, &["q1"]).await;

    assert_eq!(
        answer_web(&handle, "q1", "yes").await,
        Err(SupervisorError::InvalidAnswer(
            "question q1 is open on more than one sheep (ids 0, 1); answer by id".to_string()
        ))
    );
    assert_eq!(open_ids(&handle, 0).await, ["q1"]);
    assert_eq!(open_ids(&handle, 1).await, ["q1"]);

    let by_id = answer_to(&handle, ProcessSelector::Id(1), "q1", "yes").await;
    assert_eq!(by_id, Ok((1, "web".to_string())));
    assert_eq!(
        sent_action(&mut second.to_child_rx).await,
        ShepherdMessage::Answer(Answer::new(question_id("q1"), "yes"))
    );
    assert_eq!(open_ids(&handle, 0).await, ["q1"]);
    assert!(open_ids(&handle, 1).await.is_empty());
}

/// A channel that cannot take the answer means the process is going: the
/// question stays open for the exit to close as `gone`. The channel here is
/// built full and left unread.
#[test]
fn an_answer_the_channel_cannot_take_leaves_the_question_open() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _mailbox) = actor_with_one_online_sheep(&dir, vec![]);
    let (to_child, _wedged) = mpsc::channel(1);
    to_child.try_send(ShepherdMessage::Shutdown).unwrap();
    actor
        .sheep
        .get_mut(&0)
        .expect("the fixture registers id 0")
        .to_child = Some(to_child);
    let question = OpenQuestion::new(
        question_id("wedged"),
        QuestionText::new("Still there?").unwrap(),
        Takes::YesNo,
        0,
    );
    // 1111 is the pid the fixture's entry was armed with.
    actor.handle_ask(0, 1111, question);

    assert_eq!(
        actor.handle_answer(
            &ProcessSelector::Id(0),
            "wedged",
            "yes".to_string(),
            None,
            None,
            None,
        ),
        Err(SupervisorError::QuestionNotOpen(
            "web is exiting, so question wedged was not delivered".to_string()
        ))
    );
    assert!(actor.sheep[&0].questions.holds("wedged"));
}

#[tokio::test(start_paused = true)]
async fn an_answer_to_no_sheep_or_to_one_with_no_channel_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _runner, _events) = started(
        &dir,
        AppConfig::minimal("web", "./srv"),
        vec![ProcScript::never_exits()],
    )
    .await;

    let api = ProcessSelector::Name("api".to_string());
    assert_eq!(
        answer_to(&handle, api, "q1", "yes").await,
        Err(SupervisorError::NotFound)
    );
    assert_eq!(
        answer_web(&handle, "q1", "yes").await,
        Err(SupervisorError::QuestionNotOpen(
            "web has no shepherd channel, so it has no questions".to_string()
        ))
    );
}

#[tokio::test(start_paused = true)]
async fn an_answer_a_yes_no_question_does_not_take_is_refused_and_it_stays_open() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _io, _events) = web_asking(&dir, "restart", Takes::YesNo).await;

    assert_eq!(
        answer_web(&handle, "restart", "maybe").await,
        Err(SupervisorError::InvalidAnswer(
            QuestionError::NotYesOrNo {
                found: "maybe".to_string()
            }
            .to_string()
        ))
    );
    assert_eq!(open_ids(&handle, 0).await, ["restart"]);
}

#[tokio::test(start_paused = true)]
async fn a_selector_that_is_not_one_sheep_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, _runner, _events) =
        started(&dir, channelled("web"), vec![ProcScript::never_exits()]).await;

    for selector in [
        ProcessSelector::All,
        ProcessSelector::try_from(SelectorSpec::Regex("^web$".to_string())).unwrap(),
        ProcessSelector::Fold("api".to_string()),
    ] {
        let refused = answer_to(&handle, selector, "q1", "yes").await;
        assert_eq!(
            refused,
            Err(SupervisorError::InvalidAnswer(
                "an answer goes to one sheep: name it by id, name or name:slot, \
                 not all, a pattern or a fold"
                    .to_string()
            ))
        );
    }
}
