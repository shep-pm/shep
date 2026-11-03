//! `shep get <sheep>.dogs.<dog>` for a dog that follows the dog settings
//! standard, and for one that does not.

use serde_json::{Value as Json, json};
use shep_core::dogs::manifest::Serves;
use shep_core::paths::ShepPaths;

use crate::cli::{Format, GetArgs};
use crate::commands::standard_dog_testing::{adopt_jobs, shepherd};
use crate::exit::ExitCode;
use crate::output::Streams;

const SECTION: &str = "poll = \"5m\"\nmodel = \"sonnet\"\nlimit = 3\n";

/// What `shep get {address}` printed, and the code, against a flock whose
/// `api` carries `own` as its `jobs` table.
async fn get(
    address: &str,
    fmt: Format,
    serves: Serves,
    own: Option<Json>,
) -> (ExitCode, String, String) {
    get_in(SECTION, address, fmt, serves, own).await
}

/// [`get`] over a `jobs` section of `section`.
async fn get_in(
    section: &str,
    address: &str,
    fmt: Format,
    serves: Serves,
    own: Option<Json>,
) -> (ExitCode, String, String) {
    let dir = tempfile::tempdir().unwrap();
    adopt_jobs(dir.path(), serves);
    let paths = ShepPaths::at(dir.path().to_path_buf());
    let (client, _envelopes) = shepherd(dir.path(), own, section).await;
    let args = GetArgs {
        address: address.to_string(),
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt,
        };
        super::get(&client, &mut streams, &paths, &args).await
    };
    (
        code,
        String::from_utf8_lossy(&out).to_string(),
        String::from_utf8_lossy(&err).to_string(),
    )
}

#[tokio::test]
async fn the_table_output_marks_a_key_the_sheep_leaves_to_the_section() {
    let own = Some(json!({ "limit": 9 }));
    let (code, out, err) = get("api.dogs.jobs", Format::Table, Serves::OptedIn, own).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(
        out,
        "[dogs.jobs]\nlimit = 9\nmodel = \"sonnet\" # inherited from [jobs]\n"
    );
}

#[tokio::test]
async fn the_json_output_lists_the_inherited_keys() {
    let own = Some(json!({ "limit": 9 }));
    let (code, out, err) = get("api.dogs.jobs", Format::Json, Serves::OptedIn, own).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let envelope: Json = serde_json::from_str(&out).unwrap();
    assert_eq!(envelope["data"]["inherited"], json!(["model"]));
    assert_eq!(
        envelope["data"]["toml"],
        "[dogs.jobs]\nlimit = 9\nmodel = \"sonnet\" # inherited from [jobs]\n"
    );
}

#[tokio::test]
async fn a_dog_serving_every_sheep_answers_for_a_sheep_with_no_table() {
    let (code, out, err) = get("api.dogs.jobs", Format::Table, Serves::EverySheep, None).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(out.contains("limit = 3 # inherited from [jobs]"), "{out}");
    assert!(
        !out.contains("poll"),
        "a dog-wide key is not the sheep's: {out}"
    );
}

#[tokio::test]
async fn an_opted_in_dog_reads_a_sheep_with_no_table_as_before() {
    let (code, _out, err) = get("api.dogs.jobs", Format::Table, Serves::OptedIn, None).await;

    assert_eq!(code, ExitCode::NotFound, "{err}");
}

#[tokio::test]
async fn a_key_the_sheep_sets_carries_no_mark() {
    let own = Some(json!({ "limit": 9 }));
    let (code, out, err) = get("api.dogs.jobs.limit", Format::Table, Serves::OptedIn, own).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(!out.contains("inherited"), "{out}");
}

#[tokio::test]
async fn a_key_left_in_a_table_the_sheep_sets_part_of_is_marked_in_both_outputs() {
    let section = "poll = \"5m\"\n[db]\nhost = \"a\"\nport = 1\n";
    let own = || Some(json!({ "db": { "host": "b" } }));

    let (code, out, err) = get_in(
        section,
        "api.dogs.jobs",
        Format::Table,
        Serves::OptedIn,
        own(),
    )
    .await;
    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(out.contains("port = 1 # inherited from [jobs]"), "{out}");
    assert!(!out.contains("host = \"b\" #"), "{out}");

    let (code, out, err) = get_in(
        section,
        "api.dogs.jobs",
        Format::Json,
        Serves::OptedIn,
        own(),
    )
    .await;
    assert_eq!(code, ExitCode::Success, "{err}");
    let envelope: Json = serde_json::from_str(&out).unwrap();
    assert_eq!(envelope["data"]["inherited"], json!(["db.port"]));
}

#[tokio::test]
async fn a_table_holding_a_dog_wide_key_is_printed_as_stored_with_a_warning() {
    let own = Some(json!({ "poll": "1m", "limit": 9 }));
    let (code, out, err) = get("api.dogs.jobs", Format::Table, Serves::OptedIn, own).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(out.contains("poll = \"1m\""), "{out}");
    assert!(!out.contains("inherited"), "{out}");
    assert!(
        err.contains("`poll`") && err.contains("reads no settings"),
        "{err}"
    );
}
