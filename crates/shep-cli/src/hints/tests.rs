use std::fs::File;
use std::net::TcpListener;
use std::path::Path;

use shep_core::paths::ShepPaths;
use shep_core::protocol::{
    DogSource, Envelope, HelloAck, PROTOCOL_VERSION, ProcessInfo, Request, Response,
};
use shep_core::status::ProcStatus;
use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::cli::Format;
use crate::output::Streams;

pub(super) const URL: &str = "https://example.test/dogs.json";
const MIB: u64 = 1024 * 1024;

pub(super) fn paths_in(dir: &Path) -> ShepPaths {
    let paths = ShepPaths::resolve(&|_| None, dir);
    std::fs::create_dir_all(&paths.run).unwrap();
    paths
}

/// A sparse file `len` bytes long, so a threshold test writes no data.
fn log_of(dir: &Path, name: &str, len: u64) -> String {
    let path = dir.join(name);
    File::create(&path).unwrap().set_len(len).unwrap();
    path.to_str().unwrap().to_owned()
}

fn sheep(id: u32, name: &str) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online).build()
}

fn with_out(id: u32, name: &str, out: String) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .out_file(Some(out))
        .build()
}

fn restarted(id: u32, name: &str, restarts: u32) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online)
        .restarts(restarts)
        .build()
}

/// Caches an index whose rows are `(adopt_as, needs)`.
pub(super) fn cache_index(paths: &ShepPaths, rows: &[(&str, &[&str])]) {
    let dogs: Vec<_> = rows
        .iter()
        .map(|(adopt_as, needs)| {
            serde_json::json!({
                "name": adopt_as,
                "package": format!("shep-{adopt_as}"),
                "adopt_as": adopt_as,
                "description": "Does a thing.",
                "repo": "https://github.com/someone/shep-dog",
                "license": "MIT",
                "category": "other",
                "source": { "kind": "cargo", "version": "1.0.0" },
                "needs": needs,
            })
        })
        .collect();
    let document = serde_json::json!({ "version": 1, "dogs": dogs });
    crate::dog_index::write_cache(&paths.dog_index_cache, URL, document.to_string().as_bytes())
        .unwrap();
}

fn enable(paths: &ShepPaths, names: &[&str]) {
    let list: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    std::fs::write(
        &paths.daemon_config,
        format!("[daemon]\nenabled_dogs = [{}]\n", list.join(", ")),
    )
    .unwrap();
}

pub(super) fn install(paths: &ShepPaths, name: &str) {
    let dir = paths.dogs_dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("receipt.json"),
        r#"{"package":"shep-dog","row":{"repo":"https://github.com/someone/shep-dog"},"version":"1.0.0"}"#,
    )
    .unwrap();
}

pub(super) fn off(paths: &ShepPaths, names: &[&str]) {
    std::fs::write(
        paths.home.join("hints.json"),
        serde_json::json!({ "off": names }).to_string(),
    )
    .unwrap();
}

fn line(shown: Option<Shown>) -> String {
    shown.expect("a hint shows").line
}

#[test]
fn a_log_file_past_the_threshold_offers_the_dogs_that_need_logs() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(
        &paths,
        &[("log-rotate", &["logs"]), ("discord", &["alerts"])],
    );
    let flock = [with_out(
        1,
        "api",
        log_of(dir.path(), "out.log", 100 * MIB + 1),
    )];

    let shown = next(&paths, URL, &flock).unwrap();

    assert_eq!(shown.hint, Hint::Logs);
    assert_eq!(
        shown.line,
        "hint: api's log is past 100 MiB. Try: shep install log-rotate"
    );
    assert_eq!(
        shown.offers,
        [Offer {
            name: "log-rotate".to_owned(),
            state: OfferState::NotInstalled
        }]
    );
}

#[test]
fn a_log_file_exactly_at_the_threshold_shows_no_hint() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(&paths, &[("log-rotate", &["logs"])]);
    let flock = [with_out(1, "api", log_of(dir.path(), "out.log", 100 * MIB))];

    assert_eq!(next(&paths, URL, &flock), None);
}

#[test]
fn the_err_file_counts_as_well_as_the_out_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(&paths, &[("log-rotate", &["logs"])]);
    let api = ProcessInfo::builder(1, "api", ProcStatus::Online)
        .out_file(Some(log_of(dir.path(), "out.log", 1)))
        .err_file(Some(log_of(dir.path(), "err.log", 100 * MIB + 1)))
        .build();

    let shown = next(&paths, URL, &[api]).unwrap();

    assert_eq!(shown.hint, Hint::Logs);
}

#[test]
fn a_dog_in_the_flock_is_never_the_sheep_named() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(&paths, &[("log-rotate", &["logs"])]);
    let big = log_of(dir.path(), "out.log", 100 * MIB + 1);
    let dog = ProcessInfo::builder(1, "otel", ProcStatus::Online)
        .out_file(Some(big.clone()))
        .dog(Some(DogSource::Unrecognized))
        .build();
    let flock = [dog.clone(), with_out(2, "api", big)];

    let named = line(next(&paths, URL, &flock));

    assert!(named.starts_with("hint: api's log"), "{named}");
    assert_eq!(next(&paths, URL, &[dog]), None);
}

#[test]
fn the_first_qualifying_sheep_by_id_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(&paths, &[("log-rotate", &["logs"])]);
    let big = log_of(dir.path(), "out.log", 100 * MIB + 1);
    let flock = [with_out(7, "worker", big.clone()), with_out(3, "api", big)];

    assert!(line(next(&paths, URL, &flock)).starts_with("hint: api's log"));
}

#[test]
fn an_enabled_dog_for_logs_ends_the_logs_hint() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(&paths, &[("log-rotate", &["logs"])]);
    enable(&paths, &["log-rotate"]);
    let flock = [with_out(
        1,
        "api",
        log_of(dir.path(), "out.log", 100 * MIB + 1),
    )];

    assert_eq!(next(&paths, URL, &flock), None);
}

#[test]
fn a_sheep_restarted_three_times_offers_bark_and_the_dogs_that_need_alerts() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(
        &paths,
        &[("discord", &["alerts"]), ("log-rotate", &["logs"])],
    );

    let shown = next(&paths, URL, &[restarted(1, "api", 3)]).unwrap();

    assert_eq!(shown.hint, Hint::Alerts);
    assert_eq!(
        shown.line,
        "hint: api has restarted 3 times. Try: shep enable bark or shep install discord"
    );
    assert_eq!(shown.offers[0].state, OfferState::BuiltIn);
}

#[test]
fn two_restarts_show_no_hint() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());

    assert_eq!(next(&paths, URL, &[restarted(1, "api", 2)]), None);
}

#[test]
fn an_enabled_bark_ends_the_alerts_hint() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    enable(&paths, &["bark"]);

    assert_eq!(next(&paths, URL, &[restarted(1, "api", 5)]), None);
}

#[test]
fn an_enabled_dog_for_alerts_ends_the_alerts_hint() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(&paths, &[("discord", &["alerts"])]);
    enable(&paths, &["discord"]);

    assert_eq!(next(&paths, URL, &[restarted(1, "api", 5)]), None);
}

#[test]
fn with_no_cached_index_logs_shows_nothing_and_alerts_offers_bark() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let big = with_out(1, "api", log_of(dir.path(), "out.log", 100 * MIB + 1));

    assert_eq!(next(&paths, URL, &[big]), None);
    assert_eq!(
        line(next(&paths, URL, &[restarted(1, "api", 4)])),
        "hint: api has restarted 4 times. Try: shep enable bark"
    );
}

#[test]
fn a_row_with_no_known_need_is_never_offered() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(
        &paths,
        &[
            ("deploy", &[]),
            ("future", &["telepathy"]),
            ("log-rotate", &["logs"]),
        ],
    );
    let flock = [with_out(
        1,
        "api",
        log_of(dir.path(), "out.log", 100 * MIB + 1),
    )];

    let shown = next(&paths, URL, &flock).unwrap();

    assert_eq!(
        shown.line,
        "hint: api's log is past 100 MiB. Try: shep install log-rotate"
    );
}

#[test]
fn an_installed_but_disabled_dog_is_offered_as_enable_not_install() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(&paths, &[("log-rotate", &["logs"])]);
    install(&paths, "log-rotate");
    let flock = [with_out(
        1,
        "api",
        log_of(dir.path(), "out.log", 100 * MIB + 1),
    )];

    let shown = next(&paths, URL, &flock).unwrap();

    assert_eq!(
        shown.line,
        "hint: api's log is past 100 MiB. Try: shep enable log-rotate"
    );
    assert_eq!(shown.offers[0].state, OfferState::Installed);
}

#[test]
fn the_logs_hint_comes_before_the_alerts_hint_and_only_one_shows() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(
        &paths,
        &[("log-rotate", &["logs"]), ("discord", &["alerts"])],
    );
    let flock = [
        with_out(1, "api", log_of(dir.path(), "out.log", 100 * MIB + 1)),
        restarted(2, "worker", 9),
    ];

    let shown = next(&paths, URL, &flock).unwrap();

    assert_eq!(shown.hint, Hint::Logs);
    assert_eq!(shown.line.lines().count(), 1);
    assert!(!shown.line.contains("worker"), "{}", shown.line);
}

#[test]
fn an_off_hint_is_skipped_for_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(&paths, &[("log-rotate", &["logs"])]);
    off(&paths, &["logs"]);
    let flock = [
        with_out(1, "api", log_of(dir.path(), "out.log", 100 * MIB + 1)),
        restarted(2, "worker", 9),
    ];

    assert_eq!(next(&paths, URL, &flock).unwrap().hint, Hint::Alerts);

    off(&paths, &["logs", "alerts"]);
    assert_eq!(next(&paths, URL, &flock), None);
}

#[test]
fn the_thresholds_are_a_hundred_mebibytes_and_three_restarts() {
    assert_eq!(LOG_FILE_HINT_BYTES, 104_857_600);
    assert_eq!(RESTART_HINT_COUNT, 3);
}

#[test]
fn the_hint_lines_are_pinned_and_hold_no_dash() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(
        &paths,
        &[("log-rotate", &["logs"]), ("discord", &["alerts"])],
    );
    let logs = [with_out(
        1,
        "api",
        log_of(dir.path(), "out.log", 100 * MIB + 1),
    )];

    let lines = [
        line(next(&paths, URL, &logs)),
        line(next(&paths, URL, &[restarted(1, "api", 3)])),
    ];

    assert_eq!(
        lines,
        [
            "hint: api's log is past 100 MiB. Try: shep install log-rotate",
            "hint: api has restarted 3 times. Try: shep enable bark or shep install discord",
        ]
    );
    assert!(lines.iter().all(|l| !l.contains(['\u{2014}', '\u{2013}'])));
}

#[test]
fn the_hint_names_round_trip() {
    assert_eq!(Hint::ALL.map(Hint::name), ["logs", "alerts"]);
    assert_eq!(Hint::from_name("alerts"), Some(Hint::Alerts));
    assert_eq!(Hint::from_name("Logs"), None);
}

#[test]
fn a_hint_never_fetches_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/dogs.json", listener.local_addr().unwrap());
    let flock = [
        with_out(1, "api", log_of(dir.path(), "out.log", 100 * MIB + 1)),
        restarted(2, "worker", 9),
    ];

    let shown = next(&paths, &url, &flock);

    assert_eq!(shown.unwrap().hint, Hint::Alerts);
    let accepted = listener.accept();
    assert_eq!(
        accepted.unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "the index url was dialled"
    );
}

/// A shepherd at `paths.socket` listing `flock`. The receiver must be held.
async fn shepherd_listing(
    paths: &ShepPaths,
    flock: Vec<ProcessInfo>,
) -> UnboundedReceiver<Envelope> {
    let ack = HelloAck {
        daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: PROTOCOL_VERSION,
        pid: 4242,
        min_supported: None,
    };
    shep_client::testing::fake_daemon_answering_with_ack(&paths.socket, ack, move |request| {
        match request {
            Request::ListFlock => Response::Flock(flock.clone()),
            _ => Response::Pong,
        }
    })
    .await
}

async fn printed(paths: &ShepPaths, fmt: Format, quiet: bool, follow: bool, tty: bool) -> String {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    print_under_flock(
        &mut Streams::bare(&mut out, &mut err, fmt),
        paths,
        quiet,
        follow,
        tty,
    )
    .await;
    assert!(err.is_empty(), "a hint writes nothing to stderr");
    String::from_utf8(out).unwrap()
}

#[tokio::test]
async fn a_hint_prints_under_the_listing_on_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let _held = shepherd_listing(&paths, vec![restarted(1, "api", 3)]).await;

    let out = printed(&paths, Format::Table, false, false, true).await;

    assert_eq!(
        out,
        "hint: api has restarted 3 times. Try: shep enable bark\n"
    );
}

#[tokio::test]
async fn no_hint_prints_under_json_with_quiet_on_a_pipe_or_while_following() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    let _held = shepherd_listing(&paths, vec![restarted(1, "api", 3)]).await;
    // The same setup prints on a terminal, so each silence below is the gate.
    assert!(
        !printed(&paths, Format::Table, false, false, true)
            .await
            .is_empty()
    );

    assert_eq!(printed(&paths, Format::Json, false, false, true).await, "");
    assert_eq!(printed(&paths, Format::Table, true, false, true).await, "");
    assert_eq!(
        printed(&paths, Format::Table, false, false, false).await,
        ""
    );
    assert_eq!(printed(&paths, Format::Table, false, true, true).await, "");
}

#[tokio::test]
async fn no_shepherd_to_list_prints_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());

    assert_eq!(printed(&paths, Format::Table, false, false, true).await, "");
}

#[test]
fn a_sheep_without_a_log_file_or_an_unreadable_one_never_applies() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths_in(dir.path());
    cache_index(&paths, &[("log-rotate", &["logs"])]);
    let gone = with_out(
        2,
        "worker",
        dir.path().join("missing.log").to_str().unwrap().to_owned(),
    );

    assert_eq!(next(&paths, URL, &[sheep(1, "api"), gone]), None);
}
