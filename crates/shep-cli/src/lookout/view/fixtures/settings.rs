//! The settings screen, its snapshot, and its dogs table.

use std::path::PathBuf;
use std::time::Instant;

use shep_core::protocol::{DogSource, ProcessInfo};
use shep_core::status::ProcStatus;

use crate::commands::settings::{DogView, SettingEdit, SettingsSnapshot, Shape};
use crate::lookout::app::{App, Control, KeyPress, Msg, Settings, SettingsRow};
use crate::lookout::field::shep_toml_shape;
use crate::style::{StyleLevel, StyleSource};

use super::flock::{app_with, flock_of};
use super::palette::plain;

/// A plausible settings snapshot: a `shep.toml` that declares one key in
/// every kind of row the tree draws (a boolean, a closed set, a duration, a
/// path, a list, a map of entries), and two candidate dogs, one enabled, for
/// the tests that need a screen with real rows rather than a fresh home's
/// all-default one.
pub fn settings_snapshot() -> SettingsSnapshot {
    let document = serde_json::json!({
        "daemon": {
            "log_level": "warn",
            "log_json": false,
            "socket": "/home/ada/.shep/run/shep.sock",
            "max_cron_sleep": "30s",
            "environment": "production",
        },
        "whistle": { "allow_control": false },
        "secrets": { "allow_read": false },
        "style": { "level": "full" },
        "interpreters": { "js": "node", "py": "python3" },
        "hosts": ["ada@box"],
    });
    SettingsSnapshot {
        document: document.as_object().cloned().unwrap_or_default(),
        socket_default: PathBuf::from("/home/ada/.shep/run/shep.sock"),
        // The document declares it, so the file and the resolved value
        // agree.
        style: (StyleLevel::Full, StyleSource::Config),
        dogs: vec![
            DogView {
                name: "bark".to_string(),
                enabled: false,
                adopted_path: None,
            },
            DogView {
                name: "metrics".to_string(),
                enabled: true,
                adopted_path: None,
            },
        ],
    }
}

/// A dashboard with the settings screen already open on
/// [`settings_snapshot`], the gate closed ([`Control::ReadOnly`]).
pub fn app_in_settings() -> App {
    let mut app = app_with(flock_of(3, 0), plain());
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot()),
    });
    app
}

/// The dotted key a settings row stands for: a section's path, a key's own,
/// a map's entry as `map.name`, and the row that adds one as `map+`.
fn key_of(settings: &Settings, row: SettingsRow) -> Option<String> {
    let shape = shep_toml_shape();
    let fields = shape.fields.fields();
    Some(match row {
        SettingsRow::Table(index) => shape.outline.tables[index].path.clone(),
        SettingsRow::Field(index) => fields[index].key.clone(),
        SettingsRow::Entry { map, index } => {
            let key = &fields[map].key;
            let (name, _) = settings.snapshot().entries(key).into_iter().nth(index)?;
            format!("{key}.{name}")
        }
        SettingsRow::AddEntry(map) => format!("{}+", fields[map].key),
        SettingsRow::Dog(_) => return None,
    })
}

/// Moves the settings cursor onto the row named by the dotted `key` (see
/// [`key_of`]), from the first row, by real keypresses rather than poking
/// the cursor index directly.
///
/// # Panics
///
/// If the settings screen is not open, or `key` is not a row of it.
#[track_caller]
pub fn select_setting(app: &mut App, key: &str) {
    app.update(Msg::Key(KeyPress::SelectFirst));
    let settings = app.settings().expect("the settings screen is open");
    let target = settings
        .rows()
        .into_iter()
        .position(|row| key_of(settings, row).as_deref() == Some(key))
        .unwrap_or_else(|| panic!("{key} is a row of the settings tree"));
    for _ in 0..target {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
}

/// [`app_in_settings_with_control`] with the cursor already on the row named
/// by the dotted `key`.
pub fn app_in_settings_on(key: &str) -> App {
    let mut app = app_in_settings_with_control();
    select_setting(&mut app, key);
    app
}

/// [`app_in_settings_with_control`], but built from a caller-visible `t0`
/// rather than [`Instant::now`] read inside this function: an expiry test
/// needs to hand `Msg::Tick` an instant it can do arithmetic against.
pub fn app_in_settings_at() -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(plain(), Control::Allowed, "/home/ada/.shep".to_string(), t0);
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot()),
    });
    (app, t0)
}

/// The settings screen with `[style] level` SHADOWED: the document says
/// `full`, `source` is the layer that outranked it, and the level in force
/// is `bare`. The cursor sits on the style row, moved there by real
/// keypresses the same way [`app_in_settings_on`] moves it.
///
/// The one state where a key's value in force and its value on disk
/// disagree. Every other key's layers belong to the shepherd's process,
/// where lookout can see neither.
pub fn app_in_settings_with_shadowed_style(source: StyleSource) -> App {
    let mut app = app_in_settings_on("style.level");
    let mut snapshot = settings_snapshot();
    snapshot.style = (StyleLevel::Bare, source);
    app.update(Msg::Settings {
        result: Ok(snapshot),
    });
    app
}

/// The settings screen over a `shep.toml` that says nothing about `[style]`,
/// so `full` is in force by default. The cursor sits on the style row.
pub fn app_in_settings_with_silent_style() -> App {
    let mut app = app_in_settings_on("style.level");
    let mut snapshot = settings_snapshot();
    snapshot.document.remove("style");
    snapshot.style = (StyleLevel::Full, StyleSource::Default);
    app.update(Msg::Settings {
        result: Ok(snapshot),
    });
    app
}

/// [`app_in_settings`] with the control gate open, for the one test that
/// proves an action key stays unreachable even when actions would otherwise
/// be permitted.
pub fn app_in_settings_with_control() -> App {
    let mut app = app_in_settings();
    app.set_control_for_tests(Control::Allowed);
    app
}

/// [`settings_snapshot`]'s own keys, with `dogs` replaced, for the
/// dogs-table tests, which need particular names, `enabled` bits, and a
/// matching or mismatching flock.
fn settings_snapshot_with_dogs(dogs: Vec<DogView>) -> SettingsSnapshot {
    SettingsSnapshot {
        dogs,
        ..settings_snapshot()
    }
}

/// `otel` runs online while the file disables it: a removed name still
/// running. `ledger` is enabled in the file and absent from the flock: a dog
/// that failed to start. Exercises [`crate::lookout::view::settings::dog_rows`]'s join, not
/// the toggle.
pub fn app_in_settings_with_dog_drift() -> App {
    let flock = vec![
        ProcessInfo::builder(90, "otel", ProcStatus::Online)
            .pid(Some(90_000))
            .dog(Some(DogSource::BuiltIn { grants: None }))
            .build(),
    ];
    let mut app = app_with(flock, plain());
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot_with_dogs(vec![
            // Real paths, not `None`: `otel` and `ledger` are adopted dogs,
            // and every value in `[daemon] adopted_dogs` is a path.
            DogView {
                name: "otel".to_string(),
                enabled: false,
                adopted_path: Some(PathBuf::from("/usr/local/bin/shep-otel")),
            },
            DogView {
                name: "ledger".to_string(),
                enabled: true,
                adopted_path: Some(PathBuf::from("/opt/ledger/bin/dog")),
            },
        ])),
    });
    app
}

/// `bark` is up but has never completed a handshake
/// (`handshook: Some(false)`), so [`crate::lookout::view::settings::dog_rows`] must read it
/// `silent`, not `online`, the same correction [`crate::vocabulary::Reported`]
/// makes for the flock table.
pub fn app_in_settings_with_silent_dog() -> App {
    let flock = vec![
        ProcessInfo::builder(91, "bark", ProcStatus::Online)
            .pid(Some(91_000))
            .dog(Some(DogSource::BuiltIn { grants: None }))
            .handshook(Some(false))
            .build(),
    ];
    let mut app = app_with(flock, plain());
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot_with_dogs(vec![DogView {
            name: "bark".to_string(),
            enabled: true,
            adopted_path: None,
        }])),
    });
    app
}

/// Two candidate dogs for the toggle tests: `metrics` disabled, `otel`
/// enabled, so a test can pick whichever direction it means to arm.
fn settings_snapshot_for_toggle_tests() -> SettingsSnapshot {
    settings_snapshot_with_dogs(vec![
        DogView {
            name: "metrics".to_string(),
            enabled: false,
            adopted_path: None,
        },
        DogView {
            name: "otel".to_string(),
            enabled: true,
            adopted_path: Some(PathBuf::from("/usr/local/bin/shep-otel")),
        },
    ])
}

/// A dashboard with the settings screen open on
/// [`settings_snapshot_for_toggle_tests`], the control gate open, and the
/// cursor moved onto `name`'s dog row by real `SelectDown` keypresses.
///
/// The tree rows always sort first in `Settings::rows`, so the dog at index
/// `i` of [`settings_snapshot_for_toggle_tests`]'s own list sits at the row
/// the `Dog(i)` variant names.
pub fn app_in_settings_on_dog(name: &str) -> App {
    let mut app = app_with(flock_of(3, 0), plain());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Settings));
    app.update(Msg::Settings {
        result: Ok(settings_snapshot_for_toggle_tests()),
    });
    let dog_index = settings_snapshot_for_toggle_tests()
        .dogs
        .iter()
        .position(|dog| dog.name == name)
        .expect("name is one of settings_snapshot_for_toggle_tests's own dogs");
    let target = app
        .settings()
        .unwrap()
        .rows()
        .into_iter()
        .position(|row| row == SettingsRow::Dog(dog_index))
        .expect("the dog is a row");
    for _ in 0..target {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
    app
}

/// [`app_in_settings_on_dog`], named for the test that means to start on an
/// already-enabled dog: same fixture, same mechanism, a name that reads
/// what the test is asserting on without checking the dogs list.
pub fn app_in_settings_on_enabled_dog(name: &str) -> App {
    app_in_settings_on_dog(name)
}

/// The edit that writes `value` at the dotted `key`, shaped as the tree
/// shapes that key.
///
/// # Panics
///
/// If `key` is not a row of the settings tree.
#[track_caller]
pub fn setting_set(key: &str, value: &str) -> SettingEdit {
    let shape = shep_toml_shape();
    let field = shape.fields.by_key(key).expect("key is a row");
    SettingEdit::Set {
        path: shape.paths[key].clone(),
        shape: Shape::of(&field.kind).expect("the key is typed in place"),
        value: value.to_string(),
    }
}

/// The edit that removes the dotted `key`.
///
/// # Panics
///
/// If `key` is not a row of the settings tree.
#[track_caller]
pub fn setting_unset(key: &str) -> SettingEdit {
    SettingEdit::Unset {
        path: shep_toml_shape().paths[key].clone(),
    }
}
