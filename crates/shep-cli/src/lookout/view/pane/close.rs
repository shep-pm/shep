//! The close dialog: what happens to the edits the operator has filed.
//!
//! The dialog is prose rather than a table, because the answer depends on
//! what is filed: how many fields, whether any of them needs a respawn, and
//! whether the reload it would take overlaps or leaves a gap. Every sentence
//! below is assembled from those three facts and wrapped to the width it has.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::super::app::{CONFIRM_EXPIRY, CloseDialog};
use super::super::super::pane::ReloadKind;
use super::super::super::theme::Palette;
use super::super::cell;
use super::super::flock::fit;
use super::super::overlay;
use super::layout::{BOX_WIDTH, body_width, columns, wrap};
use crate::vocabulary::Role;

/// `""` for one, `"S"` for every other count: the plural suffix
/// [`close_dialog_heading`] and [`close_dialog_naming_sentence`] both
/// append to a bare noun.
const fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "S" }
}

/// `"NEEDS"` for one, `"NEED"` for every other count: the noun `plural`
/// inflects and the verb agreeing with it are two different words, and
/// every fixture in this file happens to file two edits, which is exactly
/// why a mismatched verb went unnoticed until a real screen showed one.
const fn needs_or_need(count: usize) -> &'static str {
    if count == 1 { "NEEDS" } else { "NEED" }
}

/// The column the heading's right clause starts at, when the row is wide
/// enough to hold both clauses: `docs/lookout/design-files/README.md`'s
/// own mock, which puts `catcher is online, pid 71578` there.
const HEADING_SHEEP_COLUMN: usize = 36;

/// The heading row: the question on the left, the sheep it is about on
/// the right.
///
/// The right clause goes first when `body` cannot hold both. The left
/// clause is the question itself, and the pane's own title band names the
/// sheep too but is dimmed behind the box, which is the whole reason the
/// right clause exists: an operator answering a question that restarts a
/// process should not have to read around the dialog to learn which one.
fn close_dialog_heading_row(dialog: &CloseDialog, body: u16) -> String {
    let left = close_dialog_heading(dialog);
    let right = close_dialog_sheep_clause(dialog);
    let left_w = columns(&left);
    // One space of separation at minimum, however far past the column the
    // left clause runs.
    let gap = HEADING_SHEEP_COLUMN.saturating_sub(left_w).max(1);
    if left_w + gap + columns(&right) > usize::from(body) {
        return left;
    }
    format!("{left}{}{right}", " ".repeat(gap))
}

/// `catcher is online, pid 71578`, or `catcher is online` for a sheep the
/// shepherd runs several of, where no single pid is the answer.
fn close_dialog_sheep_clause(dialog: &CloseDialog) -> String {
    let state = format!("{} is {}", dialog.target_name(), dialog.status());
    match dialog.pid() {
        Some(pid) => format!("{state}, pid {pid}"),
        None => state,
    }
}

/// The question the heading asks, one of three depending on which half of
/// it fired.
fn close_dialog_heading(dialog: &CloseDialog) -> String {
    match (dialog.unsent(), dialog.parked()) {
        (0, parked) => format!("{parked} FIELD{} ALREADY WAITING", plural(parked)),
        (unsent, 0) => format!(
            "{unsent} EDIT{} {} A RESPAWN",
            plural(unsent),
            needs_or_need(unsent)
        ),
        (unsent, parked) => format!(
            "{unsent} EDIT{} {} A RESPAWN, {parked} FIELD{} ALREADY DID",
            plural(unsent),
            needs_or_need(unsent),
            plural(parked)
        ),
    }
}

/// The unsent fields, named in a sentence and truncated past three:
/// `cwd and err_file take hold when the process starts again.` or `cwd,
/// err_file and 3 more take hold when the process starts again.`
fn close_dialog_naming_sentence(fields: &[String]) -> String {
    let list = match fields {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [first, second, third] => format!("{first}, {second} and {third}"),
        [first, second, rest @ ..] => {
            format!("{first}, {second} and {} more", rest.len())
        }
    };
    let verb = if fields.len() == 1 { "takes" } else { "take" };
    format!("{list} {verb} hold when the process starts again.")
}

/// The reload row's own sentence, one of four off `dialog.reload()` and
/// `dialog.instances()`.
///
/// The `SO_REUSEPORT` caveat rides on both overlap lines: an app with no
/// readiness probe overlaps either way, and needs it exactly as much as a
/// `reuse_port` app does if it binds an address.
fn close_dialog_reload_sentence(dialog: &CloseDialog) -> String {
    let graceful = dialog.graceful_timeout();
    match (dialog.reload(), dialog.instances()) {
        (ReloadKind::Overlap, 1) => {
            "the replacement starts alongside and takes over. No gap, if the app sets \
             SO_REUSEPORT itself."
                .to_owned()
        }
        (ReloadKind::Overlap, _) => {
            "one instance at a time, each replacement alongside the one it replaces. No gap, \
             if the app sets SO_REUSEPORT itself."
                .to_owned()
        }
        (ReloadKind::Serial, 1) => format!(
            "drains it, then starts the replacement. Up to {graceful}, so slower than a \
             restart for the same gap."
        ),
        (ReloadKind::Serial, n) => format!(
            "one instance at a time, each drained before its replacement starts. Up to \
             {graceful} each, 1 of {n} down at a time."
        ),
    }
}

/// One option row, indented and styled the way every other pane line in
/// this file is.
fn close_dialog_option_line(text: String, palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {}", fit(&text, width)),
        palette.ground(),
    ))
}

/// `"L   reload           "`, aligned with the `R` and `c` rows' own
/// label columns (all three are the same width): what
/// [`close_dialog_reload_lines`] indents a continuation row under.
const RELOAD_LABEL: &str = "L   reload           ";

/// The reload row, wrapped rather than truncated.
///
/// [`fit`] truncates with an ellipsis, which is right for a table cell but
/// wrong here: the sentence's own tail is the `SO_REUSEPORT` caveat
/// (`close_dialog_reload_sentence`'s own doc), the correction the design
/// added after refusing an earlier, uncaveated "No downtime, slower". A
/// truncated row ships exactly the claim that correction exists to
/// prevent. `docs/lookout/design-files/README.md`'s own mock wraps this
/// row onto a continuation line indented under the label instead, for
/// both the overlap and the serial sentence, so this does too, at every
/// width: the box's own interior is a fixed 86 columns regardless of the
/// terminal's, so this wraps even at a comfortable terminal width.
fn close_dialog_reload_lines(
    dialog: &CloseDialog,
    palette: Palette,
    body: u16,
) -> Vec<Line<'static>> {
    let label_w = u16::try_from(RELOAD_LABEL.chars().count()).unwrap_or(0);
    let sentence = close_dialog_reload_sentence(dialog);
    let available = usize::from(body.saturating_sub(label_w));
    let indent = " ".repeat(usize::from(label_w));
    wrap(&sentence, available)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let text = if index == 0 {
                format!("{RELOAD_LABEL}{chunk}")
            } else {
                format!("{indent}{chunk}")
            };
            close_dialog_option_line(text, palette, body)
        })
        .collect()
}

/// What a row of the dialog is, so that a terminal too short for all of
/// them sheds the ones it can afford to lose.
///
/// Ordered by what losing the row costs, cheapest last: [`shed_dialog_rows`]
/// drops the greatest first. [`DialogRow::Key`] is the floor and is never
/// dropped, since a key an operator cannot see is an answer they cannot
/// give, and `esc` no longer writes on its own.
///
/// [`DialogRow::Naming`] goes before [`DialogRow::Continuation`], which
/// is the call this order exists to record. Losing the naming sentence
/// costs a whole, self-contained row whose fields are also named by the
/// heading's count and by the pane's own pending section behind the box.
/// Losing a continuation costs the second half of a row that is still on
/// screen: the reload sentence's tail is the `SO_REUSEPORT` caveat, which
/// is the condition on the only cost claim this dialog makes, and
/// `docs/lookout/design-files/rulings.md` refused the frame's own
/// uncaveated `No downtime, slower` over exactly that. At 90x8 this
/// ordering keeps the sentence whole.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DialogRow {
    /// `R`, `L`, `c` and `esc`: the four rows that name a key.
    Key,
    /// The heading, naming both halves and the sheep.
    Heading,
    /// The reload sentence's own wrapped tail.
    Continuation,
    /// The sentence naming the fields a respawn is what applies.
    Naming,
    /// `Everything else you changed is already live.`
    Live,
    /// A separator.
    Blank,
}

/// The dialog's rows: what a terminal under [`overlay::floor_for`]`(BOX_WIDTH)`
/// columns gets full width, and what the boxed form draws inside its own
/// border.
///
/// `now` is the caller's own clock, against which the `esc` row states
/// what is left of [`CONFIRM_EXPIRY`] since `dialog.at()`: seconds and a
/// ten-cell gauge, saying the same thing twice on purpose, since the
/// design's own rule is that colour and glyph never carry anything the
/// words do not.
#[must_use]
pub(in crate::lookout::view) fn close_dialog_lines(
    dialog: &CloseDialog,
    palette: Palette,
    width: u16,
    now: Instant,
) -> Vec<Line<'static>> {
    close_dialog_rows(dialog, palette, width, now)
        .into_iter()
        .map(|(_, line)| line)
        .collect()
}

/// [`close_dialog_lines`], each row carrying what it would cost to lose.
fn close_dialog_rows(
    dialog: &CloseDialog,
    palette: Palette,
    width: u16,
    now: Instant,
) -> Vec<(DialogRow, Line<'static>)> {
    let body = body_width(width);
    // `band`, not `attention`: every other band on this dashboard reverses
    // its role's colour rather than merely tinting the text, and 12a's own
    // rule is that colour is always redundant with the words, so `NO_COLOR`
    // has to lose decoration, never information. `attention` alone drops
    // both under `NO_COLOR`, since it carries no modifier at all.
    let mut lines = vec![(
        DialogRow::Heading,
        Line::from(Span::styled(
            format!("  {}", fit(&close_dialog_heading_row(dialog, body), body)),
            palette.band(Role::Butter),
        )),
    )];
    if dialog.unsent() > 0 {
        let sentence = close_dialog_naming_sentence(dialog.unsent_fields());
        lines.push((
            DialogRow::Naming,
            close_dialog_option_line(sentence, palette, body),
        ));
        if dialog.live() > 0 {
            lines.push((
                DialogRow::Live,
                close_dialog_option_line(
                    "Everything else you changed is already live.".to_owned(),
                    palette,
                    body,
                ),
            ));
        }
    }
    lines.push((DialogRow::Blank, Line::from(Span::raw(""))));
    lines.push((
        DialogRow::Key,
        close_dialog_option_line(
            format!(
                "R   restart now      stop, then start. The stop takes up to {}.",
                dialog.kill_timeout()
            ),
            palette,
            body,
        ),
    ));
    lines.extend(
        close_dialog_reload_lines(dialog, palette, body)
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let kind = if index == 0 {
                    DialogRow::Key
                } else {
                    DialogRow::Continuation
                };
                (kind, line)
            }),
    );
    lines.push((
        DialogRow::Key,
        close_dialog_option_line(
            "c   continue         write them and leave it running. They wait for a respawn."
                .to_owned(),
            palette,
            body,
        ),
    ));
    lines.push((DialogRow::Blank, Line::from(Span::raw(""))));
    let elapsed = now.saturating_duration_since(dialog.at());
    let remaining = CONFIRM_EXPIRY.saturating_sub(elapsed);
    lines.push((
        DialogRow::Key,
        close_dialog_option_line(
            format!(
                "esc  keep editing, write nothing   \u{b7}   this prompt expires in {}s {}",
                remaining.as_secs(),
                cell::gauge(
                    u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX),
                    Some(u64::try_from(CONFIRM_EXPIRY.as_millis()).unwrap_or(u64::MAX)),
                    10
                )
            ),
            palette,
            body,
        ),
    ));
    lines
}

/// Drops rows from `rows` until it is no taller than `height`, in
/// [`DialogRow`]'s own order: the blank separators, then the two
/// explanatory sentences, then the reload sentence's wrapped tail, then
/// the heading.
///
/// The four [`DialogRow::Key`] rows survive every height, which is what
/// the design means by flooring the borderless form: below six rows the
/// field list this is drawn over cannot render either, and an operator
/// with no key on screen has no way to answer and, since `esc` stopped
/// writing on its own, no way to write. Shorter still than the four is a
/// terminal `view::draw` refuses outright, but [`Buffer`] indexes with a
/// panic rather than a clip, so the truncation at the end is the one that
/// keeps a resize from taking the dashboard down.
///
/// A shed continuation ends its sentence early, so the row above it takes
/// the ellipsis [`fit`] leaves on a cell cut for width. What the screen
/// cannot show, it says, the same rule the bleats feed follows when it
/// counts the lines it discarded.
///
/// `width` is the whole row's, [`GUTTER`] included, since that is what a
/// marked row must still fit inside.
fn shed_dialog_rows(rows: &mut Vec<(DialogRow, Line<'static>)>, height: u16, width: u16) {
    let height = usize::from(height);
    while rows.len() > height {
        let sheddable = rows
            .iter()
            .enumerate()
            .filter(|(_, (kind, _))| *kind != DialogRow::Key)
            .max_by_key(|(index, (kind, _))| (*kind, *index))
            .map(|(index, _)| index);
        let Some(index) = sheddable else { break };
        let cut = rows.remove(index).0 == DialogRow::Continuation;
        // The last continuation is always the one shed, so the row above
        // it is the rest of the same sentence: an earlier continuation, or
        // the `L` row itself.
        if cut && let Some((_, line)) = index.checked_sub(1).and_then(|above| rows.get_mut(above)) {
            mark_cut(line, width);
        }
    }
    rows.truncate(height);
}

/// Ends `line` with an ellipsis, inside `width` columns.
///
/// A no-op on a row that already carries one, which a row [`fit`] cut for
/// width does: one marker says the row was cut, and two say nothing more.
fn mark_cut(line: &mut Line<'static>, width: u16) {
    // The loop below cannot clear at a width of zero, since popping an
    // empty string is a no-op, and it would spin. Unreachable through the
    // one call site, which is fed the same `area.width` `view::draw`
    // refuses below `MIN_TERM_WIDTH`, so this says out loud what a second
    // call site would have to keep true.
    debug_assert!(width > 0, "mark_cut needs a column to put the marker in");
    let Some(span) = line.spans.last_mut() else {
        return;
    };
    let mut text = span.content.trim_end().to_owned();
    if text.ends_with('\u{2026}') {
        return;
    }
    while columns(&text) + 1 > usize::from(width) {
        text.pop();
    }
    text.push('\u{2026}');
    span.content = text.into();
}

/// The dialog on top of the muted pane: boxed at [`overlay::floor_for`]`(BOX_WIDTH)`
/// and above, full width with no border below it, and full width with no
/// border at any width when the box is taller than the rows there are.
///
/// A box cannot shed rows the way the borderless form can, since its
/// border pair is what makes it a box, and half a box is worse than none.
/// So a terminal too short for the whole box gives way to the borderless
/// form, which is the same answer the width rule already gives one column
/// under [`overlay::floor_for`]`(BOX_WIDTH)`.
pub(super) fn draw_close_dialog(
    dialog: &CloseDialog,
    palette: Palette,
    now: Instant,
    area: Rect,
    buffer: &mut Buffer,
) {
    if overlay::is_boxed(area.width, BOX_WIDTH) {
        let lines = close_dialog_lines(dialog, palette, BOX_WIDTH, now);
        if overlay::boxed_height(&lines) <= area.height {
            overlay::draw_boxed(&lines, BOX_WIDTH, palette, Style::reset(), area, buffer);
            return;
        }
    }
    draw_borderless_close_dialog(dialog, palette, now, area, buffer);
}

/// The full-width, borderless form: bottom-anchored over the field list,
/// the same rows a terminal under [`overlay::floor_for`]`(BOX_WIDTH)` always
/// drew before this task, so a gallery scene one column below the floor
/// still gets the form it exists to show rather than a clipped box.
///
/// [`shed_dialog_rows`] is what keeps this inside `area`: a narrow
/// terminal wraps the reload sentence over more rows, so the form is
/// tallest exactly where there is least room for it.
fn draw_borderless_close_dialog(
    dialog: &CloseDialog,
    palette: Palette,
    now: Instant,
    area: Rect,
    buffer: &mut Buffer,
) {
    let mut rows = close_dialog_rows(dialog, palette, area.width, now);
    shed_dialog_rows(&mut rows, area.height, area.width);
    let top = area.y
        + area
            .height
            .saturating_sub(u16::try_from(rows.len()).unwrap_or(0));
    // One allocation for the whole dialog rather than one per row.
    let blank = overlay::blank_of(area.width);
    for (offset, (_, line)) in rows.iter().enumerate() {
        let offset = u16::try_from(offset).unwrap_or(0);
        overlay::blank_row(buffer, area.x, top + offset, &blank, Style::reset());
        buffer.set_line(area.x, top + offset, line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::text_of;
    use super::*;
    use crate::lookout::frames::render_text;
    use crate::lookout::view::MIN_TERM_WIDTH;
    use crate::lookout::view::fixtures;
    use crate::lookout::view::flock::MIN_HEIGHT;
    use crate::output::width::visible_width;

    /// Bounded on purpose. A frame-wide `contains("respawn")` passes off
    /// 1e's own legend row, which is drawn underneath this dialog and says
    /// the word. Assert on the dialog's rows, never on the frame.
    #[test]
    fn the_dialog_names_both_halves_in_its_heading() {
        let dialog = fixtures::close_dialog_with(2, 1);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let heading = text_of(&lines)[0].trim().to_string();
        assert!(
            heading.starts_with("2 EDITS NEED A RESPAWN, 1 FIELD ALREADY DID"),
            "{heading:?}"
        );
    }

    /// The other half of the heading, and the only place the dialog says
    /// which sheep it is about: the pane's own title band says so too, but
    /// it is dimmed behind the box by the time this question is asked.
    #[test]
    fn the_heading_names_the_sheep_its_state_and_its_pid() {
        let dialog = fixtures::close_dialog_with(1, 0);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let heading = text_of(&lines)[0].trim().to_string();
        assert!(heading.ends_with("web is online, pid 71578"), "{heading:?}");
    }

    /// The other half of `running_state`'s answer: several instances name
    /// no one pid, so the clause names the sheep and its state and stops
    /// there, rather than trailing a `pid` with nothing after it.
    #[test]
    fn a_sheep_with_no_single_pid_gets_a_heading_that_names_none() {
        let dialog = fixtures::close_dialog_without_a_pid();
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let heading = text_of(&lines)[0].trim().to_string();
        assert!(heading.ends_with("web is online"), "{heading:?}");
        assert!(!heading.contains("pid"), "{heading:?}");
    }

    /// The right clause is the first thing to go when the row cannot hold
    /// both: the left clause is the question itself. Swept at every width
    /// the dialog draws at, so a clause that overran the border or
    /// collided with the question would show up as a row wider than its
    /// own body.
    #[test]
    fn a_narrow_heading_drops_the_sheep_and_keeps_the_question() {
        for width in [MIN_TERM_WIDTH, 40, 51, 60, 69, 70, BOX_WIDTH, 89, 120, 160] {
            let dialog = fixtures::close_dialog_with(2, 1);
            let lines = close_dialog_lines(&dialog, fixtures::plain(), width, dialog.at());
            let heading = text_of(&lines)[0].clone();
            assert!(
                visible_width(&heading) <= usize::from(width),
                "{width}: {heading:?}"
            );
            let trimmed = heading.trim();
            assert!(
                trimmed.starts_with("2 EDIT"),
                "the question survives at {width}: {heading:?}"
            );
            // 43 for the question, 2 for the gutter, 1 for the gap and 24
            // for `web is online, pid 71578`: the clause draws from 70
            // columns up and is gone below that, never truncated.
            //
            // 69 and 70 are in the list above for that sentence alone. The
            // widths either side of them ran 60 and then 86, so the edge
            // this line names sat in a gap the loop stepped over, and the
            // assertion encoded the rule without ever exercising it.
            let named = heading.contains("web is online, pid 71578");
            assert_eq!(named, width >= 70, "{width}: {heading:?}");
        }
    }

    /// The noun and the verb are two different words `plural` and
    /// `needs_or_need` each inflect on their own; every other fixture in
    /// this file files two edits, which is exactly why a verb that never
    /// agreed with a singular subject went unnoticed until a real screen
    /// showed one.
    #[test]
    fn a_single_edit_gets_a_singular_verb() {
        let dialog = fixtures::close_dialog_with(1, 0);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        assert!(
            text_of(&lines)[0]
                .trim()
                .starts_with("1 EDIT NEEDS A RESPAWN"),
            "{:?}",
            text_of(&lines)[0]
        );
    }

    #[test]
    fn a_serial_reload_does_not_promise_no_gap() {
        let dialog = fixtures::close_dialog_reloading(ReloadKind::Serial, 1);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let reload = fixtures::row_starting_with(&lines, "L");
        assert!(reload.contains("slower than a restart"), "{reload}");
        assert!(!reload.contains("No gap"), "{reload}");
    }

    #[test]
    fn an_overlapping_reload_carries_the_reuse_port_caveat() {
        let dialog = fixtures::close_dialog_reloading(ReloadKind::Overlap, 1);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let reload = fixtures::row_starting_with(&lines, "L");
        assert!(
            reload.contains("if the app sets SO_REUSEPORT itself"),
            "{reload}"
        );
    }

    /// The caveat's own last word, not a `contains` on a prefix of it: a
    /// row truncated with `fit`'s ellipsis would still pass
    /// `contains("No gap")`, which is exactly the bug this test exists to
    /// catch. Every width here is one the box or the borderless form
    /// actually draws at (the box's own interior is a fixed [`BOX_WIDTH`]
    /// regardless of the terminal, so a wide terminal still wraps).
    #[test]
    fn the_reload_sentence_wraps_rather_than_truncates_at_every_width() {
        for width in [
            BOX_WIDTH,
            overlay::floor_for(BOX_WIDTH) - 1,
            MIN_TERM_WIDTH,
            160,
        ] {
            for kind in [ReloadKind::Overlap, ReloadKind::Serial] {
                for instances in [1, 3] {
                    let dialog = fixtures::close_dialog_reloading(kind, instances);
                    let sentence = close_dialog_reload_sentence(&dialog);
                    let last_word = sentence.split_whitespace().next_back().unwrap();
                    let lines = close_dialog_lines(&dialog, fixtures::plain(), width, dialog.at());
                    let joined = text_of(&lines).join(" ");
                    assert!(
                        joined.contains(last_word),
                        "width {width}, {kind:?}, {instances} instances: {joined}"
                    );
                }
            }
        }
    }

    /// `docs/terminology.md:20`. The frame calls instances lambs; four
    /// places in the bundle do.
    #[test]
    fn no_line_calls_an_instance_a_lamb() {
        for kind in [ReloadKind::Overlap, ReloadKind::Serial] {
            for instances in [1, 3] {
                let dialog = fixtures::close_dialog_reloading(kind, instances);
                let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
                for row in text_of(&lines) {
                    assert!(!row.contains("lamb"), "{row}");
                }
            }
        }
    }

    #[test]
    fn the_restart_row_states_the_sheeps_own_kill_timeout() {
        let dialog = fixtures::close_dialog_with(1, 0);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let restart = fixtures::row_starting_with(&lines, "R");
        assert!(restart.contains("5s"), "{restart}");
    }

    /// The sentence draws when the filed set holds a live field alongside
    /// the one that needs a respawn, so an operator reading the dialog is
    /// not left thinking nothing else they changed took effect.
    #[test]
    fn everything_else_is_already_live_draws_beside_a_live_edit() {
        let dialog = fixtures::close_dialog_with_live_edit(true);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        assert!(
            text_of(&lines)
                .iter()
                .any(|line| line.contains("Everything else you changed is already live")),
            "{:?}",
            text_of(&lines)
        );
    }

    /// The other direction: a filed set that is entirely `cwd` (needs a
    /// respawn, nothing else) draws no such claim. A sentence that always
    /// draws would pass the test above without saying anything.
    #[test]
    fn everything_else_is_already_live_does_not_draw_with_nothing_else_filed() {
        let dialog = fixtures::close_dialog_with_live_edit(false);
        let lines = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        assert!(
            !text_of(&lines)
                .iter()
                .any(|line| line.contains("Everything else you changed is already live")),
            "{:?}",
            text_of(&lines)
        );
    }

    /// The countdown is redundant on purpose: the seconds and the gauge say
    /// the same thing twice, so both have to move together as `now`
    /// advances toward `CONFIRM_EXPIRY`.
    #[test]
    fn the_gauge_shortens_as_now_advances() {
        let dialog = fixtures::close_dialog_with(1, 0);
        let fresh = close_dialog_lines(&dialog, fixtures::plain(), 120, dialog.at());
        let fresh_row = fixtures::row_starting_with(&fresh, "esc");
        let fresh_filled = fresh_row.matches('\u{2588}').count();

        let halfway = dialog.at() + CONFIRM_EXPIRY / 2;
        let later = close_dialog_lines(&dialog, fixtures::plain(), 120, halfway);
        let later_row = fixtures::row_starting_with(&later, "esc");
        let later_filled = later_row.matches('\u{2588}').count();

        assert_eq!(fresh_filled, 10, "{fresh_row}");
        assert_eq!(later_filled, 5, "{later_row}");
    }

    #[test]
    fn the_box_draws_at_its_floor_and_not_one_column_below() {
        let floor = overlay::floor_for(BOX_WIDTH);
        assert!(overlay::is_boxed(floor, BOX_WIDTH));
        assert!(!overlay::is_boxed(floor - 1, BOX_WIDTH));
    }

    #[test]
    fn the_borderless_form_spans_the_whole_width_and_never_clips() {
        let rendered = fixtures::render_dialog(89, 48);
        let heading = fixtures::row_containing(&rendered, "NEEDS A RESPAWN");
        assert!(
            !heading.contains('▐'),
            "no border below the floor: {heading}"
        );
        assert!(
            visible_width(&heading) <= 89,
            "clipped or overran: {heading}"
        );
    }

    /// Every size the dashboard claims to support, through the real
    /// `view::draw` rather than a pane-local fixture: the fixture hands
    /// the pane a `Rect` as tall as the terminal, and the rows the dialog
    /// overran were the ones `draw` keeps back for the title band and the
    /// status bar.
    ///
    /// [`Buffer`]'s own `Index` panics rather than clipping, so a dialog
    /// taller than the rows it was given takes the whole dashboard down
    /// with it. Both forms are swept: the box gives way to the borderless
    /// form when it cannot fit, and the borderless form sheds rows.
    ///
    /// The four rows that name a key are what makes the answer reachable,
    /// and `esc` no longer writes on its own, so a dialog missing one of
    /// them leaves an operator with no way to answer and no way to write.
    #[test]
    fn the_dialog_fits_every_size_the_dashboard_supports_and_stays_answerable() {
        for width in [MIN_TERM_WIDTH, 40, 51, 89, 90, 120, 160] {
            for height in MIN_HEIGHT..=24 {
                let rendered = render_text(&fixtures::render_dialog(width, height));
                for needle in ["restart now", "reload", "continue", "keep editing"] {
                    assert!(
                        rendered.contains(needle),
                        "{width}x{height} lost {needle:?}:\n{rendered}"
                    );
                }
                // The frame above hides a one-row overrun: the status bar
                // is drawn after the pane and repaints the row a dialog
                // one too tall reached into. This draws the pane alone
                // into a buffer of exactly its own rows, where the same
                // overrun is the panic it really is.
                let _ = fixtures::draw_pane_with_dialog(width, height);
            }
        }
    }

    /// The shedding order's own claim, which is a different one from the
    /// marker's: when exactly one of the naming sentence and the reload
    /// continuation can survive, the continuation is what survives.
    ///
    /// 90x8 is the captured case, six body rows against the seven the
    /// dialog wants. Asserted as both halves at once, the sentence whole
    /// AND the naming row gone, because either half alone passes with the
    /// order flipped: a cut caveat is still marked, politely, by
    /// [`mark_cut`].
    ///
    /// The tail this protects is the `SO_REUSEPORT` caveat, the condition
    /// on the only cost claim the dialog makes, and the thing
    /// `docs/lookout/design-files/rulings.md` refused the frame's own
    /// `No downtime, slower` over.
    #[test]
    fn the_reload_caveat_outlives_the_naming_sentence() {
        let app = fixtures::app_with_close_dialog();
        let dialog = app.close_dialog().expect("the dialog is up");
        let whole = close_dialog_reload_sentence(dialog);
        let naming = close_dialog_naming_sentence(dialog.unsent_fields());

        let rendered = render_text(&fixtures::render_dialog(90, 8));
        let rows = reload_rows(&rendered);
        let last = rows.last().expect("the reload row draws at every size");
        assert!(
            rows.join(" ").contains(&whole),
            "the caveat is what survives: {rows:?}"
        );
        assert!(
            !last.ends_with('\u{2026}'),
            "nothing was cut, so nothing is marked: {rows:?}"
        );
        assert!(
            !rendered.contains(&naming),
            "the naming sentence is what went instead:\n{rendered}"
        );
    }

    /// The marker's own claim: at a height where the continuation cannot
    /// survive whatever the order, the row above it says so.
    ///
    /// 90x7 is one row shorter than the case above, so the sentence is
    /// past saving there; 33x6 is the floor, where only the four key rows
    /// fit at all.
    #[test]
    fn a_continuation_that_cannot_survive_leaves_the_cut_marked() {
        for (width, height) in [(90u16, 7u16), (MIN_TERM_WIDTH, MIN_HEIGHT)] {
            let rendered = render_text(&fixtures::render_dialog(width, height));
            let rows = reload_rows(&rendered);
            assert_eq!(rows.len(), 1, "{width}x{height}: {rows:?}");
            let last = rows.last().expect("the reload row draws at every size");
            assert!(
                last.ends_with('\u{2026}'),
                "{width}x{height}: the cut is unmarked: {rows:?}"
            );
        }
    }

    /// The invariant over every size, under both claims above: a cut is
    /// never silent. Asserted on the joined rows, never on a prefix, since
    /// a `contains` on the first row passes on exactly the broken output
    /// this came from (`No gap, if the app`, and the sentence stops).
    #[test]
    fn a_shed_reload_continuation_leaves_the_cut_marked() {
        let app = fixtures::app_with_close_dialog();
        let dialog = app.close_dialog().expect("the dialog is up");
        let whole = close_dialog_reload_sentence(dialog);
        for width in [MIN_TERM_WIDTH, 40, 51, 89, 90, 120, 160] {
            for height in MIN_HEIGHT..=24 {
                let rendered = render_text(&fixtures::render_dialog(width, height));
                let rows = reload_rows(&rendered);
                let joined = rows.join(" ");
                // Either the whole sentence is there, or some row says it
                // was cut: a shed continuation marks the row above it, and
                // a word longer than the column marks its own row. Never a
                // prefix check, which passes on the broken output.
                assert!(
                    joined.contains(&whole) || rows.iter().any(|row| row.ends_with('\u{2026}')),
                    "{width}x{height}: the caveat went missing unmarked: {rows:?}"
                );
            }
        }
    }

    /// The reload sentence's own rows in `frame`: the `L` row and the
    /// continuations indented under it, up to the `c` row that ends them.
    ///
    /// Read from inside the border when there is one, since the muted
    /// pane behind the box keeps drawing to the right of it.
    ///
    /// # Panics
    ///
    /// If no `L` row is on screen, which every size draws one of.
    #[track_caller]
    fn reload_rows(frame: &str) -> Vec<String> {
        let mut rows = Vec::new();
        for line in frame.lines() {
            let text = dialog_interior(line);
            if rows.is_empty() {
                if text.starts_with("L   reload") {
                    rows.push(text);
                }
                continue;
            }
            if text.starts_with("c   continue") || text.is_empty() {
                break;
            }
            rows.push(text);
        }
        assert!(!rows.is_empty(), "no reload row in:\n{frame}");
        rows
    }

    /// One frame row's dialog content: what the box holds, or the whole
    /// row when the borderless form is drawn.
    ///
    /// `▐` and `▌` are the box's own left and right edge glyphs
    /// ([`overlay::draw_boxed`]'s constants), spelled out rather than
    /// named: this fixture reads rendered text, not `overlay` internals.
    fn dialog_interior(line: &str) -> String {
        let inside = match (line.find('▐'), line.rfind('▌')) {
            (Some(left), Some(right)) if left < right => &line[left + '▐'.len_utf8()..right],
            _ => line,
        };
        inside.trim().to_owned()
    }

    /// Dimming changes style and leaves every character alone, so a test
    /// that asserts text here is asserting nothing.
    #[test]
    fn the_pane_behind_the_dialog_is_muted() {
        let buffer = fixtures::draw_pane_with_dialog(160, 48);
        let behind = buffer[(2, 4)].style();
        assert_eq!(behind.fg, fixtures::plain_dimmed().fg);
    }

    /// Muting is a colour operation: under `NO_COLOR` there is no ink to
    /// dim with, so only the reset half of the mute pass does anything and
    /// the pane behind goes completely flat, no reverse video and no
    /// background, rather than staying lit. The border and the
    /// reverse-video heading carry the separation on their own then.
    #[test]
    fn no_color_flattens_the_pane_behind_instead_of_leaving_it_lit() {
        let buffer = fixtures::draw_pane_with_dialog_and_palette(160, 48, fixtures::no_color());
        // The title band: `title_band_line` styles its whole row
        // `REVERSED`, the one modifier the mute pass has to clear even
        // when there is no colour to dim with.
        let title_band = buffer[(2, 0)].style();
        assert_eq!(title_band.add_modifier, ratatui::style::Modifier::empty());
    }

    /// The cell at `(x, y)` in `buffer`'s own rendered text where row `y`
    /// contains `needle`, `x` being the column `needle` starts at plus
    /// `offset`.
    ///
    /// # Panics
    ///
    /// Panics if no row contains `needle`, a fixture bug rather than a
    /// failure the test is about.
    #[track_caller]
    fn cell_in_row_containing(buffer: &Buffer, needle: &str, offset: u16) -> ratatui::style::Style {
        let text = render_text(buffer);
        let (y, line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(needle))
            .unwrap_or_else(|| panic!("no row contains {needle:?}"));
        let x = line.find(needle).unwrap_or(0);
        let x = u16::try_from(x).unwrap_or(0) + offset;
        let y = u16::try_from(y).unwrap_or(0);
        buffer[(x, y)].style()
    }

    /// 12a's own rule: colour is always redundant with the text, so
    /// `NO_COLOR` loses decoration and never information. The heading's
    /// only decoration is the `REVERSED` band every other chip on this
    /// dashboard carries; a heading styled with `attention` alone (a bare
    /// foreground colour) would lose it entirely under `NO_COLOR`, since
    /// `attention` sets no modifier for `NO_COLOR` to leave behind.
    #[test]
    fn the_heading_stays_a_reversed_band_under_no_color() {
        let buffer = fixtures::draw_pane_with_dialog_and_palette(160, 48, fixtures::no_color());
        let heading = cell_in_row_containing(&buffer, "NEEDS A RESPAWN", 0);
        assert!(
            heading
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
    }
}
