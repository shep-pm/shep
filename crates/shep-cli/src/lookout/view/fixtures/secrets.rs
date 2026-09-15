//! The secrets pane: its models, its reveals, and its rendered frames.

use std::path::Path;
use std::time::Duration;

use ratatui::buffer::Buffer;

use crate::lookout::app::{App, Body, Control, Effect, KeyPress, Msg, RevealedValue};
use crate::lookout::secrets::{SecretRow, SecretsModel, Source};
use crate::secret_readers::Reader;

use super::flock::full_app;
use super::render::render;

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
