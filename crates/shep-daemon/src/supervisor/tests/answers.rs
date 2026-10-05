//! Tests for answering a sheep's question: delivery, the first answer
//! winning, and every refusal.

use shep_core::protocol::{
    Answer, OpenQuestion, QuestionError, QuestionText, SelectorSpec, Settled, Takes,
};

use super::questions::{
    answer_to, answer_web, ask, channelled, next_settled, open_ids, open_ids_become, question_id,
    web_asking,
};
use super::*;

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

/// Two instances holding one id refuse an answer by name and name both ids,
/// never answering the first one found.
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
    let mut first = runner.io_handles(0);
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

    // One holder left under the name, so the name now reaches it.
    assert_eq!(
        answer_web(&handle, "q1", "no").await,
        Ok((0, "web".to_string()))
    );
    assert_eq!(
        sent_action(&mut first.to_child_rx).await,
        ShepherdMessage::Answer(Answer::new(question_id("q1"), "no"))
    );
}

/// Instance 0 never asked, so it is the first channelled match and
/// remembers nothing; the refusal must come from instance 1.
#[tokio::test(start_paused = true)]
async fn a_late_answer_by_name_recalls_the_instance_that_settled_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = channelled("web");
    app.instances = 2;
    let (handle, runner, _events) = started(
        &dir,
        app,
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
    )
    .await;
    let _first = runner.io_handles(0);
    let second = runner.io_handles(1);
    second
        .from_child_tx
        .send(ask("q2", "Proceed?", Takes::YesNo))
        .await
        .unwrap();
    open_ids_become(&handle, 1, &["q2"]).await;
    let by_name = ProcessSelector::Name("web".to_string());
    handle
        .answer(
            by_name,
            "q2".into(),
            "yes".into(),
            None,
            Some("ntfy".into()),
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        answer_web(&handle, "q2", "no").await,
        Err(SupervisorError::QuestionNotOpen(
            "question q2 on web was already answered via ntfy".to_string()
        ))
    );
}

/// An app that is not reading its channel keeps the question open. The
/// channel here is built full and left unread.
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
            "web is not reading its shepherd channel, so the answer was not delivered; \
             question wedged is still open"
                .to_string()
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
            "web has no open shepherd channel, so its questions cannot be answered".to_string()
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
