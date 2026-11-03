//! `shep copy` of a dog-wide key onto a sheep is refused before any write.

use serde_json::json;
use shep_core::dogs::manifest::Serves;
use shep_core::paths::ShepPaths;
use shep_core::protocol::Request;

use crate::cli::{CopyArgs, Format};
use crate::commands::standard_dog_testing::{adopt_jobs, shepherd};
use crate::exit::ExitCode;
use crate::output::Streams;

async fn copy(from: &str, to: &str, except: &[&str]) -> (ExitCode, String, Vec<Request>) {
    let dir = tempfile::tempdir().unwrap();
    adopt_jobs(dir.path(), Serves::OptedIn);
    let paths = ShepPaths::at(dir.path().to_path_buf());
    let own = json!({ "poll": "5m", "model": "sonnet" });
    let (client, mut envelopes) = shepherd(dir.path(), Some(own), "").await;
    let args = CopyArgs {
        from: from.to_string(),
        to: to.to_string(),
        replace: false,
        except: except.iter().map(ToString::to_string).collect(),
        dry_run: false,
        yes: true,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        super::copy(&client, &mut streams, &paths, &args).await
    };
    let mut sent = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        sent.push(envelope.body);
    }
    (code, String::from_utf8_lossy(&err).to_string(), sent)
}

fn wrote(sent: &[Request]) -> bool {
    sent.iter()
        .any(|request| matches!(request, Request::SetSheepDogSettings { .. }))
}

#[tokio::test]
async fn a_table_holding_a_dog_wide_key_is_refused_and_names_the_dog_level_address() {
    let (code, said, sent) = copy("api.dogs.jobs", "web.dogs.jobs", &[]).await;

    assert_eq!(code, ExitCode::InvalidConfig, "{said}");
    assert!(said.contains("shep set jobs.poll <value>"), "{said}");
    assert!(!wrote(&sent), "a write went out: {sent:?}");
}

#[tokio::test]
async fn a_dog_wide_key_named_by_the_address_is_refused() {
    let (code, said, sent) = copy("api.dogs.jobs.poll", "web.dogs.jobs.poll", &[]).await;

    assert_eq!(code, ExitCode::InvalidConfig, "{said}");
    assert!(!wrote(&sent), "a write went out: {sent:?}");
}

#[tokio::test]
async fn a_dog_wide_key_left_out_by_except_does_not_stop_the_copy() {
    let (code, said, _) = copy("api.dogs.jobs", "web.dogs.jobs", &["poll"]).await;

    assert!(!said.contains("dog-wide"), "{said}");
    assert_ne!(code, ExitCode::InvalidConfig, "{said}");
}
