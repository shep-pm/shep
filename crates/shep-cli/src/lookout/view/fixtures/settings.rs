//! The settings screen, its snapshot, and its dogs table.

use std::path::PathBuf;
use std::time::Instant;

use shep_core::protocol::{DogSource, ProcessInfo};
use shep_core::status::ProcStatus;

use crate::commands::settings::{DogView, ScalarView, SettingField, SettingsSnapshot};
use crate::lookout::app::{App, Control, KeyPress, Msg, SettingsRow};
use crate::style::StyleSource;

use super::flock::{app_with, flock_of};
use super::palette::plain;

/// A plausible settings snapshot: every scalar rendered as if `shep.toml`
/// declared it, and two candidate dogs, one enabled, for the tests that
/// need a screen with real rows rather than a fresh home's all-default one.
pub fn settings_snapshot() -> SettingsSnapshot {
    let config = |value: &str| ScalarView {
        value: value.to_string(),
        source: StyleSource::Config,
    };
    SettingsSnapshot {
        log_level: config("warn"),
        log_json: config("false"),
        socket: config("/home/ada/.shep/run/shep.sock"),
        max_cron_sleep: config("30s"),
        allow_control: config("false"),
        style_level: config("full"),
        // The document declares it, so the file and the resolved value
        // agree.
        style_level_in_file: Some("full".to_string()),
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

/// [`app_in_settings_with_control`] with the cursor already moved onto
/// `field`'s row, by real `SelectDown` keypresses rather than poking the
/// cursor index directly.
pub fn app_in_settings_on(field: SettingField) -> App {
    let mut app = app_in_settings_with_control();
    let target = app
        .settings()
        .unwrap()
        .rows()
        .iter()
        .position(|row| *row == SettingsRow::Scalar(field))
        .expect("field is one of the six scalar rows Settings::rows always carries");
    for _ in 0..target {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
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
/// The one state where a scalar's value in force and its value on disk
/// disagree. Every other field's layers belong to the shepherd's process,
/// where lookout can see neither.
pub fn app_in_settings_with_shadowed_style(source: StyleSource) -> App {
    let mut app = app_in_settings_on(SettingField::StyleLevel);
    let mut snapshot = settings_snapshot();
    snapshot.style_level = ScalarView {
        value: "bare".to_string(),
        source,
    };
    snapshot.style_level_in_file = Some("full".to_string());
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

/// [`settings_snapshot`]'s own scalars, with `dogs` replaced, for the
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
            .dog(Some(DogSource::BuiltIn))
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
            .dog(Some(DogSource::BuiltIn))
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
/// The six scalar rows always sort first in `Settings::rows`, so the dog at
/// index `i` of [`settings_snapshot_for_toggle_tests`]'s own list sits at
/// row `6 + i`.
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
    for _ in 0..(6 + dog_index) {
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
