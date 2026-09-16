/// One column of the secrets table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::lookout::view) enum Column {
    /// The key, `namespace/KEY` for a provider row.
    Key,
    /// A block run and the in-force value's exact byte count, or the
    /// revealed plaintext.
    Value,
    /// Which environment slot supplies this tab's value.
    InForce,
    /// Every environment with a slot, and how many that is.
    SetIn,
    /// How many sheep name this key, and how many are running.
    ReadBy,
    /// When a change reaches a process, or a reveal's countdown.
    Lands,
}

/// The columns each width still fits, widest first.
///
/// Each threshold is the narrowest terminal that still fits its row, which
/// `every_tier_fits_the_width_it_claims` holds to. `LANDS` drops first,
/// then `READ BY`, then `SET IN`. The remaining three are the pane: a key
/// with no value and no scope answers nothing.
pub(in crate::lookout::view) const SECRET_TIERS: &[(u16, &[Column])] = &[
    (
        160,
        &[
            Column::Key,
            Column::Value,
            Column::InForce,
            Column::SetIn,
            Column::ReadBy,
            Column::Lands,
        ],
    ),
    (
        118,
        &[
            Column::Key,
            Column::Value,
            Column::InForce,
            Column::SetIn,
            Column::ReadBy,
        ],
    ),
    (
        96,
        &[Column::Key, Column::Value, Column::InForce, Column::SetIn],
    ),
    (74, &[Column::Key, Column::Value, Column::InForce]),
];

/// The narrowest terminal this pane draws a table into: [`SECRET_TIERS`]'
/// own floor tier, a terminal width rather than a table width (the
/// thresholds count the gutter, unlike [`super::super::flock::MIN_WIDTH`]).
///
/// There is nothing below it to fall back to. The floor tier's three
/// columns are what [`SECRET_TIERS`]' own doc calls the pane, and a
/// narrower terminal used to get them anyway, clipped mid-column by
/// `Buffer::set_line` with nothing on screen saying so.
pub(super) const MIN_WIDTH: u16 = SECRET_TIERS[SECRET_TIERS.len() - 1].0;

/// The widest tier `width` still fits, or the narrowest tier below every
/// threshold.
pub(in crate::lookout::view) fn columns_for(width: u16) -> &'static [Column] {
    SECRET_TIERS
        .iter()
        .find(|(threshold, _)| width >= *threshold)
        .map_or(SECRET_TIERS[SECRET_TIERS.len() - 1].1, |(_, columns)| {
            *columns
        })
}

impl Column {
    /// This column's width in cells at the design tier.
    pub(in crate::lookout::view) const fn width(self) -> u16 {
        match self {
            Self::Key => 28,
            Self::Value => 30,
            Self::InForce => 14,
            Self::SetIn => 22,
            Self::ReadBy => 22,
            Self::Lands => 42,
        }
    }

    /// The heading printed over it.
    pub(super) const fn heading(self) -> &'static str {
        match self {
            Self::Key => "KEY",
            Self::Value => "VALUE",
            Self::InForce => "IN FORCE",
            Self::SetIn => "SET IN",
            Self::ReadBy => "READ BY",
            Self::Lands => "LANDS",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::draw_layout::cell;
    use super::super::pane_chrome::tab_line;
    use super::super::row_cells::typed_cell;

    use super::super::super::super::theme::Palette;
    use super::super::super::flock::GUTTER;

    use super::*;

    use super::super::testing::*;
    use crate::lookout::view::fixtures;

    #[test]
    fn every_tier_fits_the_width_it_claims() {
        for (threshold, columns) in SECRET_TIERS {
            let spent: u16 = columns.iter().map(|column| column.width()).sum();
            assert!(
                spent + GUTTER <= *threshold,
                "tier {threshold} spends {spent} plus {GUTTER} of gutter"
            );
        }
    }

    /// The gap between the dashboard's own floor (33) and this pane's
    /// (74) is where the clipping was: `columns_for` handed back the floor
    /// tier at every width below 74, and `Buffer::set_line` cut its 72-cell
    /// row off at the screen edge with nothing saying a column had gone.
    ///
    /// Swept rather than sampled, and the refusal is asserted whole: it has
    /// to fit the narrowest terminal it is complaining about, and a
    /// `contains` on a truncated line would not notice.
    #[test]
    fn a_terminal_narrower_than_the_floor_tier_says_so_instead_of_clipping() {
        let app = fixtures::app_with_secrets();

        assert!(
            SECRET_TIERS
                .iter()
                .all(|(threshold, _)| *threshold >= MIN_WIDTH),
            "MIN_WIDTH is the floor tier's own threshold, so no tier below \
                 it is refused away unreached"
        );
        let spent: u16 = columns_for(MIN_WIDTH).iter().map(|c| c.width()).sum();
        assert!(
            spent + GUTTER <= MIN_WIDTH,
            "the width the refusal names has to be one the floor tier \
                 actually fits: {spent} plus {GUTTER} of gutter"
        );

        for width in crate::lookout::view::MIN_TERM_WIDTH..MIN_WIDTH {
            let text = frame_text(&fixtures::render(&app, width, 24));
            assert!(text.contains("too narrow for secrets"), "{width}: {text}");
            assert!(
                text.contains(&format!("need {MIN_WIDTH} columns")),
                "{width}: {text}"
            );
            assert!(
                !text.contains("DB_PASSWORD"),
                "no half-drawn table at {width}: {text}"
            );
        }

        let text = frame_text(&fixtures::render(&app, MIN_WIDTH, 24));
        assert!(text.contains("DB_PASSWORD"), "the floor tier draws: {text}");
        assert!(!text.contains("too narrow"), "{text}");
    }

    /// The tab row used to append its suffix whatever the tabs had spent
    /// and let `Buffer::set_line` cut the overrun, so a store with more
    /// environments than one row holds lost the count, lost every tab past
    /// the edge, and cut the last visible name mid-word.
    ///
    /// Swept over counts and widths, and every assertion is on the whole
    /// line: a `contains` passes on a truncated line as happily as a whole
    /// one, which is the failure being pinned.
    #[test]
    fn the_tab_row_elides_its_tabs_rather_than_losing_its_own_count() {
        let palette = Palette::detect(None, None, None);
        for count in 1..=20usize {
            for width in [MIN_WIDTH, 100, 160] {
                for tab in [0, count / 2, count - 1] {
                    let pane = pane_with_environments(count, tab);
                    let line = fixtures::rendered(&tab_line(&pane, palette, width));

                    assert!(
                        line.chars().count() <= usize::from(width),
                        "{count} tabs at {width}, on {tab}: {line:?}"
                    );
                    assert!(
                        line.trim_end().ends_with(&format!(
                            "{count} environments in this store \u{b7} \u{2190}/\u{2192}"
                        )),
                        "the count survives whole: {count} tabs at {width}: {line:?}"
                    );
                    assert!(
                        line.contains(&format!("[environment-{tab:02}]")),
                        "the tab you are on is drawn: {count} tabs at {width}: {line:?}"
                    );

                    // Every name on the row is a whole name: the elision
                    // marker is the only thing standing for what was left
                    // out, so no `environment-0` without its second digit.
                    for token in line.split_whitespace() {
                        let name = token.trim_start_matches('[').trim_end_matches(']');
                        if name.starts_with("environment-") {
                            assert_eq!(
                                name.len(),
                                "environment-00".len(),
                                "cut mid-name: {line:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The same window on the `+ new key` row, whose key name is typed into
    /// the same column by the same helper.
    #[test]
    fn a_new_key_name_outgrowing_its_column_is_shown_from_the_end_too() {
        let short = typed_cell("SHORT", Column::Value.width());
        let long = typed_cell(
            "A_VERY_LONG_KEY_NAME_THAT_RUNS_PAST_THE_COLUMN",
            Column::Value.width(),
        );

        assert!(short.starts_with("SHORT\u{2588}"), "{short:?}");
        assert!(!short.contains('\u{2026}'), "nothing dropped: {short:?}");
        assert!(long.starts_with('\u{2026}'), "{long:?}");
        assert!(long.ends_with("THE_COLUMN\u{2588}"), "{long:?}");
    }

    #[test]
    fn the_columns_drop_in_the_specified_order() {
        let widest = columns_for(160);
        assert!(widest.contains(&Column::Lands));
        assert!(
            !columns_for(118).contains(&Column::Lands),
            "LANDS goes first"
        );
        assert!(columns_for(118).contains(&Column::ReadBy));
        assert!(
            !columns_for(96).contains(&Column::ReadBy),
            "READ BY goes second"
        );
        assert!(
            !columns_for(74).contains(&Column::SetIn),
            "SET IN goes third"
        );
        for width in [160, 118, 96, 74, 40] {
            let columns = columns_for(width);
            assert!(columns.contains(&Column::Key), "KEY is the pane at {width}");
            assert!(
                columns.contains(&Column::Value),
                "VALUE is the pane at {width}"
            );
            assert!(
                columns.contains(&Column::InForce),
                "IN FORCE is the pane at {width}"
            );
        }
    }

    #[test]
    fn a_four_kilobyte_value_never_overflows_its_column() {
        let app = fixtures::app_with_a_maximum_length_secret();
        let buffer = fixtures::render(&app, 160, 48);
        let value = cell(&buffer, first_row(), Column::Value);

        assert!(
            value.chars().count() <= Column::Value.width() as usize,
            "spilled: {value:?}"
        );
        assert!(
            value.contains("4096 bytes"),
            "the exact length is stated: {value:?}"
        );
    }

    /// `.min(len)` in `row_cells::value_cell` is what a 4096-byte value never binds:
    /// the run is already capped by the column's own width there. A short
    /// value is the case that pins it: with no cap the run would fill the
    /// column regardless of the value behind it, implying a length nowhere
    /// close to the real one.
    #[test]
    fn a_short_value_s_block_run_is_proportional_to_its_length() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);
        let value = cell(&buffer, first_row(), Column::Value);
        let blocks = value.chars().take_while(|&c| c == '\u{2588}').count();

        // `DB_PASSWORD` carries `byte_len: Some(9)`.
        assert_eq!(
            blocks, 9,
            "the run states the value's own length: {value:?}"
        );
    }

    /// `draw_layout::draw` passes `columns_for` the full row width, gutter included:
    /// `columns_for(table_width)` fits one column short of what
    /// [`SECRET_TIERS`] calibrated for, and drops `LANDS` at 160 columns
    /// with no other test catching it.
    #[test]
    fn lands_is_drawn_at_the_widest_tier() {
        let app = fixtures::app_with_secrets();
        let buffer = fixtures::render(&app, 160, 48);

        assert_eq!(cell(&buffer, heading_row(), Column::Lands).trim(), "LANDS");
        assert_eq!(cell(&buffer, first_row(), Column::Lands).trim(), "-");
    }
}
