//! Fixtures the pane test modules share.

mod bleats;
mod close_dialog;
mod dog_pane;
mod flock;
mod host;
mod lambs;
mod palette;
mod refusals;
mod render;
mod settings;
mod sheep_pane;

pub use self::bleats::{
    app_fixture, bleats_pane_with_a_wide_line, bleats_pane_with_filters, bleats_pane_with_lines,
    bleats_pane_with_long_line, bleats_pane_with_mixed_line_lengths, draw_lines, line, with_feed,
    with_feed_and_palette, with_feed_and_selection,
};
pub use self::close_dialog::{
    app_with_close_dialog, close_dialog_reloading, close_dialog_with, close_dialog_with_live_edit,
    close_dialog_without_a_pid, draw_pane_with_dialog, draw_pane_with_dialog_and_palette,
    render_dialog,
};
pub use self::dog_pane::{app_in_dog_pane, app_in_dog_pane_with_two_edits, dog_section};
pub use self::flock::{
    acting_app, allowed_app, app_with, app_with_a_built_in_dog_selected_and_control,
    app_with_a_dog, app_with_a_dog_selected_and_control, armed_app,
    armed_app_with_a_filter_and_a_notice, editing_app, filtered_app, filtered_app_of, flock_of,
    full_app, instance_in_fold, sheep_in_fold, sheep_in_fold_with_status, sheep_with,
    with_no_selection, with_selection, with_selection_and_palette,
};
pub use self::host::{sample, with_host, with_host_none};
pub use self::lambs::{
    app_with_lamb_reading_at, lamb_line_of, sheep_with_lambs, with_lamb_reading,
    with_lamb_reading_for,
};
pub use self::palette::{coloured, no_color, plain, plain_dimmed};
pub use self::refusals::{a_refusal, invalid_config};
pub use self::render::{render, render_all, rendered, row_containing, row_starting_with, rows_of};
pub use self::settings::{
    app_in_settings, app_in_settings_at, app_in_settings_on, app_in_settings_on_dog,
    app_in_settings_on_enabled_dog, app_in_settings_with_control, app_in_settings_with_dog_drift,
    app_in_settings_with_shadowed_style, app_in_settings_with_silent_dog, settings_snapshot,
};
pub use self::sheep_pane::{
    app_in_sheep_pane, app_in_sheep_pane_on_a_draining_sheep, app_in_sheep_pane_on_a_stopped_sheep,
    app_in_sheep_pane_read_only, app_in_sheep_pane_with_a_parked_field,
    app_in_sheep_pane_with_control, app_in_sheep_pane_with_env,
    app_in_sheep_pane_with_nothing_parked, app_in_sheep_pane_with_one_edit,
    app_in_sheep_pane_with_two_edits, app_in_sheep_pane_with_two_parked_fields,
    app_with_plain_palette_in_sheep_pane, file_edit, select_env_key, select_field,
    sheep_config_view, type_into_the_open_editor,
};

use std::path::Path;
use std::time::Duration;

use ratatui::buffer::Buffer;

use crate::lookout::app::{App, Body, Control, Effect, KeyPress, Msg, RevealedValue};
use crate::lookout::secrets::{SecretRow, SecretsModel, Source};
use crate::secret_readers::Reader;

/// What the last dial said when the ladder ran out, in
/// `crate::lookout::source::LinkError::Unreachable`'s own shape.
///
/// The link panel renders it verbatim, so every test that freezes a
/// dashboard feeds it verbatim rather than inventing a shorter sentence the
/// panel would never see.
pub const FROZEN_WHY: &str = "the shepherd did not answer: could not connect to `/home/ada/.shep/run/shep.sock`: Connection refused (os error 61)";

/// The secrets pane, opened and loaded: `DB_PASSWORD` set for `production`
/// only, `ELSEWHERE_ONLY` set for `ci` only, `SET_EVERYWHERE` set for `all`,
/// `SET_IN_ALL_THREE` set for every environment slot including `all`, the
/// tab on `production` (the first load's own default, since `environment`
/// below is what it asks for).
pub fn app_with_secrets() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec![
                "all".to_string(),
                "ci".to_string(),
                "production".to_string(),
            ],
            rows: vec![
                SecretRow {
                    key: "DB_PASSWORD".to_string(),
                    source: Source::Operator,
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(9),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "ELSEWHERE_ONLY".to_string(),
                    source: Source::Operator,
                    in_force: None,
                    set_in: vec!["ci".to_string()],
                    byte_len: None,
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "SET_EVERYWHERE".to_string(),
                    source: Source::Operator,
                    in_force: Some("all".to_string()),
                    set_in: vec!["all".to_string()],
                    byte_len: Some(4),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "SET_IN_ALL_THREE".to_string(),
                    source: Source::Operator,
                    in_force: Some("all".to_string()),
                    set_in: vec![
                        "all".to_string(),
                        "ci".to_string(),
                        "production".to_string(),
                    ],
                    byte_len: Some(4),
                    readers: Vec::new(),
                },
            ],
            ..SecretsModel::default()
        })),
    });
    app
}

/// The secrets pane with one row whose value is exactly `MAX_VALUE_BYTES`
/// long, for the column-overflow test.
pub fn app_with_a_maximum_length_secret() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![SecretRow {
                key: "HUGE".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(shep_core::secrets::MAX_VALUE_BYTES),
                readers: Vec::new(),
            }],
            ..SecretsModel::default()
        })),
    });
    app
}

/// The secrets pane with one row from a provider's own cache, for the
/// read-only-group test.
pub fn app_with_a_pushed_secret() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![SecretRow {
                key: "vercel/API_TOKEN".to_string(),
                source: Source::Namespace("vercel".to_string()),
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(6),
                readers: Vec::new(),
            }],
            ..SecretsModel::default()
        })),
    });
    app
}

/// One operator row and one provider row, for the `+ new key` affordance's
/// own position test: it has to draw between the two groups, not just at
/// either end the way [`app_with_secrets`] (no namespace) or
/// [`app_with_a_pushed_secret`] (no operator row) alone would show.
pub fn app_with_secrets_and_a_provider_row() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![
                SecretRow {
                    key: "DB_PASSWORD".to_string(),
                    source: Source::Operator,
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(9),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "vercel/API_TOKEN".to_string(),
                    source: Source::Namespace("vercel".to_string()),
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(6),
                    readers: Vec::new(),
                },
            ],
            ..SecretsModel::default()
        })),
    });
    app
}

/// Operator, namespace, operator, in that order: `SecretsModel::rows` is
/// contiguous by source everywhere else in this pane, so this is the one
/// fixture that is not, for the test pinning `SecretsPane::new_key_anchor`
/// as the single computation both the cursor and the renderer read.
pub fn app_with_interleaved_secret_sources() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![
                SecretRow {
                    key: "FIRST_OPERATOR_KEY".to_string(),
                    source: Source::Operator,
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(1),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "vercel/API_TOKEN".to_string(),
                    source: Source::Namespace("vercel".to_string()),
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(6),
                    readers: Vec::new(),
                },
                SecretRow {
                    key: "SECOND_OPERATOR_KEY".to_string(),
                    source: Source::Operator,
                    in_force: Some("production".to_string()),
                    set_in: vec!["production".to_string()],
                    byte_len: Some(2),
                    readers: Vec::new(),
                },
            ],
            ..SecretsModel::default()
        })),
    });
    app
}

/// The value [`app_with_secrets_and_reads`] stores, and so the exact text a
/// reveal has to put on screen.
pub const REVEALED_VALUE: &str = "hunter2-not-really";

/// The secrets pane over a real store under `home`, `DB_PASSWORD` selected
/// and the reveal gate set by `allow_read`.
///
/// A file on disk rather than a hand-built model, because a reveal reads
/// its one value back out of the store: a fixture that only listed rows
/// could not exercise one. The caller owns `home` and has to keep it alive
/// for as long as the app.
pub fn app_with_secrets_and_reads(home: &Path, allow_read: bool) -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(secrets_model(home, allow_read))),
    });
    app
}

/// The model [`app_with_secrets_and_reads`] loads, and the store on disk it
/// reads from, so a test can load it a second time with the gate moved.
pub fn secrets_model(home: &Path, allow_read: bool) -> SecretsModel {
    let store = home.join("secrets.json");
    shep_core::secrets::set(&store, "DB_PASSWORD", "production", REVEALED_VALUE).unwrap();
    SecretsModel {
        environments: vec!["all".to_string(), "production".to_string()],
        rows: vec![
            SecretRow {
                key: "DB_PASSWORD".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(REVEALED_VALUE.len()),
                readers: Vec::new(),
            },
            // Never selected, so a test can hold one row against the other
            // and see that a reveal reaches exactly one of them.
            SecretRow {
                key: "OTHER_KEY".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(3),
                readers: Vec::new(),
            },
        ],
        allow_read,
        store,
        ..SecretsModel::default()
    }
}

/// One operator row, `DB_PASSWORD` set for `all`, over three environment
/// tabs (`all` first, so `TabNext` actually moves): the shared model behind
/// every set-a-value fixture below.
fn secrets_write_model() -> SecretsModel {
    SecretsModel {
        environments: vec![
            "all".to_string(),
            "ci".to_string(),
            "production".to_string(),
        ],
        rows: vec![SecretRow {
            key: "DB_PASSWORD".to_string(),
            source: Source::Operator,
            in_force: Some("all".to_string()),
            set_in: vec!["all".to_string()],
            byte_len: Some(9),
            readers: Vec::new(),
        }],
        ..SecretsModel::default()
    }
}

/// The secrets pane, opened and loaded on [`secrets_write_model`], read-only
/// (the default [`Control`]).
pub fn app_with_secrets_read_only() -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "all".to_string(),
        result: Ok(Box::new(secrets_write_model())),
    });
    app
}

/// [`app_with_secrets_read_only`] with the control gate open, for every
/// test that means to write.
pub fn app_with_secrets_and_control() -> App {
    let mut app = app_with_secrets_read_only();
    app.set_control_for_tests(Control::Allowed);
    app
}

/// [`app_with_secrets`] with the control gate open: a named tab
/// (`production`) holding rows whose value comes from the `all` slot, which
/// is the shape a delete has to refuse rather than retarget.
pub fn app_with_secrets_on_a_named_tab_and_control() -> App {
    let mut app = app_with_secrets();
    app.set_control_for_tests(Control::Allowed);
    app
}

/// [`app_with_secrets_and_control`] with `DB_PASSWORD`'s value input open:
/// `Enter` on the operator's only row, already selected by default.
pub fn app_typing_a_value() -> App {
    let mut app = app_with_secrets_and_control();
    app.update(Msg::Key(KeyPress::Confirm));
    app
}

/// [`app_with_secrets_and_control`] with `DB_PASSWORD`'s delete armed:
/// `D` on the operator's only row, already selected by default.
pub fn app_armed_to_delete_a_secret() -> App {
    let mut app = app_with_secrets_and_control();
    app.update(Msg::Key(KeyPress::SecretDelete));
    app
}

/// [`app_with_secrets_and_control`] with the `+ new key` row's name input
/// open: `G` lands on the trailing `+ new key` row, then `Enter` opens it.
pub fn app_typing_a_new_key() -> App {
    let mut app = app_with_secrets_and_control();
    app.update(Msg::Key(KeyPress::SelectLast));
    app.update(Msg::Key(KeyPress::Confirm));
    app
}

/// [`app_with_a_pushed_secret`] with the control gate open, for the test
/// proving a provider row refuses a write anyway.
pub fn app_with_a_pushed_secret_selected_and_control() -> App {
    let mut app = app_with_a_pushed_secret();
    app.set_control_for_tests(Control::Allowed);
    app
}

/// [`app_revealing`] with the control gate open, for the test proving a
/// successful write clears a reveal.
pub fn app_revealing_with_control() -> App {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_revealing(dir.path());
    app.set_control_for_tests(Control::Allowed);
    // Leaked deliberately: the fixture owns no `home` the caller can keep
    // alive, and the pane never touches the directory again once revealed.
    let _ = dir.keep();
    app
}

/// Presses `v` and does the read its effect asks for, handing back the
/// answer rather than applying it, so a test can move the pane underneath a
/// reveal that is still in flight.
///
/// # Panics
///
/// If `v` raised no read, since the caller is about to answer one.
#[track_caller]
pub fn ask_to_reveal(app: &mut App) -> Msg {
    let Effect::RevealSecret {
        store,
        provider_cache,
        row,
        environment,
    } = app.update(Msg::Key(KeyPress::Reveal))
    else {
        panic!("`v` over an open gate reads the store");
    };
    Msg::Revealed {
        key: row.key.clone(),
        environment,
        value: crate::lookout::secrets::stored_value(&store, &provider_cache, &row)
            .map(RevealedValue),
    }
}

/// The same pane with `DB_PASSWORD` already on screen, revealed the way an
/// operator reveals it: the keypress, the read it asks for, and the answer.
///
/// # Panics
///
/// If the reveal did not land, so a test asserting that some trigger clears
/// one cannot pass against an app that never revealed anything.
#[track_caller]
pub fn app_revealing(home: &Path) -> App {
    let mut app = app_with_secrets_and_reads(home, true);
    let answer = ask_to_reveal(&mut app);
    app.update(answer);
    assert!(
        matches!(app.body(), Body::Secrets(pane) if pane.reveal.is_some()),
        "the fixture starts with a value on screen"
    );
    app
}

/// A single operator row, `DB_PASSWORD`, selected by default, named by two
/// sheep: `catcher` online, `web` not. For the WHO READS IT panel's own
/// caption test.
fn app_with_one_row_and_readers(readers: Vec<Reader>) -> App {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![SecretRow {
                key: "DB_PASSWORD".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(9),
                readers,
            }],
            // Readers and the age both come off the muster roll, so a
            // fixture with one and not the other is a state the loader
            // cannot reach.
            roll_age: Some(Duration::from_secs(184)),
            ..SecretsModel::default()
        })),
    });
    app
}

/// The secrets pane rendered with `DB_PASSWORD` named by one online and one
/// offline reader.
pub fn render_secrets_with_readers() -> Buffer {
    let app = app_with_one_row_and_readers(vec![
        Reader {
            name: "catcher".to_string(),
            environment: "production".to_string(),
            online: true,
        },
        Reader {
            name: "web".to_string(),
            environment: "production".to_string(),
            online: false,
        },
    ]);
    render(&app, 160, 48)
}

/// The secrets pane rendered with `DB_PASSWORD` named by five readers, more
/// than WHO READS IT has rows for. For the overflow line's own test.
pub fn render_secrets_with_more_readers_than_fit() -> Buffer {
    let app = app_with_one_row_and_readers(
        ["catcher", "web", "worker", "api", "scheduler"]
            .into_iter()
            .map(|name| Reader {
                name: name.to_string(),
                environment: "production".to_string(),
                online: true,
            })
            .collect(),
    );
    render(&app, 160, 48)
}

/// The secrets pane rendered with `[secrets] allow_read` off, the default
/// [`SecretsModel::allow_read`].
pub fn render_secrets_gate_shut() -> Buffer {
    render(&app_with_secrets(), 160, 48)
}

/// The secrets pane rendered with the muster roll `age` old.
pub fn render_secrets_with_roll_age(age: Duration) -> Buffer {
    let mut app = full_app();
    app.update(Msg::Key(KeyPress::Secrets));
    app.update(Msg::Secrets {
        environment: "production".to_string(),
        result: Ok(Box::new(SecretsModel {
            environments: vec!["production".to_string()],
            rows: vec![SecretRow {
                key: "DB_PASSWORD".to_string(),
                source: Source::Operator,
                in_force: Some("production".to_string()),
                set_in: vec!["production".to_string()],
                byte_len: Some(9),
                readers: Vec::new(),
            }],
            roll_age: Some(age),
            ..SecretsModel::default()
        })),
    });
    render(&app, 160, 48)
}

/// The secrets pane rendered with no muster roll at all, [`SecretsModel::roll_age`]
/// still `None`: the state the roll status line has to tell apart from a key
/// nothing reads.
///
/// The same screen [`render_secrets_gate_shut`] draws, since
/// `SecretsModel::default` shuts the gate and carries no roll at once. One
/// expression, two names, each saying which half its own test is about.
pub fn render_secrets_with_no_roll() -> Buffer {
    render_secrets_gate_shut()
}

/// The active group's own field rows, as their key names: a bounded slice
/// of the config pane's state rather than a search over the rendered
/// frame, which is what keeps a test on this from passing off a match in
/// the legend or another section.
///
/// # Panics
///
/// Panics if the pane is closed, which is a fixture bug rather than a
/// failure the test is about.
#[track_caller]
pub fn config_pane_field_rows_for_tests(app: &App) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    pane.rows()
        .into_iter()
        .filter_map(|row| match row {
            crate::lookout::pane::PaneRow::Field(index) => {
                Some(pane.fields().fields()[index].key.clone())
            }
            crate::lookout::pane::PaneRow::Env(_) | crate::lookout::pane::PaneRow::AddEnv => None,
        })
        .collect()
}

/// The active group's own env rows: one entry per env key, then
/// `+ add a key`. Searched below the `env` section header and nowhere else,
/// so a test on this cannot pass off a match from the field list above it:
/// the keys and the field names share one namespace on screen, and a sheep
/// has fields called `user` and `env`.
///
/// Takes a `ConfigPane` directly rather than an `App`, since some of this
/// pane's own tests build one without a dashboard around it.
///
/// # Panics
///
/// Panics if it draws no row for a key or for `+ add a key`, which is a
/// fixture bug rather than a failure the test is about.
#[track_caller]
pub fn config_pane_env_rows_for_tests(pane: &crate::lookout::pane::ConfigPane) -> Vec<String> {
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), 160, 0);
    let rendered_lines: Vec<String> = lines.iter().map(rendered).collect();
    // The `env` section header is the bound. Everything above it is a field
    // row or chrome, and a prefix match over the whole frame would hand back
    // the `user` field's row for an env key named `user`.
    //
    // The header sits at column 2 and every row under it at column 3, which
    // is what tells the header apart from a field called `env`. It is not
    // matched whole because the explanation panel is merged to the right of
    // it at this width, so the line carries the panel's own row too.
    let header = rendered_lines
        .iter()
        .position(|line| {
            line.strip_prefix("  env")
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
        .expect("the pane draws an env section header");
    let env_rows = &rendered_lines[header + 1..];
    let mut rows: Vec<String> = pane
        .env_key_names()
        .iter()
        .map(|name| {
            env_rows
                .iter()
                .find(|line| {
                    line.trim_start_matches(['>', ' '])
                        .starts_with(name.as_str())
                })
                .unwrap_or_else(|| panic!("no row for env key {name}"))
                .clone()
        })
        .collect();
    rows.push(
        env_rows
            .iter()
            .find(|line| line.contains("add a key"))
            .expect("the pane draws a + add a key row")
            .clone(),
    );
    rows
}

/// Every filed edit's own key, as [`config_pane_field_rows_for_tests`] does
/// for the active group's own fields: a bounded slice of the pane's own
/// edit set, never a search over the rendered frame.
///
/// # Panics
///
/// Panics if the pane is closed.
#[track_caller]
pub fn config_pane_pending_rows_for_tests(app: &App) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    pane.edits()
        .iter()
        .map(|(key, _)| match key {
            crate::lookout::edits::EditKey::Field(name) => name.clone(),
            crate::lookout::edits::EditKey::Env(name) => format!("env.{name}"),
        })
        .collect()
}

/// The one rendered line naming `key`, wherever it draws: the active
/// group's own field row, or the pending-edits section when `key` belongs
/// to a group not on screen. Bounded to that single row by stripping the
/// mark, lock and flag columns and requiring what is left to start with
/// `key`, which is what keeps this from matching a longer key or the
/// legend.
///
/// # Panics
///
/// Panics if the pane is closed or draws no row for `key`.
#[track_caller]
pub fn config_pane_row_for_tests(app: &App, key: &str) -> String {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), 160, 0);
    lines
        .iter()
        .map(rendered)
        .find(|line| {
            line.trim_start_matches(['>', ' ', '=', '~', '!', '*'])
                .starts_with(key)
        })
        .unwrap_or_else(|| panic!("no row for {key}"))
}

/// The pane's own title band, alone: line zero of the rendered frame, the
/// one line [`crate::lookout::view::pane::pane_lines`] ever puts the edit
/// count in.
///
/// # Panics
///
/// Panics if the pane is closed.
#[track_caller]
pub fn config_pane_title_band_for_tests(app: &App, width: u16) -> String {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    rendered(&lines[0])
}

/// The pane's own tab row, alone: the one line naming every group in
/// [`shep_core::config::GROUP_ORDER`], found by its own `tab next group`
/// phrase rather than by a fixed index, so a chrome line gained or lost
/// above it does not silently move which row this reads.
///
/// # Panics
///
/// Panics if the pane is closed or draws no tab row at `width`.
#[track_caller]
pub fn config_pane_tab_row_for_tests(app: &App, width: u16) -> String {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    lines
        .iter()
        .map(rendered)
        .find(|line| line.contains("tab next group"))
        .expect("the pane draws a tab row at this width")
}

/// Whether the pane draws a tab row at `width`, without panicking when it
/// does not: the non-panicking half of [`config_pane_tab_row_for_tests`],
/// for a caller (a dog pane, which has no groups) asserting the row's
/// absence rather than reading its content.
pub fn config_pane_draws_a_tab_row(app: &App, width: u16) -> bool {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    lines
        .iter()
        .map(rendered)
        .any(|line| line.contains("tab next group"))
}

/// Whether the pane's own merged frame includes the explanation panel at
/// `width`: the wiring question `pane_lines` answers by width alone, which
/// [`config_pane_panel_for_tests`] cannot: that helper calls `panel_lines`
/// directly, and `panel_lines` draws unconditionally, carrying none of
/// `pane_lines`' own decision about whether the terminal is wide enough to
/// show it at all.
pub fn config_pane_draws_a_panel(app: &App, width: u16) -> bool {
    let pane = app.config_pane().expect("the pane is open");
    let lines = crate::lookout::view::pane::pane_lines(pane, plain(), width, 0);
    lines
        .iter()
        .map(rendered)
        .any(|line| line.contains("FOCUSED"))
}

/// The explanation panel for whichever field the pane's own cursor is on,
/// as plain rows: what [`crate::lookout::view::pane::panel_lines`] draws,
/// styles dropped, at the app's own palette.
///
/// # Panics
///
/// Panics if the pane is closed.
#[track_caller]
pub fn config_pane_panel_for_tests(app: &App, width: u16) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    crate::lookout::view::pane::panel_lines(pane, app.palette(), width)
        .iter()
        .map(rendered)
        .collect()
}

/// The explanation panel for the field named `key`, regardless of where the
/// pane's own cursor sits: a bounded look at one field's own panel content
/// rather than a walk that would first have to move the cursor there.
///
/// # Panics
///
/// Panics if the pane is closed or has no field named `key`.
#[track_caller]
pub fn config_pane_panel_focused_on(app: &App, key: &str, width: u16) -> Vec<String> {
    let pane = app.config_pane().expect("the pane is open");
    let field = pane
        .fields()
        .by_key(key)
        .unwrap_or_else(|| panic!("no field named {key}"));
    crate::lookout::view::pane::panel_for_field(field, pane, app.palette(), width)
        .iter()
        .map(rendered)
        .collect()
}
