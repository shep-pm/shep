//! The explanation panel: what the focused field is, what it holds now,
//! what it defaults to, and what a change to it would cost.
//!
//! It draws only where the width affords it. Below that the same help wraps
//! inline under the field list instead, which is why every row here is
//! bounded rather than truncated.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::super::field::Field;
use super::super::super::pane::{ConfigPane, PaneRow};
use super::super::super::theme::Palette;
use super::super::super::validation;
use super::chrome::hairline_line;
use super::field_row::{impact_tag, type_label};
use super::layout::{BLURB_WRAP, PANEL_MAX, clipped, mask_secret, wrap};
use crate::vocabulary::Role;

/// A row of `prefix` (already at its own fixed width) followed by `text`,
/// clipped to whatever of `width` the prefix leaves: the guard that keeps
/// a live value or a schema-authored sentence from pushing the row past
/// the panel's own column.
pub(super) fn bounded_row(
    prefix: Span<'static>,
    prefix_cols: u16,
    text: &str,
    text_style: Style,
    width: u16,
) -> Line<'static> {
    let budget = width.saturating_sub(prefix_cols);
    Line::from(vec![
        prefix,
        Span::styled(clipped(text, budget), text_style),
    ])
}

/// The right-hand explanation panel: everything about `field` alone, drawn
/// at `width` columns, clamped to [`PANEL_MAX`] as a defensive floor under
/// a caller that has not already run `width` through [`super::layout::panel_width`]'s own
/// ladder.
///
/// Six regions, top to bottom, each omitted entirely when its own source is
/// empty rather than drawn with nothing under its heading: the `FOCUSED`
/// chip and the blurb are unconditional (a field always has a key, a kind
/// and a help string), `now`/`default`/`example` show a placeholder rather
/// than disappearing since `now` is never empty, the impact tag is omitted
/// for a dog (whose own binary decides what a change costs, not this
/// pane), and `VALIDATION`/`NEIGHBOURS` are each omitted on their own when
/// [`validation::bullets`] or [`Field::neighbours`] has nothing to say.
pub(in crate::lookout::view) fn panel_for_field(
    field: &Field,
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
) -> Vec<Line<'static>> {
    let width = width.min(PANEL_MAX);
    let mut lines = Vec::new();

    // 1: the FOCUSED chip, the field name, its type.
    lines.push(Line::from(vec![
        Span::styled(" FOCUSED ", palette.band(Role::Butter)),
        Span::raw(format!(" {}", field.key)),
        Span::styled(format!("  {}", type_label(field)), palette.muted()),
    ]));

    // 2: the blurb, wrapped. The leading two-space indent below is part of
    // the row's own width, so the wrap budget is `width` minus those two
    // columns, not `width` itself: at the design target's fixed 72 columns
    // `BLURB_WRAP` (66) left enough headroom that this never showed, but
    // the ladder's own floor of 50 does not.
    for row in wrap(
        &field.help,
        usize::from(BLURB_WRAP.min(width.saturating_sub(2))),
    ) {
        lines.push(Line::from(Span::raw(format!("  {row}"))));
    }

    // 3: now, default, example. `now` is never empty; the other two show a
    // placeholder rather than dropping their own row, so the panel always
    // names all three questions even when the schema answers only one.
    // `now` is masked the same way `field_line` masks its own value cell:
    // it is `pane.display_value`, the live config, and a dog's schema can
    // mark that secret. `default` and `example` are never masked: both come
    // from the schema itself (`init.default`/`init.example`), authored by
    // whoever wrote the schema, not by an operator, so neither can carry a
    // value this pane owes any secrecy to.
    lines.push(Line::default());
    let now = mask_secret(field.secret, pane.display_value(&field.key));
    let default = field.default.clone().unwrap_or_else(|| "(none)".to_owned());
    let example = field.example.clone().unwrap_or_else(|| "(none)".to_owned());
    for (label, value) in [("now", now), ("default", default), ("example", example)] {
        let prefix = format!("  {label:<8}");
        let prefix_cols = u16::try_from(prefix.chars().count()).unwrap_or(u16::MAX);
        lines.push(bounded_row(
            Span::styled(prefix, palette.muted()),
            prefix_cols,
            &value,
            Style::default(),
            width,
        ));
    }

    // 4: the impact tag and its sentence, omitted for a dog.
    if let Some(group) = pane.cost(&field.key) {
        let (glyph, word, sentence) = impact_tag(group);
        lines.push(Line::default());
        let prefix = format!("  {glyph} {word}  ");
        let prefix_cols = u16::try_from(prefix.chars().count()).unwrap_or(u16::MAX);
        lines.push(bounded_row(
            Span::raw(prefix),
            prefix_cols,
            sentence,
            Style::default(),
            width,
        ));
        let hint = "pick the timing when you close the pane";
        let hint_cols = u16::try_from(hint.chars().count()).unwrap_or(width);
        let pad = width.saturating_sub(hint_cols);
        lines.push(Line::from(Span::raw(format!(
            "{}{hint}",
            " ".repeat(usize::from(pad))
        ))));
    }

    // 5: VALIDATION, omitted when the field has nothing accepted or refused.
    let bullets = validation::bullets(field);
    if !bullets.is_empty() {
        lines.push(Line::default());
        lines.push(hairline_line(palette, width));
        lines.push(Line::from(Span::styled("  VALIDATION", palette.muted())));
        for text in &bullets.accepts {
            lines.push(bounded_row(
                Span::styled("  \u{2588} ", palette.band(Role::Meadow)),
                4,
                text,
                Style::default(),
                width,
            ));
        }
        for text in &bullets.refuses {
            lines.push(bounded_row(
                Span::styled("  \u{2588} ", palette.band(Role::Bark)),
                4,
                &format!("refused: {text}"),
                Style::default(),
                width,
            ));
        }
    }

    // 6: NEIGHBOURS, omitted when the field names none.
    if !field.neighbours.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("  NEIGHBOURS", palette.muted())));
        for neighbour in &field.neighbours {
            let prefix = format!("  {}  ", neighbour.field);
            let prefix_cols = u16::try_from(prefix.chars().count()).unwrap_or(u16::MAX);
            lines.push(bounded_row(
                Span::raw(prefix),
                prefix_cols,
                &neighbour.note,
                Style::default(),
                width,
            ));
        }
    }

    lines
}

/// The explanation panel for whichever field the pane's own cursor is on,
/// or nothing when the cursor is not on a field at all (the env rows and
/// the pending-edits section carry no [`Field`] of their own).
///
/// The panel follows the cursor: it describes the row [`ConfigPane::cursor`]
/// names, never the row this call happened to be drawn from, so a caller
/// that redraws on every cursor move sees new content on the same
/// keypress that moved it.
#[must_use]
pub(in crate::lookout::view) fn panel_lines(
    pane: &ConfigPane,
    palette: Palette,
    width: u16,
) -> Vec<Line<'static>> {
    let Some(PaneRow::Field(index)) = pane.cursor() else {
        return Vec::new();
    };
    let Some(field) = pane.fields().fields().get(index) else {
        return Vec::new();
    };
    panel_for_field(field, pane, palette, width)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{secret_dog_pane_with_an_edit, text_of};
    use super::super::pane_lines;
    use super::*;
    use crate::lookout::app::{KeyPress, Msg};
    use crate::lookout::view::fixtures;

    /// `panel_for_field`'s `now` row is the panel's own live-config read,
    /// same as `field_line`'s value cell, and has to mask a secret the same
    /// way. Cursor left off `token` on purpose: every new test in the
    /// previous round left it on field 0, which is why the panel's `now`
    /// row leaked a real webhook URL and nothing caught it.
    #[test]
    fn the_explanation_panel_masks_a_secret_fields_value() {
        let pane = secret_dog_pane_with_an_edit();
        let field = pane.fields().by_key("token").expect("token is a field");
        let lines = panel_for_field(field, &pane, fixtures::plain(), 120);
        let text: String = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            !text.contains("ab12cd34"),
            "the current value leaked: {text}"
        );
        assert!(text.contains("<set>"), "{text}");
    }

    #[test]
    fn the_panel_describes_the_focused_field_only() {
        let app = fixtures::app_in_sheep_pane();
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        assert!(panel.iter().any(|row| row.contains("cwd")), "{panel:?}");
        assert!(
            !panel.iter().any(|row| row.contains("kill_timeout")),
            "the panel is showing a field that is not focused: {panel:?}"
        );
    }

    #[test]
    fn the_panel_names_now_default_and_example() {
        let app = fixtures::app_in_sheep_pane();
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        for label in ["now", "default", "example"] {
            assert!(
                panel.iter().any(|row| row.trim_start().starts_with(label)),
                "no {label} row: {panel:?}"
            );
        }
    }

    /// The panel follows the cursor, and does so on the keypress that moves
    /// it rather than on the next redraw.
    #[test]
    fn the_panel_follows_the_selection() {
        let mut app = fixtures::app_in_sheep_pane();
        let first = fixtures::config_pane_panel_for_tests(&app, 160);
        app.update(Msg::Key(KeyPress::SelectDown));
        let second = fixtures::config_pane_panel_for_tests(&app, 160);
        assert_ne!(first, second);
    }

    #[test]
    fn a_duration_field_takes_its_bullets_from_the_type_table() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::Group(4)));
        let panel = fixtures::config_pane_panel_focused_on(&app, "min_uptime", 160);
        assert!(
            panel.iter().any(|row| row.contains("milliseconds")),
            "{panel:?}"
        );
    }

    /// A field with no accepted forms and no neighbours renders no heading,
    /// not an empty one. Same rule as the detail pane's cfg cell.
    #[test]
    fn a_field_with_nothing_to_say_renders_no_headings() {
        let app = fixtures::app_in_sheep_pane();
        let panel = fixtures::config_pane_panel_focused_on(&app, "fold", 160);
        assert!(
            !panel.iter().any(|row| row.contains("VALIDATION")),
            "{panel:?}"
        );
        assert!(
            !panel.iter().any(|row| row.contains("NEIGHBOURS")),
            "{panel:?}"
        );
    }

    /// Colour is never the only carrier: a refused form has to read as
    /// refused with every colour stripped.
    ///
    /// `name` rather than `cwd`: `cwd`'s own "cannot enter" claim moved to
    /// an accepted form once a bad `cwd` started earning a warning instead
    /// of a refusal (`SetSheepField`'s `warning`), and `name`'s refusals are
    /// still a hard `normalize` rule with nothing advisory about them.
    #[test]
    fn a_refused_form_reads_as_refused_without_colour() {
        let mut app = fixtures::app_with_plain_palette_in_sheep_pane();
        fixtures::select_field(&mut app, "name");
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        let refusal = panel
            .iter()
            .find(|row| row.contains("path separator"))
            .expect("name states a refusal");
        assert!(refusal.contains("refused"), "{refusal}");
    }

    #[test]
    fn the_blurb_wraps_rather_than_truncating() {
        let app = fixtures::app_in_sheep_pane();
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        assert!(
            panel.iter().all(|row| row.chars().count() <= 72),
            "a panel row overflows its column: {panel:?}"
        );
        assert!(panel.iter().filter(|row| !row.trim().is_empty()).count() > 3);
    }

    /// Nothing on the live screen drew the panel, because nothing called
    /// it outside a test fixture. This pins the wiring itself, through the
    /// same [`pane_lines`] the real draw path calls, not through a
    /// fixture built to reach [`panel_lines`] directly.
    ///
    /// The panel draws across a range, from the design target down to
    /// [`super::layout::panel_width`]'s own floor at 90 columns, not only at the design
    /// target. What this pins is the floor: nothing below it.
    #[test]
    fn the_panel_draws_beside_the_field_list_at_the_design_target() {
        let app = fixtures::app_in_sheep_pane();
        let pane = app.config_pane().expect("the pane is open");
        let at_target = text_of(&pane_lines(pane, fixtures::plain(), 160, 48));
        assert!(
            at_target.iter().any(|line| line.contains("FOCUSED")),
            "the panel never drew at the design target: {at_target:?}"
        );
        let below_floor = text_of(&pane_lines(pane, fixtures::plain(), 89, 48));
        assert!(
            !below_floor.iter().any(|line| line.contains("FOCUSED")),
            "the panel must not draw below its own floor: {below_floor:?}"
        );
    }

    /// A dog pane has no groups, so it takes `pane_lines`'s other branch,
    /// which got the panel after the grouped one did and is the branch that
    /// went without it for a while. Same layout, same panel, its empty
    /// regions simply empty.
    #[test]
    fn a_dogs_panel_draws_beside_its_field_list_at_the_design_target() {
        let app = fixtures::app_in_dog_pane();
        assert!(
            fixtures::config_pane_draws_a_panel(&app, 160),
            "the panel never drew for a dog at the design target"
        );
        assert!(
            !fixtures::config_pane_draws_a_panel(&app, 89),
            "the panel must not draw below its own floor"
        );
    }

    /// A dog's `cost` is always `None`, so its impact region is empty: no
    /// sentence, and no reverse-video tag rendered with nothing to say.
    /// Nothing rather than a zero, the same rule a field with no
    /// validation bullets already follows.
    #[test]
    fn a_dogs_panel_has_no_impact_region() {
        let app = fixtures::app_in_dog_pane();
        let panel = fixtures::config_pane_panel_for_tests(&app, 160);
        assert!(
            panel.iter().any(|line| line.contains("FOCUSED")),
            "the panel must actually be drawn for its own absence to mean anything: {panel:?}"
        );
        assert!(
            !panel
                .iter()
                .any(|line| line.contains("pick the timing when you close the pane")),
            "a dog's cost is always None, so no impact sentence should render: {panel:?}"
        );
    }

    /// A dog's schema carries no group, so `pane_lines` draws no tab row
    /// for it even beside the panel.
    #[test]
    fn a_dogs_panel_layout_draws_no_tab_row() {
        let app = fixtures::app_in_dog_pane();
        assert!(
            fixtures::config_pane_draws_a_panel(&app, 160),
            "the panel must actually be drawn for its own absence to mean anything"
        );
        assert!(
            !fixtures::config_pane_draws_a_tab_row(&app, 160),
            "a dog has no groups to tab through"
        );
    }
}
