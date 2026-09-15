//! The lines around the field list: the title and its band, the row of
//! group tabs, the hairline under them, the column headers, the legend, and
//! the pending-edit line.
//!
//! All of it is paid for out of the same height the rows come from, which
//! is why a short terminal sheds these in a fixed order rather than
//! shrinking them.

use ratatui::text::{Line, Span};
use shep_core::config::GROUP_ORDER;

use super::super::super::pane::{ConfigPane, PaneEdit, PaneTarget};
use super::super::super::theme::Palette;
use super::super::cell;
use super::super::flock::fit;
use super::layout::{body_width, mask_secret, widths};
use crate::vocabulary::Role;

/// The pane's own title: which sheep or dog is being edited.
///
/// The dashboard's title line above this one names `$SHEP_HOME` and
/// nothing else, so this names whose 40 fields are on screen.
///
/// Carries no control-dependent word: what the keys do belongs in the key
/// hint (`view::status::pane_hint`), which already reads the gate.
pub(super) fn title_line(pane: &ConfigPane, palette: Palette, width: u16) -> Line<'static> {
    let kind = match pane.target() {
        PaneTarget::Sheep { .. } => "sheep config",
        PaneTarget::Dog { .. } => "dog config",
    };
    Line::from(Span::styled(
        format!(
            "  {}",
            fit(
                &format!("{}  ({kind})", pane.target().name()),
                body_width(width)
            )
        ),
        palette.muted(),
    ))
}

/// Whether `pane`'s own fields carry a group at all.
///
/// True for every sheep, whose Flockfile schema tags all 41 fields. False
/// for a dog, whose schema declares none (see [`ConfigPane::dog`]), which
/// is what keeps the tab row and the pending/env sections below a
/// sheep-only elaboration: a control that filters nothing would be a
/// tab row lying about what it does.
pub(super) fn has_groups(pane: &ConfigPane) -> bool {
    pane.fields()
        .fields()
        .iter()
        .any(|field| field.group.is_some())
}

/// The pane's own reverse-video summary band: which target's config this
/// is, and how many edits are filed and unsent.
///
/// The only line that names the target: [`title_line`] duplicated it
/// directly underneath, and the design's own row allocation gives the
/// sheep or dog's name exactly one row.
///
/// Never the shepherd's own word for a field that is written and parked:
/// `pending` is what `shep flock`'s CFG column and this same pane's `!`
/// flag already mean, and a second meaning for the same word on the
/// screen an operator moves to next is exactly the confusion this counts
/// around instead.
///
/// Nothing rather than a zero, the same rule `view::detail`'s `cfg` cell
/// follows: an untouched pane names no count at all, not `0 edits`.
pub(super) fn title_band_line(pane: &ConfigPane, palette: Palette, width: u16) -> Line<'static> {
    let kind = match pane.target() {
        PaneTarget::Sheep { .. } => "sheep config",
        PaneTarget::Dog { .. } => "dog config",
    };
    let mut text = format!("{}  ({kind})", pane.target().name());
    let count = pane.edits().len();
    match count {
        0 => {}
        1 => text.push_str("  1 edit"),
        n => text.push_str(&format!("  {n} edits")),
    }
    Line::from(Span::styled(
        cell::band(&text, usize::from(width)),
        palette.band(Role::Butter),
    ))
}

/// The tab row: every group in [`GROUP_ORDER`], the active one drawn as a
/// paper-2 chip in ink and the rest in ink-3, then the two keys that move
/// between them.
pub(super) fn tab_row_line(pane: &ConfigPane, palette: Palette, width: u16) -> Line<'static> {
    let active = pane.group();
    let chip = |group: &str| format!(" {group} ");
    let chips = GROUP_ORDER
        .iter()
        .map(|group| chip(group))
        .collect::<Vec<_>>()
        .join(" ");
    let full = format!("{chips}    tab next group   1\u{2026}8 jump");
    let text = fit(&full, body_width(width));
    let marker = chip(active);
    let mut spans = vec![Span::raw("  ")];
    if let Some(start) = text.find(&marker) {
        let end = start + marker.len();
        if start > 0 {
            spans.push(Span::styled(text[..start].to_owned(), palette.muted()));
        }
        spans.push(Span::styled(text[start..end].to_owned(), palette.ground()));
        if end < text.len() {
            spans.push(Span::styled(text[end..].to_owned(), palette.muted()));
        }
    } else {
        // The active chip fell off the fitted text at a width too narrow
        // to hold it: the row still shows, muted throughout, rather than
        // claiming a chip it cannot draw.
        spans.push(Span::styled(text, palette.muted()));
    }
    Line::from(spans)
}

/// A rule of box-drawing horizontals, the full width of the body.
pub(super) fn hairline_line(palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {}", cell::rule(usize::from(body_width(width)))),
        palette.line(),
    ))
}

/// The column header row: `KEY`, `VALUE` and `COST`, aligned over
/// [`field_line`]'s own cells. Unconditional: a slot that could vanish
/// under a wrapped blurb has nowhere left to put `LANDS`, so the header
/// stays and the blurb takes the room below it.
pub(super) fn column_header_line(palette: Palette, width: u16, show_lands: bool) -> Line<'static> {
    let (key_w, value_w, cost_w) = widths(body_width(width), show_lands);
    let mut text = String::from("  ");
    text.push_str(&fit("FIELD", key_w));
    if value_w > 0 {
        text.push_str("  ");
        text.push_str(&fit("VALUE", value_w));
    }
    if cost_w > 0 {
        text.push_str("  ");
        text.push_str(&fit("LANDS", cost_w));
    }
    Line::from(Span::styled(text, palette.muted()))
}

/// The pane's own marker legend, drawn once at the foot rather than
/// repeated per group: the status bar's own hint already names every key
/// this pane answers to (`view::status::pane_hint`), so this line explains
/// the glyphs instead, adapted from the design's own legend
/// (`docs/lookout/design-files/README.md`, the 1e frame's row 45) to the
/// four the pane actually draws (see [`field_line`]).
///
/// Names no key: `status.rs`'s `esc close` is the one place that wording
/// lives, and a second copy here would only need to be kept in sync with
/// it.
///
/// `* yours` rather than `* overridden`, because the status bar has said
/// `* yours` on every screen that draws the glyph since before this pane
/// existed. One glyph with two words for it was visible on one screen at
/// once.
pub(super) fn legend_line(palette: Palette, width: u16) -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "  {}",
            fit(
                "= read-only, set it in the Flockfile   ~ no widget for this shape   \
                 ! parked, awaits a respawn   * yours   -> changed by you, not yet written",
                body_width(width)
            )
        ),
        palette.muted(),
    ))
}

/// One entry of the pending-edits section: the field or env key it is
/// filed under, and what it will send.
///
/// A field edit shows `old -> new`, the same cell [`field_line`] draws for
/// one still visible in the active group, and masks a secret field's
/// values the same way that cell does: nothing today reaches either path,
/// since the Flockfile schema marks no field secret and a dog never
/// carries a group, but a dog's own schema can mark one and this section
/// draws every group's edits regardless of which is active. An env edit
/// shows only that it is set or removed, since the pane never holds an
/// env value to show either side of.
pub(super) fn pending_edit_line(
    pane: &ConfigPane,
    edit: &PaneEdit,
    width: u16,
    palette: Palette,
) -> Line<'static> {
    let text = match edit {
        PaneEdit::Set { key, .. } => {
            let secret = pane.fields().by_key(key).is_some_and(|field| field.secret);
            let old = pane.display_value(key);
            let new = pane.edited_value(key).unwrap_or_default();
            let old = mask_secret(secret, old);
            let new = mask_secret(secret, new);
            format!("{key}  {old} -> {new}")
        }
        PaneEdit::SetEnv { key, value } => match value {
            Some(_) => format!("env.{key}  -> <set>"),
            None => format!("env.{key}  removed"),
        },
    };
    Line::from(Span::styled(
        format!("    {}", fit(&text, body_width(width).saturating_sub(4))),
        palette.attention(),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{secret_dog_pane_with_an_edit, text_of, web_pane};
    use super::super::pane_lines;
    use super::*;
    use crate::lookout::app::{KeyPress, Msg};
    use crate::lookout::view::fixtures;

    #[test]
    fn the_title_names_the_target_and_no_longer_calls_it_read_only() {
        let text = text_of(&pane_lines(&web_pane(), fixtures::plain(), 120, 0));
        // Named once, on the title band: `title_line` no longer draws a
        // second row repeating it (see `title_band_line`'s own doc).
        assert!(text[0].contains("web"), "{:?}", text[0]);
        assert!(text[0].contains("(sheep config)"), "{:?}", text[0]);
        assert!(!text[0].contains("read-only"), "{:?}", text[0]);
    }

    /// `pending_edit_line` has its own secret check rather than relying on
    /// nothing ever reaching it: a dog can mark a field secret, and the
    /// pending-edits section draws every filed edit regardless of which
    /// group is active, so a secret's row is on screen the moment its edit
    /// is filed, not just while its own field row scrolls into view.
    #[test]
    fn the_pending_section_masks_a_secret_fields_old_and_new_value() {
        let pane = secret_dog_pane_with_an_edit();
        let (_, entry) = pane.edits().iter().next().expect("one edit was filed");
        let line = pending_edit_line(&pane, entry.edit(), 120, fixtures::plain());
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(!text.contains("ab12cd34"), "the old value leaked: {text}");
        assert!(!text.contains("ef56gh78"), "the new value leaked: {text}");
        assert!(text.contains("<set> -> <set>"), "{text}");
    }

    /// Eight groups, in `GROUP_ORDER`, and every one of them reachable.
    /// This is the test that would have caught a filter axis nothing
    /// could set.
    ///
    /// Bounded to the tab row alone
    /// ([`fixtures::config_pane_tab_row_for_tests`]), not a search over the
    /// whole rendered frame: every group's name is drawn on every render
    /// regardless of which is active, so a frame-wide `contains` would pass
    /// whatever `next_group` did or did not do. [`tab_draws_the_active_group_as_its_own_chip`]
    /// covers the half this test cannot: which one is drawn as the chip.
    #[test]
    fn tab_walks_every_group_and_each_one_shows_its_own_fields() {
        let mut app = fixtures::app_in_sheep_pane();
        let mut seen = Vec::new();
        for _ in 0..GROUP_ORDER.len() {
            let group = app.config_pane().unwrap().group().to_owned();
            let tab_row = fixtures::config_pane_tab_row_for_tests(&app, 160);
            for name in GROUP_ORDER {
                assert!(
                    tab_row.contains(name),
                    "the tab row does not name {name}: {tab_row:?}"
                );
            }
            seen.push(group);
            app.update(Msg::Key(KeyPress::NextGroup));
        }
        assert_eq!(seen, GROUP_ORDER.to_vec());
    }

    /// All eight group names are listed every render, so the only thing
    /// that changes as `next_group` walks is which one carries the
    /// paper-2 ground: the chip. Checked in colour, since [`fixtures::plain`]
    /// makes [`super::super::super::theme::Palette::ground`] a no-op and
    /// every span would look alike.
    #[test]
    fn tab_draws_the_active_group_as_its_own_chip() {
        for active in GROUP_ORDER {
            let mut pane = web_pane();
            let digit =
                u8::try_from(GROUP_ORDER.iter().position(|g| g == active).unwrap() + 1).unwrap();
            pane.set_group(digit);
            let line = tab_row_line(&pane, fixtures::coloured(), 160);
            let painted: Vec<&str> = line
                .spans
                .iter()
                .filter(|span| span.style.bg.is_some())
                .map(|span| span.content.as_ref().trim())
                .collect();
            assert_eq!(
                painted,
                vec![*active],
                "exactly one chip should carry a ground, the active group's own"
            );
        }
    }

    #[test]
    fn tab_wraps_from_the_last_group_to_the_first() {
        let mut app = fixtures::app_in_sheep_pane();
        for _ in 0..GROUP_ORDER.len() {
            app.update(Msg::Key(KeyPress::NextGroup));
        }
        assert_eq!(app.config_pane().unwrap().group(), GROUP_ORDER[0]);
    }

    /// The digits reach the same eight groups `tab` does. A key that
    /// reaches nothing is exactly the shape this plan exists to avoid.
    #[test]
    fn the_digits_reach_the_same_groups_tab_does() {
        for (index, wanted) in GROUP_ORDER.iter().enumerate() {
            let mut app = fixtures::app_in_sheep_pane();
            let digit = u8::try_from(index + 1).unwrap();
            app.update(Msg::Key(KeyPress::Group(digit)));
            assert_eq!(&app.config_pane().unwrap().group(), wanted);
        }
    }

    #[test]
    fn the_list_shows_only_the_active_groups_fields() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::Group(1)));
        let rows = fixtures::config_pane_field_rows_for_tests(&app);
        assert!(rows.iter().any(|row| row.contains("cwd")), "{rows:?}");
        assert!(
            !rows.iter().any(|row| row.contains("kill_timeout")),
            "a shutdown field is showing under process: {rows:?}"
        );
    }

    /// The pending section spans every group, which is the whole reason
    /// it is a section rather than a marker.
    #[test]
    fn the_pending_section_lists_an_edit_from_another_group() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Group(1)));
        let rows = fixtures::config_pane_pending_rows_for_tests(&app);
        assert!(
            rows.iter().any(|row| row.contains("max_memory")),
            "{rows:?}"
        );
    }

    /// `pending` is the shepherd's word for a field already written and
    /// parked until a respawn, and it is what the flock table's own `cfg
    /// !2 pending` cell means. This pane's own count of unsent edits must
    /// not borrow it.
    #[test]
    fn the_title_band_counts_edits_without_calling_them_pending() {
        let app = fixtures::app_in_sheep_pane_with_two_edits();
        let band = fixtures::config_pane_title_band_for_tests(&app, 160);
        assert!(band.contains("2 edits"), "{band}");
        assert!(!band.contains("pending"), "{band}");
    }
}
