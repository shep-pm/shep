//! The column schema: which columns exist, what each one is called and how
//! wide it is, and which set a given terminal width can afford.
//!
//! Two schemas, because a fold row rolls up a whole fold and a flat row
//! shows one process: [`Column`] for the flat table, [`FoldColumn`] for the
//! fold one. Each has its own ladder of tiers, widest first, and the header
//! line that goes above it.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::layout::{MIN_WIDTH, fit};

/// The floor on the NAME column, which takes whatever the fixed columns
/// leave.
pub const NAME_MIN: u16 = 8;

/// The ceiling on the NAME column.
///
/// NAME takes the remainder, which is right up to a point and absurd past
/// it: on a 224-column terminal the remainder is 84 cells for names that
/// are rarely longer than twenty, so the table becomes a field of
/// whitespace with the numbers pushed to the far right, where they are
/// harder to read against each other than they were before. Past this
/// width the table simply ends and the rest of the row stays empty, which
/// is what the design's own frames did with their right margin.
///
/// 32 rather than the frames' 24: it clears the longest name in this
/// repository's own example Flockfile, `http-server-gated-by-sentinel`.
pub const NAME_MAX: u16 = 32;

/// One column of the flock table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    /// The sheep's stable numeric id.
    Id,
    /// Its name. The flexible column.
    Name,
    /// Its lifecycle status, the one coloured cell.
    Status,
    /// Its OS pid while running.
    Pid,
    /// Restarts since registration.
    Restarts,
    /// Its last exit, once it is not running. Rendered by
    /// [`crate::output::exit_cell`], the same function
    /// `output::rows::FlockRows`'s own EXIT column calls.
    Exit,
    /// Whether a config load has parked a change for this sheep's next
    /// spawn, or an operator has overridden a field its Flockfile no longer
    /// declares. Rendered by [`crate::output::cfg_cell`], the same function
    /// `output::rows::FlockRows`'s own CFG column calls.
    Cfg,
    /// The last twenty seconds of tree CPU, one cell per sample. `shep flock`
    /// draws no equivalent; a still frame has nowhere to put one.
    CpuSpark,
    /// Tree CPU as a percentage of one core.
    Cpu,
    /// Resident set size against
    /// [`shep_core::protocol::ProcessInfo::max_memory`], as a filled bar.
    /// `shep flock` draws no equivalent, for the same reason as
    /// [`Self::CpuSpark`].
    MemCeil,
    /// Tree resident set size.
    Mem,
    /// Time since its last successful start.
    Uptime,
    /// Fold membership.
    Fold,
    /// A short marker a dog attaches to a sheep over the client protocol's
    /// `SetSmit` request, last in the header order to match
    /// `output::rows::FlockRows`'s. shep paints what a dog wrote and never
    /// parses it.
    Smit,
}

impl Column {
    /// The header text. Every column `output::rows::FlockRows` also draws
    /// shares its vocabulary, enforced by
    /// `every_shared_header_still_matches_flock_rows_exactly` below.
    #[must_use]
    pub const fn header(self) -> &'static str {
        match self {
            Self::Id => "ID",
            Self::Name => "NAME",
            Self::Status => "STATUS",
            Self::Pid => "PID",
            Self::Restarts => "RESTARTS",
            Self::Exit => "EXIT",
            Self::Cfg => "CFG",
            Self::CpuSpark => "CPU 20s",
            Self::Cpu => "CPU",
            Self::MemCeil => "MEM/CEIL",
            Self::Mem => "MEM",
            Self::Uptime => "UPTIME",
            Self::Fold => "FOLD",
            Self::Smit => "SMIT",
        }
    }

    /// The header text on a frozen dashboard.
    ///
    /// [`Self::header`] for every column but [`Self::Uptime`], whose cells
    /// hold a duration that stopped advancing the moment the link did.
    /// `UPTIME` over a stalled number is the one cell on the frozen frame
    /// that reads as live, and the whole point of that screen is that none
    /// of them may.
    ///
    /// `FROZEN`, not the design's own `FROZEN AT`: [`Self::width`] gives
    /// this column 8 cells, `FROZEN AT` is 9, and widening it moves every
    /// threshold in [`TIERS`]. The band two rows above already carries the
    /// timestamp the `AT` would point at.
    #[must_use]
    pub const fn frozen_header(self) -> &'static str {
        match self {
            Self::Uptime => "FROZEN",
            _ => self.header(),
        }
    }

    /// The fixed width of this column's cells. `Name` reports `0`: it is
    /// the column that takes the remainder, and [`name_width`] computes it.
    #[must_use]
    pub const fn width(self) -> u16 {
        match self {
            Self::Id => 4,
            Self::Name => 0,
            // 15: `waiting-restart`, the longest word `Reported::word`
            // returns.
            Self::Status => 15,
            Self::Pid => 7,
            Self::Restarts => 8,
            // 9: `SIGVTALRM`/`SIGSTKFLT`, the longest names
            // `nix::sys::signal::Signal::as_str` returns.
            Self::Exit => 9,
            // 4: `!12`/`*12`, a `cfg_cell`'s own longest realistic value.
            Self::Cfg => 4,
            Self::CpuSpark => 10,
            Self::Cpu => 6,
            Self::MemCeil => 10,
            Self::Mem => 8,
            Self::Uptime => 8,
            Self::Fold => 10,
            // 13: `visible_width("▲ main@a1b2c3")`, the measured width of the
            // real strings a deploy dog paints. A longer smit truncates via
            // [`fit`] rather than growing the column.
            Self::Smit => 13,
        }
    }
}

/// Every column, including the two the terminal must be widest to keep.
///
/// `CpuSpark` and `MemCeil` sit beside `Cpu` and `Mem`, the cells they add
/// context to, rather than up front where the header test's ordering
/// (`every_shared_header_still_matches_flock_rows_exactly`) would put them
/// ahead of `Pid`/`Restarts`/`Exit`/`Cfg` and break that vocabulary check.
pub(super) const ALL: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Pid,
    Column::Restarts,
    Column::Exit,
    Column::Cfg,
    Column::CpuSpark,
    Column::Cpu,
    Column::MemCeil,
    Column::Mem,
    Column::Uptime,
    Column::Fold,
    Column::Smit,
];

/// `ALL` minus `MemCeil`, the first column a narrowing terminal sheds.
const NO_CEIL: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Pid,
    Column::Restarts,
    Column::Exit,
    Column::Cfg,
    Column::CpuSpark,
    Column::Cpu,
    Column::Mem,
    Column::Uptime,
    Column::Fold,
    Column::Smit,
];

/// `NO_CEIL` minus `CpuSpark`: today's full set, and today's threshold.
const NO_SPARK: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Pid,
    Column::Restarts,
    Column::Exit,
    Column::Cfg,
    Column::Cpu,
    Column::Mem,
    Column::Uptime,
    Column::Fold,
    Column::Smit,
];
// `NO_SPARK` minus CFG, the next column dropped. See `TIERS`'s own doc for
// the drop order.
const NO_CFG: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Pid,
    Column::Restarts,
    Column::Exit,
    Column::Cpu,
    Column::Mem,
    Column::Uptime,
    Column::Fold,
    Column::Smit,
];

const NO_SMIT: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Pid,
    Column::Restarts,
    Column::Exit,
    Column::Cpu,
    Column::Mem,
    Column::Uptime,
    Column::Fold,
];

const NO_FOLD: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Pid,
    Column::Restarts,
    Column::Exit,
    Column::Cpu,
    Column::Mem,
    Column::Uptime,
];

const NO_EXIT: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Pid,
    Column::Restarts,
    Column::Cpu,
    Column::Mem,
    Column::Uptime,
];

const NO_RESTARTS: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Pid,
    Column::Cpu,
    Column::Mem,
    Column::Uptime,
];

const NO_PID: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Cpu,
    Column::Mem,
    Column::Uptime,
];

const NO_MEM: &[Column] = &[
    Column::Id,
    Column::Name,
    Column::Status,
    Column::Cpu,
    Column::Uptime,
];

const NO_CPU: &[Column] = &[Column::Id, Column::Name, Column::Status, Column::Uptime];

const FLOOR: &[Column] = &[Column::Id, Column::Name, Column::Status];

/// The narrowest terminal that still draws the `CFG` column.
///
/// Read by the status bar's own test: the legend explaining `*` and `!`
/// only has to fit where the glyphs it explains are drawn.
#[cfg(test)]
pub(in crate::lookout::view) fn cfg_tier_width() -> u16 {
    TIERS
        .iter()
        .filter(|(_, columns)| columns.contains(&Column::Cfg))
        .map(|(threshold, _)| *threshold)
        .min()
        .expect("a tier draws CFG")
}

/// Width thresholds, widest first. Each entry is the narrowest terminal that
/// still gets that column set.
///
/// The drop order is least-diagnostic first: FOLD is grouping metadata,
/// RESTARTS and PID answer follow-up questions, CPU and MEM explain why a
/// running sheep is behaving badly, and EXIT renders `-` for every running
/// sheep, the common case. `ID NAME STATUS` is the floor.
///
/// CFG has its own drop tier, one tier before SMIT's, keeping `NO_CFG`'s
/// threshold at 116 so a 120-column terminal, the gallery's fixture width,
/// still shows SMIT.
///
/// `MemCeil` and `CpuSpark` drop first of all, ahead of `Cfg`: both restate
/// a number another column already carries, so a terminal too narrow for
/// them still shows the value, just not its shape over time.
const TIERS: &[(u16, &[Column])] = &[
    (146, ALL),
    (134, NO_CEIL),
    (122, NO_SPARK),
    (116, NO_CFG),
    (101, NO_SMIT),
    (89, NO_FOLD),
    (78, NO_EXIT),
    (68, NO_RESTARTS),
    (59, NO_PID),
    (49, NO_MEM),
    (41, NO_CPU),
    (MIN_WIDTH, FLOOR),
];

/// The widest column set that fits `width`.
#[must_use]
pub fn columns_for(width: u16) -> &'static [Column] {
    TIERS
        .iter()
        .find(|(threshold, _)| width >= *threshold)
        .map_or(FLOOR, |(_, columns)| *columns)
}

/// What NAME gets, once the fixed columns and the separators are paid for.
#[must_use]
pub fn name_width(width: u16, columns: &[Column]) -> u16 {
    let fixed: u16 = columns.iter().map(|column| column.width()).sum();
    name_width_from(width, fixed, columns.len())
}

/// The arithmetic [`name_width`] does, shared with [`fold_name_width`] so
/// the fold view's own name column follows the same clamp and gap rule
/// rather than a second copy of it.
fn name_width_from(width: u16, fixed: u16, columns_len: usize) -> u16 {
    let gaps = u16::try_from(columns_len.saturating_sub(1)).unwrap_or(0) * 2;
    width
        .saturating_sub(fixed)
        .saturating_sub(gaps)
        .clamp(NAME_MIN, NAME_MAX)
}

/// One column of the fold view: the flock gathered by `AppConfig::fold`
/// instead of by name ([`super::super::super::app::Grouping::ByFold`]).
///
/// Separate from [`Column`], which is the flat table's set: the two share
/// only `STATUS`, and [`Column::Fold`] already means one sheep's own fold
/// name rather than anything about grouping. Two sets that each stay simple
/// beat one carrying a mode flag into every arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldColumn {
    /// The fold header's or member's name, taking the remainder.
    Name,
    /// The row's lifecycle status. The one word this shares with [`Column`].
    Status,
    /// A 20-cell gauge of this fold's share of total flock memory, drawn in
    /// a 22-cell column. Blank on a member row: the share is a fact about
    /// the fold, not about one of its members.
    Share,
    /// Summed memory on a header, the member's own reading on a member row.
    Mem,
    /// Summed CPU on a header, the member's own reading on a member row.
    Cpu,
    /// The shortest member's uptime on a header, the member's own on a
    /// member row.
    Uptime,
    /// Summed restarts on a header, the member's own count on a member row.
    Restarts,
    /// The share's own percentage, on a fold header. Blank on a member row.
    Notes,
}

impl FoldColumn {
    /// The fixed width of this column's cells. `Name` reports `0`: it is
    /// the column that takes the remainder, computed by [`fold_name_width`].
    #[must_use]
    pub const fn width(self) -> u16 {
        match self {
            Self::Name => 0,
            Self::Status => 12,
            Self::Share => 22,
            Self::Mem => 10,
            Self::Cpu => 9,
            Self::Uptime => 10,
            Self::Restarts => 8,
            Self::Notes => 63,
        }
    }

    /// The header text, drawn by [`fold_columns_header_line`].
    #[must_use]
    pub const fn header(self) -> &'static str {
        match self {
            Self::Name => "NAME",
            Self::Status => "STATUS",
            Self::Share => "SHARE",
            Self::Mem => "MEM",
            Self::Cpu => "CPU",
            Self::Uptime => "UPTIME",
            Self::Restarts => "RESTARTS",
            Self::Notes => "NOTES",
        }
    }
}

/// Every fold-view column.
const FOLD_ALL: &[FoldColumn] = &[
    FoldColumn::Name,
    FoldColumn::Status,
    FoldColumn::Share,
    FoldColumn::Mem,
    FoldColumn::Cpu,
    FoldColumn::Uptime,
    FoldColumn::Restarts,
    FoldColumn::Notes,
];

/// `FOLD_ALL` minus `Share`, the first column a narrowing terminal sheds:
/// a 22-cell gauge that exists nowhere in flat view is the easiest thing
/// here to live without.
const FOLD_NO_SHARE: &[FoldColumn] = &[
    FoldColumn::Name,
    FoldColumn::Status,
    FoldColumn::Mem,
    FoldColumn::Cpu,
    FoldColumn::Uptime,
    FoldColumn::Restarts,
    FoldColumn::Notes,
];

/// `FOLD_NO_SHARE` minus `Notes`, the 63-cell column that otherwise
/// dominates a narrow row.
const FOLD_NO_NOTES: &[FoldColumn] = &[
    FoldColumn::Name,
    FoldColumn::Status,
    FoldColumn::Mem,
    FoldColumn::Cpu,
    FoldColumn::Uptime,
    FoldColumn::Restarts,
];

const FOLD_NO_RESTARTS: &[FoldColumn] = &[
    FoldColumn::Name,
    FoldColumn::Status,
    FoldColumn::Mem,
    FoldColumn::Cpu,
    FoldColumn::Uptime,
];

const FOLD_NO_CPU: &[FoldColumn] = &[
    FoldColumn::Name,
    FoldColumn::Status,
    FoldColumn::Mem,
    FoldColumn::Uptime,
];

const FOLD_NO_UPTIME: &[FoldColumn] = &[FoldColumn::Name, FoldColumn::Status, FoldColumn::Mem];

/// `NAME` and `STATUS`, the floor: the same two facts flat view's own floor
/// keeps, minus `ID`, which the fold view never had.
const FOLD_FLOOR: &[FoldColumn] = &[FoldColumn::Name, FoldColumn::Status];

/// Width thresholds for the fold view, widest first, in the shape of
/// [`TIERS`]. Each threshold is the narrowest terminal that still fits its
/// column set's fixed columns, their gaps, and [`NAME_MIN`]: see
/// `every_fold_tier_fits_the_width_it_claims`.
const FOLD_TIERS: &[(u16, &[FoldColumn])] = &[
    (158, FOLD_ALL),
    (134, FOLD_NO_SHARE),
    (69, FOLD_NO_NOTES),
    (59, FOLD_NO_RESTARTS),
    (48, FOLD_NO_CPU),
    (36, FOLD_NO_UPTIME),
    (MIN_WIDTH, FOLD_FLOOR),
];

/// The widest fold-view column set that fits `width`, [`columns_for`]'s
/// twin for the fold view's own, separate column set.
#[must_use]
pub fn fold_columns_for(width: u16) -> &'static [FoldColumn] {
    FOLD_TIERS
        .iter()
        .find(|(threshold, _)| width >= *threshold)
        .map_or(FOLD_FLOOR, |(_, columns)| *columns)
}

/// [`name_width`]'s twin for [`FoldColumn`].
pub(super) fn fold_name_width(width: u16, columns: &[FoldColumn]) -> u16 {
    let fixed: u16 = columns.iter().map(|column| column.width()).sum();
    name_width_from(width, fixed, columns.len()).max(FOLD_NAME_MIN)
}

/// The fold view's name floor, two columns wider than [`NAME_MIN`].
///
/// A fold header's name cell carries a disclosure triangle and a space that
/// flat view's does not, so the same floor would spend the whole difference
/// on the marker and truncate the `\u{d7}N` count instead: `\u{25b8} batch\u{2026}`
/// rather than `\u{25b8} batch \u{d7}2`. The count is the rollup, so losing it to
/// keep the marker trades one design element for another.
///
/// [`FOLD_TIERS`]' thresholds carry the same two columns, which is why they
/// sit two above the widths the column widths alone would need.
const FOLD_NAME_MIN: u16 = NAME_MIN + 2;

/// The header line: every column name, muted.
///
/// `frozen` swaps [`Column::header`] for [`Column::frozen_header`], which
/// differs for exactly one column. See that method for why the rename is
/// the header's job rather than the cell's.
#[must_use]
pub fn header_line(columns: &[Column], width: u16, style: Style, frozen: bool) -> Line<'static> {
    let name = name_width(width, columns);
    let mut text = String::new();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            text.push_str("  ");
        }
        let cell_width = if *column == Column::Name {
            name
        } else {
            column.width()
        };
        let header = if frozen {
            column.frozen_header()
        } else {
            column.header()
        };
        text.push_str(&fit(header, cell_width));
    }
    Line::from(Span::styled(text, style))
}

/// [`header_line`]'s twin for the fold view: every [`FoldColumn`]'s own
/// header text, muted, in [`fold_columns_for`]'s widths.
#[must_use]
pub fn fold_columns_header_line(columns: &[FoldColumn], width: u16, style: Style) -> Line<'static> {
    let name = fold_name_width(width, columns);
    let mut text = String::new();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            text.push_str("  ");
        }
        let cell_width = if *column == FoldColumn::Name {
            name
        } else {
            column.width()
        };
        text.push_str(&fit(column.header(), cell_width));
    }
    Line::from(Span::styled(text, style))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FROZEN` has to fit the column it renames, and the column is not
    /// growing to hold it: [`Column::width`] gives `Uptime` 8 cells, and
    /// every threshold in [`TIERS`] is derived from the sum of those
    /// widths. The design's own `FROZEN AT` is 9.
    #[test]
    fn the_frozen_header_fits_the_column_it_renames() {
        use crate::output::width::visible_width;

        assert_eq!(Column::Uptime.frozen_header(), "FROZEN");
        assert!(
            visible_width(Column::Uptime.frozen_header()) <= usize::from(Column::Uptime.width()),
            "FROZEN does not fit UPTIME's own 8 cells"
        );
        assert!(
            visible_width("FROZEN AT") > usize::from(Column::Uptime.width()),
            "the design's own header now fits, so this rename has no reason to exist"
        );
    }

    /// Exactly one column reads differently once the link is gone.
    #[test]
    fn only_the_uptime_column_is_renamed_by_a_freeze() {
        let renamed: Vec<&str> = ALL
            .iter()
            .filter(|column| column.frozen_header() != column.header())
            .map(|column| column.header())
            .collect();
        assert_eq!(renamed, vec!["UPTIME"]);
    }

    /// FOLD goes first (grouping metadata), then EXIT (silent for a running
    /// sheep, the common case), then RESTARTS and PID, then CPU and MEM (the
    /// last to explain why a running sheep misbehaves). ID/NAME/STATUS is
    /// the floor.
    #[test]
    fn columns_drop_in_a_fixed_order_as_the_terminal_narrows() {
        assert_eq!(columns_for(300).len(), 14);
        assert_eq!(columns_for(122).len(), 12);
        // CFG is the first column gone, ahead of even SMIT. See `TIERS`'s
        // own doc for the reasoning.
        assert!(!columns_for(121).contains(&Column::Cfg));
        assert!(columns_for(121).contains(&Column::Smit));
        assert_eq!(columns_for(116).len(), 11);
        assert!(!columns_for(115).contains(&Column::Smit));
        assert!(columns_for(115).contains(&Column::Fold));
        assert_eq!(columns_for(101).len(), 10);
        assert!(!columns_for(100).contains(&Column::Fold));
        assert!(columns_for(100).contains(&Column::Exit));
        assert!(!columns_for(88).contains(&Column::Exit));
        assert!(columns_for(88).contains(&Column::Restarts));
        assert!(!columns_for(77).contains(&Column::Restarts));
        assert!(!columns_for(67).contains(&Column::Pid));
        assert!(!columns_for(58).contains(&Column::Mem));
        assert!(!columns_for(48).contains(&Column::Cpu));
        assert_eq!(columns_for(31), &[Column::Id, Column::Name, Column::Status]);
        // Every tier keeps the three that are the pane.
        for width in [31u16, 40, 48, 58, 67, 77, 88, 100, 300] {
            let cols = columns_for(width);
            for required in [Column::Id, Column::Name, Column::Status] {
                assert!(
                    cols.contains(&required),
                    "width {width} dropped {required:?}"
                );
            }
        }
    }

    /// The gallery's own fixtures render at 120 columns, so this pins the
    /// one width a future column's drop-order choice must not narrow past.
    #[test]
    fn smit_survives_the_gallerys_own_wide_fixtures() {
        assert!(columns_for(120).contains(&Column::Smit));
    }

    #[test]
    fn the_two_new_rungs_restore_todays_table_before_shedding_anything_old() {
        assert_eq!(columns_for(146), ALL);
        assert!(!columns_for(134).contains(&Column::MemCeil));
        assert!(columns_for(134).contains(&Column::CpuSpark));
        assert!(!columns_for(122).contains(&Column::CpuSpark));
        assert_eq!(
            columns_for(122),
            NO_SPARK,
            "122 is today's full set, unchanged"
        );
    }

    /// Enforces `Column::header`'s claim of one vocabulary across both
    /// surfaces, rather than leaving it aspirational. Replaces
    /// `the_full_column_set_matches_flock_rows_headers_exactly`, which
    /// compared `ALL` to `FlockRows::headers()` directly and broke the
    /// moment `ALL` grew two headers `shep flock` cannot draw.
    #[test]
    fn every_shared_header_still_matches_flock_rows_exactly() {
        use crate::output::Render;

        let shared: Vec<&str> = ALL
            .iter()
            .map(|column| column.header())
            .filter(|header| crate::output::FlockRows::headers().contains(header))
            .collect();
        assert_eq!(shared, crate::output::FlockRows::headers());
    }

    #[test]
    fn the_only_headers_lookout_adds_are_the_two_shep_flock_cannot_draw() {
        use crate::output::Render;

        let extra: Vec<&str> = ALL
            .iter()
            .map(|column| column.header())
            .filter(|header| !crate::output::FlockRows::headers().contains(header))
            .collect();
        assert_eq!(extra, vec!["CPU 20s", "MEM/CEIL"]);
    }

    #[test]
    fn every_tier_fits_the_width_it_claims() {
        for width in MIN_WIDTH..=200 {
            let cols = columns_for(width);
            let fixed: u16 = cols.iter().map(|c| c.width()).sum();
            let gaps = u16::try_from(cols.len() - 1).unwrap() * 2;
            assert!(
                fixed + gaps + NAME_MIN <= width,
                "width {width} chose {} columns needing {}",
                cols.len(),
                fixed + gaps + NAME_MIN
            );
        }
    }

    /// The same invariant the flat ladder carries: a tier chosen for a width
    /// must actually fit in it.
    #[test]
    fn every_fold_tier_fits_the_width_it_claims() {
        for width in MIN_WIDTH..=200 {
            let cols = fold_columns_for(width);
            let fixed: u16 = cols.iter().map(|c| c.width()).sum();
            let gaps = u16::try_from(cols.len() - 1).unwrap() * 2;
            assert!(
                fixed + gaps + FOLD_NAME_MIN <= width,
                "width {width} chose {} columns needing {}",
                cols.len(),
                fixed + gaps + FOLD_NAME_MIN
            );
        }
    }

    /// The share bar goes first because it is the widest thing that is not
    /// the name, and it does not exist in flat view to be missed.
    #[test]
    fn the_share_bar_is_the_first_column_to_go() {
        let wide = fold_columns_for(200);
        assert!(wide.contains(&FoldColumn::Share));
        let narrow = fold_columns_for(100);
        assert!(!narrow.contains(&FoldColumn::Share), "got {narrow:?}");
    }
}
