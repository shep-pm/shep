use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

#[cfg(all(test, unix))]
use std::path::Path;

use shep_core::paths::ShepPaths;

use super::*;
use crate::commands::dogs::declared_review;
use crate::dog_index::{AvailableDog, DogSourceKind};
use crate::lookout::app::modal::{CUT_MARK, OUTPUT_CAP};
#[cfg(all(test, unix))]
use crate::lookout::testing::{is_gone, sleeping_cargo};
use crate::lookout::view::fixtures;

/// A dashboard with the gate open and one catalogue row landed.
fn app_with_row(dog: AvailableDog, state: DogState) -> App {
    app_with_version(dog, "0.2.4", state)
}

/// [`app_with_row`] with the row's VERSION cell as given.
fn app_with_version(dog: AvailableDog, version: &str, state: DogState) -> App {
    let t0 = Instant::now();
    let mut app = App::new(
        crate::lookout::theme::Palette::detect(None, None, None),
        Control::Allowed,
        "/home/ada/.shep".to_string(),
        t0,
    );
    app.update(Msg::Snapshot {
        rows: fixtures::flock_of(2, 1),
        at: t0,
    });
    app.open_catalogue();
    app.update(Msg::Catalogue {
        result: Ok(Box::new(CatalogueRead {
            rows: vec![CatalogueRow {
                dog,
                version: version.to_owned(),
                state,
            }],
            refreshing: false,
        })),
    });
    app
}

fn cargo_dog(name: &str) -> AvailableDog {
    AvailableDog {
        name: name.to_owned(),
        package: format!("shep-{name}"),
        adopt_as: name.to_owned(),
        description: format!("{name} watches the flock and says so."),
        repo: format!("https://github.com/shep-pm/shep-{name}"),
        license: "MIT".to_owned(),
        category: "metrics".to_owned(),
        source: DogSourceKind::Cargo {
            version: Some("0.2.4".to_owned()),
            sha256: BTreeMap::new(),
        },
        permissions: None,
        needs: Vec::new(),
    }
}

/// A dashboard with `otel` mid-install, and the instant its clock started at.
fn installing() -> (App, Instant) {
    let t0 = Instant::now();
    let mut app = App::new(
        crate::lookout::theme::Palette::detect(None, None, None),
        Control::Allowed,
        "/home/ada/.shep".to_string(),
        t0,
    );
    assert!(matches!(
        app.start_install("otel"),
        Effect::StartDogJob { .. }
    ));
    (app, t0)
}

/// The question `shep install` would print for a dog declaring `flock.read`.
fn prompt(name: &str) -> Prompt {
    Prompt::for_review(
        name,
        "install",
        &declared_review(&[("flock.read", true, "reads the flock to report on it")]),
        None,
    )
    .expect("a declared permission is a question")
}

/// The tag of the open grant question, or `None`.
fn grant_tag(app: &App) -> Option<u64> {
    match app.modal() {
        Some(Modal::Grant { tag, .. }) => Some(*tag),
        _ => None,
    }
}

/// The question the open grant modal is asking.
#[track_caller]
fn grant_question(app: &App) -> String {
    match app.modal() {
        Some(Modal::Grant { prompt, .. }) => prompt.question().to_owned(),
        other => panic!("no grant modal is open: {other:?}"),
    }
}

/// The tag of the open modal.
#[track_caller]
fn open_tag(app: &App) -> u64 {
    app.modal().map(Modal::tag).expect("a modal is open")
}

#[test]
fn an_adopted_entry_offers_install_but_never_upgrade() {
    let mut app = app_with_version(cargo_dog("otel"), "0.2.1 \u{2192} 0.2.4", DogState::Adopted);
    assert!(app.can_install(&app.catalogue_rows()[0]));
    assert!(!app.can_upgrade(&app.catalogue_rows()[0]));
    app.update(Msg::Key(KeyPress::Confirm));
    assert!(matches!(
        app.update(Msg::Key(KeyPress::Confirm)),
        Effect::StartDogJob {
            job: Job::Install { .. },
            ..
        }
    ));
    app.set_control_for_tests(Control::ReadOnly);
    assert!(!app.can_install(&app.catalogue_rows()[0]));
}

#[test]
fn enter_on_an_installable_entry_starts_the_job_and_a_second_waits_for_it() {
    let mut app = app_with_row(cargo_dog("otel"), DogState::NotInstalled);
    app.update(Msg::Key(KeyPress::Confirm));
    assert!(matches!(app.body(), Body::Catalogue(pane) if pane.entry.as_deref() == Some("otel")));

    let authority = WriteAuthority::granted(&app).expect("the gate is open");
    assert_eq!(
        app.update(Msg::Key(KeyPress::Confirm)),
        Effect::StartDogJob {
            job: Job::Install {
                adopt_as: "otel".to_owned(),
            },
            authority,
        }
    );
    assert_eq!(
        app.dog_job_line().as_deref(),
        Some("installing otel  starting  0s")
    );

    assert_eq!(app.start_install("rotate"), Effect::None, "nothing started");
    match app.modal() {
        Some(Modal::Output { title, text, .. }) => {
            assert_eq!(title.as_str(), "installing");
            assert!(
                text.as_str().contains("otel is still installing"),
                "{text:?}"
            );
        }
        other => panic!("expected the waiting modal, got {other:?}"),
    }
}

#[test]
fn an_installed_or_uninstallable_entry_starts_nothing() {
    let mut installed = app_with_row(cargo_dog("otel"), DogState::Installed);
    installed.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(installed.update(Msg::Key(KeyPress::Confirm)), Effect::None);
    assert_eq!(installed.dog_job_line(), None);

    let mut manual = app_with_row(
        AvailableDog {
            source: DogSourceKind::Manual {
                instructions: "Download a release from the repo.".to_owned(),
            },
            ..cargo_dog("otel")
        },
        DogState::NotInstalled,
    );
    manual.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(manual.update(Msg::Key(KeyPress::Confirm)), Effect::None);
    assert_eq!(manual.dog_job_line(), None);
}

#[test]
fn enter_on_a_row_behind_the_index_starts_an_upgrade_and_on_a_current_one_nothing() {
    let mut behind = app_with_version(cargo_dog("otel"), "0.2.1 \u{2192} 0.2.4", DogState::Enabled);
    behind.update(Msg::Key(KeyPress::Confirm));
    let authority = WriteAuthority::granted(&behind).expect("the gate is open");
    assert_eq!(
        behind.update(Msg::Key(KeyPress::Confirm)),
        Effect::StartDogJob {
            job: Job::Upgrade {
                name: "otel".to_owned(),
            },
            authority,
        }
    );
    assert_eq!(
        behind.dog_job_line().as_deref(),
        Some("upgrading otel  starting  0s")
    );

    let mut current = app_with_version(cargo_dog("otel"), "0.2.4", DogState::Installed);
    current.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(current.update(Msg::Key(KeyPress::Confirm)), Effect::None);
    assert_eq!(current.dog_job_line(), None);
}

/// An entry open on an installed `otel`, with the gate open.
fn installed_entry() -> App {
    let mut app = app_with_version(cargo_dog("otel"), "0.2.4", DogState::Enabled);
    app.update(Msg::Key(KeyPress::Confirm));
    app
}

#[test]
fn d_on_an_installed_entry_asks_and_a_yes_starts_the_delete() {
    let mut app = installed_entry();
    assert_eq!(app.update(Msg::Key(KeyPress::ArmDelete)), Effect::None);
    match app.modal() {
        Some(Modal::Confirm { question, .. }) => assert_eq!(
            question.as_str(),
            "Delete otel? An adopted dog with no receipt keeps its file."
        ),
        other => panic!("expected the delete question, got {other:?}"),
    }
    assert_eq!(app.dog_job_line(), None, "nothing runs before the answer");

    app.note_modal_reach(Some(0));
    let authority = WriteAuthority::granted(&app).expect("the gate is open");
    assert_eq!(
        app.update(Msg::Key(KeyPress::Confirm)),
        Effect::StartDogJob {
            job: Job::Delete {
                name: "otel".to_owned(),
            },
            authority,
        }
    );
    assert_eq!(
        app.dog_job_line().as_deref(),
        Some("deleting otel  starting  0s")
    );
}

#[test]
fn a_no_to_the_delete_question_starts_nothing() {
    let mut app = installed_entry();
    app.update(Msg::Key(KeyPress::ArmDelete));
    app.note_modal_reach(Some(0));

    assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
    assert!(app.modal().is_none(), "the question closed");
    assert_eq!(app.dog_job_line(), None);
}

#[test]
fn d_on_an_uninstalled_entry_or_in_the_list_asks_nothing() {
    let mut uninstalled = app_with_row(cargo_dog("otel"), DogState::NotInstalled);
    uninstalled.update(Msg::Key(KeyPress::Confirm));
    assert_eq!(
        uninstalled.update(Msg::Key(KeyPress::ArmDelete)),
        Effect::None
    );
    assert!(uninstalled.modal().is_none());

    let mut list = app_with_row(cargo_dog("otel"), DogState::Installed);
    assert_eq!(list.update(Msg::Key(KeyPress::ArmDelete)), Effect::None);
    assert!(list.modal().is_none(), "the list has no delete");
}

#[test]
fn a_built_in_dogs_entry_has_no_delete_action() {
    let mut app = app_with_row(cargo_dog("metrics"), DogState::Enabled);
    app.update(Msg::Key(KeyPress::Confirm));

    assert_eq!(app.update(Msg::Key(KeyPress::ArmDelete)), Effect::None);
    assert!(app.modal().is_none(), "no question for a built-in dog");
}

#[test]
fn a_closed_gate_refuses_the_upgrade_and_the_delete() {
    let mut app = app_with_version(
        cargo_dog("otel"),
        "0.2.1 \u{2192} 0.2.4",
        DogState::Installed,
    );
    app.set_control_for_tests(Control::ReadOnly);
    app.update(Msg::Key(KeyPress::Confirm));

    assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
    assert_eq!(app.update(Msg::Key(KeyPress::ArmDelete)), Effect::None);
    assert!(app.modal().is_none());
    assert_eq!(app.dog_job_line(), None);
}

#[test]
fn a_delete_asked_while_a_job_runs_waits_for_it() {
    let mut app = installed_entry();
    assert!(matches!(
        app.start_install("rotate"),
        Effect::StartDogJob { .. }
    ));
    app.update(Msg::Key(KeyPress::ArmDelete));
    app.note_modal_reach(Some(0));

    assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
    match app.modal() {
        Some(Modal::Output { title, text, .. }) => {
            assert_eq!(title.as_str(), "deleting");
            assert!(
                text.as_str().contains("rotate is still installing"),
                "{text:?}"
            );
        }
        other => panic!("expected the waiting modal, got {other:?}"),
    }
}

#[test]
fn a_landed_upgrade_or_delete_names_what_it_did_and_rereads_the_rows() {
    let behind = "0.2.1 \u{2192} 0.2.4";
    // The row a real job leaves: an upgrade catches the version up and keeps
    // the dog enabled, a delete leaves the index's own version, not installed.
    for (job, started, said, version, state) in [
        (
            Job::Upgrade {
                name: "otel".to_owned(),
            },
            "upgrading otel  starting  0s",
            "upgraded otel",
            "0.2.4",
            DogState::Enabled,
        ),
        (
            Job::Delete {
                name: "otel".to_owned(),
            },
            "deleting otel  starting  0s",
            "deleted otel",
            "0.2.4",
            DogState::NotInstalled,
        ),
    ] {
        let mut app = app_with_version(cargo_dog("otel"), behind, DogState::Enabled);
        app.update(Msg::Key(KeyPress::Confirm));
        let Effect::StartDogJob { .. } = app.start_job(job) else {
            panic!("the job did not start");
        };
        assert_eq!(app.dog_job_line().as_deref(), Some(started));

        let effect = app.update(Msg::DogJobDone {
            name: "otel".to_owned(),
            ok: true,
            output: String::new(),
        });
        assert_eq!(effect, Effect::LoadCatalogue { fetch: false }, "{said}");
        assert_eq!(app.dog_job_line().as_deref(), Some(said));
        assert_eq!(
            app.catalogue_rows().first().map(|row| row.version.as_str()),
            Some(behind),
            "{said}: the rows are the old ones until the re-read lands"
        );

        app.update(Msg::Catalogue {
            result: Ok(Box::new(CatalogueRead {
                rows: vec![CatalogueRow {
                    dog: cargo_dog("otel"),
                    version: version.to_owned(),
                    state,
                }],
                refreshing: false,
            })),
        });
        let row = app
            .catalogue_rows()
            .first()
            .expect("the row is still there");
        assert_eq!(
            (row.version.as_str(), row.state, row.is_behind()),
            (version, state, false),
            "{said}"
        );
        app.update(Msg::Tick {
            now: Instant::now() + DONE_SHOWS,
        });
        let effect = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            !matches!(
                effect,
                Effect::StartDogJob {
                    job: Job::Upgrade { .. },
                    ..
                }
            ),
            "{said}: nothing is left to upgrade: {effect:?}"
        );
    }
}

#[test]
fn a_failed_upgrade_opens_the_failure_modal_with_its_own_title() {
    let mut app = installed_entry();
    app.start_job(Job::Upgrade {
        name: "otel".to_owned(),
    });

    app.update(Msg::DogJobDone {
        name: "otel".to_owned(),
        ok: false,
        output: "the new version asks for a permission".to_owned(),
    });
    assert!(
        matches!(app.modal(), Some(Modal::Output { title, .. }) if title.as_str() == "upgrading otel"),
        "{:?}",
        app.modal()
    );
    assert_eq!(app.dog_job_line(), None);
}

#[test]
fn a_closed_gate_refuses_the_install_and_names_the_gate() {
    let mut app = app_with_row(cargo_dog("otel"), DogState::NotInstalled);
    app.set_control_for_tests(Control::ReadOnly);
    app.update(Msg::Key(KeyPress::Confirm));

    assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
    assert_eq!(
        app.dog_job_line(),
        None,
        "no entry action behind a closed gate"
    );

    assert_eq!(app.start_install("otel"), Effect::None);
    assert_eq!(
        app.notice().map(ToString::to_string).as_deref(),
        Some(READ_ONLY_REFUSAL)
    );
    assert_eq!(app.dog_job_line(), None);
}

#[test]
fn a_step_moves_the_bar_and_a_landed_job_clears_it_and_rereads_the_rows() {
    let (mut app, t0) = installing();

    assert_eq!(
        app.update(Msg::DogStep {
            name: "otel".to_owned(),
            step: Step::Downloading,
        }),
        Effect::None
    );
    assert_eq!(
        app.dog_job_line().as_deref(),
        Some("installing otel  downloading  0s")
    );

    app.update(Msg::Tick {
        now: t0 + Duration::from_secs(7),
    });
    assert_eq!(
        app.dog_job_line().as_deref(),
        Some("installing otel  downloading  7s")
    );

    app.update(Msg::DogStep {
        name: "otel".to_owned(),
        step: Step::Building,
    });
    assert_eq!(
        app.dog_job_line().as_deref(),
        Some("installing otel  building with cargo  7s")
    );

    let effect = app.update(Msg::DogJobDone {
        name: "otel".to_owned(),
        ok: true,
        output: "installed otel".to_owned(),
    });
    assert_eq!(effect, Effect::LoadCatalogue { fetch: false });
    assert_eq!(app.dog_job_line().as_deref(), Some("installed otel"));

    // The item retires on its own, and a new job may start after it.
    app.update(Msg::Tick {
        now: t0 + Duration::from_secs(7) + DONE_SHOWS + Duration::from_secs(1),
    });
    assert_eq!(app.dog_job_line(), None);
    assert!(matches!(
        app.start_install("otel"),
        Effect::StartDogJob { .. }
    ));
}

#[test]
fn a_step_for_another_dog_moves_nothing() {
    let (mut app, _) = installing();
    app.update(Msg::DogStep {
        name: "rotate".to_owned(),
        step: Step::Downloading,
    });
    assert_eq!(
        app.dog_job_line().as_deref(),
        Some("installing otel  starting  0s")
    );
}

#[test]
fn a_failed_job_opens_a_modal_with_the_output_capped() {
    let (mut app, _) = installing();
    let output = "x".repeat(OUTPUT_CAP + 500);

    assert_eq!(
        app.update(Msg::DogJobDone {
            name: "otel".to_owned(),
            ok: false,
            output,
        }),
        Effect::None
    );
    match app.modal() {
        Some(Modal::Output { title, text, .. }) => {
            assert_eq!(title.as_str(), "installing otel");
            assert_eq!(text.as_str().len(), OUTPUT_CAP + CUT_MARK.len());
            assert!(text.as_str().ends_with(CUT_MARK), "not capped: {text:?}");
        }
        other => panic!("expected the failure modal, got {other:?}"),
    }
    assert_eq!(app.dog_job_line(), None, "the bar is clear");
}

#[test]
fn a_grant_modal_answers_the_callback_that_asked_for_it() {
    let (mut app, _) = installing();
    app.note_modal_reach(Some(0));
    let (yes_tx, yes_rx) = mpsc::channel();
    let (no_tx, no_rx) = mpsc::channel();

    app.update(Msg::DogGrant {
        name: "otel".to_owned(),
        prompt: prompt("otel"),
        reply: Arc::new(GrantReply(yes_tx)),
    });
    assert!(
        matches!(app.modal(), Some(Modal::Grant { prompt, .. }) if prompt.question().contains("otel")),
        "{:?}",
        app.modal()
    );
    let tag = open_tag(&app);
    assert_eq!(
        app.update(Msg::Key(KeyPress::Confirm)),
        Effect::ModalAnswered { tag, yes: true },
        "the modal is on screen and answers"
    );
    app.answer_grant(tag, true);
    assert_eq!(yes_rx.try_recv(), Ok(true), "Enter answers yes");

    app.update(Msg::DogGrant {
        name: "otel".to_owned(),
        prompt: prompt("otel"),
        reply: Arc::new(GrantReply(no_tx)),
    });
    app.note_modal_reach(Some(0));
    let tag = open_tag(&app);
    assert_eq!(
        app.update(Msg::Key(KeyPress::Escape)),
        Effect::ModalAnswered { tag, yes: false }
    );
    app.answer_grant(tag, false);
    assert_eq!(no_rx.try_recv(), Ok(false), "any other key answers no");
}

#[test]
fn a_grant_for_a_job_that_has_ended_answers_no_on_its_own() {
    let (mut app, _) = installing();
    app.note_modal_reach(Some(0));
    let (tx, rx) = mpsc::channel();
    app.update(Msg::DogJobDone {
        name: "otel".to_owned(),
        ok: true,
        output: String::new(),
    });

    app.update(Msg::DogGrant {
        name: "otel".to_owned(),
        prompt: prompt("otel"),
        reply: Arc::new(GrantReply(tx)),
    });

    assert!(app.modal().is_none(), "no modal for a job that is gone");
    assert_eq!(
        rx.try_recv(),
        Err(mpsc::TryRecvError::Disconnected),
        "the dropped reply reads as a no"
    );
}

#[test]
fn quitting_mid_job_asks_first_and_a_no_keeps_the_job() {
    let (mut app, _) = installing();
    app.note_modal_reach(Some(0));

    assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::None);
    match app.modal() {
        Some(Modal::Confirm { question, .. }) => {
            assert!(question.contains("installing otel"), "{question:?}");
        }
        other => panic!("expected the quit question, got {other:?}"),
    }
    open_tag(&app);
    app.note_modal_reach(Some(0));
    assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
    assert!(app.modal().is_none());
    assert_eq!(
        app.dog_job_line().as_deref(),
        Some("installing otel  starting  0s"),
        "the job is still running"
    );

    assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::None);
    assert!(matches!(app.modal(), Some(Modal::Confirm { .. })));
    app.note_modal_reach(Some(0));
    assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::Quit);
}

/// The job landing under the open quit question must not turn the answer into
/// a keystroke that does nothing.
#[test]
fn a_yes_to_quitting_after_the_job_lands_still_quits() {
    let (mut app, _) = installing();
    app.note_modal_reach(Some(0));
    assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::None);
    let tag = open_tag(&app);

    app.update(Msg::DogJobDone {
        name: "otel".to_owned(),
        ok: true,
        output: String::new(),
    });
    assert_eq!(app.dog_job_line().as_deref(), Some("installed otel"));
    assert_eq!(open_tag(&app), tag, "the question is still open");

    assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::Quit);
}

#[test]
fn a_no_to_quitting_after_the_job_lands_leaves_the_dashboard_open() {
    let (mut app, _) = installing();
    app.note_modal_reach(Some(0));
    assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::None);
    app.update(Msg::DogJobDone {
        name: "otel".to_owned(),
        ok: true,
        output: String::new(),
    });
    assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
    assert!(app.modal().is_none(), "the question closed");
    // Nothing runs now, so the next `q` quits as it always has.
    assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::Quit);
}

#[test]
fn quitting_with_no_job_running_quits_straight_away() {
    let mut app = app_with_row(cargo_dog("otel"), DogState::NotInstalled);
    assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::Quit);
}

/// The manifest `otel` answers with: it needs `log-rotate`.
#[cfg(all(test, unix))]
const OTEL: &str = r#"{"manifest":1,"name":"otel","version":"0.2.4","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock"}],"depends":{"dogs":[{"name":"log-rotate","version":">=0.1, <0.3"}]}}"#;

/// The manifest `log-rotate` answers with.
#[cfg(all(test, unix))]
const LOG_ROTATE: &str = r#"{"manifest":1,"name":"log-rotate","version":"0.2.0","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock"}]}"#;

/// A home with a cached index holding `otel` and `log-rotate`, and a `cargo`
/// that builds either package and answers with its own manifest.
#[cfg(all(test, unix))]
struct World {
    _dir: tempfile::TempDir,
    paths: ShepPaths,
    cargo: std::path::PathBuf,
}

#[cfg(all(test, unix))]
impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temp home");
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let (bin, built) = (dir.path().join("stub-bin"), dir.path().join("built-dogs"));
        std::fs::create_dir_all(&bin).expect("the stub bin");
        std::fs::create_dir_all(&built).expect("the built dogs");
        for (package, manifest) in [("shep-otel", OTEL), ("shep-log-rotate", LOG_ROTATE)] {
            std::fs::write(
                built.join(package),
                format!("#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then cat <<'EOF'\n{manifest}\nEOF\nfi\nexit 0\n"),
            )
            .expect("a built dog");
        }
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  [ \"$prev\" = --root ] && root=\"$a\"\n  prev=\"$a\"\n  last=\"$a\"\ndone\nmkdir -p \"$root/bin\"\ncp {built}/\"$last\" \"$root/bin/$last\"\nchmod 755 \"$root/bin/$last\"\n",
            built = built.display(),
        );
        let cargo = bin.join("cargo");
        std::fs::write(&cargo, script).expect("the stub cargo");
        std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("an executable stub");
        let document = serde_json::json!({
            "version": 1,
            "dogs": [
                {
                    "name": "Otel", "package": "shep-otel", "adopt_as": "otel",
                    "description": "exports traces",
                    "repo": "https://github.com/shep-pm/shep-otel",
                    "license": "MIT", "category": "metrics",
                    "source": { "kind": "cargo", "version": "0.2.4" }
                },
                {
                    "name": "Log rotate", "package": "shep-log-rotate", "adopt_as": "log-rotate",
                    "description": "rotates logs",
                    "repo": "https://github.com/shep-pm/shep-log-rotate",
                    "license": "MIT", "category": "logs",
                    "source": { "kind": "cargo", "version": "0.2.0" }
                }
            ]
        });
        crate::dog_index::write_cache(
            &paths.dog_index_cache,
            &crate::dog_index::index_url(),
            document.to_string().as_bytes(),
        )
        .expect("a cached index");
        Self {
            _dir: dir,
            paths,
            cargo,
        }
    }
}

/// How long a session may take before the test calls it stuck: a stub cargo
/// and a canned crates.io, so this is a generous bound rather than a guess.
#[cfg(all(test, unix))]
const SESSION_WAIT: Duration = Duration::from_secs(30);

/// Drives one job end to end, answering every grant question with `yes`.
///
/// Not `start_paused`: the job runs on its own thread against its own real
/// clock, and a paused test clock would fire every `timeout` here instantly.
#[cfg(all(test, unix))]
#[tokio::test]
async fn dependencies_are_asked_in_the_clis_order_and_a_no_stops_the_next_question() {
    let world = World::new();
    let (mut app, _) = installing();
    let mut run = DogRunner::Stub(world.cargo.clone()).start(
        &world.paths,
        Job::Install {
            adopt_as: "otel".to_owned(),
        },
    );
    app.note_modal_reach(Some(0));

    let mut asked: Vec<String> = Vec::new();
    let mut ended: Option<(bool, String)> = None;
    while ended.is_none() {
        let msg = tokio::time::timeout(SESSION_WAIT, run.msgs.recv())
            .await
            .expect("the job reported within the bound")
            .expect("the job's channel stays open until the run ends");
        if let Msg::DogJobDone { ok, output, .. } = &msg {
            ended = Some((*ok, output.clone()));
        }
        app.update(msg);
        // Answer every question the job just asked, with a no.
        while let Some(tag) = grant_tag(&app) {
            asked.push(grant_question(&app));
            app.note_modal_reach(Some(0));
            assert_eq!(
                app.update(Msg::Key(KeyPress::Escape)),
                Effect::ModalAnswered { tag, yes: false }
            );
            app.answer_grant(tag, false);
        }
    }

    let (ok, output) = ended.expect("the job ended");
    assert_eq!(asked.len(), 1, "only the dependency was asked: {asked:?}");
    assert!(asked[0].contains("log-rotate"), "{asked:?}");
    assert!(!ok, "{output}");
    assert!(output.contains("you said no to it"), "{output}");
    assert_eq!(app.dog_job_line(), None, "the bar is clear");
    assert!(
        !world.paths.dogs_dir.join("otel/receipt.json").exists(),
        "a stopped install writes no receipt"
    );
}

/// How long the stub cargo may take to record its pid, and how long a build
/// the run ended may take to show itself as stopped.
#[cfg(all(test, unix))]
const TAKEN_ATTEMPTS: usize = 1_500;

#[cfg(all(test, unix))]
const STOPPED_ATTEMPTS: usize = 100;

#[cfg(all(test, unix))]
const BUILD_POLL: Duration = Duration::from_millis(20);

/// The pid the stub cargo recorded, once it has recorded one.
#[cfg(all(test, unix))]
#[track_caller]
fn recorded_pid(pid_file: &Path) -> u32 {
    for _ in 0..TAKEN_ATTEMPTS {
        if let Ok(written) = std::fs::read_to_string(pid_file) {
            return written.trim().parse().expect("the stub wrote a pid");
        }
        std::thread::sleep(BUILD_POLL);
    }
    panic!("the stub cargo never started");
}

/// The handle a quit holds is the thread that holds the `cargo`, so ending a
/// run stops the build instead of leaving it to a thread a process exit is
/// about to kill without running its destructors.
///
/// No `start_paused`: the run is a real thread running a real `cargo`.
#[cfg(all(test, unix))]
#[test]
fn ending_a_run_stops_the_build_and_waits_for_the_thread_holding_it() {
    // Short, not the default `$TMPDIR`: a long `$SHEP_HOME` overflows
    // `SUN_LEN` for the control socket path this home names.
    let dir = tempfile::Builder::new()
        .prefix("s")
        .tempdir()
        .expect("a temp home");
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (bin, pid_file) = (dir.path().join("bin"), dir.path().join("cargo.pid"));
    std::fs::create_dir_all(&bin).expect("the stub bin");
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, sleeping_cargo(&pid_file)).expect("the stub cargo");
    std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("an executable stub");
    crate::dog_index::write_cache(
        &paths.dog_index_cache,
        &crate::dog_index::index_url(),
        serde_json::json!({
            "version": 1,
            "dogs": [{
                "name": "Otel", "package": "shep-otel", "adopt_as": "otel",
                "description": "exports traces",
                "repo": "https://github.com/shep-pm/shep-otel",
                "license": "MIT", "category": "metrics",
                "source": { "kind": "cargo", "version": "0.2.4" }
            }]
        })
        .to_string()
        .as_bytes(),
    )
    .expect("a cached index");

    let mut run = DogRunner::Stub(cargo).start(
        &paths,
        Job::Install {
            adopt_as: "otel".to_owned(),
        },
    );
    let pid = recorded_pid(&pid_file);

    assert!(run.end(), "the job's own thread ended its run");

    let mut stopped = false;
    for _ in 0..STOPPED_ATTEMPTS {
        if is_gone(pid) {
            stopped = true;
            break;
        }
        std::thread::sleep(BUILD_POLL);
    }
    assert!(stopped, "cargo {pid} is still building after the run ended");
}

/// A quit while the session is asking a grant question: the replies the
/// dashboard holds go first, or the session stays parked in its callback and
/// the run never ends to be waited for.
///
/// No `start_paused`: the run is a real thread with a real callback.
#[cfg(all(test, unix))]
#[test]
fn a_quit_while_a_grant_is_asked_ends_the_run_that_is_waiting_on_it() {
    let world = World::new();
    let (mut app, _) = installing();
    let mut run = DogRunner::Stub(world.cargo.clone()).start(
        &world.paths,
        Job::Install {
            adopt_as: "otel".to_owned(),
        },
    );
    app.note_modal_reach(Some(0));

    // The reports drained into the dashboard the way the loop drains them,
    // until it is holding a question the session is blocked on.
    let mut asked = false;
    for _ in 0..TAKEN_ATTEMPTS {
        while let Ok(msg) = run.msgs.try_recv() {
            app.update(msg);
        }
        asked = matches!(app.modal(), Some(Modal::Grant { .. }));
        if asked {
            break;
        }
        std::thread::sleep(BUILD_POLL);
    }
    assert!(asked, "the session never asked a grant question");

    app.release_dog_grants();
    assert!(
        run.end(),
        "the session left its callback and ended its own run"
    );
    assert!(
        !world.paths.dogs_dir.join("otel/receipt.json").exists(),
        "a run cut off mid-question adopted a dog"
    );
}

/// The wait is bounded: a session parked on a question nobody will answer
/// cannot hold the caller past [`END_BUDGET`].
#[cfg(all(test, unix))]
#[test]
fn ending_a_run_waits_no_longer_than_its_budget() {
    let world = World::new();
    let mut run = DogRunner::Stub(world.cargo.clone()).start(
        &world.paths,
        Job::Install {
            adopt_as: "otel".to_owned(),
        },
    );

    // The question, held here and never answered: the session is parked in a
    // callback, which is the one thing a cancel cannot interrupt.
    let mut held: Option<Arc<GrantReply>> = None;
    for _ in 0..TAKEN_ATTEMPTS {
        if let Ok(Msg::DogGrant { reply, .. }) = run.msgs.try_recv() {
            held = Some(reply);
            break;
        }
        std::thread::sleep(BUILD_POLL);
    }
    assert!(held.is_some(), "the session never asked a grant question");

    let asked_at = Instant::now();
    assert!(!run.end(), "a parked session ended its run");
    assert!(
        asked_at.elapsed() >= END_BUDGET,
        "the wait gave up after {:?}",
        asked_at.elapsed()
    );
}
