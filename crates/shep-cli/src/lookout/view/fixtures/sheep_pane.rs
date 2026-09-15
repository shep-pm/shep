//! A sheep's config pane: the view behind it, and the keys that drive it.

use shep_core::config::AppConfig;
use shep_core::protocol::{ProcessInfo, Response, SheepConfigView};
use shep_core::status::ProcStatus;

use crate::lookout::app::{App, Control, KeyPress, Msg, Sent};

use super::flock::with_selection;

/// One sheep's config as the shepherd would answer it: `web`, with two
/// fields an operator has overridden, one parked until a respawn, and two
/// env keys whose values the view never carries.
pub fn sheep_config_view() -> SheepConfigView {
    sheep_config_view_parking(vec!["kill_signal".to_string()])
}

/// [`sheep_config_view`] with `pending` in place of the one field it parks.
pub(super) fn sheep_config_view_parking(pending: Vec<String>) -> SheepConfigView {
    let mut config = AppConfig {
        name: "web".to_string(),
        script: "./srv".to_string(),
        args: vec!["--port".to_string(), "8080".to_string()],
        max_restarts: 32,
        instances: 3,
        ..AppConfig::default()
    };
    config
        .env
        .insert("DB_HOST".to_string(), "db.internal".to_string());
    config
        .env
        .insert("LOG_LEVEL".to_string(), "debug".to_string());
    SheepConfigView::new(
        config,
        vec!["max_restarts".to_string(), "reuse_port".to_string()],
        pending,
    )
}

/// A sheep pane, control open, with exactly the env keys named. Values are
/// what the fixture's own caller reads to know what it asked for; no value
/// for any key ever reaches the pane itself, since `SheepConfigView::new`
/// strips them before the struct is built.
pub fn app_in_sheep_pane_with_env(env: &[(&str, &str)]) -> App {
    let mut app = with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    let mut config = AppConfig {
        name: "web".to_string(),
        ..AppConfig::default()
    };
    for (key, value) in env {
        config.env.insert((*key).to_string(), (*value).to_string());
    }
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(SheepConfigView::new(
            config,
            Vec::new(),
            Vec::new(),
        )))),
    });
    app
}

/// Walks an open config pane's cursor onto the env row named `key`, the
/// way [`select_field`] walks it onto a field: no fixture reaches into the
/// pane to place it.
///
/// # Panics
///
/// Panics if the pane is closed or has no env key by that name, which is a
/// fixture bug rather than a failure the test is about.
#[track_caller]
pub fn select_env_key(app: &mut App, key: &str) {
    let pane = app.config_pane().expect("the pane is open");
    let index = pane
        .rows()
        .iter()
        .position(|row| match row {
            crate::lookout::pane::PaneRow::Env(env_index) => {
                pane.env_key_names().get(*env_index).map(String::as_str) == Some(key)
            }
            crate::lookout::pane::PaneRow::Field(_) | crate::lookout::pane::PaneRow::AddEnv => {
                false
            }
        })
        .unwrap_or_else(|| panic!("no env key named {key}"));
    app.update(Msg::Key(KeyPress::SelectFirst));
    for _ in 0..index {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
}

/// Opens the editor on whatever row the cursor is already on, types
/// `text`, and applies it: `Confirm` then a character at a time then
/// `TextApply`, the way an operator drives either editor this pane opens.
pub fn type_into_the_open_editor(app: &mut App, text: &str) {
    app.update(Msg::Key(KeyPress::Confirm));
    for character in text.chars() {
        app.update(Msg::Key(KeyPress::TextChar(character)));
    }
    app.update(Msg::Key(KeyPress::TextApply));
}

/// [`app_in_sheep_pane`] with the control gate open: the pane can write.
///
/// The gate is set BEFORE the pane opens, so nothing about how it opened
/// depends on it, which is what makes a read-only refusal and a permitted
/// write comparable frames.
pub fn app_in_sheep_pane_with_control() -> App {
    let mut app = with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view()))),
    });
    app
}

/// [`app_in_sheep_pane_with_control`], named for the tests about the apply
/// menu: [`sheep_config_view`] parks `kill_signal` and nothing else.
pub fn app_in_sheep_pane_with_a_parked_field() -> App {
    app_in_sheep_pane_with_control()
}

/// The same pane over a sheep whose config parks two fields.
pub fn app_in_sheep_pane_with_two_parked_fields() -> App {
    app_in_sheep_pane_parking(vec!["kill_signal".to_string(), "script".to_string()])
}

/// The same pane over a sheep the running process is fully caught up with.
pub fn app_in_sheep_pane_with_nothing_parked() -> App {
    app_in_sheep_pane_parking(Vec::new())
}

/// [`app_in_sheep_pane_with_control`] over a view parking `pending`.
fn app_in_sheep_pane_parking(pending: Vec<String>) -> App {
    let mut app = with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view_parking(
            pending,
        )))),
    });
    app
}

/// A dashboard with `web` selected and its config pane open, opened the way
/// the event loop opens it: `e`, then the shepherd's own reply.
///
/// Read-only by default, since nothing here calls
/// `set_control_for_tests(Control::Allowed)`: the tests this fixture backs
/// are about reading and about the closed gate. [`file_edit`] opens the
/// gate itself for the tests that need to file an edit here.
pub fn app_in_sheep_pane() -> App {
    let mut app = with_selection(
        ProcessInfo::builder(9, "web", ProcStatus::Online)
            .pid(Some(48_000))
            .build(),
    );
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view()))),
    });
    app
}

/// [`app_in_sheep_pane`], explicit about the gate rather than reading it
/// off that fixture's own default: a read-only pane is the one fact a test
/// on this cares about, and this name says so without depending on
/// `app_in_sheep_pane`'s default staying what it is today.
pub fn app_in_sheep_pane_read_only() -> App {
    let mut app = app_in_sheep_pane();
    app.set_control_for_tests(Control::ReadOnly);
    app
}

/// [`app_in_sheep_pane`], over a sheep the shepherd reports stopped: nothing
/// holds the old config, so the close dialog's `R`/`L` half has nothing to
/// offer and `esc` never asks about a respawn. Nothing parked either, since
/// a stopped sheep has no running process for the shepherd to have parked
/// a write against.
pub fn app_in_sheep_pane_on_a_stopped_sheep() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Stopped).build());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view_parking(
            Vec::new(),
        )))),
    });
    app
}

/// [`app_in_sheep_pane`], over a sheep the shepherd reports `Stopping`: its
/// drainee is going away and is not a restart target
/// ([`ProcStatus::Stopping`]'s own doc), so this is the other half of
/// "not running" `App::sheep_is_running` excludes, alongside `Stopped`.
pub fn app_in_sheep_pane_on_a_draining_sheep() -> App {
    let mut app = with_selection(ProcessInfo::builder(9, "web", ProcStatus::Stopping).build());
    app.set_control_for_tests(Control::Allowed);
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "web".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(sheep_config_view_parking(
            Vec::new(),
        )))),
    });
    app
}

/// Files an edit for `key` through the real keys an operator would press:
/// select it, open the editor, replace the buffer with `value`, apply.
///
/// Grants control first: filing an edit is not what a read-only test is
/// about, and every caller of this fixture wants the edit to land.
pub fn file_edit(app: &mut App, key: &str, value: &str) {
    app.set_control_for_tests(Control::Allowed);
    select_field(app, key);
    app.update(Msg::Key(KeyPress::Confirm));
    for _ in 0..64 {
        app.update(Msg::Key(KeyPress::TextBackspace));
    }
    for character in value.chars() {
        app.update(Msg::Key(KeyPress::TextChar(character)));
    }
    app.update(Msg::Key(KeyPress::TextApply));
}

/// Walks an open config pane's cursor onto `key`, the way an operator
/// walks it: `tab`/a digit onto `key`'s own group first, when it carries
/// one, then down the filtered list onto the row itself. No fixture
/// reaches into the pane to place it.
///
/// Panics if the pane is closed or has no field by that name, which is a
/// fixture bug rather than a failure the test is about.
pub fn select_field(app: &mut App, key: &str) {
    let pane = app.config_pane().expect("the pane is open");
    let group = pane
        .fields()
        .by_key(key)
        .unwrap_or_else(|| panic!("no field named {key}"))
        .group
        .clone();
    if let Some(group) = group
        && let Some(position) = shep_core::config::GROUP_ORDER
            .iter()
            .position(|known| *known == group)
    {
        let digit = u8::try_from(position + 1).expect("eight groups fit a u8");
        app.update(Msg::Key(KeyPress::Group(digit)));
    }
    let pane = app.config_pane().expect("the pane is open");
    let index = pane
        .rows()
        .iter()
        .position(|row| match row {
            crate::lookout::pane::PaneRow::Field(field_index) => {
                pane.fields().fields()[*field_index].key == key
            }
            crate::lookout::pane::PaneRow::Env(_) | crate::lookout::pane::PaneRow::AddEnv => false,
        })
        .unwrap_or_else(|| panic!("{key} is not in the active group's rows"));
    app.update(Msg::Key(KeyPress::SelectFirst));
    for _ in 0..index {
        app.update(Msg::Key(KeyPress::SelectDown));
    }
}

/// [`app_in_sheep_pane_with_nothing_parked`] with two edits filed, driven
/// by real key presses: `cwd` typed, and `max_memory` typed.
///
/// Two, and not one, because a batch of one cannot tell a loop from a
/// `take(1)`. Two different fields rather than two shapes of edit, since
/// the set is keyed by field and a second edit to one key replaces it.
/// The two also carry different groups on purpose (`cwd` is `process`,
/// `max_memory` is `restart`), which is what the pending-edits section's
/// own tests need: a group's own field list only ever shows one group at
/// a time, so a test that an edit from elsewhere still turns up has to
/// file one there.
///
/// Nothing parked, so `Escape` writes and leaves rather than stopping to
/// offer the apply menu.
pub fn app_in_sheep_pane_with_two_edits() -> App {
    let mut app = app_in_sheep_pane_with_nothing_parked();
    for (key, typed) in [("cwd", "/srv/web"), ("max_memory", "40")] {
        select_field(&mut app, key);
        app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..64 {
            app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for character in typed.chars() {
            app.update(Msg::Key(KeyPress::TextChar(character)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
    }
    assert_eq!(
        app.config_pane().expect("the pane is open").edits().len(),
        2,
        "the fixture files two edits"
    );
    app
}

/// [`app_in_sheep_pane_with_nothing_parked`] with `cwd` alone filed: one
/// write, so a whole-batch refusal has exactly one ticket to refuse.
pub fn app_in_sheep_pane_with_one_edit() -> App {
    let mut app = app_in_sheep_pane_with_nothing_parked();
    file_edit(&mut app, "cwd", "/srv/web");
    assert_eq!(
        app.config_pane().expect("the pane is open").edits().len(),
        1,
        "the fixture files one edit"
    );
    app
}

/// [`app_in_sheep_pane`], named for the one test that cares the palette
/// carries no colour. [`app_in_sheep_pane`] already builds on [`plain`], so
/// this alias adds no behaviour; it exists to make that guarantee visible
/// at the call site rather than left implicit in a fixture named for
/// something else.
pub fn app_with_plain_palette_in_sheep_pane() -> App {
    app_in_sheep_pane()
}
