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
