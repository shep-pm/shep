//! An adopted dog's `env` on the settings screen: the keys typed into the
//! key/value editor, the write that lands them in `shep.toml`, and the value
//! that is on no screen, in no `Debug` and in no state once it has been sent.

use std::path::Path;

use super::*;
use crate::lookout::view::fixtures;

const OTEL_BINARY: &str = "/usr/local/bin/shep-otel";

/// A settings screen over `text` with `otel`'s row open.
fn opened_over(text: &str) -> (tempfile::TempDir, shep_core::paths::ShepPaths, App) {
    let (dir, paths, mut app) = fixtures::settings_over(text);
    fixtures::open_adopted_dog_row(&mut app, "otel", Path::new(OTEL_BINARY));
    (dir, paths, app)
}

/// Each character of `text` as the key it is in the editor.
fn type_text(app: &mut App, text: &str) {
    for typed in text.chars() {
        let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
    }
}

/// `+ new` filled in with `key` and `value`, up to and including the `Enter`
/// that arms the write.
fn arm_new_key(app: &mut App, dog: &str, key: &str, value: &str) {
    fixtures::select_setting(app, &format!("dog.{dog}.env+"));
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    type_text(app, key);
    let _ = app.update(Msg::Key(KeyPress::TextApply));
    type_text(app, value);
    let _ = app.update(Msg::Key(KeyPress::TextApply));
}

/// The confirm that sends the armed write.
#[track_caller]
fn confirm(app: &mut App) -> (DogEnvWrite, Vec<Sent>) {
    match app.update(Msg::Key(KeyPress::Confirm)) {
        Effect::WriteDogEnv { write, then } => (write, then),
        other => panic!("Enter on an armed env write sends it, got {other:?}"),
    }
}

/// Runs `write` against the file and hands the app the answer, the way the
/// event loop does.
fn land(app: &mut App, path: &Path, write: DogEnvWrite, then: Vec<Sent>) -> Effect {
    let result = write.apply(path).map_err(|why| why.to_string());
    app.update(Msg::DogEnvWritten {
        name: write.name,
        ticket: write.ticket,
        then,
        result,
    })
}

#[test]
fn a_new_key_on_a_bare_path_entry_is_written_as_path_and_env_with_the_comments_kept() {
    let (_dir, paths, mut app) = opened_over(fixtures::OTEL_BARE);

    arm_new_key(&mut app, "otel", "PADDOCK_KEY", "x");
    let (write, then) = confirm(&mut app);
    let _ = land(&mut app, &paths.daemon_config, write, then);

    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).unwrap(),
        "# which dogs run\n[daemon]\nenabled_dogs = [\"otel\"]\n\n[daemon.adopted_dogs]\n\
         # the exporter\notel = { path = \"/usr/local/bin/shep-otel\", env = { PADDOCK_KEY = \"x\" } } # built from main\n\
         zulu = \"/opt/zulu\"\n"
    );
}

#[test]
fn removing_the_last_key_leaves_env_out_of_the_entry() {
    let (_dir, paths, mut app) = opened_over(fixtures::OTEL_WITH_ENV);

    fixtures::select_setting(&mut app, "dog.otel.env.PADDOCK_KEY");
    assert_eq!(app.update(Msg::Key(KeyPress::Remove)), Effect::None);
    let text = app.settings().unwrap().pending().unwrap().text.to_string();
    assert!(text.contains("remove PADDOCK_KEY from otel's env"), "{text}");
    assert!(!text.contains("hunter2"), "{text}");
    let (write, then) = confirm(&mut app);
    let _ = land(&mut app, &paths.daemon_config, write, then);

    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).unwrap(),
        "[daemon]\nenabled_dogs = [\"otel\"]\n\n[daemon.adopted_dogs.otel]\npath = \"/usr/local/bin/shep-otel\"\n"
    );
}

#[test]
fn a_key_added_to_an_inline_entry_leaves_it_inline() {
    let (_dir, paths, mut app) = opened_over(
        "[daemon]\nenabled_dogs = [\"otel\"]\n\n[daemon.adopted_dogs]\notel = { path = \"/usr/local/bin/shep-otel\", env = { RUST_LOG = \"warn\" } }\n",
    );

    arm_new_key(&mut app, "otel", "PADDOCK_KEY", "x");
    let (write, then) = confirm(&mut app);
    let _ = land(&mut app, &paths.daemon_config, write, then);

    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).unwrap(),
        "[daemon]\nenabled_dogs = [\"otel\"]\n\n[daemon.adopted_dogs]\notel = { path = \"/usr/local/bin/shep-otel\", env = { RUST_LOG = \"warn\", PADDOCK_KEY = \"x\" } }\n"
    );
}

#[test]
fn a_key_the_file_already_holds_is_refused_by_name_with_the_editor_open() {
    let (_dir, _paths, mut app) = opened_over(fixtures::OTEL_WITH_ENV);

    fixtures::select_setting(&mut app, "dog.otel.env+");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    type_text(&mut app, "PADDOCK_KEY");
    let _ = app.update(Msg::Key(KeyPress::TextApply));
    type_text(&mut app, "y");
    let effect = app.update(Msg::Key(KeyPress::TextApply));

    assert_eq!(effect, Effect::None);
    let notice = app.notice().expect("the refusal is said").to_string();
    assert!(notice.contains("PADDOCK_KEY is already in otel's env"), "{notice}");
    assert!(
        app.settings().unwrap().env_typing().is_some(),
        "the editor stays open for the operator to fix the key"
    );
    assert_eq!(app.mode(), InputMode::Text);
}

#[test]
fn e_on_a_key_opens_an_empty_editor_and_an_empty_value_removes_the_key() {
    let (_dir, paths, mut app) = opened_over(fixtures::OTEL_WITH_ENV);

    fixtures::select_setting(&mut app, "dog.otel.env.PADDOCK_KEY");
    let _ = app.update(Msg::Key(KeyPress::Edit));
    let (dog, editor) = app.settings().unwrap().env_typing().expect("the editor opens");
    assert_eq!(dog, "otel");
    assert_eq!(editor.value().as_str(), "", "no value is ever read back");
    let _ = app.update(Msg::Key(KeyPress::TextApply));
    let text = app.settings().unwrap().pending().unwrap().text.to_string();
    assert!(text.contains("remove PADDOCK_KEY from otel's env"), "{text}");
    let (write, then) = confirm(&mut app);
    let _ = land(&mut app, &paths.daemon_config, write, then);

    assert!(
        !std::fs::read_to_string(&paths.daemon_config)
            .unwrap()
            .contains("PADDOCK_KEY")
    );
}

#[test]
fn a_changed_value_replaces_the_old_one_in_place() {
    let (_dir, paths, mut app) = opened_over(fixtures::OTEL_WITH_ENV);

    fixtures::select_setting(&mut app, "dog.otel.env.PADDOCK_KEY");
    let _ = app.update(Msg::Key(KeyPress::Edit));
    type_text(&mut app, "new");
    let _ = app.update(Msg::Key(KeyPress::TextApply));
    let (write, then) = confirm(&mut app);
    let _ = land(&mut app, &paths.daemon_config, write, then);

    assert!(
        std::fs::read_to_string(&paths.daemon_config)
            .unwrap()
            .contains("env = { PADDOCK_KEY = \"new\" }")
    );
}

#[test]
fn a_landed_write_says_a_running_dog_keeps_its_env_and_how_to_give_it_the_new_one() {
    let (_dir, paths, mut app) = opened_over(fixtures::OTEL_BARE);

    arm_new_key(&mut app, "otel", "PADDOCK_KEY", "x");
    let (write, then) = confirm(&mut app);
    let effect = land(&mut app, &paths.daemon_config, write, then);

    assert_eq!(effect, Effect::LoadSettings, "the key list reads the file");
    let notice = app.notice().unwrap().to_string();
    assert!(notice.contains("keeps the one it started with"), "{notice}");
    assert!(notice.contains("`shep daemon reload`"), "{notice}");
    assert!(
        notice.contains("`shep disable otel` then `shep enable otel`"),
        "{notice}"
    );
}

#[test]
fn a_read_only_lookout_opens_no_editor() {
    let (_dir, _paths, mut app) = opened_over(fixtures::OTEL_WITH_ENV);
    app.set_control_for_tests(Control::ReadOnly);

    fixtures::select_setting(&mut app, "dog.otel.env+");
    let _ = app.update(Msg::Key(KeyPress::Confirm));

    assert!(app.settings().unwrap().env_typing().is_none());
    assert_eq!(app.mode(), InputMode::Normal);
    assert!(app.notice().unwrap().is_grave());
}

#[test]
fn escape_drops_the_editor_and_what_was_typed() {
    let (_dir, _paths, mut app) = opened_over(fixtures::OTEL_BARE);

    fixtures::select_setting(&mut app, "dog.otel.env+");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    type_text(&mut app, "PADDOCK_KEY");
    let _ = app.update(Msg::Key(KeyPress::TextAbandon));

    assert!(app.settings().unwrap().env_typing().is_none());
    assert!(app.settings().unwrap().pending().is_none());
    assert_eq!(app.mode(), InputMode::Normal);
}

#[test]
fn a_key_that_is_not_a_shell_name_is_refused_and_keeps_the_cursor_on_it() {
    let (_dir, _paths, mut app) = opened_over(fixtures::OTEL_BARE);

    fixtures::select_setting(&mut app, "dog.otel.env+");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    type_text(&mut app, "9lives");
    let _ = app.update(Msg::Key(KeyPress::TextApply));

    let notice = app.notice().unwrap().to_string();
    assert!(notice.contains("\"9lives\" is not a valid key"), "{notice}");
    let (_, editor) = app.settings().unwrap().env_typing().unwrap();
    assert_eq!(editor.focus(), crate::lookout::keyvalue::Field::Key);
}

/// The value is typed, armed and sent, and after that is in no state the app
/// holds.
#[test]
fn no_state_holds_the_value_once_the_write_is_sent() {
    let (_dir, paths, mut app) = opened_over(fixtures::OTEL_BARE);

    arm_new_key(&mut app, "otel", "PADDOCK_KEY", "hunter2");
    assert!(
        !format!("{:?}", app.settings().unwrap().pending).contains("hunter2"),
        "the armed edit prints a byte count"
    );
    let (write, then) = confirm(&mut app);
    assert!(!format!("{write:?}").contains("hunter2"));
    assert!(
        matches!(app.settings().unwrap().pending, Some(Pending::Sent { .. })),
        "the sent marker carries a sentence and a ticket, no value"
    );
    let _ = land(&mut app, &paths.daemon_config, write, then);

    assert!(app.settings().unwrap().pending().is_none());
    assert!(!format!("{app:?}").contains("hunter2"));
}

/// IR-41: the editor and the armed edit hold the value while it is typed, and
/// neither prints it.
#[test]
fn the_pending_env_edit_prints_lengths_and_never_the_text() {
    let (_dir, _paths, mut app) = opened_over(fixtures::OTEL_BARE);

    fixtures::select_setting(&mut app, "dog.otel.env+");
    let _ = app.update(Msg::Key(KeyPress::Confirm));
    type_text(&mut app, "PADDOCK_KEY");
    let _ = app.update(Msg::Key(KeyPress::TextApply));
    type_text(&mut app, "hunter2");
    assert_eq!(
        format!("{:?}", app.settings().unwrap().pending.as_ref().unwrap()),
        "TypingEnv { dog: \"otel\", editor: KeyValues { add: true, key: <11 chars>, value: <7 chars>, focus: Value } }"
    );

    let _ = app.update(Msg::Key(KeyPress::TextApply));
    let armed = format!("{:?}", app.settings().unwrap().pending.as_ref().unwrap());
    assert!(armed.contains("[(\"PADDOCK_KEY\", Some(EnvValue(<7 bytes>)))]"), "{armed}");
    assert!(!armed.contains("hunter2"), "{armed}");
}
