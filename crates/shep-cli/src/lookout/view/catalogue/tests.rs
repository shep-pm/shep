use super::super::MIN_TERM_WIDTH;
use super::super::fixtures::{app_with, flock_of, plain};
use super::super::testing::draw_to;
use crate::dog_index::{AvailableDog, DogSourceKind, IndexedPermission};
use crate::lookout::app::{App, CatalogueRead, CatalogueRow, Control, DogState, KeyPress, Msg};
use std::collections::BTreeMap;

const WIDE: u16 = 80;
const NARROW: u16 = MIN_TERM_WIDTH;
const HEIGHT: u16 = 24;

fn permission(name: &str, required: bool) -> IndexedPermission {
    IndexedPermission {
        name: name.to_owned(),
        required,
    }
}

fn dog(
    name: &str,
    source: DogSourceKind,
    permissions: Option<Vec<IndexedPermission>>,
) -> AvailableDog {
    AvailableDog {
        name: name.to_owned(),
        package: format!("shep-{name}"),
        adopt_as: name.to_owned(),
        description: format!("{name} watches the flock and says so."),
        repo: format!("https://github.com/shep-pm/shep-{name}"),
        license: "MIT".to_owned(),
        category: "logs".to_owned(),
        source,
        permissions,
        needs: Vec::new(),
    }
}

fn cargo(version: &str) -> DogSourceKind {
    DogSourceKind::Cargo {
        version: Some(version.to_owned()),
        sha256: BTreeMap::new(),
    }
}

fn row(dog: AvailableDog, version: &str, state: DogState) -> CatalogueRow {
    CatalogueRow {
        dog,
        version: version.to_owned(),
        state,
    }
}

/// A dashboard on the catalogue, `rows` landed, `refreshing` as given.
fn app_on(rows: Vec<CatalogueRow>, refreshing: bool) -> App {
    let mut app = app_with(flock_of(1, 0), plain());
    app.open_catalogue();
    app.update(Msg::Catalogue {
        result: Ok(Box::new(CatalogueRead { rows, refreshing })),
    });
    app
}

fn three_rows() -> Vec<CatalogueRow> {
    vec![
        row(
            dog("alpha", cargo("0.2.4"), None),
            "0.2.1 \u{2192} 0.2.4",
            DogState::Enabled,
        ),
        row(
            dog("beta", cargo("1.0.0"), None),
            "1.0.0",
            DogState::NotInstalled,
        ),
        row(
            dog("gamma", cargo("0.3.0"), None),
            "0.3.0",
            DogState::Installed,
        ),
    ]
}

/// The words on the first line of `frame` containing `needle`.
fn words_of<'a>(frame: &'a str, needle: &str) -> Vec<&'a str> {
    frame
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not drawn:\n{frame}"))
        .split_whitespace()
        .collect()
}

/// Every word on screen in reading order, so wrapped text still reads as the
/// sentence it was.
fn flat(frame: &str) -> String {
    frame.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn open_first_entry(app: &mut App) {
    app.update(Msg::Key(KeyPress::Confirm));
}

#[test]
fn package_is_beside_name_and_drops_before_category() {
    let app = app_on(three_rows(), false);
    let frame = draw_to(&app, 120, HEIGHT);
    assert_eq!(
        words_of(&frame, "NAME"),
        ["NAME", "PACKAGE", "CATEGORY", "VERSION", "STATE"]
    );
    assert_eq!(
        words_of(&frame, "alpha"),
        [
            ">",
            "alpha",
            "shep-alpha",
            "logs",
            "0.2.1",
            "\u{2192}",
            "0.2.4",
            "enabled"
        ]
    );
    let frame = draw_to(&app, WIDE, HEIGHT);
    assert_eq!(
        words_of(&frame, "NAME"),
        ["NAME", "CATEGORY", "VERSION", "STATE"]
    );
}

#[test]
fn the_list_at_80_columns_has_four_columns_and_marks_the_cursor() {
    let app = app_on(three_rows(), false);

    let frame = draw_to(&app, WIDE, HEIGHT);

    assert_eq!(
        words_of(&frame, "CATEGORY"),
        ["NAME", "CATEGORY", "VERSION", "STATE"]
    );
    assert_eq!(
        words_of(&frame, "alpha"),
        [
            ">", "alpha", "logs", "0.2.1", "\u{2192}", "0.2.4", "enabled"
        ],
        "the cursor row, with the VERSION cell the row carries"
    );
    assert_eq!(
        words_of(&frame, "beta"),
        ["beta", "logs", "1.0.0", "not", "installed"]
    );
    assert_eq!(
        words_of(&frame, "gamma"),
        ["gamma", "logs", "0.3.0", "installed"]
    );
}

#[test]
fn the_list_at_the_narrow_floor_keeps_the_name_and_the_state() {
    let app = app_on(three_rows(), false);

    let frame = draw_to(&app, NARROW, HEIGHT);

    assert_eq!(words_of(&frame, "NAME"), ["NAME", "STATE"]);
    assert_eq!(words_of(&frame, "alpha"), [">", "alpha", "enabled"]);
    assert_eq!(words_of(&frame, "beta"), ["beta", "not", "installed"]);
    for line in frame.lines() {
        assert!(line.chars().count() <= usize::from(NARROW), "{line:?}");
    }
}

#[test]
fn the_cursor_follows_the_selection() {
    let mut app = app_on(three_rows(), false);
    app.update(Msg::Key(KeyPress::SelectDown));

    let frame = draw_to(&app, WIDE, HEIGHT);

    assert_eq!(words_of(&frame, "alpha")[0], "alpha");
    assert_eq!(words_of(&frame, "beta")[0], ">");
}

#[test]
fn a_refresh_in_flight_says_refreshing_at_both_widths() {
    let app = app_on(three_rows(), true);

    for width in [WIDE, NARROW] {
        let frame = draw_to(&app, width, HEIGHT);
        assert!(frame.contains("refreshing"), "{width}:\n{frame}");
        assert!(frame.contains("alpha"), "the old rows stay: {width}");
    }
}

#[test]
fn no_cache_and_no_network_is_one_line_and_no_rows() {
    let app = app_on(Vec::new(), false);

    for width in [WIDE, NARROW] {
        let frame = draw_to(&app, width, HEIGHT);
        let said: Vec<&str> = frame
            .lines()
            .filter(|line| line.contains("no index cached"))
            .collect();
        assert_eq!(said.len(), 1, "{width}:\n{frame}");
        assert!(said[0].contains("no network"), "{width}: {:?}", said[0]);
        assert!(!frame.contains("alpha"), "{width}:\n{frame}");
        assert!(!frame.contains("refreshing"), "{width}:\n{frame}");
    }
}

fn entry_with(permissions: Option<Vec<IndexedPermission>>) -> App {
    let rows = vec![row(
        dog("alpha", cargo("0.2.4"), permissions),
        "0.2.1 \u{2192} 0.2.4",
        DogState::Installed,
    )];
    let mut app = app_on(rows, false);
    open_first_entry(&mut app);
    app
}

#[test]
fn an_entry_shows_what_the_index_says_about_the_dog() {
    let app = entry_with(None);

    let frame = draw_to(&app, WIDE, HEIGHT);

    let text = flat(&frame);
    for expected in [
        "alpha watches the flock and says so.",
        "https://github.com/shep-pm/shep-alpha",
        "package shep-alpha",
        "license MIT",
        "kind cargo 0.2.4",
        "state installed",
    ] {
        assert!(text.contains(expected), "{expected:?} in:\n{frame}");
    }
    assert!(!frame.contains("NAME"), "the list is not drawn under it");
}

#[test]
fn an_entry_marks_each_listed_permission_required_or_optional() {
    let app = entry_with(Some(vec![
        permission("flock.read", true),
        permission("events:sheep.", false),
    ]));

    for width in [WIDE, NARROW] {
        let frame = draw_to(&app, width, HEIGHT);
        let text = flat(&frame);
        assert!(
            text.contains("permissions flock.read (required) events:sheep. (optional)"),
            "{width}:\n{frame}"
        );
    }
}

#[test]
fn an_entry_whose_row_lists_no_permissions_says_not_listed() {
    let app = entry_with(None);

    for width in [WIDE, NARROW] {
        let frame = draw_to(&app, width, HEIGHT);
        assert_eq!(
            words_of(&frame, "permissions"),
            ["permissions", "not", "listed"],
            "{width}"
        );
    }
}

#[test]
fn an_entry_whose_version_declares_none_says_none() {
    let app = entry_with(Some(Vec::new()));

    for width in [WIDE, NARROW] {
        let frame = draw_to(&app, width, HEIGHT);
        assert_eq!(
            words_of(&frame, "permissions"),
            ["permissions", "none"],
            "{width}"
        );
    }
}

#[test]
fn a_kind_shep_cannot_install_shows_its_instructions_and_no_install_line() {
    let manual = DogSourceKind::Manual {
        instructions: "Download a release from the repo.".to_owned(),
    };
    let git = DogSourceKind::CargoGit {
        url: "https://github.com/shep-pm/shep-alpha".to_owned(),
    };
    for (source, instructions) in [
        (manual, "Download a release from the repo."),
        (
            git,
            "$ cargo install --git https://github.com/shep-pm/shep-alpha",
        ),
    ] {
        let rows = vec![row(dog("alpha", source, None), "-", DogState::NotInstalled)];
        let mut app = app_on(rows, false);
        open_first_entry(&mut app);

        for width in [WIDE, NARROW] {
            let frame = draw_to(&app, width, HEIGHT);
            let text = flat(&frame);
            assert!(!text.contains("shep install"), "{width}:\n{frame}");
            // A long word breaks inside itself at the narrow floor, so the
            // comparison ignores the spaces the break leaves.
            let packed: String = text.split_whitespace().collect();
            let wanted: String = instructions.split_whitespace().collect();
            assert!(packed.contains(&wanted), "{width}:\n{frame}");
        }
    }
}

/// The entry's hint line, in the bar and in the entry itself: `enter` is
/// named only where `Enter` installs. An installed dog, an instructions-only
/// one and a closed gate each name nothing.
#[test]
fn the_entry_names_the_install_key_only_where_there_is_an_install() {
    let manual = || DogSourceKind::Manual {
        instructions: "Download a release from the repo.".to_owned(),
    };
    for (name, source, state, named) in [
        ("beta", cargo("1.0.0"), DogState::NotInstalled, true),
        ("gamma", cargo("0.3.0"), DogState::Installed, false),
        ("delta", manual(), DogState::NotInstalled, false),
    ] {
        let mut app = app_on(vec![row(dog(name, source, None), "1.0.0", state)], false);
        app.set_control_for_tests(Control::Allowed);
        open_first_entry(&mut app);

        let frame = draw_to(&app, WIDE, HEIGHT);
        let bar = frame.lines().last().unwrap_or_default();
        assert_eq!(
            (bar.contains("enter install"), bar.contains("esc back")),
            (named, true),
            "{name} {state}:\n{frame}"
        );
        assert_eq!(
            flat(&frame).contains("enter installs it"),
            named,
            "{name} {state}:\n{frame}"
        );
    }
}

/// The entry's hint line names `enter upgrade` only for a row behind the
/// index and `D delete` only for an installed one, built-in dogs excepted.
#[test]
fn the_entry_names_upgrade_and_delete_only_where_they_apply() {
    for (name, version, state, upgrade, delete) in [
        ("beta", "1.0.0", DogState::NotInstalled, false, false),
        ("gamma", "0.3.0", DogState::Installed, false, true),
        (
            "alpha",
            "0.2.1 \u{2192} 0.2.4",
            DogState::Enabled,
            true,
            true,
        ),
        ("metrics", "0.3.0", DogState::Enabled, false, false),
    ] {
        let mut app = app_on(
            vec![row(dog(name, cargo("1.0.0"), None), version, state)],
            false,
        );
        app.set_control_for_tests(Control::Allowed);
        open_first_entry(&mut app);

        let frame = draw_to(&app, 100, HEIGHT);
        let bar = frame.lines().last().unwrap_or_default();
        assert_eq!(
            (bar.contains("enter upgrade"), bar.contains("D delete")),
            (upgrade, delete),
            "{name} {state}:\n{frame}"
        );
        assert_eq!(
            flat(&frame).contains("enter upgrades it"),
            upgrade,
            "{name} {state}:\n{frame}"
        );
    }
}

#[test]
fn an_installable_row_shows_no_build_instructions() {
    let app = entry_with(None);

    let frame = draw_to(&app, WIDE, HEIGHT);

    assert!(!frame.contains("cargo install"), "{frame}");
}

#[test]
fn the_state_word_is_whole_at_every_width_the_list_draws_in() {
    let app = app_on(three_rows(), false);

    for width in NARROW..=100 {
        let frame = draw_to(&app, width, HEIGHT);
        let beta = frame
            .lines()
            .find(|line| line.contains("beta"))
            .unwrap_or_else(|| panic!("{width}:\n{frame}"));
        assert!(
            beta.trim_end().ends_with("not installed"),
            "{width}: {beta:?}"
        );
    }
}
