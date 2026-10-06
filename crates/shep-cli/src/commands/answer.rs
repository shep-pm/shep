//! `answer`: list the open questions in the flock, or answer one.
//!
//! Bare, it lists every open question in the flock. With a sheep, a question
//! id and the answer, it sends the answer to the shepherd, which decides
//! whether the sheep and question exist and whether the answer fits. Its
//! refusals carry the wording the operator reads, so they are reported as
//! they came.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use shep_client::Client;
use shep_core::protocol::{ProcessInfo, Request, Response, SelectorSpec, Takes};
use shep_core::selector::ProcessSelector;

use crate::cli::{AnswerArgs, Format};
use crate::commands::rpc::{client_error, request_payload, unexpected_response};
use crate::commands::selector::parse_selector;
use crate::exit::ExitCode;
use crate::output::{OutputEnvelope, QuestionRows, SCHEMA_VERSION, Streams, write_outcome};

/// What `shep answer` prints when nothing is open.
const NOTHING_WAITING: &str = "no sheep is waiting on an answer";

/// Lists the open questions when `args` names no sheep, and otherwise answers
/// the one `args` names.
pub async fn answer(client: &Client, streams: &mut Streams<'_>, args: &AnswerArgs) -> ExitCode {
    match (&args.sheep, &args.question) {
        (Some(sheep), Some(question)) => {
            let selector = match parse_selector(streams, sheep) {
                Ok(selector) => selector,
                Err(code) => return code,
            };
            submit(client, streams, &selector, question, &args.words).await
        }
        _ => list(client, streams).await,
    }
}

async fn list(client: &Client, streams: &mut Streams<'_>) -> ExitCode {
    let flock = match request_payload(client, streams, Request::ListFlock, None, flock_of).await {
        Ok(flock) => flock,
        Err(code) => return code,
    };
    let rows = QuestionRows::from_flock(&flock, now_ms());
    if rows.0.is_empty() && streams.fmt == Format::Table {
        return write_outcome(writeln!(streams.out, "{NOTHING_WAITING}"));
    }
    write_outcome(crate::output::emit(
        &mut *streams.out,
        streams.fmt,
        "answer",
        rows,
        streams.style,
    ))
}

/// Learns what the question takes, splits `words` by it, and sends the
/// answer.
///
/// A question the listing does not show is sent anyway: the shepherd's
/// refusal says how it closed, which this side cannot.
async fn submit(
    client: &Client,
    streams: &mut Streams<'_>,
    selector: &ProcessSelector,
    question: &str,
    words: &[String],
) -> ExitCode {
    let flock = match request_payload(client, streams, Request::ListFlock, None, flock_of).await {
        Ok(flock) => flock,
        Err(code) => return code,
    };
    let takes = takes_of(&flock, selector, question);
    let (answer, note) = split_words(takes, words);
    send_answer(
        client,
        streams,
        SelectorSpec::from(selector),
        question,
        answer,
        note,
    )
    .await
}

fn flock_of(response: Response) -> Option<Vec<ProcessInfo>> {
    match response {
        Response::Flock(flock) => Some(flock),
        _ => None,
    }
}

/// What `question` takes on the first sheep `selector` matches that has it
/// open.
fn takes_of(flock: &[ProcessInfo], selector: &ProcessSelector, question: &str) -> Option<Takes> {
    flock
        .iter()
        .filter(|sheep| {
            selector.matches(&sheep.name, sheep.id, sheep.fold.as_deref(), sheep.instance)
        })
        .filter_map(|sheep| sheep.questions.as_deref())
        .flatten()
        .find(|open| open.question.as_str() == question)
        .map(|open| open.takes)
}

/// Splits what the operator typed into the answer and its note.
///
/// A yes-no question takes its first word as the answer and the rest as the
/// note. A text question, a kind this build does not know and a question that
/// was not found take every word as the answer.
fn split_words(takes: Option<Takes>, words: &[String]) -> (String, Option<String>) {
    match (takes, words.split_first()) {
        (Some(Takes::YesNo), Some((first, rest))) => {
            let note = (!rest.is_empty()).then(|| rest.join(" "));
            (first.clone(), note)
        }
        _ => (words.join(" "), None),
    }
}

/// What a delivered answer reports, as `--format json` carries it.
#[derive(Debug, Serialize)]
struct Answered {
    id: u32,
    name: String,
    question: String,
}

async fn send_answer(
    client: &Client,
    streams: &mut Streams<'_>,
    selector: SelectorSpec,
    question: &str,
    answer: String,
    note: Option<String>,
) -> ExitCode {
    let body = Request::Answer {
        selector,
        question: question.to_string(),
        answer,
        note,
        via: None,
        who: None,
    };
    let delivered = match client.request(body).await {
        Ok(Response::Answered { id, name, question }) => Answered { id, name, question },
        Ok(_unrecognised) => return unexpected_response(streams),
        Err(err) => return client_error(streams, &err),
    };
    let written = match streams.fmt {
        Format::Json => {
            let envelope = OutputEnvelope {
                schema_version: SCHEMA_VERSION,
                command: "answer",
                data: &delivered,
            };
            serde_json::to_writer(&mut *streams.out, &envelope)
                .map_err(std::io::Error::from)
                .and_then(|()| writeln!(streams.out))
        }
        Format::Table => writeln!(
            streams.out,
            "answered {} on {}",
            delivered.question, delivered.name
        ),
    };
    write_outcome(written)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use shep_client::testing::{fake_client_answering, fake_client_replying_err};
    use shep_core::protocol::{OpenQuestion, QuestionId, QuestionText, RpcErrorCode};
    use shep_core::status::ProcStatus;

    use super::*;

    fn words(all: &[&str]) -> Vec<String> {
        all.iter().map(|word| (*word).to_string()).collect()
    }

    #[test]
    fn a_yes_no_answer_alone_has_no_note() {
        assert_eq!(
            split_words(Some(Takes::YesNo), &words(&["yes"])),
            ("yes".to_string(), None)
        );
    }

    #[test]
    fn a_yes_no_answer_takes_the_rest_as_its_note_however_it_was_split() {
        let expected = ("no".to_string(), Some("rebase first".to_string()));
        assert_eq!(
            split_words(Some(Takes::YesNo), &words(&["no", "rebase", "first"])),
            expected
        );
        assert_eq!(
            split_words(Some(Takes::YesNo), &words(&["no", "rebase first"])),
            expected
        );
    }

    #[test]
    fn a_text_answer_joins_every_word_and_has_no_note() {
        assert_eq!(
            split_words(Some(Takes::Text), &words(&["no", "rebase", "first"])),
            ("no rebase first".to_string(), None)
        );
        assert_eq!(
            split_words(Some(Takes::Text), &words(&["two words"])),
            ("two words".to_string(), None)
        );
    }

    #[test]
    fn an_unknown_question_joins_every_word() {
        assert_eq!(
            split_words(None, &words(&["no", "rebase", "first"])),
            ("no rebase first".to_string(), None)
        );
    }

    fn asking(takes: Takes) -> ProcessInfo {
        ProcessInfo::builder(7, "web", ProcStatus::Online)
            .questions(Some(vec![OpenQuestion::new(
                QuestionId::new("q1").unwrap(),
                QuestionText::new("Ship it?").unwrap(),
                takes,
                1_000,
            )]))
            .build()
    }

    fn args(sheep: Option<&str>, question: Option<&str>, rest: &[&str]) -> AnswerArgs {
        AnswerArgs {
            sheep: sheep.map(str::to_string),
            question: question.map(str::to_string),
            words: words(rest),
        }
    }

    /// Runs the verb against a fake shepherd that holds `flock` and accepts
    /// every answer, and hands back what the client put on the wire.
    async fn run(
        flock: Vec<ProcessInfo>,
        args: &AnswerArgs,
        fmt: Format,
    ) -> (ExitCode, String, Vec<shep_core::protocol::Request>) {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_answering(&path, move |request| match request {
            Request::ListFlock => Response::Flock(flock.clone()),
            Request::Answer { question, .. } => Response::Answered {
                id: 7,
                name: "web".to_string(),
                question: question.clone(),
            },
            _ => Response::Pong,
        })
        .await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt,
            };
            answer(&client, &mut streams, args).await
        };
        let mut sent = Vec::new();
        while let Ok(envelope) = envelopes.try_recv() {
            sent.push(envelope.body);
        }
        (code, String::from_utf8(out).unwrap(), sent)
    }

    #[tokio::test]
    async fn the_request_carries_the_selector_question_answer_and_note() {
        let (code, out, sent) = run(
            vec![asking(Takes::YesNo)],
            &args(Some("web"), Some("q1"), &["no", "rebase", "first"]),
            Format::Table,
        )
        .await;
        assert_eq!(code, ExitCode::Success);
        assert_eq!(out, "answered q1 on web\n");
        assert_eq!(
            sent.last(),
            Some(&Request::Answer {
                selector: SelectorSpec::Name("web".to_string()),
                question: "q1".to_string(),
                answer: "no".to_string(),
                note: Some("rebase first".to_string()),
                via: None,
                who: None,
            })
        );
    }

    #[tokio::test]
    async fn a_question_the_listing_lacks_is_still_sent_with_every_word_as_the_answer() {
        let (code, _out, sent) = run(
            Vec::new(),
            &args(Some("web"), Some("gone"), &["no", "thanks"]),
            Format::Table,
        )
        .await;
        assert_eq!(code, ExitCode::Success);
        assert!(matches!(
            sent.last(),
            Some(Request::Answer { answer, note: None, .. }) if answer == "no thanks"
        ));
    }

    #[tokio::test]
    async fn a_delivered_answer_in_json_carries_id_name_and_question() {
        let (_code, out, _sent) = run(
            vec![asking(Takes::Text)],
            &args(Some("web"), Some("q1"), &["fine"]),
            Format::Json,
        )
        .await;
        let envelope: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(envelope["command"], "answer");
        assert_eq!(
            envelope["data"],
            serde_json::json!({"id": 7, "name": "web", "question": "q1"})
        );
    }

    #[tokio::test]
    async fn no_arguments_lists_the_open_questions() {
        let (code, out, sent) = run(
            vec![asking(Takes::YesNo)],
            &args(None, None, &[]),
            Format::Table,
        )
        .await;
        assert_eq!(code, ExitCode::Success);
        assert_eq!(sent, vec![Request::ListFlock]);
        assert!(out.contains("Ship it?"), "{out}");
    }

    #[tokio::test]
    async fn an_empty_listing_says_so_and_succeeds() {
        let (code, out, _sent) = run(Vec::new(), &args(None, None, &[]), Format::Table).await;
        assert_eq!(code, ExitCode::Success);
        assert_eq!(out, "no sheep is waiting on an answer\n");
    }

    #[tokio::test]
    async fn an_empty_listing_in_json_is_an_empty_array() {
        let (_code, out, _sent) = run(Vec::new(), &args(None, None, &[]), Format::Json).await;
        let envelope: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(envelope["data"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn a_malformed_selector_exits_usage_without_a_round_trip() {
        let (code, _out, sent) = run(
            Vec::new(),
            &args(Some("/[/"), Some("q1"), &["yes"]),
            Format::Table,
        )
        .await;
        assert_eq!(code, ExitCode::Usage);
        assert!(sent.is_empty());
    }

    /// The refusal reaches the answer itself, so this calls `send_answer`:
    /// the scripted refusal answers the first request it sees.
    async fn refused_with(code: RpcErrorCode) -> (ExitCode, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _served) = fake_client_replying_err(&path, code, "refused").await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            send_answer(
                &client,
                &mut streams,
                SelectorSpec::Name("web".to_string()),
                "q1",
                "yes".to_string(),
                None,
            )
            .await
        };
        (exit, err)
    }

    #[tokio::test]
    async fn a_not_found_refusal_exits_3_and_is_worded_by_the_shepherd() {
        let (code, err) = refused_with(RpcErrorCode::NotFound).await;
        assert_eq!(code, ExitCode::NotFound);
        assert!(String::from_utf8_lossy(&err).contains("refused"));
    }

    #[tokio::test]
    async fn an_invalid_config_refusal_exits_4() {
        let (code, _err) = refused_with(RpcErrorCode::InvalidConfig).await;
        assert_eq!(code, ExitCode::InvalidConfig);
    }
}
