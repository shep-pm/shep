//! The sheep pane: one sheep's histories, config, env keys and feed.
//!
//! The charts read [`App`](super::app::App)'s own ring buffers rather than
//! anything here, so history keeps filling while the pane is shut and a
//! pane opened on a sheep that has been running all along starts full.

use std::time::Duration;

use shep_core::protocol::SheepConfigView;

use super::app::RowKey;
use super::link::FLOCK_POLL;
use super::pane_bleats::BleatsPane;
use super::view::sheep::column_len;
use super::viewport::Viewport;

/// The span a chart of `body_cells` covers, one poll per cell.
///
/// Computed rather than written into a label: the body is
/// `min(width - 20, HISTORY)`, so the window is 4m40s at 160 columns and
/// 4m00s at 140, and any literal would be wrong at one of them.
#[must_use]
pub fn window(body_cells: usize) -> Duration {
    FLOCK_POLL * body_cells as u32
}

/// `peak` rounded up a 1-2-5 ladder, never below `floor`.
///
/// The ladder is what puts the gutter labels on round numbers. The floor
/// is [`super::app::CPU_CEILING_FLOOR`]'s reason in a second place: below
/// it there is nothing to see, and saying so is the honest answer.
#[must_use]
pub fn scale_top(peak: f64, floor: f64) -> f64 {
    let peak = peak.max(floor);
    let decade = 10f64.powf(peak.log10().floor());
    for step in [1.0, 2.0, 5.0, 10.0] {
        let candidate = step * decade;
        if candidate >= peak {
            return candidate;
        }
    }
    10.0 * decade
}

/// One sheep, given the whole screen.
///
/// `Debug` is derived (IR-41): [`SheepConfigView`]'s own `Debug` is already
/// redacted, since it carries a key set; [`BleatsPane`]'s carries a row key
/// and a filter set, neither of which is a value the pane withholds; and a
/// [`RowKey`] is a bare integer.
///
/// The left column ([`super::view::sheep::draw_column`]) is the only
/// scroller a sheep pane owns today: the two charts have no cursor, and the
/// feed is [`BleatsPane`]'s own. `Viewport` counts lines rather than data
/// rows here, since a group header costs a blank, a rule and a label that
/// no field owns; [`column_len`] is the one place both this and the column
/// itself read that count off, so they cannot drift apart.
#[derive(Debug)]
pub struct SheepPane {
    sheep: RowKey,
    /// `None` until `Request::SheepConfig` answers. The left column draws
    /// its own waiting line rather than an empty group list, which would
    /// read as a sheep with no config at all.
    config: Option<SheepConfigView>,
    /// The full feed, filtered to this sheep. Read by a later task; kept
    /// current from the moment the pane opens (and re-pinned on
    /// [`Self::set_sheep`]) so the feed is not starting cold the first time
    /// something draws it.
    feed: BleatsPane,
    /// The config/env column's own cursor and offset.
    view: Viewport,
}

impl SheepPane {
    /// Opens on `sheep`, with no config yet and the feed following the tail.
    #[must_use]
    pub fn new(sheep: RowKey) -> Self {
        let feed = BleatsPane::new(sheep.clone());
        Self {
            sheep,
            config: None,
            feed,
            view: Viewport::new(),
        }
    }

    /// The config/env column's own cursor and offset.
    #[must_use]
    pub fn view(&self) -> &Viewport {
        &self.view
    }

    /// `COLUMN_BODY_ROWS` never changes, so every entry point below records
    /// it itself rather than asking a caller to have called
    /// `App::note_body_rows` first, the way the editing panes' own
    /// `set_rows` does: the column's height is fixed by the frame, not by
    /// the terminal.
    ///
    /// Named `body_len`, not `column_len`, so it reads as its own method
    /// rather than as the free [`column_len`] it wraps calling itself.
    fn body_len(&self) -> usize {
        column_len(self.config())
    }

    /// `j`, one line.
    pub(crate) fn move_by(&mut self, delta: isize) {
        let len = self.body_len();
        self.view
            .set_rows(super::view::sheep::COLUMN_BODY_ROWS, len);
        self.view.move_by(delta, len);
    }

    /// `g`.
    pub(crate) fn move_to_first(&mut self) {
        let len = self.body_len();
        self.view
            .set_rows(super::view::sheep::COLUMN_BODY_ROWS, len);
        self.view.move_to(0, len);
    }

    /// `G`.
    pub(crate) fn move_to_last(&mut self) {
        let len = self.body_len();
        self.view
            .set_rows(super::view::sheep::COLUMN_BODY_ROWS, len);
        self.view.move_to(len.saturating_sub(1), len);
    }

    /// The sheep this pane describes right now. Changes under `J`/`K`
    /// without the pane closing.
    #[must_use]
    pub fn sheep(&self) -> &RowKey {
        &self.sheep
    }

    /// The sheep the embedded feed is scoped to.
    ///
    /// Always equal to [`Self::sheep`] once [`Self::set_sheep`] re-pins
    /// both, but read through the feed itself rather than through
    /// `sheep()`: a stepping regression that re-pinned one field and not the
    /// other would still pass a test that read `sheep()` alone.
    #[must_use]
    pub fn feed_sheep(&self) -> &RowKey {
        self.feed.sheep()
    }

    /// The embedded feed's own state, for the sheep pane's keymap to route
    /// the keys it owns (`/`, `o`, `m`, `f`, `w`, `n`, `N`) and for
    /// [`super::view::sheep::draw`] to render it.
    #[must_use]
    pub fn feed(&self) -> &BleatsPane {
        &self.feed
    }

    /// [`Self::feed`]'s mutable twin, for the same keymap.
    pub(crate) fn feed_mut(&mut self) -> &mut BleatsPane {
        &mut self.feed
    }

    /// The sheep's config, or `None` while the read is still in flight, or
    /// after a refusal that left nothing to show.
    #[must_use]
    pub fn config(&self) -> Option<&SheepConfigView> {
        self.config.as_ref()
    }

    /// Adopts a `Request::SheepConfig` reply.
    pub fn adopt_config(&mut self, view: SheepConfigView) {
        self.config = Some(view);
    }

    /// Points the pane at a different sheep, the way `J`/`K` do.
    ///
    /// The old sheep's config is dropped rather than kept on screen under
    /// the new sheep's name, and the feed is re-pinned to match: both would
    /// otherwise describe a sheep the pane no longer names.
    pub(crate) fn set_sheep(&mut self, sheep: RowKey) {
        self.feed = BleatsPane::new(sheep.clone());
        self.sheep = sheep;
        self.config = None;
    }
}

#[cfg(test)]
mod tests {
    use shep_core::config::AppConfig;

    use super::*;

    /// One cell per poll. The frame says six minutes at 5s samples and both
    /// halves are wrong: the poll is 2s, so 140 cells is 4m40s.
    #[test]
    fn the_window_is_one_poll_per_cell() {
        assert_eq!(window(140), Duration::from_secs(280));
        assert_eq!(window(120), Duration::from_secs(240));
    }

    /// A 1-2-5 ladder so the gutter labels land on round numbers.
    #[test]
    fn a_scale_top_rounds_up_the_ladder() {
        assert_eq!(scale_top(34.0, 2.0), 50.0);
        assert_eq!(scale_top(6.0, 2.0), 10.0);
        assert_eq!(scale_top(1.2, 2.0), 2.0);
    }

    /// Floored, so a flock genuinely doing nothing stays flat instead of
    /// having its rounding noise stretched into a shape.
    #[test]
    fn a_scale_top_never_falls_below_its_floor() {
        assert_eq!(scale_top(0.01, 2.0), 2.0);
    }

    /// [`SheepConfigView`]'s own `Debug` already withholds `args` and `cwd`;
    /// this pins that [`SheepPane`]'s derived one does not undo that by
    /// printing the feed or the viewport instead. Matches the shape
    /// `ConfigPane`'s own exact-string test uses (`pane.rs:786`).
    #[test]
    fn the_panes_debug_names_no_field_value() {
        let mut pane = SheepPane::new(RowKey::Sheep(9));
        let config = AppConfig {
            name: "web".into(),
            cwd: Some("/home/ada/secret-project".into()),
            args: vec!["--token".into(), "hunter2".into()],
            ..AppConfig::default()
        };
        pane.adopt_config(SheepConfigView::new(config, Vec::new(), Vec::new()));
        assert_eq!(
            format!("{pane:?}"),
            r#"SheepPane { sheep: Sheep(9), config: Some(SheepConfigView { name: "web", env_keys: 0, env_secrets: 0, overridden: 0, pending: 0 }), feed: BleatsPane { sheep: Sheep(9), filters: Filters { stream: None, min_level: None, matcher: None, order: [] }, match_snapshot: None, scroll_offset: 0, following: true, body_rows: 0, width: 0, wrap: false }, view: Viewport { cursor: 0, offset: 0, rows: 0 } }"#
        );
    }

    /// A default Flockfile's own config, wide enough that the column's body
    /// runs past `COLUMN_BODY_ROWS` and the scroll tests below have
    /// somewhere to move to.
    fn scrollable_pane() -> SheepPane {
        let mut pane = SheepPane::new(RowKey::Sheep(1));
        pane.adopt_config(SheepConfigView::new(
            AppConfig::default(),
            Vec::new(),
            Vec::new(),
        ));
        pane
    }

    /// `j`/`k` (`move_by`) walk the column's cursor one line at a time and
    /// clamp rather than run past either end, the same rule `move_by`'s own
    /// doc gives.
    #[test]
    fn move_by_walks_the_column_and_clamps_at_both_ends() {
        let mut pane = scrollable_pane();
        let len = pane.body_len();
        assert_eq!(pane.view().cursor(), 0);
        pane.move_by(1);
        assert_eq!(pane.view().cursor(), 1);
        pane.move_by(-5);
        assert_eq!(pane.view().cursor(), 0, "clamped at the top");
        pane.move_by(isize::try_from(len).unwrap() + 5);
        assert_eq!(pane.view().cursor(), len - 1, "clamped at the bottom");
    }

    /// `g` and `G` (`move_to_first`/`move_to_last`) jump straight to either
    /// end rather than walking there one `move_by` at a time.
    #[test]
    fn move_to_first_and_last_jump_to_either_end() {
        let mut pane = scrollable_pane();
        let len = pane.body_len();
        pane.move_to_last();
        assert_eq!(pane.view().cursor(), len - 1);
        pane.move_to_first();
        assert_eq!(pane.view().cursor(), 0);
    }

    /// Scrolling the cursor past the visible `COLUMN_BODY_ROWS` moves the
    /// offset forward, and `move_to_last` never leaves it past `len - rows`:
    /// the same `ensure_visible` contract `Viewport` already carries, pinned
    /// here through the pane's own entry points rather than `Viewport`
    /// alone, since those are what a keypress actually calls.
    #[test]
    fn scrolling_past_the_visible_rows_advances_the_offset() {
        let mut pane = scrollable_pane();
        let len = pane.body_len();
        assert!(
            len > super::super::view::sheep::COLUMN_BODY_ROWS,
            "setup: a default config must scroll: {len} lines, {} visible",
            super::super::view::sheep::COLUMN_BODY_ROWS
        );
        for _ in 0..super::super::view::sheep::COLUMN_BODY_ROWS + 3 {
            pane.move_by(1);
        }
        assert!(pane.view().offset() > 0, "offset: {}", pane.view().offset());
        pane.move_to_last();
        assert_eq!(
            pane.view().offset(),
            len - super::super::view::sheep::COLUMN_BODY_ROWS,
            "the last page, not scrolled past the end"
        );
    }
}
