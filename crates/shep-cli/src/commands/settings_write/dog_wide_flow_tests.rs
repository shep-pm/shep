//! `shep set` of a dog-wide key on a sheep is refused before the write.

use serde_json::json;
use shep_core::dogs::manifest::Serves;
use shep_core::paths::ShepPaths;
use shep_core::protocol::Request;

use crate::cli::{Format, SetArgs};
use crate::commands::standard_dog_testing::{adopt_jobs, shepherd};
use crate::exit::ExitCode;
use crate::output::Streams;

async fn set(address: &str, value: &str) -> (ExitCode, String, Vec<Request>) {
    let dir = tempfile::tempdir().unwrap();
    adopt_jobs(dir.path(), Serves::OptedIn);
    let paths = ShepPaths::at(dir.path().to_path_buf());
    let (client, mut envelopes) =
        shepherd(dir.path(), Some(json!({ "model": "sonnet" })), "").await;
    let args = SetArgs {
        address: address.to_string(),
        value: value.to_string(),
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
        super::set(&client, &mut streams, &paths, &args).await
    };
    let mut sent = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        sent.push(envelope.body);
    }
    (code, String::from_utf8_lossy(&err).to_string(), sent)
}

#[tokio::test]
async fn a_dog_wide_key_on_a_sheep_is_refused_before_a_write_is_sent() {
    let (code, said, sent) = set("api.dogs.jobs.poll", "5m").await;

    assert_eq!(code, ExitCode::InvalidConfig, "{said} {sent:?}");
    assert!(said.contains("shep set jobs.poll <value>"), "{said}");
    assert!(
        !sent
            .iter()
            .any(|request| matches!(request, Request::SetSheepDogSettings { .. })),
        "a write went out: {sent:?}"
    );
}

#[tokio::test]
async fn a_key_the_sheep_may_override_is_written() {
    let (code, said, sent) = set("api.dogs.jobs.model", "haiku").await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(
        sent.iter()
            .any(|request| matches!(request, Request::SetSheepDogSettings { .. })),
        "{sent:?}"
    );
}
