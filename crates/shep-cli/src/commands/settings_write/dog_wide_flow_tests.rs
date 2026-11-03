//! `shep set` of a dog-wide key on a sheep is refused before the write.
//!
//! Two reads go out first, the flock and the sheep's config: the address
//! cannot be told apart from a dog's without them.

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

/// The requests among `sent` that change something, by the variant's name.
fn writes(sent: &[Request]) -> Vec<&Request> {
    sent.iter()
        .filter(|request| format!("{request:?}").starts_with("Set"))
        .collect()
}

#[tokio::test]
async fn a_dog_wide_key_on_a_sheep_is_refused_before_a_write_is_sent() {
    let (code, said, sent) = set("api.dogs.jobs.poll", "5m").await;

    assert_eq!(code, ExitCode::InvalidConfig, "{said} {sent:?}");
    assert!(said.contains("shep set jobs.poll <value>"), "{said}");
    assert!(writes(&sent).is_empty(), "a write went out: {sent:?}");
    let names: Vec<String> = sent
        .iter()
        .map(|request| format!("{request:?}"))
        .map(|text| {
            text.split([' ', '{', '('])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(names, ["ListFlock", "SheepConfig"], "{sent:?}");
}

#[tokio::test]
async fn a_key_the_sheep_may_override_is_written_to_its_table_for_that_dog() {
    let (code, said, sent) = set("api.dogs.jobs.model", "haiku").await;

    assert_eq!(code, ExitCode::Success, "{said}");
    let writes = writes(&sent);
    let [Request::SetSheepDogSettings { name, dog, table }] = writes.as_slice() else {
        panic!("expected the one table write: {sent:?}");
    };
    assert_eq!((name.as_str(), dog.as_str()), ("api", "jobs"));
    let table = table.as_ref().expect("a table, not a removal");
    assert_eq!(
        serde_json::Value::Object(table.as_map().clone()),
        json!({ "model": "haiku" })
    );
}
