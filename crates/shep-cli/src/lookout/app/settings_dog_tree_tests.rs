//! The settings screen's dog rows: folded until the operator opens one, probed
//! only on opening, and written back through the same paths the dog pane and
//! `shep dogs grant` use.
//!
//! The dog pane's tests cover the pane. These cover the row: what opening it
//! costs, what a failed probe leaves behind, and that the two surfaces cannot
//! leave `dogs.toml` and `shep.toml` in two different shapes.

use std::path::Path;

use shep_core::protocol::Response;

use super::testing::{close_bark, wire_batch};
use super::*;
use crate::exit::ExitCode;
use crate::lookout::view::fixtures;

/// A built-in dog whose record names a permission its manifest never declared:
/// a dog that was downgraded, or an entry an operator typed by hand.
const BARK_WITH_A_STRAY: &str = "[daemon]\nenabled_dogs = [\"bark\"]\n\n[daemon.dog_grants.bark]\ngranted = [\"flock.read\", \"flock.control\"]\nreviewed = true\n";

/// `bark` enabled and not boot first, and the same with `boot_first_dogs`
/// naming it: the two ends of the boot first toggle.
const BARK_PLAIN: &str = "[daemon]\nenabled_dogs = [\"bark\"]\n";
const BARK_BOOT_FIRST: &str = "[daemon]\nenabled_dogs = [\"bark\"]\nboot_first_dogs = [\"bark\"]\n";

/// The index of `name`'s dog row in the snapshot's own dogs list.
#[track_caller]
fn dog_index(app: &App, name: &str) -> usize {
    app.settings()
        .expect("the settings screen is open")
        .snapshot()
        .dogs
        .iter()
        .position(|dog| dog.name == name)
        .expect("the fixture carries this dog")
}

/// The settings cursor onto item `permission` of `name`'s grants checklist.
///
/// # Panics
///
/// If the checklist does not offer `permission`.
#[track_caller]
fn select_grant(app: &mut App, name: &str, permission: &str) {
    let offered = app
        .settings()
        .expect("the settings screen is open")
        .dog_named(name)
        .expect("the fixture carries this dog")
        .offered
        .clone();
    let index = offered
        .iter()
        .position(|held| held == permission)
        .unwrap_or_else(|| panic!("{name}'s checklist offers {offered:?}, not {permission:?}"));
    fixtures::select_setting(app, &format!("dog.{name}.grant.{index}"));
}

/// The rows under one dog's header, named `dog.<key>` in the order the screen
/// draws them: its `[daemon] keys, the checklist items under `grants`, then its
/// own tree's labels.
fn rows_under(app: &App, name: &str) -> Vec<String> {
    let settings = app.settings().expect("the settings screen is open");
    settings
        .rows()
        .into_iter()
        .filter_map(|row| match row {
            SettingsRow::DogKey { dog, key } => {
                Some(format!("{}.{}", settings.dog(dog)?.name, key.label()))
            }
            SettingsRow::DogGrant { dog, index } => {
                let permission = settings.dog(dog)?.offered.get(index)?;
                Some(format!("{}.grant.{permission}", settings.dog(dog)?.name))
            }
            SettingsRow::DogEnv(dog) => Some(format!("{}.env", settings.dog(dog)?.name)),
            SettingsRow::DogEnvKey { dog, index } => {
                let dog = settings.dog(dog)?;
                Some(format!("{}.env.{}", dog.name, dog.env_keys.get(index)?))
            }
            SettingsRow::DogEnvNew(dog) => Some(format!("{}.env.+ new", settings.dog(dog)?.name)),
            SettingsRow::DogField { dog, row } => {
                let pane = settings.dog_tree(dog)?.pane()?;
                let node = pane.tree_row(pane.rows().get(row)?.to_owned())?;
                Some(format!("{}.{}", settings.dog(dog)?.name, node.label))
            }
            _ => None,
        })
        .filter(|named| named.starts_with(&format!("{name}.")))
        .collect()
}

/// The dog pane's own tree, `(depth, label)` per row, which is what the settings
/// screen's copy of it has to match.
fn pane_tree(pane: &ConfigPane) -> Vec<(usize, String)> {
    pane.rows()
        .iter()
        .map(|row| {
            let node = pane.tree_row(row.to_owned()).expect("a dog's tree nests");
            (node.depth, node.label)
        })
        .collect()
}

/// The dog pane opened from the flock, for the tests that compare a row's
/// output with the pane's.
fn dog_pane_bark() -> ConfigPane {
    fixtures::app_in_dog_pane()
        .config_pane()
        .expect("the fixture opens the pane")
        .clone()
}

/// The screen opens with every dog folded and asks for no dog's schema: a probe
/// runs the dog's binary, so a screen listing several dogs pays for none until
/// an operator opens one.
#[test]
fn the_screen_opens_with_every_dog_folded_and_no_probe_asked() {
    let mut app = fixtures::app_with(fixtures::flock_of(3, 0), fixtures::plain());
    app.set_control_for_tests(Control::Allowed);
    assert_eq!(
        app.update(Msg::Key(KeyPress::Settings)),
        Effect::LoadSettings,
        "opening the screen asks for shep.toml"
    );
    assert_eq!(
        app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        }),
        Effect::None,
        "and asks no dog's binary to publish a schema"
    );

    let settings = app.settings().expect("the settings screen is open");
    assert_eq!(settings.snapshot().dogs.len(), 2);
    for index in 0..settings.snapshot().dogs.len() {
        assert!(
            settings.dog_tree(index).is_none(),
            "{} has read nothing about its own config",
            settings.dog(index).unwrap().name
        );
    }
    assert!(
        settings.rows().iter().all(|row| matches!(
            row,
            SettingsRow::Table(_)
                | SettingsRow::Field(_)
                | SettingsRow::Entry { .. }
                | SettingsRow::AddEntry(_)
                | SettingsRow::Dog(_)
        )),
        "the folded screen shows one row per dog and nothing under it: {:?}",
        settings.rows()
    );
}

/// Opening a row spends exactly one probe, and a second `e` while that probe is
/// out spends nothing.
#[test]
fn opening_a_dog_row_asks_for_its_schema_once() {
    let mut app = fixtures::app_in_settings_with_control();
    fixtures::select_setting(&mut app, "dog.bark");
    let effect = app.update(Msg::Key(KeyPress::Edit));
    let Effect::LoadDogPane { name, adopted_path } = effect else {
        panic!("opening a dog row asks for its schema, got {effect:?}");
    };
    assert_eq!(name, "bark");
    assert_eq!(
        adopted_path, None,
        "a built-in dog is this binary, so there is no path to probe"
    );
    assert_eq!(
        app.update(Msg::Key(KeyPress::Edit)),
        Effect::None,
        "the row is mid-read: a second e would run the binary twice"
    );
}

/// An opened dog's rows are the dog pane's rows, hung under the header after
/// the four `[daemon] keys: the same schema and the same section build the same
/// tree, at the same depths.
#[test]
fn an_opened_dog_draws_the_same_tree_the_dog_pane_draws() {
    let app = fixtures::app_in_settings_with_a_dog_open();
    let settings = app.settings().expect("the settings screen is open");
    let index = dog_index(&app, "bark");
    let here = settings
        .dog_tree(index)
        .expect("the row holds a tree")
        .pane()
        .expect("and the pane the tree is drawn from");
    let there = dog_pane_bark();

    assert_eq!(
        pane_tree(here),
        pane_tree(&there),
        "the same schema and the same section build the same tree"
    );

    let labels: Vec<String> = pane_tree(&there)
        .into_iter()
        .map(|(_, label)| label)
        .collect();
    let expected: Vec<String> = ["boot first", "grants", "source", "channel"]
        .iter()
        .map(|key| format!("bark.{key}"))
        .chain(labels.iter().map(|label| format!("bark.{label}")))
        .collect();
    assert_eq!(rows_under(&app, "bark"), expected);
}

/// An adopted dog's `env` sits under its header beside the four keys: a table
/// row, one row per key the entry holds, then the row that adds one. The
/// pane's own tree follows, and carries none of them.
#[test]
fn an_adopted_dogs_env_keys_sit_under_its_header_with_a_row_to_add_one() {
    let (_dir, _paths, mut app) = fixtures::settings_over(fixtures::OTEL_WITH_ENV);
    fixtures::open_adopted_dog_row(&mut app, "otel", Path::new("/usr/local/bin/shep-otel"));
    assert_eq!(
        rows_under(&app, "otel"),
        [
            "otel.boot first",
            "otel.grants",
            "otel.source",
            "otel.channel",
            "otel.env",
            "otel.env.PADDOCK_KEY",
            "otel.env.+ new",
            "otel.ratio",
        ]
    );
}

/// A built-in dog has no `adopted_dogs` entry to hold an `env`, so its open
/// row has no `env` row.
#[test]
fn a_built_in_dogs_open_row_has_no_env_row() {
    let (_dir, _paths, mut app) = fixtures::settings_over(BARK_PLAIN);
    fixtures::open_dog_row(&mut app, "bark");
    assert!(
        !rows_under(&app, "bark")
            .iter()
            .any(|row| row.starts_with("bark.env")),
        "{:?}",
        rows_under(&app, "bark")
    );
}

/// A field edited under the dog's row leaves the section a dog pane's close
/// leaves for the same field: same gate, same typing, same text in
/// `Request::SetDogConfig`, operator's comments intact.
#[test]
fn a_field_edited_under_the_row_writes_the_section_the_dog_pane_writes() {
    let from_row = {
        let mut app = fixtures::app_in_settings_with_a_dog_open();
        fixtures::select_setting(&mut app, "dog.bark.poll");
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        let seed = {
            let (_, buffer) = app
                .settings()
                .expect("the settings screen is open")
                .typing()
                .expect("the row opens an editor");
            buffer.as_str().to_string()
        };
        assert_eq!(seed, "60s", "the row's editor opens on the field's value");
        type_text(&mut app, "60s", "30s");
        let effect = app.update(Msg::Key(KeyPress::Confirm));
        let Effect::Send(Sent::SetDogSection { name, toml, .. }) = effect else {
            panic!("Enter sends the section, got {effect:?}");
        };
        assert_eq!(name, "bark");
        toml
    };
    let from_pane = {
        let mut app = fixtures::app_in_dog_pane();
        let index = app
            .config_pane()
            .expect("the pane is open")
            .fields()
            .fields()
            .iter()
            .position(|field| field.key == "poll")
            .expect("poll is a bark field");
        app.update(Msg::Key(KeyPress::SelectFirst));
        for _ in 0..index {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
        app.update(Msg::Key(KeyPress::Confirm));
        type_text(&mut app, "60s", "30s");
        let batch = wire_batch(close_bark(&mut app));
        let [Sent::SetDogSection { toml, .. }] = batch.as_slice() else {
            panic!("closing the pane sends the section");
        };
        toml.clone()
    };

    assert_eq!(from_row.as_str(), from_pane.as_str());
    assert!(
        from_row.as_str().contains("poll = \"30s\""),
        "{}",
        from_row.as_str()
    );
    assert!(
        from_row.as_str().contains("# how often"),
        "the operator's own comment survives a write from the row: {}",
        from_row.as_str()
    );
}

/// Clears `was` from the editor open on the cursor's row and types `now`,
/// through the same text keys an operator presses.
#[track_caller]
fn type_text(app: &mut App, was: &str, now: &str) {
    for _ in 0..was.chars().count() {
        app.update(Msg::Key(KeyPress::TextBackspace));
    }
    for c in now.chars() {
        app.update(Msg::Key(KeyPress::TextChar(c)));
    }
    app.update(Msg::Key(KeyPress::TextApply));
}

/// A probe that fails leaves the row folded, says why on the row itself, and
/// points at `dogs.toml`, which is what a dog pane does with the same failure.
#[test]
fn a_failed_probe_leaves_the_row_folded_with_the_refusal_on_it() {
    let mut app = fixtures::app_in_settings_with_control();
    fixtures::select_setting(&mut app, "dog.bark");
    let _ = app.update(Msg::Key(KeyPress::Edit));
    let effect = app.update(Msg::DogPane {
        name: "bark".to_string(),
        adopted_path: None,
        env_keys: Vec::new(),
        result: Err("bark publishes no schema; edit dogs.toml with $EDITOR".to_string()),
    });
    assert_eq!(effect, Effect::None, "a failed probe asks for nothing else");

    let settings = app.settings().expect("the settings screen is open");
    let index = dog_index(&app, "bark");
    assert!(
        matches!(settings.dog_tree(index), Some(DogTree::Refused { .. })),
        "the row holds the refusal"
    );
    assert!(
        settings.rows().iter().all(|row| !matches!(
            row,
            SettingsRow::DogKey { .. }
                | SettingsRow::DogGrant { .. }
                | SettingsRow::DogField { .. }
        )),
        "and nothing under it: {:?}",
        settings.rows()
    );

    let buffer = fixtures::render(&app, 120, 30);
    let row = fixtures::row_containing(&buffer, "bark publishes no schema");
    assert!(row.contains("dogs.toml"), "{row}");

    // `e` again probes rather than leaving the refusal permanent.
    assert!(matches!(
        app.update(Msg::Key(KeyPress::Edit)),
        Effect::LoadDogPane { .. }
    ));
}

/// The checklist is what the dog's manifest declares: a permission the record
/// holds that the manifest never asked for gets no row, the same one
/// `shep dogs grant` refuses.
#[test]
fn the_checklist_offers_what_the_manifest_declares_and_nothing_else() {
    let (_dir, paths, mut app) = fixtures::settings_over(BARK_WITH_A_STRAY);
    fixtures::open_dog_row(&mut app, "bark");
    let offered = app
        .settings()
        .expect("the settings screen is open")
        .dog_named("bark")
        .expect("the file names bark")
        .offered
        .clone();
    assert_eq!(
        offered,
        ["flock.read", "events:process.", "events:log."],
        "bark's own manifest, in its own order"
    );

    let under = rows_under(&app, "bark");
    assert!(
        under.contains(&"bark.grant.flock.read".to_string()),
        "{under:?}"
    );
    assert!(
        !under.iter().any(|named| named.contains("flock.control")),
        "a permission bark never declared is not offered as a row: {under:?}"
    );

    // The same refusal the checklist's absence predicts.
    let refusal = crate::commands::dogs::record_change(
        &paths,
        crate::commands::dogs::Change::Grant,
        "bark",
        "flock.control",
    );
    match refusal {
        Err(crate::commands::dogs::GrantRefusal::Usage { message, .. }) => {
            assert!(message.contains("did not declare"), "{message}");
        }
        other => panic!("an undeclared grant must be refused, got {other:?}"),
    }
}

/// A dog with no `dog_grants` entry offers no checklist, which is the same fact
/// `shep dogs grant` refuses on.
#[test]
fn a_dog_with_no_grants_record_offers_no_checklist() {
    let app = fixtures::app_in_settings_with_a_dog_open();
    let offered = app
        .settings()
        .expect("the settings screen is open")
        .dog_named("bark")
        .expect("the fixture carries bark")
        .offered
        .clone();
    assert!(offered.is_empty(), "{offered:?}");
    assert!(
        !rows_under(&app, "bark")
            .iter()
            .any(|named| named.starts_with("bark.grant.")),
        "{:?}",
        rows_under(&app, "bark")
    );
}

/// Ticking a permission on the checklist writes the entry `shep dogs grant`
/// writes: the same call, on a twin home, byte for byte.
#[tokio::test]
async fn a_grant_from_the_checklist_writes_what_shep_dogs_grant_writes() {
    let (_dir, paths, mut app) = fixtures::settings_over(fixtures::BARK_RECORDED);
    fixtures::open_dog_row(&mut app, "bark");
    select_grant(&mut app, "bark", "events:log.");

    assert_eq!(app.update(Msg::Key(KeyPress::Cycle)), Effect::None);
    let effect = app.update(Msg::Key(KeyPress::Confirm));
    let Effect::WriteDogTune { tune, ticket, .. } = effect else {
        panic!("Enter sends the tune, got {effect:?}");
    };
    assert_eq!(
        tune,
        DogTune::Grant {
            name: "bark".to_string(),
            permission: "events:log.".to_string(),
            grant: true,
        }
    );

    // What the event loop does with that effect.
    let applied = crate::commands::dogs::record_change(
        &paths,
        crate::commands::dogs::Change::Grant,
        tune.dog(),
        "events:log.",
    )
    .expect("events:log. is a permission bark declares");
    assert!(applied.enabled, "the fixture's bark is enabled");
    let by_lookout = std::fs::read_to_string(&paths.daemon_config).unwrap();

    // The verb, on the same file.
    let (_twin, twin_paths) = fixtures::home_with(fixtures::BARK_RECORDED);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = crate::commands::dogs::change(
        &mut crate::output::Streams::bare(&mut out, &mut err, crate::cli::Format::Table),
        &twin_paths,
        crate::commands::dogs::Change::Grant,
        "bark",
        "events:log.",
    )
    .await;
    assert_eq!(
        code,
        ExitCode::Success,
        "{}",
        String::from_utf8(err).unwrap()
    );
    let by_verb = std::fs::read_to_string(&twin_paths.daemon_config).unwrap();

    assert_eq!(by_lookout, by_verb);
    assert!(by_lookout.contains("events:log."), "{by_lookout}");

    // The reply that write raises: an enabled dog is told, the way the verb
    // tells it, and the screen re-reads once the answer lands.
    let effect = app.update(Msg::DogTuneWritten {
        tune: tune.clone(),
        ticket,
        result: Ok(DogTuneLanded::Grants {
            entry: applied.entry,
            enabled: applied.enabled,
        }),
    });
    let Effect::Send(sent @ Sent::GrantDog { .. }) = effect else {
        panic!("an enabled dog is told its new grants: {effect:?}");
    };
    let effect = app.update(Msg::Replied {
        sent,
        result: Ok(Response::DogStarted(
            ProcessInfo::builder(50, "bark", ProcStatus::Online)
                .pid(Some(50_000))
                .dog(Some(DogSource::BuiltIn { grants: None }))
                .build(),
        )),
    });
    assert_eq!(effect, Effect::LoadSettings);
    assert!(
        !app.notice().expect("one sentence lands").is_grave(),
        "{}",
        app.notice().unwrap()
    );
}

/// `boot first` puts the dog in `[daemon] boot_first_dogs` and takes it out,
/// through the same writer the CLI's own settings edits use.
#[test]
fn the_boot_first_toggle_adds_and_removes_the_dog() {
    for (file, first) in [(BARK_PLAIN, true), (BARK_BOOT_FIRST, false)] {
        let (_dir, paths, mut app) = fixtures::settings_over(file);
        fixtures::open_dog_row(&mut app, "bark");
        fixtures::select_setting(&mut app, "dog.bark.boot first");

        assert_eq!(app.update(Msg::Key(KeyPress::Cycle)), Effect::None);
        let effect = app.update(Msg::Key(KeyPress::Confirm));
        let Effect::WriteDogTune { tune, .. } = effect else {
            panic!("Enter sends the tune, got {effect:?}");
        };
        assert_eq!(
            tune,
            DogTune::BootFirst {
                name: "bark".to_string(),
                first,
            },
            "space arms the opposite of the bit the file has"
        );

        crate::commands::dogs::boot_first_in_config(&paths.daemon_config, tune.dog(), first)
            .expect("boot_first_dogs is a name in or out of an array");
        let names = crate::commands::shep_toml::ShepToml::read_only(&paths.daemon_config)
            .unwrap()
            .boot_first_dog_names();
        assert_eq!(
            names.iter().any(|name| name == "bark"),
            first,
            "{names:?} after asking for {first}"
        );
    }
}

/// `source` and `channel` are refused before the gate is consulted: one is
/// `shep adopt`'s to write and the other is the dog's own answer.
#[test]
fn source_and_channel_are_read_only_rows() {
    for (key, names) in [
        ("source", "shep adopt"),
        ("channel", "asked for the channel"),
    ] {
        let mut app = fixtures::app_in_settings_with_a_dog_open();
        fixtures::select_setting(&mut app, &format!("dog.bark.{key}"));
        assert_eq!(
            app.update(Msg::Key(KeyPress::Cycle)),
            Effect::None,
            "nothing arms on a read-only row"
        );
        assert!(
            app.settings()
                .expect("the settings screen is open")
                .pending()
                .is_none(),
            "and nothing is armed"
        );
        let said = app.notice().expect("the row says why").to_string();
        assert!(said.contains(names), "{key}: {said}");
        assert!(
            matches!(
                app.settings().unwrap().cursor(),
                Some(SettingsRow::DogKey { .. })
            ),
            "the cursor stays on the row"
        );
    }
}

/// A key the section does not set is drawn `(unset)`, which is a rendering and
/// not a value. Its editor opens empty, the way a dog pane's does, so `Enter`
/// cannot arm a write of the placeholder itself into `dogs.toml`.
#[test]
fn an_unset_dog_field_opens_its_editor_empty_and_never_with_the_placeholder() {
    let mut app = fixtures::app_in_settings_with_a_dog_open();
    fixtures::select_setting(&mut app, "dog.bark.sink_timeout");
    assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);

    let (label, buffer) = app
        .settings()
        .expect("the settings screen is open")
        .typing()
        .expect("a typed field opens an editor");
    assert_eq!(label, "bark sink_timeout");
    assert_eq!(
        buffer.as_str(),
        "",
        "the row draws (unset); the buffer must not hold it"
    );

    // The rest is the same key sequence a dog pane gets, so nothing it arms
    // can name the placeholder.
    let _ = app.update(Msg::Key(KeyPress::TextApply));
    if let Some(prompt) = app.settings().expect("still open").pending() {
        assert!(
            !prompt.text.contains("(unset)"),
            "the armed write carries the placeholder: {}",
            prompt.text
        );
    }
    if let Some((_, buffer)) = app.settings().expect("still open").typing() {
        assert!(
            !buffer.as_str().contains("(unset)"),
            "a reopened editor holds the placeholder: {}",
            buffer.as_str()
        );
    }
}

/// A section write that never left the queue has to clear the screen's sent
/// line. The reply it was waiting on can no longer arrive, and a prompt left
/// at "sent" reads as a write in flight forever.
#[test]
fn a_dog_section_write_that_was_never_sent_clears_the_prompt() {
    let mut app = fixtures::app_in_settings_with_a_dog_open();
    fixtures::select_setting(&mut app, "dog.bark.poll");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    type_text(&mut app, "60s", "30s");
    let effect = app.update(Msg::Key(KeyPress::Confirm));
    let Effect::Send(sent @ Sent::SetDogSection { .. }) = effect else {
        panic!("Enter sends the section, got {effect:?}");
    };
    assert!(
        app.settings().expect("open").pending().is_some(),
        "the prompt says the write is in flight"
    );

    let _ = app.update(Msg::Unsent { sent });
    assert!(
        app.settings().expect("still open").pending().is_none(),
        "a write that never went out leaves nothing waiting"
    );
    assert!(app.notice().expect("one sentence").is_grave());
}

/// The enabled bit travels with the write's answer, because `record_change`
/// read it under the lock. Leaving the settings screen before the answer lands
/// changes what is on screen, not whether the running dog has to be told.
#[test]
fn a_grant_still_reaches_the_shepherd_when_the_screen_closed_first() {
    let (_dir, paths, mut app) = fixtures::settings_over(fixtures::BARK_RECORDED);
    fixtures::open_dog_row(&mut app, "bark");
    select_grant(&mut app, "bark", "events:log.");
    assert_eq!(app.update(Msg::Key(KeyPress::Cycle)), Effect::None);
    let Effect::WriteDogTune { tune, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm)) else {
        panic!("Enter sends the tune");
    };
    let applied = crate::commands::dogs::record_change(
        &paths,
        crate::commands::dogs::Change::Grant,
        "bark",
        "events:log.",
    )
    .expect("events:log. is a permission bark declares");
    assert!(applied.enabled, "the fixture's bark is enabled");

    let _ = app.update(Msg::Key(KeyPress::Escape));
    assert!(app.settings().is_none(), "the operator left the screen");

    let effect = app.update(Msg::DogTuneWritten {
        tune,
        ticket,
        result: Ok(DogTuneLanded::Grants {
            entry: applied.entry,
            enabled: applied.enabled,
        }),
    });
    assert!(
        matches!(effect, Effect::Send(Sent::GrantDog { .. })),
        "a running dog is told its new grants even with the screen closed: {effect:?}"
    );
}
