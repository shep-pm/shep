use super::super::super::app::{Control, SecretsPane};
use super::super::super::secrets::Source;
use super::super::super::theme::Palette;
use super::super::flock::fit;
use super::column_tiers::Column;
use crate::output::human_duration;
use crate::vocabulary::Role;
use ratatui::text::{Line, Span};

/// [`pane_band`]'s label.
const PANE_BAND_LABEL: &str = "SECRETS   flock-wide values a Flockfile refers to and never carries";

/// What the store is, never printed to a log or read before spawn.
pub(super) const STORE_LINE: &str = "store $SHEP_HOME/secrets.json \u{b7} not encrypted \u{b7} never \
     printed to a log, never carried in a bleat \u{b7} read at spawn, not now";

/// This pane's own band: design rule 1, `docs/lookout/design-files/README.md:45`.
/// Butter ground, since the pane is one you can change something in.
pub(super) fn pane_band(width: u16, palette: Palette) -> Line<'static> {
    Line::from(Span::styled(
        super::super::cell::band(PANE_BAND_LABEL, usize::from(width)),
        palette.band(Role::Butter),
    ))
}

/// Both secrets gates, since neither implies the other: `[secrets]
/// allow_read` decides whether a value may be shown, `lookout.allow_control`
/// whether the pane may change anything.
pub(super) fn gates_line(pane: &SecretsPane, control: Control, palette: Palette) -> Line<'static> {
    let allowed = matches!(control, Control::Allowed);
    Line::from(Span::styled(
        format!(
            "reveal  [secrets] allow_read = {} in shep.toml \u{b7} change  \
             lookout.allow_control = {allowed}",
            pane.model.allow_read
        ),
        palette.muted(),
    ))
}

/// The column headings, muted, packed the same way [`row_line`](crate::lookout::view::secrets::row_cells::row_line) packs a
/// data row.
pub(super) fn heading_line(columns: &[Column], palette: Palette) -> Line<'static> {
    let mut text = String::new();
    for column in columns {
        text.push_str(&fit(column.heading(), column.width()));
    }
    Line::from(Span::styled(text, palette.muted()))
}

/// The operator store's own read refusal when there is one, otherwise how
/// long ago the muster roll (and so `READ BY`) was written. Blank when
/// neither applies.
pub(super) fn roll_status_line(pane: &SecretsPane, palette: Palette) -> Line<'static> {
    if let Some(message) = &pane.model.unreadable {
        return Line::from(Span::styled(
            format!("operator store unreadable: {message}"),
            palette.refusal(),
        ));
    }
    if let Some(age) = pane.model.roll_age {
        let ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
        return Line::from(Span::styled(
            format!("READ BY as of the roll, read {} ago", human_duration(ms)),
            palette.muted(),
        ));
    }
    // Distinct from a key nothing reads: that reads "-" in `READ BY` with a
    // roll behind it. This is the roll itself missing.
    Line::from(Span::styled(
        "no muster roll yet: READ BY and WHO READS IT show nothing until the shepherd writes one",
        palette.muted(),
    ))
}

/// The tab row: every environment [`super::super::super::secrets::SecretsModel`]
/// found a slot for, plus `all`. The active one is bracketed
/// (`[production]`) in addition to whatever the palette paints, since a
/// signal carried by colour alone says nothing under `NO_COLOR`.
///
/// Right-aligned within `width`: design rule 2, every measurement states
/// its denominator, and this one is the count of tabs drawn above it,
/// `all` included, so the number is checkable against the row it sits under.
pub(super) fn tab_line(pane: &SecretsPane, palette: Palette, width: u16) -> Line<'static> {
    let labels: Vec<String> = pane
        .model
        .environments
        .iter()
        .enumerate()
        .map(|(index, name)| {
            if index == pane.tab {
                format!("[{name}]")
            } else {
                name.clone()
            }
        })
        .collect();
    let environment_count = labels.len();
    let suffix = format!("{environment_count} environments in this store \u{b7} \u{2190}/\u{2192}");

    // The suffix is this row's own denominator (design rule 2) and gives
    // way to nothing: the count is how an operator knows there are tabs
    // the row is not showing. The tabs elide around it instead.
    let budget = usize::from(width).saturating_sub(suffix.chars().count() + TAB_GAP);
    let anchor = if pane.tab < labels.len() { pane.tab } else { 0 };
    let (first, last) = tab_window(&labels, anchor, budget);

    let mut spans = Vec::with_capacity(labels.len() * 2 + 4);
    let mut drawn = 0usize;
    if first > 0 {
        spans.push(Span::styled("\u{2026}", palette.muted()));
        drawn += 1;
    }
    for (index, label) in labels.iter().enumerate().take(last + 1).skip(first) {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
            drawn += TAB_GAP;
        }
        drawn += label.chars().count();
        let style = if index == pane.tab {
            palette.attention()
        } else {
            palette.muted()
        };
        spans.push(Span::styled(label.clone(), style));
    }
    if last + 1 < labels.len() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled("\u{2026}", palette.muted()));
        drawn += TAB_ELISION;
    }

    let pad = usize::from(width)
        .saturating_sub(drawn)
        .saturating_sub(suffix.chars().count());
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    spans.push(Span::styled(suffix, palette.muted()));
    Line::from(spans)
}

/// The gap between two tabs, and between the last tab and the suffix.
const TAB_GAP: usize = 2;

/// What a run of hidden tabs costs the row: the `\u{2026}` standing in for it,
/// plus its own gap.
const TAB_ELISION: usize = TAB_GAP + 1;

/// The widest run of labels around `active` that fits `budget`, both ends
/// inclusive.
///
/// The active tab always draws, even when it alone overruns `budget`: a tab
/// row hiding the tab you are on says less than nothing. The run grows
/// rightwards first, so a cursor at the head of the list reads left to
/// right, and each step pays for the `\u{2026}` its own side still owes.
fn tab_window(labels: &[String], active: usize, budget: usize) -> (usize, usize) {
    let (mut first, mut last) = (active, active);
    let mut used = labels.get(active).map_or(0, |l| l.chars().count());
    loop {
        let owed_left = usize::from(first > 0) * TAB_ELISION;
        let owed_right = usize::from(last + 1 < labels.len()) * TAB_ELISION;
        if let Some(next) = labels.get(last + 1) {
            let cost = TAB_GAP + next.chars().count();
            let still_owed = usize::from(last + 2 < labels.len()) * TAB_ELISION;
            if used + cost + owed_left + still_owed <= budget {
                used += cost;
                last += 1;
                continue;
            }
        }
        if first > 0 {
            let cost = TAB_GAP + labels[first - 1].chars().count();
            let still_owed = usize::from(first > 1) * TAB_ELISION;
            if used + cost + still_owed + owed_right <= budget {
                used += cost;
                first -= 1;
                continue;
            }
        }
        return (first, last);
    }
}

/// One group's header row: its label, its member count, and `read-only
/// here` for a provider namespace, which owns nothing an operator can edit.
///
/// The disclosure triangle mirrors `flock::fold_header_cell`'s own: filled
/// when open, outlined when [`SecretsPane::collapsed`](crate::lookout::app::SecretsPane::collapsed) holds this
/// namespace. The operator group never collapses: `on_secrets_key`'s `z`
/// only ever inserts a namespace into `collapsed`.
pub(super) fn group_header_line(
    pane: &SecretsPane,
    source: &Source,
    palette: Palette,
    width: u16,
) -> Line<'static> {
    let count = pane.model.rows_for(source).count();
    let text = match source {
        Source::Operator => format!("\u{25be} operator \u{d7}{count} \u{b7} you set these"),
        Source::Namespace(namespace) => {
            let marker = if pane.collapsed.contains(namespace) {
                '\u{25b8}'
            } else {
                '\u{25be}'
            };
            format!(
                "{marker} {namespace} (dog) \u{d7}{count} \u{b7} pushed by a provider \u{b7} \
                 read-only here"
            )
        }
    };
    Line::from(Span::styled(fit(&text, width), palette.muted()))
}

#[cfg(test)]
mod tests {

    use std::time::Duration;

    use crate::lookout::view::fixtures;

    #[test]
    fn a_stale_roll_states_its_age() {
        let buffer = fixtures::render_secrets_with_roll_age(Duration::from_secs(3600));
        let text = fixtures::rows_of(&buffer);

        assert!(
            text.iter().any(|l| l.contains("roll") && l.contains("1h")),
            "a failed roll write only warns, so the age is the only signal: {text:?}"
        );
    }
}
