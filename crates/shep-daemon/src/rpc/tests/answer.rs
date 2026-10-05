//! `Answer`: the round trip to `Answered`, and a selector that is not one
//! sheep.

use std::sync::Arc;

use shep_core::protocol::{QuestionId, QuestionText, Takes};

use super::*;
use crate::channel::{ChildMessage, ShepherdMessage};
use crate::fake::ScriptedRunner;
use crate::supervisor::spawn_supervisor;
use crate::testing::SharedRunner;

/// The harness keeps no handle on its runner, so the case swaps in a
/// supervisor over one it keeps: the ask has to come from the child's end.
#[tokio::test(start_paused = true)]
async fn an_answer_round_trips_to_answered_and_reaches_the_child() {
    let mut h = harness(vec![]);
    let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits()]));
    h.ctx.supervisor = spawn_supervisor(
        SharedRunner(Arc::clone(&runner)),
        h.ctx.paths.clone(),
        h.ctx.events.clone(),
    );
    let mut web = AppConfig::minimal("web", "./srv");
    web.channel = true;
    let started = reply_of(dispatch(envelope(1, Request::Start { apps: vec![web] }), &h.ctx).await);
    assert!(started.result.is_ok(), "{:?}", started.result);
    let mut io = runner.io_handles(0);
    io.from_child_tx
        .send(ChildMessage::Ask {
            question: QuestionId::new("ship").unwrap(),
            text: QuestionText::new("Ship it?").unwrap(),
            takes: Takes::YesNo,
        })
        .await
        .unwrap();
    // Two task hops from the listing; the paused clock moves on each sleep.
    let listed = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if list_flock(&h.ctx, 2).await[0].questions.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(listed.is_ok(), "the question never reached the listing");

    let reply = reply_of(
        dispatch(
            envelope(
                3,
                Request::Answer {
                    selector: SelectorSpec::Name("web".to_string()),
                    question: "ship".to_string(),
                    answer: "yes".to_string(),
                    note: None,
                    via: Some("cli".to_string()),
                    who: None,
                },
            ),
            &h.ctx,
        )
        .await,
    );

    assert_eq!(
        reply.result,
        Ok(Response::Answered {
            id: 0,
            name: "web".to_string(),
            question: "ship".to_string(),
        })
    );
    let sent = tokio::time::timeout(Duration::from_secs(120), io.to_child_rx.recv())
        .await
        .expect("nothing reached the child's end of the channel");
    assert_eq!(
        sent,
        Some(ShepherdMessage::Answer(
            shep_core::protocol::Answer::new(QuestionId::new("ship").unwrap(), "yes")
                .with_via("cli")
        ))
    );
}

#[tokio::test(start_paused = true)]
async fn an_answer_to_every_sheep_is_refused_as_invalid_config() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web(&h).await;

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Answer {
                    selector: SelectorSpec::All,
                    question: "ship".to_string(),
                    answer: "yes".to_string(),
                    note: None,
                    via: None,
                    who: None,
                },
            ),
            &h.ctx,
        )
        .await,
    );

    let err = reply.result.unwrap_err();
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(err.message.contains("one sheep"), "{}", err.message);
}
