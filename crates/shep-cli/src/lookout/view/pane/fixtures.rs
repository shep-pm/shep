//! Panes and readers the tests in this module's siblings build on.
//!
//! One place rather than one copy per file: the same `web` sheep is the
//! subject of tests in nearly every one of them, and a fixture that drifts
//! between two copies is a test that passes for the wrong reason.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::text::Line;

use super::super::super::app::App;
use super::super::super::pane::{ConfigPane, PaneRow};
use super::super::fixtures;
use super::draw::pane_lines;
use crate::lookout::frames::render_text;

/// The pane the rest of this module renders: `web`, with two overridden
/// fields, one pending and two env keys.
pub(super) fn web_pane() -> ConfigPane {
    ConfigPane::sheep(fixtures::sheep_config_view())
}

/// Every line as a plain string, styles dropped.
pub(super) fn text_of(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

/// A dog pane whose one array field is marked `x-shep-secret`, with two
/// elements already set.
///
/// No list sub-screen: this pane has no widget for an array at all, and a
/// secret array is read-only in every pane, so the row is the whole screen
/// has to get right.
pub(super) fn secret_array_dog_pane() -> ConfigPane {
    let schema = serde_json::json!({
        "properties": {
            "tokens": {
                "type": "array",
                "items": { "type": "string" },
                "x-shep-secret": true,
            }
        }
    });
    let mut pane = ConfigPane::dog(
        "watch".into(),
        None,
        schema,
        "tokens = [\"ab12cd34\", \"ef56gh78\"]\n".into(),
        Vec::new(),
    );
    pane.move_to_key("tokens");
    pane
}

/// A dog whose `db` table holds an `x-shep-secret` field one level down,
/// cursor on `db.password`, so both that row and the panel beside it draw
/// the secret. The table itself is two dotted rows, not one.
pub(super) fn nested_secret_dog_pane() -> ConfigPane {
    let schema = serde_json::json!({
        "properties": { "db": { "$ref": "#/$defs/Db" } },
        "$defs": {
            "Db": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "password": { "type": "string", "x-shep-secret": true },
                },
            },
        },
    });
    let mut pane = ConfigPane::dog(
        "watch".into(),
        None,
        schema,
        "[db]\nhost = \"pg.internal\"\npassword = \"ab12cd34\"\n".into(),
        Vec::new(),
    );
    pane.move_to_key("db.password");
    pane
}

/// A dog whose `hosts` is an array of tables, one holding an
/// `x-shep-secret` key. A shape [`flattened`] has no rows to walk, so this
/// stays one row drawn whole as JSON, with the secret inside it.
pub(super) fn opaque_secret_dog_pane() -> ConfigPane {
    let schema = serde_json::json!({
        "properties": { "hosts": { "type": "array", "items": { "$ref": "#/$defs/Host" } } },
        "$defs": {
            "Host": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "key": { "type": "string", "x-shep-secret": true },
                },
            },
        },
    });
    let mut pane = ConfigPane::dog(
        "watch".into(),
        None,
        schema,
        "[[hosts]]\nname = \"a\"\nkey = \"ab12cd34\"\n".into(),
        Vec::new(),
    );
    pane.move_to_key("hosts");
    pane
}

/// A dog whose one string field is `x-shep-secret`, edited once. No
/// Flockfile field is secret today, but a dog's schema can mark one,
/// and the pending-edits section has to mask it the same way
/// [`field_line`] already does for the row it scrolls off of.
pub(super) fn secret_dog_pane_with_an_edit() -> ConfigPane {
    let schema = serde_json::json!({
        "properties": {
            "token": {
                "type": "string",
                "x-shep-secret": true,
            }
        }
    });
    let mut pane = ConfigPane::dog(
        "watch".into(),
        None,
        schema,
        "token = \"ab12cd34\"\n".into(),
        Vec::new(),
    );
    pane.move_to_key("token");
    pane.begin_typing();
    for c in "ef56gh78".chars() {
        pane.type_char(c);
    }
    assert!(
        pane.apply_typing().is_none(),
        "the fixture must type a value the field takes"
    );
    pane
}

/// A sheep whose `args` are `args`, for the list sub-screen's own
/// tests.
pub(super) fn web_with_args(args: &[&str]) -> shep_core::protocol::SheepConfigView {
    let config = shep_core::config::AppConfig {
        name: "web".to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        ..Default::default()
    };
    shep_core::protocol::SheepConfigView::new(config, Vec::new(), Vec::new())
}

/// The pane's cursor, walked onto `key` the way an operator walks it.
/// A thin wrapper: [`fixtures::select_field`] is this exact walk, and
/// this module had its own copy before the tab row gave a field's
/// group somewhere to switch to first.
pub(super) fn pane_to(app: &mut App, key: &str) {
    fixtures::select_field(app, key);
}

/// The whole frame at `height`, 120 columns wide, through the same
/// `note_body_rows` and `draw` the event loop runs before each one.
pub(super) fn screen_at(app: &mut App, height: u16) -> String {
    screen_of(app, 120, height)
}
/// [`screen_at`] at a width of the caller's own choosing.
pub(super) fn screen_of(app: &mut App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    app.note_body_rows(super::super::body_rows(area));
    app.note_body_width(width);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::draw(app, frame))
        .unwrap();
    render_text(terminal.backend().buffer())
}
/// How many rows the frame marks as selected. One, always.
pub(super) fn marked(text: &str) -> usize {
    text.lines().filter(|line| line.starts_with('>')).count()
}

/// The longest word in the cursor's field help, which is what the blurb
/// tests match on.
///
/// The blurb wraps to `BLURB_WRAP`, so a `contains` against the whole help
/// text fails for a reason unrelated to what these tests pin. The longest
/// word is the one least likely to be split by a wrap or shared with
/// another row.
pub(super) fn blurb_anchor(pane: &ConfigPane) -> String {
    let help = field_help_under_cursor(pane);
    help.split_whitespace()
        .max_by_key(|word| word.len())
        .expect("the field help is empty")
        .to_owned()
}
/// The `help` string of the field under the cursor, whichever it is.
pub(super) fn field_help_under_cursor(pane: &ConfigPane) -> String {
    let Some(PaneRow::Field(index)) = pane.cursor() else {
        panic!("the cursor is not on a field");
    };
    pane.fields().fields()[index].help.clone()
}

/// A dog pane over the bark dog, with a sink in its section.
pub(super) fn bark_pane() -> ConfigPane {
    let schema = crate::dog::builtin_schema("bark").expect("bark is a built-in");
    ConfigPane::dog("bark".into(), None, schema, "poll = \"60s\"\nhistory_bytes = 4096\n\n[sinks.ops]\nkind = \"slack\"\nurl = \"https://hooks.example/x\"\n"
            .into(), Vec::new())
}

/// A dog pane over a section whose nested table is written as its own
/// `[models.worker]` header: the credential two tables down is a row of its
/// own, under `models` and `worker`, and `worker` starts closed.
pub(super) fn nested_table_dog_pane() -> ConfigPane {
    ConfigPane::dog(
        "jobs".into(),
        None,
        fixtures::nested_dog_schema(),
        fixtures::nested_dog_section(),
        Vec::new(),
    )
}

/// The same nested schema as a table `web` carries for `jobs`, so the
/// pane is a per-sheep table pane.
pub(super) fn nested_sheep_dog_pane() -> ConfigPane {
    let table = serde_json::json!({
        "name": "small",
        "models": { "worker": { "model": "gpt", "token": "sk-live-51Hx9Qa" } },
    });
    ConfigPane::sheep_dog(
        "web".into(),
        "jobs".into(),
        &fixtures::nested_dog_schema(),
        table.as_object().cloned().expect("a table"),
    )
}

/// [`pane_lines`]'s own rows for the sheep [`fixtures::app_in_sheep_pane`]
/// opens, rendered plain: the config pane's own equivalent of
/// [`fixtures::draw_lines`], which draws the bleats pane instead.
pub(super) fn config_pane_lines_for_tests(
    app: &App,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    let pane = app.config_pane().expect("the pane is open");
    pane_lines(pane, fixtures::plain(), width, height)
}
