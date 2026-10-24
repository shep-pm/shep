//! `shep enable` and `shep disable` on a name nothing answers to: the exit
//! code and the dogs the refusal lists.

use super::*;
use crate::commands::lifecycle::toggle::{Toggle, naming_the_dogs, toggle, toggle_sheep};

async fn toggle_with_dogs(
    client: &Client,
    verb: Toggle,
    name: &str,
    dogs: &[String],
) -> (ExitCode, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        toggle_sheep(client, &mut streams, verb, name, dogs).await
    };
    (code, String::from_utf8(err).unwrap())
}

fn dogs(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// fails if the dogs stop being quoted and joined as `enable`'s old refusal
/// printed them.
#[test]
fn the_dogs_follow_the_shepherds_words_as_an_english_list() {
    assert_eq!(
        naming_the_dogs("no match", &dogs(&["metrics", "bark"])),
        r#"no match; the dogs are "metrics" and "bark""#
    );
}

/// fails if a mistyped dog name exits 4 again, or exits 3 with nothing to
/// say which names would have worked.
#[tokio::test]
async fn an_unknown_name_exits_not_found_and_lists_the_dogs() {
    // The fake daemon scripts one reply, so each verb gets its own.
    for verb in [Toggle::Enable, Toggle::Disable] {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_client_replying_err(
            &path,
            RpcErrorCode::NotFound,
            "selector matched no registered sheep",
        )
        .await;
        let (code, said) =
            toggle_with_dogs(&client, verb, "metrcs", &dogs(&["metrics", "bark", "otel"])).await;

        assert_eq!(code, ExitCode::NotFound, "{said}");
        assert!(
            said.contains("selector matched no registered sheep"),
            "{said}"
        );
        assert!(
            said.contains(r#"the dogs are "metrics", "bark", and "otel""#),
            "{said}"
        );
    }
}

/// fails if a sweep that reaches nothing blames a dog name nobody typed.
#[tokio::test]
async fn a_sweep_that_matches_nothing_does_not_list_the_dogs() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _daemon) =
        fake_client_replying_err(&path, RpcErrorCode::NotFound, "no match").await;

    let (code, said) =
        toggle_with_dogs(&client, Toggle::Enable, "/^web/", &dogs(&["metrics"])).await;

    assert_eq!(code, ExitCode::NotFound, "{said}");
    assert!(!said.contains("dogs"), "{said}");
}

/// fails if a typo with no shepherd stops exiting 5, the code a script
/// reads as "start one first".
#[tokio::test]
async fn an_unknown_name_with_no_shepherd_exits_unreachable() {
    let home = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, home.path());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        toggle(&mut streams, &paths, Toggle::Enable, "metrcs", true).await
    };

    assert_eq!(code, ExitCode::DaemonUnreachable);
    assert!(out.is_empty());
}
