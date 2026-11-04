use std::time::{Duration, SystemTime};

use shep_core::paths::ShepPaths;

use super::*;
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::{Index, parse_index};
use crate::lookout::app::testing::started;

const URL: &str = "https://example.test/dogs.json";

/// A home on disk the row builder reads receipts and `shep.toml` from.
struct Home {
    _dir: tempfile::TempDir,
    paths: ShepPaths,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::at(dir.path().to_path_buf());
        Self { _dir: dir, paths }
    }

    /// A receipt as `shep install` wrote it, pinned to `version`.
    fn receipt(&self, name: &str, package: &str, version: &str) {
        let dir = self.paths.dogs_dir.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "package": package,
            "row": { "repo": "https://github.com/shep-pm/example" },
            "version": version,
            "manifest": { "version": version },
        });
        std::fs::write(dir.join("receipt.json"), body.to_string()).unwrap();
    }

    fn config(&self, text: &str) -> ShepToml {
        let path = self.paths.home.join("shep.toml");
        std::fs::write(&path, text).unwrap();
        ShepToml::read_only(&path).unwrap()
    }
}

/// One row of an index document.
fn row(name: &str, category: &str, version: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "package": format!("shep-{name}"),
        "adopt_as": name,
        "description": format!("{name} does a thing."),
        "repo": format!("https://github.com/shep-pm/shep-{name}"),
        "license": "MIT",
        "category": category,
        "source": { "kind": "cargo", "version": version }
    })
}

fn index_of(rows: &[serde_json::Value]) -> Index {
    let document = serde_json::json!({ "version": 1, "dogs": rows });
    parse_index(document.to_string().as_bytes()).unwrap()
}

fn state_of(rows: &[CatalogueRow], name: &str) -> &'static str {
    rows.iter()
        .find(|row| row.dog.name == name)
        .unwrap_or_else(|| panic!("no row for {name}"))
        .state
        .as_str()
}

#[test]
fn rows_say_not_installed_installed_or_enabled_from_the_receipt_and_the_config() {
    let home = Home::new();
    home.receipt("otel", "shep-otel", "0.1.0");
    home.receipt("rotate", "shep-rotate", "0.1.0");
    let config = home.config(
        "[daemon]\nenabled_dogs = [\"rotate\", \"metrics\"]\n\
         [daemon.adopted_dogs]\nrotate = \"/bin/shep-rotate\"\nhand = \"/bin/shep-hand\"\n",
    );
    let index = index_of(&[
        row("otel", "metrics", "0.1.0"),
        row("rotate", "logs", "0.1.0"),
        row("hand", "logs", "0.1.0"),
        row("fresh", "alerts", "0.1.0"),
    ]);

    let rows = build_rows(&index, &home.paths, &config);

    assert_eq!(state_of(&rows, "fresh"), "not installed");
    assert_eq!(
        state_of(&rows, "otel"),
        "installed",
        "a receipt, never enabled"
    );
    assert_eq!(
        state_of(&rows, "hand"),
        "installed",
        "adopted with no receipt"
    );
    assert_eq!(state_of(&rows, "rotate"), "enabled");
}

fn version_of(rows: &[CatalogueRow], name: &str) -> String {
    rows.iter()
        .find(|row| row.dog.name == name)
        .unwrap_or_else(|| panic!("no row for {name}"))
        .version
        .clone()
}

#[test]
fn an_installed_dog_behind_the_index_reads_old_arrow_new() {
    let home = Home::new();
    home.receipt("otel", "shep-otel", "0.2.1");
    home.receipt("current", "shep-current", "0.2.4");
    let config = home.config("");
    let index = index_of(&[
        row("otel", "metrics", "0.2.4"),
        row("current", "metrics", "0.2.4"),
        row("fresh", "metrics", "1.0.0"),
    ]);

    let rows = build_rows(&index, &home.paths, &config);

    assert_eq!(version_of(&rows, "otel"), "0.2.1 \u{2192} 0.2.4");
    assert_eq!(version_of(&rows, "current"), "0.2.4");
    assert_eq!(
        version_of(&rows, "fresh"),
        "1.0.0",
        "the index's own version"
    );
}

#[test]
fn rows_run_by_category_in_catalogue_order_then_by_name() {
    let home = Home::new();
    let config = home.config("");
    let index = index_of(&[
        row("zed", "logs", "1.0.0"),
        row("beta", "alerts", "1.0.0"),
        row("alpha", "alerts", "1.0.0"),
        row("mid", "metrics", "1.0.0"),
        row("odd", "no-such-category", "1.0.0"),
    ]);

    let rows = build_rows(&index, &home.paths, &config);

    let names: Vec<&str> = rows.iter().map(|row| row.dog.name.as_str()).collect();
    assert_eq!(
        names,
        ["zed", "mid", "alpha", "beta", "odd"],
        "logs, metrics, alerts, then the unknown category's fallback"
    );
}

/// A dashboard on the catalogue with `rows` already landed from a cache that
/// needs no fetch.
fn app_on_catalogue(rows: Vec<CatalogueRow>) -> App {
    let (mut app, _t0) = started();
    assert_eq!(app.open_catalogue(), Effect::LoadCatalogue { fetch: true });
    let effect = app.update(Msg::Catalogue {
        result: Ok(Box::new(CatalogueRead {
            rows,
            refreshing: false,
        })),
    });
    assert_eq!(effect, Effect::None);
    app
}

fn three_rows() -> Vec<CatalogueRow> {
    let home = Home::new();
    let config = home.config("");
    let index = index_of(&[
        row("alpha", "logs", "1.0.0"),
        row("beta", "logs", "1.0.0"),
        row("gamma", "logs", "1.0.0"),
    ]);
    build_rows(&index, &home.paths, &config)
}

fn open_entry(app: &App) -> Option<&str> {
    match app.body() {
        Body::Catalogue(pane) => pane.entry.as_deref(),
        other => panic!("not on the catalogue: {other:?}"),
    }
}

#[test]
fn enter_opens_the_entry_escape_returns_to_the_list_and_a_second_escape_leaves() {
    let mut app = app_on_catalogue(three_rows());
    app.update(Msg::Key(KeyPress::SelectDown));

    app.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(open_entry(&app), Some("beta"), "the row under the cursor");

    let effect = app.update(Msg::Key(KeyPress::Escape));
    assert_eq!(effect, Effect::None);
    assert_eq!(open_entry(&app), None, "back on the list");
    assert!(matches!(app.body(), Body::Catalogue(pane) if pane.selected == 1));

    let effect = app.update(Msg::Key(KeyPress::Escape));
    assert_eq!(effect, Effect::None);
    assert!(
        matches!(app.body(), Body::FlockTable),
        "left for the dashboard"
    );
}

fn read_of(rows: Vec<CatalogueRow>, refreshing: bool) -> Msg {
    Msg::Catalogue {
        result: Ok(Box::new(CatalogueRead { rows, refreshing })),
    }
}

/// Writes `names` as the cached index for `URL`, last written `age` ago.
fn cache_of(home: &Home, names: &[&str], age: Duration) {
    let rows: Vec<serde_json::Value> = names
        .iter()
        .map(|name| row(name, "logs", "1.0.0"))
        .collect();
    let document = serde_json::json!({ "version": 1, "dogs": rows });
    let cache = &home.paths.dog_index_cache;
    crate::dog_index::write_cache(cache, URL, document.to_string().as_bytes()).unwrap();
    std::fs::File::options()
        .write(true)
        .open(cache)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

#[test]
fn a_stale_cache_draws_at_once_and_raises_a_background_fetch() {
    let home = Home::new();
    cache_of(&home, &["alpha", "beta"], MAX_AGE + Duration::from_secs(60));

    let read = read_cache(&home.paths, URL, true).unwrap();
    assert_eq!(read.rows.len(), 2, "the stale rows draw first");
    assert!(read.refreshing, "and a fetch is due");

    let (mut app, _t0) = started();
    app.open_catalogue();
    let effect = app.update(Msg::Catalogue { result: Ok(read) });
    assert_eq!(effect, Effect::FetchCatalogue);
    assert!(matches!(app.body(), Body::Catalogue(pane) if pane.refreshing));
}

#[test]
fn a_fresh_cache_raises_no_fetch_and_fetch_unset_never_does() {
    let home = Home::new();
    cache_of(&home, &["alpha"], Duration::from_secs(60));
    assert!(!read_cache(&home.paths, URL, true).unwrap().refreshing);

    cache_of(&home, &["alpha"], MAX_AGE + Duration::from_secs(60));
    let read = read_cache(&home.paths, URL, false).unwrap();
    assert_eq!(read.rows.len(), 1);
    assert!(
        !read.refreshing,
        "a stale cache with fetch unset stays on disk"
    );
    let empty = Home::new();
    assert!(!read_cache(&empty.paths, URL, false).unwrap().refreshing);
}

#[test]
fn a_landed_fetch_replaces_the_rows_and_the_cursor_stays_on_its_dog() {
    let mut app = app_on_catalogue(three_rows());
    app.update(Msg::Key(KeyPress::SelectLast));
    app.update(read_of(three_rows(), true));

    let mut fresh = three_rows();
    fresh.insert(0, fresh[0].clone());
    fresh[0].dog.adopt_as = "aardvark".to_owned();
    let effect = app.update(read_of(fresh, false));

    assert_eq!(effect, Effect::None);
    assert_eq!(
        app.catalogue_rows().len(),
        4,
        "the new rows replaced the old"
    );
    assert!(
        matches!(app.body(), Body::Catalogue(pane) if pane.selected == 3 && !pane.refreshing),
        "gamma moved down one and the cursor went with it"
    );
}

#[test]
fn a_failed_fetch_keeps_the_rows_clears_refreshing_and_says_nothing() {
    let mut app = app_on_catalogue(three_rows());
    app.update(read_of(three_rows(), true));

    let effect = app.update(Msg::Catalogue {
        result: Err("timed out".to_owned()),
    });

    assert_eq!(effect, Effect::None);
    assert_eq!(app.catalogue_rows().len(), 3);
    assert!(matches!(app.body(), Body::Catalogue(pane) if !pane.refreshing));
    assert!(app.notice().is_none());
}

#[test]
fn a_failed_fetch_with_no_rows_says_why() {
    let (mut app, _t0) = started();
    app.open_catalogue();

    app.update(Msg::Catalogue {
        result: Err("timed out".to_owned()),
    });

    assert!(app.catalogue_rows().is_empty());
    assert_eq!(
        app.notice().map(ToString::to_string).as_deref(),
        Some("timed out")
    );
}

#[test]
fn no_cache_gives_no_rows_and_a_fetch_only_when_asked() {
    let home = Home::new();

    let with_fetch = read_cache(&home.paths, URL, true).unwrap();
    assert!(with_fetch.rows.is_empty());
    assert!(with_fetch.refreshing, "the first fetch is due");

    let without = read_cache(&home.paths, URL, false).unwrap();
    assert!(without.rows.is_empty());
    assert!(
        !without.refreshing,
        "nothing to wait for, so the pane says so"
    );
}

#[test]
fn a_dog_that_leaves_the_index_closes_its_open_entry() {
    let mut app = app_on_catalogue(three_rows());
    app.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(open_entry(&app), Some("alpha"));

    app.update(read_of(three_rows().split_off(1), false));

    assert_eq!(open_entry(&app), None);
}

#[test]
fn over_a_host_the_catalogue_stays_shut() {
    let (mut app, _t0) = started();
    app.set_remote_host("build-01");

    let effect = app.open_catalogue();

    assert_eq!(effect, Effect::None);
    assert!(matches!(app.body(), Body::FlockTable));
    assert!(app.notice().is_some());
}

#[test]
fn no_cache_and_no_network_says_so_in_one_line_and_lists_nothing() {
    let home = Home::new();
    let (mut app, _t0) = started();
    app.open_catalogue();

    let read = read_cache(&home.paths, URL, true).unwrap();
    assert_eq!(
        app.update(Msg::Catalogue { result: Ok(read) }),
        Effect::FetchCatalogue
    );
    assert_eq!(
        app.catalogue_caption(),
        "refreshing",
        "while the fetch is out"
    );

    app.update(Msg::Catalogue {
        result: Err("no route to host".to_owned()),
    });

    assert!(app.catalogue_rows().is_empty());
    assert_eq!(app.catalogue_caption(), "no index cached, no network");
}
