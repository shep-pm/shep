//! One field's row: its key, its value, and the cost column saying what
//! changing it would cost.
//!
//! A row is three cells wide and every cell is measured rather than
//! assumed, because a name, a value and a cost label are all operator data
//! and any of them can be wider than its column.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use shep_core::config::ApplyGroup;

use super::super::super::field::{Field, FieldKind, ValueKind};
use super::super::super::pane::{ConfigPane, Lock, PaneRow};
use super::super::super::theme::Palette;
use super::super::flock::{fit, mark};
use super::layout::{BLURB_WRAP, body_width, cost_label, mask_secret, panel_width, widths, wrap};

/// A section header, indented to match every field row's own mark-and-gap
/// prefix, the same as [`super::super::settings`]'s own.
pub(super) fn section_header(label: &str, palette: Palette) -> Line<'static> {
    Line::from(Span::styled(format!("  {label}"), palette.muted()))
}

/// One field's row: the selection mark, a lock glyph, a flag, the key, the
/// value and what changing it costs.
///
/// The flag matches `shep flock`'s CFG column: `!` for a field parked
/// until a respawn, `*` for one an operator has overridden. Pending wins
/// when both apply, since the value on screen is not what the running
/// child holds.
///
/// The lock is a glyph, not a style, since a style says nothing in
/// `plain`. It sits between the mark and the flag rather than in the cost
/// cell, since cost is the first column [`widths`] drops. See [`Lock`].
///
/// A field with a filed edit shows `old -> new` in the value cell, in
/// butter, and keeps its own flag and lock exactly as computed: `!` and `*`
/// are the shepherd's own words for a different fact, and an edit an
/// operator has not sent yet earns neither.
pub(super) fn field_line(
    pane: &ConfigPane,
    index: usize,
    selected: bool,
    width: u16,
    palette: Palette,
    show_lands: bool,
) -> Line<'static> {
    let Some(field) = pane.fields().fields().get(index) else {
        return Line::default();
    };
    let (key_w, value_w, cost_w) = widths(body_width(width), show_lands);
    let flag = match (pane.is_pending(&field.key), pane.is_overridden(&field.key)) {
        (true, _) => '!',
        (false, true) => '*',
        (false, false) => ' ',
    };
    // A secret's value is never rendered, only whether there is one. The
    // Flockfile schema marks nothing secret today; a dog's own schema can,
    // and this pane draws both. `display_value`, not `value`: a
    // `MemSize`/`UpDuration` field's bare number is resolved for the row,
    // not for whatever an editor would seed.
    let raw = pane.display_value(&field.key);
    // An editor open on this field replaces the cell with what is being
    // typed, cursor included. The same `\u{258f}` every text box in
    // lookout draws; a character rather than a reversed cell because the
    // ANSI gallery renders foregrounds only.
    let typing = pane
        .typing()
        .filter(|typing| typing.key == field.key)
        .map(|typing| &typing.buffer);
    let value = match typing {
        Some(buffer) => format!("{buffer}\u{258f}"),
        None => mask_secret(field.secret, raw),
    };

    let lock = match pane.lock(&field.key) {
        // Fixed: no surface edits this one, not just this pane.
        Some(Lock::Refused) => '=',
        // Shown only. A Flockfile still writes it, and the cost cell beside
        // it reports what doing so would cost.
        Some(Lock::NoWidget) => '~',
        None => ' ',
    };
    let mut rest = String::from(lock);
    rest.push_str(&fit(&format!("{flag}{}", field.key), key_w));
    let cost_cell = (cost_w > 0).then(|| fit(pane.cost(&field.key).map_or("", cost_label), cost_w));

    // The selected row's own paint: a paper-2 ground the full width of the
    // line, and the mark in column 1 in butter rather than plain text.
    // `palette.ground()` is the same call the flock table's selected row
    // makes; `palette.attention()` is `theme.rs`'s own butter, the same
    // colour an edited value already borrows a few lines below.
    let ground = if selected {
        palette.ground()
    } else {
        Style::default()
    };
    let mark_style = if selected {
        palette.attention().patch(ground)
    } else {
        Style::default()
    };
    let mark_span = Span::styled(mark(selected), mark_style);

    // A filed edit, with nothing being typed over it right now: the value
    // cell becomes `old -> new` instead of the stored value alone. Never
    // reached for a locked field, since no key files an edit for one, so
    // `field.editable` is not re-checked here.
    let edited = typing
        .is_none()
        .then(|| pane.edited_value(&field.key))
        .flatten();
    let Some(new_value) = edited else {
        let mut text = rest;
        if value_w > 0 {
            text.push_str("  ");
            text.push_str(&fit(&value, value_w));
        }
        if let Some(cost) = &cost_cell {
            text.push_str("  ");
            text.push_str(cost);
        }
        // Muting reinforces the glyph but carries no fact alone: a `plain`
        // palette renders muted as nothing, so style could not tell a
        // locked row from an editable one on its own.
        let rest_style = if field.editable {
            Style::default()
        } else {
            palette.muted()
        };
        return Line::from(vec![
            mark_span,
            Span::styled(text, rest_style.patch(ground)),
        ]);
    };
    let new_value = mask_secret(field.secret, new_value);
    let mut spans = vec![mark_span, Span::styled(rest, ground)];
    if value_w > 0 {
        spans.push(Span::styled("  ", ground));
        spans.push(Span::styled(
            fit(&format!("{value} -> {new_value}"), value_w),
            palette.attention().patch(ground),
        ));
    }
    if let Some(cost) = cost_cell {
        spans.push(Span::styled("  ", ground));
        spans.push(Span::styled(cost, ground));
    }
    Line::from(spans)
}

/// The rows the field list reserves under its title: the selected field's
/// own help text, wrapped, at widths where the explanation panel cannot
/// draw it. Empty where the panel does.
///
/// Pushes the field-help lines [`top_lines`] returns onto `lines`, spending
/// `budget` down as it goes and stopping one line short of empty: a long
/// wrapped help string must not spend the last line the cursor's own row
/// needs.
pub(super) fn push_wrapped_blurb(
    lines: &mut Vec<Line<'static>>,
    budget: &mut usize,
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
) {
    for (text, style) in top_lines(pane, palette, width) {
        if *budget <= 1 {
            break;
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", fit(&text, body_width(width))),
            style,
        )));
        *budget -= 1;
    }
}

pub(super) fn top_lines(pane: &ConfigPane, palette: Palette, width: u16) -> Vec<(String, Style)> {
    if panel_width(width).is_some() {
        return Vec::new();
    }
    let Some(PaneRow::Field(index)) = pane.cursor() else {
        return Vec::new();
    };
    let Some(field) = pane.fields().fields().get(index) else {
        return Vec::new();
    };
    // Two columns for the indent this row draws with, the same budget
    // `panel_for_field`'s own blurb wraps to.
    wrap(
        &field.help,
        usize::from(BLURB_WRAP.min(width.saturating_sub(2))),
    )
    .into_iter()
    // No indent here. Both callers prepend the pane's own two columns, and
    // the wrap budget above already reserves them, so adding them a second
    // time put the blurb four columns in while every other row in the pane
    // sits at two. `panel_for_field`'s copy of this indents once because it
    // is the thing writing the row.
    .map(|row| (row, palette.muted()))
    .collect()
}

/// What an operator would call `field`'s shape: not the schema keyword, the
/// grammar the widget and [`super::super::super::validation`] already treat it as.
pub(super) fn type_label(field: &Field) -> &'static str {
    match field.value_kind {
        Some(ValueKind::UpDuration) => return "duration",
        Some(ValueKind::MemSize) => return "memory size",
        None => {}
    }
    match &field.kind {
        FieldKind::Bool => "bool",
        FieldKind::Integer => "integer",
        FieldKind::Text | FieldKind::Suggested(_) => "text",
        FieldKind::Choice(_) => "choice",
        FieldKind::Map => "map",
        FieldKind::List(_) => "list",
        FieldKind::Opaque => "opaque",
    }
}

/// The glyph, the word [`cost_label`] already prints in the LANDS/COST
/// cell, and one sentence naming what that word costs the operator, keyed
/// on [`ApplyGroup`] the same way [`cost_label`] is.
///
/// `●` for a change the running sheep picks up on its own; `▲` for one
/// that waits on a start the operator has to choose the timing of. Both
/// are checked against the same East-Asian-width rule `flock.rs:46`
/// applies to `mark`'s own glyph, in this module's own tests: a
/// double-width rendering would shift every column after it.
pub(super) fn impact_tag(group: ApplyGroup) -> (char, &'static str, &'static str) {
    match group {
        ApplyGroup::Live => (
            '\u{25cf}',
            "now",
            "the running sheep picks it up without stopping",
        ),
        ApplyGroup::NextSpawn => (
            '\u{25b2}',
            "next start",
            "takes effect the next time the sheep starts",
        ),
        ApplyGroup::Structural => (
            '\u{25b2}',
            "read-only",
            "shep never writes this; set it in the Flockfile instead",
        ),
        ApplyGroup::NeedsRespawn | _ => {
            ('\u{25b2}', "respawn", "the sheep must stop and start again")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{secret_dog_pane_with_an_edit, text_of, web_pane};
    use super::super::pane_lines;
    use super::*;
    use crate::lookout::view::MIN_TERM_WIDTH;
    use crate::lookout::view::fixtures;
    use crate::output::width::char_columns;
    use shep_core::config::GROUP_ORDER;

    /// Checks `args` too, since shep writes it happily: muting must mark
    /// both reasons a row can be locked, not just the structural one.
    #[test]
    fn a_structural_field_renders_muted_and_the_cost_column_says_why() {
        let lines = all_group_lines(fixtures::coloured());
        let instances = lines
            .iter()
            .find(|line| text_of(core::slice::from_ref(line))[0].contains("instances"))
            .expect("every field is drawn at 89 columns");
        assert!(
            text_of(core::slice::from_ref(instances))[0].contains("read-only"),
            "{instances:?}"
        );
        assert_eq!(
            instances.spans[1].style,
            fixtures::coloured().muted(),
            "a refused row is muted"
        );

        let probe = lines
            .iter()
            .find(|line| text_of(core::slice::from_ref(line))[0].contains("~ liveness_probe"))
            .expect("a field with no widget is drawn too");
        let rendered = text_of(core::slice::from_ref(probe))[0].clone();
        assert!(
            rendered.contains("now") && !rendered.contains("read-only"),
            "shep writes `liveness_probe`, so its cost is a real cost: {rendered:?}"
        );
        assert_eq!(
            probe.spans[1].style,
            fixtures::coloured().muted(),
            "muting says `not from here`, which is true of both kinds"
        );
    }

    /// Every field row over all eight groups, walked with `next_group`,
    /// styled by `palette`. What a test that used to find any field at
    /// one width in one shot now needs, since the active group is the
    /// only one a single render draws.
    ///
    /// 89 columns, one short of [`panel_width`]'s own floor: these tests
    /// are about a field row's own COST cell and lock glyph, which the
    /// panel suppresses once it draws, so the width has to stay below the
    /// ladder rather than at the 120 it used before the ladder existed.
    fn all_group_lines(palette: Palette) -> Vec<Line<'static>> {
        let mut pane = web_pane();
        let mut lines = Vec::new();
        for _ in 0..GROUP_ORDER.len() {
            lines.extend(pane_lines(&pane, palette, 89, 0));
            pane.next_group();
        }
        lines
    }

    /// A field row split into its four fixed leading parts: the selection
    /// mark, the lock glyph, the flag and the key. Positional rather than a
    /// `starts_with`, which only ever matched unselected rows and so could
    /// not see a glyph on the one row the cursor was on.
    fn parts(line: &str) -> Option<(char, char, char, String)> {
        let mut chars = line.chars();
        let mark = chars.next()?;
        let lock = chars.next()?;
        let flag = chars.next()?;
        let key = chars.as_str().split_whitespace().next()?.to_string();
        (mark == '>' || mark == ' ').then_some((mark, lock, flag, key))
    }

    /// Every field row, as its four leading parts. Headers, markers, blanks
    /// and the title are dropped: none of them names a field.
    fn rows_of(text: &[String]) -> Vec<(char, char, char, String)> {
        let keys: Vec<String> = web_pane()
            .fields()
            .fields()
            .iter()
            .map(|field| field.key.clone())
            .collect();
        text.iter()
            .filter_map(|line| parts(line))
            .filter(|(_, _, _, key)| keys.contains(key))
            .collect()
    }

    #[test]
    fn the_flags_mark_exactly_the_overridden_and_pending_fields() {
        let text = text_of(&all_group_lines(fixtures::plain()));
        let flagged = |wanted: char| -> Vec<String> {
            rows_of(&text)
                .into_iter()
                .filter(|(_, _, flag, _)| *flag == wanted)
                .map(|(_, _, _, key)| key)
                .collect()
        };
        assert_eq!(flagged('*'), ["reuse_port", "max_restarts"]);
        assert_eq!(flagged('!'), ["kill_signal"]);
        // 41, not 42: `env` no longer draws its own field row, folded into
        // the env rows below the field list instead.
        assert_eq!(
            rows_of(&text).len(),
            41,
            "every field but env is drawn at 89"
        );
    }

    /// `=` is shep refusing the write outright; `~` is only this pane
    /// having no widget for the shape. The two probes and the level rules
    /// shep writes happily carry `~`, so their cost cell must say `respawn`
    /// or `now`, never `read-only`.
    #[test]
    fn a_refused_field_and_one_the_pane_has_no_widget_for_get_different_glyphs() {
        let text = text_of(&all_group_lines(fixtures::plain()));
        let glyphed = |wanted: char| -> Vec<String> {
            rows_of(&text)
                .into_iter()
                .filter(|(_, lock, _, _)| *lock == wanted)
                .map(|(_, _, _, key)| key)
                .collect()
        };
        assert_eq!(glyphed('='), ["instances", "name"]);
        assert_eq!(
            glyphed('~'),
            ["level_rules", "liveness_probe", "readiness_probe"]
        );
        // 41, not 42: `env` no longer draws its own field row.
        assert_eq!(glyphed(' ').len(), 41 - 2 - 3);
    }

    /// `kill_timeout` and `exp_backoff_restart_delay` default to 1600ms
    /// and 100ms, which `UpDuration::Display` prints as bare digits with
    /// no unit; `listen_timeout` defaults to 3s, which it already prints
    /// with one.
    #[test]
    fn a_bare_duration_or_mem_size_shows_its_resolved_unit_in_the_row() {
        let text = text_of(&all_group_lines(fixtures::plain()));
        let row = |key: &str| {
            text.iter()
                .find(|line| line.contains(key))
                .unwrap_or_else(|| panic!("{key} is drawn at 89 columns: {text:?}"))
        };
        assert!(
            row("kill_timeout").contains("1600ms"),
            "{:?}",
            row("kill_timeout")
        );
        assert!(
            row("exp_backoff_restart_delay").contains("100ms"),
            "{:?}",
            row("exp_backoff_restart_delay")
        );
        assert!(
            row("listen_timeout").contains("3s"),
            "{:?}",
            row("listen_timeout")
        );
    }

    /// `MIN_TERM_WIDTH` drops the cost cell and `plain` renders muted as
    /// nothing, so the glyph is the whole signal in exactly the case an
    /// operator is most likely to be in.
    #[test]
    fn the_two_glyphs_survive_the_narrowest_width_and_a_palette_with_no_colour() {
        let width = MIN_TERM_WIDTH;
        let mut pane = web_pane();
        let mut rows = Vec::new();
        let mut all_text = Vec::new();
        for _ in 0..GROUP_ORDER.len() {
            pane.move_to_last();
            let text = text_of(&pane_lines(&pane, fixtures::plain(), width, 0));
            rows.extend(rows_of(&text));
            all_text.extend(text);
            pane.next_group();
        }
        // `parts(line).is_some()` alone is not enough at this width: the
        // legend line spells out `= read-only, set it in the Flockfile`,
        // and its own leading two spaces plus an `=` parse the same shape
        // `parts` reads off a field row. Real
        // field keys only, the same filter `rows_of` applies, keeps this
        // test about the cost cell rather than the legend.
        let keys: Vec<String> = web_pane()
            .fields()
            .fields()
            .iter()
            .map(|field| field.key.clone())
            .collect();
        assert!(
            !all_text.iter().any(|line| {
                parts(line).is_some_and(|(_, _, _, key)| keys.contains(&key))
                    && line.contains("read-only")
            }),
            "no field row can afford the cost cell at {width}: {all_text:?}"
        );
        let glyph = |key: &str| rows.iter().find(|(_, _, _, k)| k == key).map(|r| r.1);
        assert_eq!(glyph("instances"), Some('='));
        assert_eq!(glyph("liveness_probe"), Some('~'));
        assert_eq!(glyph("watch"), Some(' '));
    }

    /// At the design target the panel draws the blurb, and the fix must not
    /// have added a second copy above the field list.
    #[test]
    fn the_panel_is_the_only_blurb_where_it_draws() {
        let pane = web_pane();
        assert!(panel_width(160).is_some(), "160 must have a panel");
        assert!(
            top_lines(&pane, fixtures::plain(), 160).is_empty(),
            "the inline blurb drew beside the panel, so the help is on \
             screen twice"
        );
        assert!(
            !top_lines(&pane, fixtures::plain(), 89).is_empty(),
            "and it must still draw where the panel cannot"
        );
    }

    /// The field list's own `old -> new` cell, which had no test of its
    /// own while it spelled the mask inline. It is the path that draws a
    /// secret an operator has just edited, on the row they edited it on.
    #[test]
    fn the_field_row_masks_both_halves_of_a_secrets_edited_value() {
        let pane = secret_dog_pane_with_an_edit();
        let text = text_of(&pane_lines(&pane, fixtures::plain(), 120, 0));
        let row = text
            .iter()
            .find(|line| line.contains(" token"))
            .expect("token is drawn at 120 columns");
        assert!(row.contains("<set> -> <set>"), "{row:?}");
        assert!(
            !text.join("\n").contains("ab12cd34"),
            "the old value leaked: {text:?}"
        );
        assert!(
            !text.join("\n").contains("ef56gh78"),
            "the new value leaked: {text:?}"
        );
    }

    /// An edited row shows its own change rather than borrowing the `!`
    /// the shepherd's parked-field marker already owns.
    #[test]
    fn an_edited_row_shows_old_then_new_and_takes_no_marker() {
        let app = fixtures::app_in_sheep_pane_with_two_edits();
        let row = fixtures::config_pane_row_for_tests(&app, "max_memory");
        assert!(row.contains("->"), "{row}");
        assert!(!row.trim_start().starts_with('!'), "{row}");
    }

    /// `▲` and `●` join the vocabulary `flock.rs:46` already checks `mark`
    /// against: East-Asian Ambiguous width would shift every column after
    /// the glyph.
    #[test]
    fn the_impact_glyphs_are_one_column_wide() {
        assert_eq!(char_columns('\u{25b2}'), 1);
        assert_eq!(char_columns('\u{25cf}'), 1);
    }
}
