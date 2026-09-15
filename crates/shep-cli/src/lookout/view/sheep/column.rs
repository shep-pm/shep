//! The read-only config and env column: the left of the two columns under
//! the charts, and the rule that closes it.
//!
//! Reads [`SheepPane::config`](crate::lookout::pane_sheep::SheepPane), never
//! the dashboard's selection, so a pane pinned to a sheep the flock table
//! has reseated away from still draws the sheep it was opened on. Shows a
//! value the way `shep edit` already shows it, with no cursor and no lock
//! glyph: those belong to the editing pane this column never opens.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};
use shep_core::protocol::SheepConfigView;

use crate::lookout::field::{Field, FieldSet};
use crate::lookout::pane;
use crate::lookout::pane_sheep::SheepPane;
use crate::lookout::theme::Palette;

use super::super::flock::fit;
use super::layout::{
    COLUMN_BODY_ROWS, COLUMN_HEADER_ROW, COLUMN_NAME_W, COLUMN_WIDTH, DIVIDER_COL,
};

/// Where the config/env column and the feed start, in rows relative to
/// `area`: [`COLUMN_HEADER_ROW`] whenever `height` still reaches it, or
/// right below the identity band once it drops under
/// [`MIN_HEIGHT_FOR_CHARTS`](super::layout::MIN_HEIGHT_FOR_CHARTS) and the
/// charts stop drawing at all. The config
/// and feed columns are what the pane is for, so they give ground last,
/// reclaiming the rows the charts would have used rather than staying
/// pinned to a row a short terminal can never reach.
pub(super) fn column_top_row(height: u16) -> u16 {
    if height > COLUMN_HEADER_ROW {
        COLUMN_HEADER_ROW
    } else {
        1
    }
}

/// The eight Flockfile groups' fields, plus the env keys, as the column
/// scrolls through them: one entry per rendered row, and every group label
/// this pass actually emitted, in the order it emitted them.
///
/// Both halves are read off the same walk, so a group header the field
/// loop below skips or misorders shows up wrong in both places at once
/// rather than only in the one a test happens to check.
fn column_body(view: &SheepConfigView, palette: Palette) -> (Vec<Line<'static>>, Vec<String>) {
    let (fields, values) = pane::sheep_fields(&view.config);
    let mut lines = Vec::new();
    let mut groups = Vec::new();
    let mut current: Option<&str> = None;
    for field in fields.fields() {
        // `env` is its own section below, with its own keys; a second row
        // here would repeat it under a `Map` field's own placeholder text.
        if field.key == "env" {
            continue;
        }
        if field.group.as_deref() != current {
            current = field.group.as_deref();
            let label = current.unwrap_or_default().to_owned();
            push_group_header(&mut lines, &label, palette);
            groups.push(label);
        }
        let pending = view.pending.iter().any(|key| key == &field.key);
        let overridden = view.overridden.iter().any(|key| key == &field.key);
        lines.push(field_row_line(
            &fields, field, &values, pending, overridden, palette,
        ));
    }
    push_group_header(&mut lines, "env", palette);
    for key in &view.env_keys {
        let sealed = view.env_secrets.iter().any(|secret| secret == key);
        lines.push(env_row_line(key, sealed, palette));
    }
    (lines, groups)
}

/// A blank separator, a rule the column's own width, then `label`: the
/// chrome [`column_body`] spends above every group and above the env
/// section it closes with.
fn push_group_header(lines: &mut Vec<Line<'static>>, label: &str, palette: Palette) {
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "\u{2500}".repeat(usize::from(COLUMN_WIDTH)),
        palette.muted(),
    )));
    lines.push(Line::from(Span::styled(label.to_owned(), palette.muted())));
}

/// [`column_body`]'s lines alone, for [`draw_column`] and [`column_len`],
/// neither of which needs the group labels back.
fn column_body_lines(view: &SheepConfigView, palette: Palette) -> Vec<Line<'static>> {
    column_body(view, palette).0
}

/// The one line the column draws while [`SheepPane::config`] is still
/// `None`: read once by [`draw_column`] and by
/// [`SheepPane::body_len`](crate::lookout::pane_sheep::SheepPane), so
/// scrolling can never claim a body one line longer than what is actually
/// drawn.
fn waiting_line(palette: Palette) -> Line<'static> {
    Line::from(Span::styled("reading config\u{2026}", palette.muted()))
}

/// How many lines [`draw_column`] would need to show every one of
/// `config`'s groups and env keys, or the single waiting line while there
/// is none yet. What [`SheepPane`]'s own `Viewport` scrolls through.
///
/// Colour never changes a line count, so [`Palette::detect`] is called
/// with nothing set rather than threading the real palette through from a
/// key handler that has no `Frame` to read one off.
#[must_use]
pub(crate) fn column_len(config: Option<&SheepConfigView>) -> usize {
    match config {
        Some(view) => column_body_lines(view, Palette::detect(None, None, None)).len(),
        None => 1,
    }
}

/// One field's row: the name, `!`-flagged and butter when
/// [`SheepConfigView::pending`] names it, `*`-flagged when
/// [`SheepConfigView::overridden`] names it instead (pending wins when both
/// apply, `view::pane::field_row::field_line`'s own rule, since the value on
/// screen
/// is not what the running child holds), the value, `(unset)`/`(default)`
/// muted like the name, anything else in the column's own body colour, and
/// `awaits respawn` right-aligned when pending.
///
/// Read-only: no lock glyph, no cost cell. Those belong to the editing
/// pane this column never opens; a value here is what `shep edit` already
/// shows, laid out for a screen with no cursor to carry.
fn field_row_line(
    fields: &FieldSet,
    field: &Field,
    values: &Map<String, Value>,
    pending: bool,
    overridden: bool,
    palette: Palette,
) -> Line<'static> {
    let raw = field_value_text(fields, field, values);
    let flag = if pending {
        "!"
    } else if overridden {
        "*"
    } else {
        ""
    };
    let note = if pending { "awaits respawn" } else { "" };
    let value_w = usize::from(COLUMN_WIDTH).saturating_sub(usize::from(COLUMN_NAME_W) + 2);
    let left_w = value_w.saturating_sub(note.chars().count());
    let key_style = if pending {
        palette.attention()
    } else {
        palette.muted()
    };
    let value_style = if pending {
        palette.attention()
    } else if matches!(raw.as_str(), "(unset)" | "(default)") {
        palette.muted()
    } else {
        Style::default()
    };
    let mut spans = vec![
        Span::styled(
            fit(&format!("{flag}{}", field.key), COLUMN_NAME_W),
            key_style,
        ),
        Span::raw("  "),
        Span::styled(fit(&raw, left_w as u16), value_style),
    ];
    if !note.is_empty() {
        spans.push(Span::styled(note.to_owned(), palette.attention()));
    }
    Line::from(spans)
}

/// `field`'s current value, the way this column shows it: `<set>` for a
/// [`Field::secret`] one (never the value, the same guard
/// [`ConfigPane::field_line`](crate::lookout::pane::ConfigPane) draws its own
/// row through), `(unset)` for a JSON `null` or a key the config never
/// carried, `(default)` when what is there is exactly the schema's own
/// default, else [`pane::resolved_display`]'s answer: a bare
/// `MemSize`/`UpDuration` number annotated with its unit, the same as
/// [`ConfigPane::display_value`](crate::lookout::pane::ConfigPane::display_value)
/// draws for the same field.
fn field_value_text(fields: &FieldSet, field: &Field, values: &Map<String, Value>) -> String {
    let raw = match values.get(&field.key) {
        None | Some(Value::Null) => return "(unset)".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(other) => other.to_string(),
    };
    if field.secret {
        "<set>".to_owned()
    } else if field.default.as_deref() == Some(raw.as_str()) {
        "(default)".to_owned()
    } else {
        pane::resolved_display(fields, &field.key, &raw)
    }
}

/// One env row: the key alone, or, when [`SheepConfigView::env_secrets`]
/// names it, a butter block run standing in for the value it never
/// carries, the word `sealed`, and `edit in S` right-aligned. No value is
/// ever read: the block run is a fixed run of glyphs, not sized to
/// anything the store holds.
fn env_row_line(key: &str, sealed: bool, palette: Palette) -> Line<'static> {
    if !sealed {
        return Line::from(Span::raw(key.to_owned()));
    }
    let note = "edit in S";
    let value_w = usize::from(COLUMN_WIDTH).saturating_sub(usize::from(COLUMN_NAME_W) + 2);
    let left_w = value_w.saturating_sub(note.chars().count());
    let mid = format!("{}  sealed", "\u{2588}".repeat(8));
    Line::from(vec![
        Span::styled(fit(key, COLUMN_NAME_W), palette.muted()),
        Span::raw("  "),
        Span::styled(fit(&mid, left_w as u16), palette.attention()),
        Span::styled(note.to_owned(), palette.muted()),
    ])
}

/// The header row: `e edit`, and how many fields
/// [`SheepConfigView::pending`] is still carrying, once there is a config
/// to read one off. No `tab next group`: no key routes one, the same defect
/// the status bar carried until Task 7 fixed it for that frame.
fn column_header_line(pane: &SheepPane, palette: Palette) -> Line<'static> {
    let mut text = "\u{2588}\u{2588} CONFIG & ENV   e edit".to_owned();
    if let Some(pending) = pane
        .config()
        .map(|view| view.pending.len())
        .filter(|count| *count > 0)
    {
        text.push_str(&format!("   {pending} pending"));
    }
    Line::from(Span::styled(text, palette.muted()))
}

/// `top` to [`COLUMN_BODY_ROWS`] rows below it: the header, then as much of
/// [`column_body`] as [`SheepPane::view`]'s own offset scrolls into.
///
/// `top` is [`column_top_row`]'s answer, not always [`COLUMN_HEADER_ROW`]:
/// a short terminal moves the whole column up rather than truncating it
/// from a fixed row that terminal can never reach.
///
/// Reads [`SheepPane::config`], never
/// [`App::selected`](crate::lookout::app::App::selected): the same rule
/// [`draw`](super::draw)'s own identity band and
/// [`draw_charts`](super::charts::draw_charts) already follow, for
/// the reason both of their own doc comments give.
pub(super) fn draw_column(
    pane: &SheepPane,
    top: u16,
    area: Rect,
    buffer: &mut Buffer,
    palette: Palette,
) {
    write_column_row(buffer, area, top, &column_header_line(pane, palette));
    let body = match pane.config() {
        Some(view) => column_body_lines(view, palette),
        None => vec![waiting_line(palette)],
    };
    let offset = pane.view().offset();
    for (i, line) in body.iter().skip(offset).take(COLUMN_BODY_ROWS).enumerate() {
        write_column_row(buffer, area, top + 1 + i as u16, line);
    }
}

/// Writes one already-styled line into `buffer`, `row` cells below `area`'s
/// own top, clipped to [`COLUMN_WIDTH`] rather than the pane's own width:
/// this column never spends the cells the feed sits after.
fn write_column_row(buffer: &mut Buffer, area: Rect, row: u16, line: &Line<'static>) {
    if row >= area.height {
        return;
    }
    buffer.set_line(area.x, area.y + row, line, COLUMN_WIDTH);
}

/// The `\u{2502}` rule between the config/env column and the feed, one cell
/// wide, for every row the two sides draw into, from `top` (the same row
/// [`draw_column`] and [`draw_feed`](super::feed::draw_feed) were handed)
/// through
/// [`COLUMN_BODY_ROWS`] below it.
pub(super) fn draw_divider(top: u16, area: Rect, buffer: &mut Buffer, palette: Palette) {
    let rule = Line::from(Span::styled("\u{2502}", palette.muted()));
    for row in top..=(top + COLUMN_BODY_ROWS as u16) {
        if row >= area.height {
            break;
        }
        buffer.set_line(area.x + DIVIDER_COL, area.y + row, &rule, 1);
    }
}

#[cfg(test)]
mod tests {
    use shep_core::config::AppConfig;
    use shep_core::protocol::{ProcessInfo, Response};
    use shep_core::status::ProcStatus;

    use crate::lookout::app::{Body, KeyPress, Msg, RowKey, Sent};
    use crate::lookout::frames::render_text;

    use super::super::super::fixtures;
    use super::super::draw;
    use super::super::layout::{COLUMN_LAST_ROW, FEED_WIDTH, FEED_X, MIN_HEIGHT_FOR_COLUMN};
    use super::*;

    /// `web`, with `max_memory` parked until a respawn and two env keys.
    fn web_view() -> SheepConfigView {
        let mut config = AppConfig {
            name: "web".to_owned(),
            ..AppConfig::default()
        };
        config
            .env
            .insert("DB_HOST".to_owned(), "db.internal".to_owned());
        config
            .env
            .insert("API_KEY".to_owned(), "{{secret:API}}".to_owned());
        SheepConfigView::new(config, Vec::new(), vec!["max_memory".to_owned()])
    }

    /// `web`, with `max_memory` overridden and nothing pending: the
    /// editing pane's own `*` glyph (`view/pane.rs`'s `field_line`), pinned
    /// on this column's side of Decision 4's "same markers" rule.
    fn web_view_overridden() -> SheepConfigView {
        let config = AppConfig {
            name: "web".to_owned(),
            ..AppConfig::default()
        };
        SheepConfigView::new(config, vec!["max_memory".to_owned()], Vec::new())
    }

    /// A bare config carrying exactly `pairs` as its env, nothing pending or
    /// overridden: what [`a_sealed_key_is_marked_and_a_plain_one_is_not`]
    /// needs to tell a plain key from a sealed one without `web_view`'s own
    /// pending field in the way.
    fn view_with_env(pairs: &[(&str, &str)]) -> SheepConfigView {
        let mut config = AppConfig {
            name: "test".to_owned(),
            ..AppConfig::default()
        };
        for (key, value) in pairs {
            config.env.insert((*key).to_owned(), (*value).to_owned());
        }
        SheepConfigView::new(config, Vec::new(), Vec::new())
    }

    /// One line as a plain string, styles dropped: the same flattening
    /// `view::pane`'s own `text_of` does, one line at a time.
    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// The groups [`column_body`] actually emitted, in the order it emitted
    /// them: read off the same walk [`draw_column`] draws from, not
    /// recomputed independently, so a bug in that walk shows up here too.
    fn group_labels_of(view: &SheepConfigView) -> Vec<String> {
        column_body(view, fixtures::plain()).1
    }

    /// The rendered text of the one row naming `key`.
    fn field_row_of(view: &SheepConfigView, key: &str) -> String {
        column_body_lines(view, fixtures::plain())
            .iter()
            .map(line_text)
            .find(|line| line.trim_start_matches(['!', '*']).starts_with(key))
            .unwrap_or_else(|| panic!("no row for {key}"))
    }

    /// The env section's own rows, everything after the `env` label line.
    fn env_rows_of(view: &SheepConfigView) -> Vec<String> {
        let lines: Vec<String> = column_body_lines(view, fixtures::plain())
            .iter()
            .map(line_text)
            .collect();
        let label = lines
            .iter()
            .position(|line| line.trim() == "env")
            .expect("column_body always closes with an env label");
        lines[label + 1..].to_vec()
    }

    /// The one row naming `key`, out of `rows`.
    fn row_for<'a>(key: &str, rows: &'a [String]) -> &'a str {
        rows.iter()
            .find(|row| row.contains(key))
            .unwrap_or_else(|| panic!("no row for {key} in {rows:?}"))
    }

    /// The whole column's text, `None` standing in for a pane whose config
    /// has not landed yet.
    fn column_of(config: Option<SheepConfigView>) -> String {
        let lines = match &config {
            Some(view) => column_body_lines(view, fixtures::plain()),
            None => vec![waiting_line(fixtures::plain())],
        };
        lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
    }

    /// The group labels [`crate::lookout::pane::ConfigPane`]'s own field set
    /// carries for `web_view`'s config, in schema order, deduplicated
    /// consecutively the same way [`column_body`]'s own walk dedupes. The
    /// source of truth [`the_groups_are_the_schemas_eight_in_its_own_order`]
    /// checks the column against, instead of a copy of the order typed by
    /// hand: `column_body` layers its own `env`-skipping logic on top of the
    /// same [`pane::sheep_fields`] this reads, so a real cross-check is what
    /// would catch that column-specific divergence and a literal cannot.
    fn config_pane_group_labels(view: &SheepConfigView) -> Vec<String> {
        let pane = crate::lookout::pane::ConfigPane::sheep(view.clone());
        let mut labels = Vec::new();
        let mut current: Option<&str> = None;
        for field in pane.fields().fields() {
            if field.group.as_deref() != current {
                current = field.group.as_deref();
                labels.push(current.unwrap_or_default().to_owned());
            }
        }
        labels
    }

    /// Eight groups, in the schema's own order, the same order
    /// [`ConfigPane`](crate::lookout::pane::ConfigPane)'s own field set gives
    /// for the same config. The frame lists seven and puts `restart` second;
    /// `cron` is missing from it entirely.
    #[test]
    fn the_groups_are_the_schemas_eight_in_its_own_order() {
        let view = web_view();
        assert_eq!(group_labels_of(&view), config_pane_group_labels(&view));
    }

    /// A field parked until the next respawn is marked and says so.
    #[test]
    fn a_pending_field_is_marked_and_annotated() {
        let row = field_row_of(&web_view(), "max_memory");
        assert!(row.starts_with('!'), "{row:?}");
        assert!(row.contains("awaits respawn"), "{row:?}");
    }

    /// An overridden field carries the editing pane's own `*` glyph
    /// (`view/pane.rs`'s `field_line`), not `!`: pending and overridden are
    /// different facts, and `field_row_line` had no branch for this one at
    /// all before this task, so `SheepConfigView::overridden` was read by
    /// nothing in this column. `restart_delay` is neither pending nor
    /// overridden in this fixture, so its row must stay unmarked: a
    /// regression that marked every row once the list was non-empty would
    /// still pass the `max_memory` assertion alone.
    #[test]
    fn an_overridden_field_is_marked_with_the_editing_panes_own_glyph() {
        let row = field_row_of(&web_view_overridden(), "max_memory");
        assert!(row.starts_with('*'), "{row:?}");
        assert!(
            !row.contains("awaits respawn"),
            "overridden is not pending: {row:?}"
        );

        let other_row = field_row_of(&web_view_overridden(), "restart_delay");
        assert!(
            !other_row.starts_with('*') && !other_row.starts_with('!'),
            "an unrelated field must not pick up the marker: {other_row:?}"
        );
    }

    /// The wire clears env before the struct is built, so no pane can show a
    /// value. This test exists so a later change that starts carrying them
    /// fails here rather than shipping.
    #[test]
    fn no_env_value_reaches_the_column() {
        let rendered = env_rows_of(&web_view()).join("");
        assert!(!rendered.contains("hunter2"), "{rendered:?}");
    }

    /// A key the store fills renders sealed; a Flockfile key does not.
    #[test]
    fn a_sealed_key_is_marked_and_a_plain_one_is_not() {
        let rows = env_rows_of(&view_with_env(&[
            ("PLAIN", "v"),
            ("SEALED", "{{secret:PW}}"),
        ]));
        assert!(row_for("SEALED", &rows).contains("sealed"));
        assert!(!row_for("PLAIN", &rows).contains("sealed"));
    }

    /// Until the reply lands there is no config, and an empty group list
    /// would read as a sheep that has none.
    #[test]
    fn a_pane_without_its_config_yet_says_so() {
        assert!(column_of(None).contains("reading config"));
    }

    /// `exp_backoff_restart_delay` is the Flockfile schema's longest field
    /// name: 25 characters, 26 with the pending `!` flag. `COLUMN_NAME_W`
    /// has to fit that exactly, not "wide enough" by a comment's own say-so.
    #[test]
    fn the_longest_pending_field_name_is_not_truncated() {
        let config = AppConfig {
            name: "web".to_owned(),
            ..AppConfig::default()
        };
        let view = SheepConfigView::new(
            config,
            Vec::new(),
            vec!["exp_backoff_restart_delay".to_owned()],
        );
        let row = field_row_of(&view, "exp_backoff_restart_delay");
        assert!(
            row.starts_with("!exp_backoff_restart_delay  "),
            "the flagged name and its separator must survive whole: {row:?}"
        );
    }

    /// [`field_value_text`] resolves a bare `MemSize` number the same way
    /// [`ConfigPane::display_value`](crate::lookout::pane::ConfigPane::display_value)
    /// does for the editing pane: both read [`pane::resolved_display`], so a
    /// `max_memory` that reads `52428800` on one screen cannot read
    /// `52428800 B` on the other.
    #[test]
    fn the_column_and_the_editing_pane_resolve_the_same_mem_size_units() {
        use shep_core::values::MemSize;

        let mut config = AppConfig {
            name: "web".to_owned(),
            ..AppConfig::default()
        };
        // Not a multiple of any binary unit, so `MemSize`'s own `Display`
        // falls through to the bare-digit branch `resolved_display` exists
        // to annotate; a round number like `50M` would serialize with its
        // unit already and prove nothing.
        config.max_memory = Some(MemSize::from_bytes(1_234_567));
        let column_view = SheepConfigView::new(config.clone(), Vec::new(), Vec::new());
        let editing_view = SheepConfigView::new(config, Vec::new(), Vec::new());

        let row = field_row_of(&column_view, "max_memory");
        let pane = crate::lookout::pane::ConfigPane::sheep(editing_view);
        let resolved = pane.display_value("max_memory");
        assert!(
            resolved.ends_with(" B"),
            "setup: the fixture must actually exercise the unit suffix: {resolved:?}"
        );
        assert!(
            row.contains(&resolved),
            "column row {row:?} does not carry the editing pane's own {resolved:?}"
        );
    }

    /// [`Field::secret`] guards the sheep column the same way
    /// [`ConfigPane`](crate::lookout::pane::ConfigPane)'s own row draws it: the
    /// value is never rendered, only that there is one. No Flockfile field
    /// carries the flag today, so this is unit-level, over a fabricated
    /// field rather than a real config.
    #[test]
    fn a_secret_field_renders_set_and_never_its_value() {
        use crate::lookout::field::FieldKind;

        let field = Field {
            key: "webhook".to_owned(),
            help: String::new(),
            group: None,
            kind: FieldKind::Text,
            value_kind: None,
            default: None,
            default_value: None,
            secret: true,
            editable: true,
            example: None,
            accepts: Vec::new(),
            refuses: Vec::new(),
            neighbours: Vec::new(),
        };
        let fields = FieldSet::from_fields(vec![field.clone()], &[]);
        let mut values = Map::new();
        values.insert("webhook".to_owned(), Value::String("hunter2".to_owned()));
        assert_eq!(field_value_text(&fields, &field, &values), "<set>");
    }

    /// The regression Tasks 7 and 8 both shipped once each: a pane pinned to
    /// a sheep the flock table has since reseated its selection away from.
    /// The column reads `SheepPane::config` (set only by `adopt_config` and
    /// `set_sheep`, never by the reseat), so it has no equivalent bug
    /// surface to begin with; this pins that a later change cannot grow one
    /// by threading `App::selected` into the column instead.
    #[test]
    fn the_column_does_not_draw_the_sheep_that_replaced_the_pinned_one() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online).build(),
                ProcessInfo::builder(2, "bravo", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "setup: the pane opened on alpha"
        );
        let alpha_config = AppConfig {
            name: "alpha".to_owned(),
            ..AppConfig::default()
        };
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "alpha".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(SheepConfigView::new(
                alpha_config,
                Vec::new(),
                Vec::new(),
            )))),
        });
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(2, "bravo", ProcStatus::Online).build()],
            at: std::time::Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(2)),
            "setup: the reseat moved the selection to bravo"
        );

        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, 80, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        let text = render_text(&buffer);
        assert!(
            text.contains("name                        alpha"),
            "must still draw the pinned sheep's own `name` field: {text:?}"
        );
        assert!(!text.contains("bravo"), "{text:?}");
    }

    /// The divider sits one cell past the config column, at
    /// [`COLUMN_WIDTH`], for every row the two sides draw into.
    #[test]
    fn the_divider_runs_the_full_height_of_the_column_and_feed() {
        let mut app = fixtures::app_with(
            vec![ProcessInfo::builder(1, "alpha", ProcStatus::Online).build()],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let Body::Sheep(pane) = app.body() else {
            panic!("the pane is still open");
        };
        let area = Rect::new(0, 0, FEED_X + FEED_WIDTH, MIN_HEIGHT_FOR_COLUMN);
        let mut buffer = Buffer::empty(area);
        draw(&app, pane, area, &mut buffer);
        for row in COLUMN_HEADER_ROW..=COLUMN_LAST_ROW {
            assert_eq!(
                buffer[(COLUMN_WIDTH, row)].symbol(),
                "\u{2502}",
                "row {row} has no divider"
            );
        }
    }
}
