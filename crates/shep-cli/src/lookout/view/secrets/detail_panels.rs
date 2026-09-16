use super::super::super::app::SecretsPane;
use super::super::super::secrets::SecretRow;
use super::super::super::theme::Palette;
use super::super::flock::fit;
use super::super::status;
use super::row_cells::byte_count;
use crate::secret_readers::Reader;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The FOCUSED panel's width, fixed: the frame's own layout,
/// `docs/brainstorming/specs/2026-09-08-lookout-1h-secrets-design.md`
/// ("The frame"). WHO READS IT takes whatever is left of the row.
const FOCUSED_WIDTH: u16 = 88;

/// How many rows the two panels' own content spends, below their shared
/// header line: fixed at the frame's own count, whatever either panel has
/// to say.
const PANEL_CONTENT_ROWS: u16 = 4;

/// Everything below the hairline: the panels' own header line plus their
/// content, on top of the hairline itself.
pub(super) const PANEL_ROWS: u16 = 1 + 1 + PANEL_CONTENT_ROWS;

/// One [`Reader`]'s line in WHO READS IT: glyph and words together, since a
/// signal carried by colour alone says nothing under `NO_COLOR`.
///
/// Never "holds the value": nothing tells a sheep spawned before a `set`
/// from one spawned after, so the caption states only what the roll can
/// prove either way.
fn reader_line(reader: &Reader) -> String {
    if reader.online {
        format!(
            "\u{2588} {}   online, was given a value at spawn",
            reader.name
        )
    } else {
        format!(
            "\u{2591} {}   not running, reads it at next start",
            reader.name
        )
    }
}

/// What both panels say when no row is selected, so the `+ new key`
/// affordance cannot have one panel calling it nothing and the other
/// describing a key that does not exist.
const NO_KEY_SELECTED: &str = "no key selected";

/// FOCUSED's four content lines for `row`, or a placeholder when nothing is
/// selected (the `+ new key` row, or an empty pane).
///
/// The clipboard sentence lives here rather than only in `y`'s own status
/// notice, since that notice reaches an operator after the value is already
/// on their system clipboard: this is the standing warning, read before `y`
/// is ever pressed.
fn focused_lines(
    row: Option<&SecretRow>,
    width: u16,
) -> [Line<'static>; PANEL_CONTENT_ROWS as usize] {
    let Some(row) = row else {
        return [
            Line::from(Span::raw(fit(NO_KEY_SELECTED, width))),
            Line::default(),
            Line::default(),
            Line::default(),
        ];
    };
    let set_in = if row.set_in.is_empty() {
        "not set here".to_string()
    } else {
        format!("set in {}", row.set_in.join(", "))
    };
    let detail = match row.byte_len {
        Some(len) => format!(
            "{set_in} \u{b7} length {} \u{b7} named by {} of the flock",
            byte_count(len),
            row.readers.len()
        ),
        None => set_in,
    };
    [
        Line::from(Span::raw(fit(
            "the value leaves the screen after 10s. nothing records that you looked.",
            width,
        ))),
        Line::from(Span::raw(fit(
            "the store is 0600: anyone who can reveal can also delete a log.",
            width,
        ))),
        Line::from(Span::raw(fit(
            "the system clipboard is readable by every process on the desktop.",
            width,
        ))),
        Line::from(Span::raw(fit(&detail, width))),
    ]
}

/// How many readers [`who_reads_it_lines`] has room to list before it has
/// to spend a line on an overflow notice instead.
const READER_ROWS: usize = PANEL_CONTENT_ROWS as usize - 1;

/// The text for each of [`READER_ROWS`] reader lines: one per reader when
/// they all fit, otherwise the first `READER_ROWS - 1` plus a line naming
/// how many more there are, so this panel's own total always matches
/// [`focused_lines`]' `named by {} of the flock` rather than looking
/// complete at three when a key has five.
fn reader_row_texts(readers: &[Reader]) -> Vec<String> {
    if readers.is_empty() {
        return vec!["nothing names this key".to_string()];
    }
    if readers.len() <= READER_ROWS {
        return readers.iter().map(reader_line).collect();
    }
    let mut texts: Vec<String> = readers[..READER_ROWS - 1].iter().map(reader_line).collect();
    let overflow = readers.len() - (READER_ROWS - 1);
    texts.push(format!(
        "+ {overflow} more, named by {} of the flock",
        readers.len()
    ));
    texts
}

/// WHO READS IT's four content lines for `row`: up to [`READER_ROWS`]
/// readers, then a caption naming where a reference can live, matching
/// [`focused_lines`]' own row count so the two panels stay lined up.
fn who_reads_it_lines(
    row: Option<&SecretRow>,
    width: u16,
) -> [Line<'static>; PANEL_CONTENT_ROWS as usize] {
    // No row is the `+ new key` affordance, where there is no key for
    // anything to name. Says what FOCUSED says rather than answering a
    // question about a key that does not exist yet.
    let Some(row) = row else {
        return [
            Line::from(Span::raw(fit(NO_KEY_SELECTED, width))),
            Line::default(),
            Line::default(),
            Line::default(),
        ];
    };
    let texts = reader_row_texts(&row.readers);
    let mut lines: Vec<Line<'static>> = (0..READER_ROWS)
        .map(|index| match texts.get(index) {
            Some(text) => Line::from(Span::raw(fit(text, width))),
            None => Line::default(),
        })
        .collect();
    lines.push(Line::from(Span::styled(
        fit("named in env, args, out_file or err_file", width),
        Style::default(),
    )));
    lines.try_into().unwrap_or_else(|_| {
        [
            Line::default(),
            Line::default(),
            Line::default(),
            Line::default(),
        ]
    })
}

/// Draws the FOCUSED and WHO READS IT panels into the last [`PANEL_ROWS`]
/// rows of `area`, below a hairline of their own: FOCUSED at
/// [`FOCUSED_WIDTH`], WHO READS IT taking the rest, per the frame.
pub(super) fn draw_panels(
    pane: &SecretsPane,
    palette: Palette,
    area: Rect,
    buffer: &mut Buffer,
    top: u16,
) {
    let width = area.width;
    buffer.set_line(
        area.x,
        top,
        &status::rule_line(palette.line(), width),
        width,
    );
    let row = pane.model.rows.get(pane.selected);
    let key = row.map_or("", |row| row.key.as_str());
    let right_x = area.x + FOCUSED_WIDTH.min(width);
    let right_width = width.saturating_sub(FOCUSED_WIDTH);

    buffer.set_line(
        area.x,
        top + 1,
        &Line::from(Span::styled(
            fit(&format!("FOCUSED  {key}"), FOCUSED_WIDTH.min(width)),
            palette.muted(),
        )),
        FOCUSED_WIDTH.min(width),
    );
    if right_width > 0 {
        buffer.set_line(
            right_x,
            top + 1,
            &Line::from(Span::styled(
                fit(&format!("WHO READS IT  {key}"), right_width),
                palette.muted(),
            )),
            right_width,
        );
    }

    for (offset, line) in focused_lines(row, FOCUSED_WIDTH.min(width))
        .iter()
        .enumerate()
    {
        let offset = u16::try_from(offset).unwrap_or(0);
        buffer.set_line(area.x, top + 2 + offset, line, FOCUSED_WIDTH.min(width));
    }
    if right_width > 0 {
        for (offset, line) in who_reads_it_lines(row, right_width).iter().enumerate() {
            let offset = u16::try_from(offset).unwrap_or(0);
            buffer.set_line(right_x, top + 2 + offset, line, right_width);
        }
    }
}

#[cfg(test)]
mod tests {

    use crate::lookout::app::{KeyPress, Msg};

    use crate::lookout::view::fixtures;

    /// The affordance names no key, so WHO READS IT cannot answer a
    /// question about one. Both panels say the same thing there.
    #[test]
    fn the_new_key_row_says_no_key_is_selected_in_both_panels() {
        let mut app = fixtures::app_with_secrets_and_control();
        app.update(Msg::Key(KeyPress::SelectLast));
        let buffer = fixtures::render(&app, 160, 48);
        let text = fixtures::rows_of(&buffer);

        assert!(
            !text.iter().any(|l| l.contains("nothing names this key")),
            "there is no key here for anything to name: {text:?}"
        );
        let panels = text
            .iter()
            .find(|l| l.contains("no key selected"))
            .unwrap_or_else(|| panic!("neither panel says it: {text:?}"));
        assert_eq!(
            panels.matches("no key selected").count(),
            2,
            "FOCUSED and WHO READS IT sit on one line and both say it: {panels:?}"
        );
    }

    #[test]
    fn a_reader_is_never_said_to_hold_the_current_value() {
        let buffer = fixtures::render_secrets_with_readers();
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter()
                .any(|l| l.contains("was given a value at spawn")),
            "an online reader: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("reads it at next start")),
            "an offline one: {text:?}"
        );
        assert!(
            !text.iter().any(|l| l.contains("holds the value")),
            "nothing can tell a sheep spawned before a set from one spawned \
                 after, so the pane must not claim it: {text:?}"
        );
    }

    /// The gate is named in the chrome and nowhere else: `focused_lines`
    /// never spells `allow_read`, so a test asserting it over the whole
    /// buffer under a name about the panel passes for the wrong reason.
    #[test]
    fn the_chrome_states_the_gate_and_no_panel_promises_an_audit() {
        let buffer = fixtures::render_secrets_gate_shut();
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter()
                .any(|l| l.contains("allow_read") && l.contains("reveal")),
            "the chrome's own gate line: {text:?}"
        );
        assert!(
            text.iter()
                .any(|l| l.contains("nothing records that you looked")),
            "FOCUSED says what is not kept: {text:?}"
        );
        assert!(
            !text.iter().any(|l| l.contains("audit")),
            "there is no audit log, so promising one is a promise nothing keeps"
        );
    }

    /// Five readers on one key, three rows to list them in: WHO READS IT
    /// has to say two are missing rather than looking complete at three,
    /// and its own total has to match FOCUSED's `named by 5 of the flock`.
    #[test]
    fn who_reads_it_states_an_overflow_it_cannot_list() {
        let buffer = fixtures::render_secrets_with_more_readers_than_fit();
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter().any(|l| l.contains("named by 5 of the flock")),
            "FOCUSED states the true count: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("+ 3 more")),
            "WHO READS IT states what it could not list: {text:?}"
        );
    }

    #[test]
    fn a_missing_roll_says_so_rather_than_showing_an_empty_reader_list() {
        let buffer = fixtures::render_secrets_with_no_roll();
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter().any(|l| l.contains("no muster roll")),
            "an absent roll and a key nothing reads look identical otherwise: {text:?}"
        );
    }
}
