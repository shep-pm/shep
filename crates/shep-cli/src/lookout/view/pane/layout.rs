//! What the pane spends its width on, and the measuring every line goes
//! through.
//!
//! Column widths are fixed rather than measured from content, the same
//! choice the flock table makes and for the same reason. The one variable
//! is whether the explanation panel fits beside the field list, which
//! [`panel_width`] decides once per frame and every layout below reads.

use ratatui::text::Line;
use shep_core::config::ApplyGroup;

use crate::output::width::char_columns;

/// The columns every line spends on the selection mark and the space after
/// it, before any cell is drawn. `settings::GUTTER`'s twin, private there,
/// and it exists for the reason that one does: a budget that forgets it is
/// a budget every line overruns.
pub(super) const GUTTER: u16 = 2;

/// The close dialog's own interior width, once it is boxed: what
/// [`super::close::close_dialog_lines`] lays its rows out to when [`super::close::draw_close_dialog`]
/// draws the boxed form.
pub(super) const BOX_WIDTH: u16 = 86;

/// The KEY cell at its full width, flag character included. Twenty-six is
/// `exp_backoff_restart_delay` plus its flag, the longest key the Flockfile
/// schema declares, so no field name is truncated at a width that can
/// afford the whole column.
pub(super) const KEY_W: u16 = 26;

/// The floor KEY shrinks to before the COST column is dropped instead.
pub(super) const KEY_MIN: u16 = 8;

/// The floor VALUE shrinks to. Below this the pane drops COST, and below
/// that it draws KEY alone.
pub(super) const VALUE_MIN: u16 = 8;

/// The position cell on the list sub-screen. Three columns holds an index
/// into an array longer than any Flockfile has, and no more: an element is
/// what the row is about.
pub(super) const POSITION_W: u16 = 3;

/// The COST cell. Ten columns, which is exactly `next start`, the longest
/// word [`cost_label`] prints.
pub(super) const COST_W: u16 = 10;

/// The narrowest body that still draws KEY, VALUE and COST.
pub(super) const FULL_WIDTH: u16 = KEY_W + 2 + VALUE_MIN + 2 + COST_W;

/// The narrowest body that still draws a VALUE beside the KEY.
pub(super) const VALUE_WIDTH: u16 = KEY_MIN + 2 + VALUE_MIN;

/// The blurb's own wrap width, independent of whatever width the panel
/// itself draws at: prose reads better narrower than the panel's outer
/// ceiling allows, the same way a magazine column is narrower than the
/// page.
pub(super) const BLURB_WRAP: u16 = 66;

/// The narrowest left column that still draws a marker, a key and a value.
pub(super) const LEFT_MIN: u16 = 40;

/// The narrowest panel that holds a wrapped blurb and a validation list.
pub(super) const PANEL_MIN: u16 = 50;

/// The panel at the design target, and the ceiling everywhere else.
pub(super) const PANEL_MAX: u16 = 72;

/// How wide the explanation panel is, and [`None`] when it does not draw.
///
/// 45% of 160 is 72, so the design target from
/// `docs/brainstorming/specs/2026-09-08-lookout-1e-editing-pane-design.md`
/// falls out of the formula rather than being special cased. Below it the
/// clamp to [`PANEL_MAX`] holds the panel at the design target's own width
/// rather than growing it to fill a wider terminal: the left column takes
/// the remainder instead, the same way [`super::super::flock`]'s own table grows.
/// Above [`LEFT_MIN`] short of `width` the panel cannot hold a wrapped
/// blurb and a validation list beside a left column with room for a
/// marker, a key and a value, and [`super::draw::pane_lines`] falls back to the single
/// column it always drew.
pub(super) fn panel_width(width: u16) -> Option<u16> {
    let wanted = (u32::from(width) * 45 / 100) as u16;
    let panel = wanted.clamp(PANEL_MIN, PANEL_MAX);
    (width.saturating_sub(panel) >= LEFT_MIN).then_some(panel)
}

/// The narrowest terminal that draws `LANDS` beside the panel rather than
/// giving way to it. The design target itself: at 160 the frame's own
/// `FIELD / VALUE / LANDS` header and the `FOCUSED` panel share a row, per
/// `docs/lookout/design-files/rulings.md`'s 1e ruling and
/// `docs/brainstorming/specs/2026-09-08-lookout-1e-editing-pane-design.md`'s
/// width table.
pub(super) const LANDS_WITH_PANEL_MIN: u16 = 160;

/// Whether `LANDS` draws on a terminal `width` columns wide that also draws
/// the panel (`has_panel`).
///
/// `LANDS` and the panel do different jobs: the column is what an operator
/// scans to read every field's cost at once, the panel is a sentence about
/// only the field under the cursor. Where both fit, both draw. Where they
/// cannot, `LANDS` gives way first, since the panel's own impact sentence
/// already names the focused field's cost and nothing else on screen names
/// the column's.
///
/// Always `true` when the panel is absent: nothing else on screen carries
/// cost then, so `LANDS` has no reason to hide.
pub(super) const fn lands_fits_beside_panel(width: u16, has_panel: bool) -> bool {
    !has_panel || width >= LANDS_WITH_PANEL_MIN
}

/// The width the rows are laid out in: the terminal minus [`GUTTER`].
pub(super) const fn body_width(width: u16) -> u16 {
    width.saturating_sub(GUTTER)
}

/// What a change to a field in `group` costs, in an operator's words.
///
/// A prediction from the field's class, not the outcome of a specific
/// write: `watch` says `now` but can park, and `autostart` says `next
/// start` but takes effect at muster. The status bar reports what
/// actually happened; the row's `!` flag is the durable answer.
///
/// [`ApplyGroup`] is `#[non_exhaustive]`, so an untaught field falls back
/// to `respawn`, the most conservative of the four.
pub(super) const fn cost_label(group: ApplyGroup) -> &'static str {
    match group {
        ApplyGroup::Live => "now",
        ApplyGroup::NextSpawn => "next start",
        ApplyGroup::Structural => "read-only",
        ApplyGroup::NeedsRespawn | _ => "respawn",
    }
}

/// The three cell widths for a body of `width`: KEY, VALUE, COST. A zero
/// means the column is not drawn at all.
///
/// The three always sum, with their two-space separators, to exactly
/// `width`, so no line can overrun the terminal it was laid out for.
/// COST goes first when the terminal narrows: arming a field repeats its
/// cost verbatim in the status bar, which is the same reasoning
/// [`super::super::settings`] gives for dropping its own cost cell first.
///
/// `show_lands` is `false` only where the explanation panel is drawn and
/// the terminal is too narrow to hold both beside it (see
/// [`lands_fits_beside_panel`]): the panel still names the focused field's
/// own cost in its impact sentence, so nothing on screen goes unsaid.
/// `false` here behaves as if `width` had fallen under [`FULL_WIDTH`] on its
/// own, freeing what COST would have spent onto VALUE instead.
pub(super) fn widths(width: u16, show_lands: bool) -> (u16, u16, u16) {
    if show_lands && width >= FULL_WIDTH {
        let rest = width - COST_W - 2;
        let key = KEY_W.min(rest - VALUE_MIN - 2);
        (key, rest - key - 2, COST_W)
    } else if width >= VALUE_WIDTH {
        let key = KEY_W.min(width - VALUE_MIN - 2);
        (key, width - key - 2, 0)
    } else {
        (width, 0, 0)
    }
}

/// Masks a secret field's rendered value: `(unset)` passes through, since
/// there is nothing to hide, and anything else becomes `<set>`.
///
/// The one rule every render path in this file applies to a secret before
/// its value reaches the screen, and every one of them calls this rather
/// than spelling the condition again: [`super::field_row::field_line`]'s stored cell and its
/// `old -> new` cell, `list::list_line`'s elements, and both halves of
/// [`super::chrome::pending_edit_line`]. Four inline copies of it agreed with each other
/// until they were routed through here, which is the state a fifth call
/// site would have had to keep up.
pub(super) fn mask_secret(secret: bool, raw: String) -> String {
    if secret && raw != "(unset)" {
        "<set>".to_owned()
    } else {
        raw
    }
}

/// How many columns `text` occupies, the same count [`super::super::flock::fit`] and
/// [`clipped`] measure against.
pub(super) fn columns(text: &str) -> usize {
    text.chars().map(char_columns).sum()
}

/// The columns `line`'s spans occupy, added rather than measured against a
/// fixed cell: a blank separator or an unselected section header is
/// shorter than the left column's own width on its own, and only the
/// panel's own starting column cares where it ends.
pub(super) fn line_columns(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .flat_map(|span| span.content.chars())
        .map(char_columns)
        .sum()
}

/// Wraps `text` at `width` columns, breaking on spaces. A single word
/// longer than `width` is placed on its own (overlong) line rather than
/// split mid-word: [`super::super::super::field::Field::help`] is prose, not data, and a rare overlong
/// word is a smaller wrong than a hyphen this module invented.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_owned()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_w: usize = word.chars().map(char_columns).sum();
        let current_w: usize = current.chars().map(char_columns).sum();
        if current.is_empty() {
            current.push_str(word);
        } else if current_w + 1 + word_w <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// `text`, truncated (never padded) to at most `width` columns, marking a
/// cut with a trailing `…` the way [`super::super::flock::fit`] does.
///
/// Not [`super::super::flock::fit`]: the panel's rows are prose, not a fixed-width table cell,
/// so a short value stays short rather than growing padded trailing spaces
/// across the column. This is the guard the panel's own width tests check:
/// a row built from a live value or a schema-authored sentence cannot push
/// the panel past its own column, even though nothing in today's schemas
/// is long enough to exercise the truncation itself.
pub(super) fn clipped(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let columns: usize = text.chars().map(char_columns).sum();
    if columns <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_columns(c);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_design_target_splits_eighty_eight_and_seventy_two() {
        assert_eq!(panel_width(160), Some(72));
    }

    #[test]
    fn the_panel_goes_below_ninety_columns() {
        assert!(panel_width(90).is_some());
        assert_eq!(panel_width(89), None);
    }

    #[test]
    fn the_panel_never_drops_below_its_own_floor() {
        for width in 90..=200 {
            let panel = panel_width(width).expect("the panel is drawn above 89");
            assert!((50..=72).contains(&panel), "{panel} at {width}");
        }
    }

    #[test]
    fn the_left_column_never_falls_below_its_own_floor() {
        for width in 90..=200 {
            let panel = panel_width(width).expect("the panel is drawn above 89");
            assert!(width - panel >= 40, "{} left at {width}", width - panel);
        }
    }
}
