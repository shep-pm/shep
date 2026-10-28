//! The full-screen bleats pane's own state: which sheep it is pinned to, and
//! the three filters an operator can stack over the feed it reads.

use super::app::RowKey;
use super::level::{Classifier, Level};
use super::tail::{Stream, TailLine};
use super::textbox::TextBox;

mod filters;
mod match_edit;

use filters::Axis;

// Re-exported so `view::bleats_full`'s compile-count tests keep reading it
// at `crate::lookout::pane_bleats::compiles`.
#[cfg(test)]
pub(crate) use filters::compiles;
pub use filters::{Filters, MatchKind};

/// The full-screen bleats pane's own state.
///
/// Holds the sheep it opened on rather than reading the dashboard's
/// selection: full screen leaves no table on which to change one, so the
/// pane describes a single sheep for as long as it is open. Also holds the
/// filters stacked on top of that sheep's feed, and, while the match box is
/// open, what the match axis held before the edit began. `Debug` is derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleatsPane {
    sheep: RowKey,
    filters: Filters,
    /// `Some` while the match box owns `InputMode::Text`; the match axis's
    /// text as it stood before this edit started, so [`Self::abandon_match_edit`]
    /// can restore it. `None` the rest of the time.
    match_snapshot: Option<match_edit::MatchEdit>,
    /// How many surviving lines back from the newest one the view is
    /// scrolled, counting filtered lines (what [`Self::visible`] returns),
    /// not raw ones. `0` is the newest surviving line, the tail.
    ///
    /// A stored value is never trusted past a filter change:
    /// [`super::view::bleats_full`] recomputes the window from this and the
    /// current survivor count on every draw.
    scroll_offset: usize,
    /// Whether the view stays pinned to the newest surviving line as new
    /// ones arrive. `true` on open: an operator who has not scrolled wants
    /// the live tail. A deliberate move off the tail clears it.
    ///
    /// [`Self::scroll_down`] is the one key that does not, walking back
    /// toward a tail an earlier step left. [`Self::jump_to_end`] and
    /// [`Self::toggle_follow`] restore it and reset [`Self::scroll_offset`].
    following: bool,
    /// The body rows available to page by, set from
    /// [`super::app::App::note_body_rows`]. `0` until the first draw.
    body_rows: usize,
    /// The area's own column width, set from
    /// [`super::app::App::note_body_width`]. `0` until the first draw, which
    /// [`super::view::bleats_full::page_amount_up`] reads as "no column count
    /// to wrap against" and falls back to [`Self::body_rows`] unmodified,
    /// the same amount a page always moved by before wrap existed.
    width: u16,
    /// Whether a line too wide for the pane wraps onto extra rows instead
    /// of truncating with an ellipsis. `false` on open: unwrapped is the
    /// pane's original behavior, and every existing feed still reads that
    /// way until an operator asks for the other one.
    wrap: bool,
}

impl BleatsPane {
    /// Opens the pane on one sheep, with no filters set, following the tail.
    #[must_use]
    pub fn new(sheep: RowKey) -> Self {
        Self {
            sheep,
            filters: Filters::default(),
            match_snapshot: None,
            scroll_offset: 0,
            following: true,
            body_rows: 0,
            width: 0,
            wrap: false,
        }
    }

    /// The sheep this pane describes, fixed for its lifetime.
    #[must_use]
    pub fn sheep(&self) -> &RowKey {
        &self.sheep
    }

    /// The match axis's current text, or `None` when it is not set.
    ///
    /// A thin alias for [`Filters::match_text`], `#[cfg(test)]` like
    /// [`super::app::App::bleats_pane_mut_for_tests`]: nothing in production
    /// reads a promoted pane's carried filter back out, only
    /// `crate::lookout::app`'s own
    /// `b_promotes_the_feed_to_full_screen_with_its_filters`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn match_filter(&self) -> Option<&str> {
        self.filters.match_text()
    }

    /// The filters currently stacked on this pane's feed.
    ///
    /// Read by [`super::view::bleats_full::draw`] to draw the filter row's
    /// chips.
    #[must_use]
    pub fn filters(&self) -> &Filters {
        &self.filters
    }

    /// Restricts the feed to one stream, or both when `stream` is `None`.
    pub fn set_stream(&mut self, stream: Option<Stream>) {
        self.filters.note_axis(Axis::Stream, stream.is_some());
        self.filters.stream = stream;
    }

    /// Sets the minimum level a line must meet to show, or clears the floor
    /// when `level` is `None`.
    pub fn set_min_level(&mut self, level: Option<Level>) {
        self.filters.note_axis(Axis::Level, level.is_some());
        self.filters.min_level = level;
    }

    /// Sets the text a line's body must match to show: a plain substring, or
    /// a `/…/`-delimited regex (see [`MatchKind`]).
    ///
    /// An empty string clears the axis rather than matching every line:
    /// there is no chip an operator can point at for "match nothing
    /// typed yet", so an empty search box behaves as if the axis were
    /// never set.
    pub fn set_match(&mut self, text: String) {
        self.filters.set_match(text);
    }

    /// The lines from `lines` that survive every filter axis currently set.
    ///
    /// Read by [`super::view::bleats_full::draw`], both to draw only the
    /// lines that pass and to count how many did.
    ///
    /// `levels` is how this sheep's lines are read, built by
    /// [`super::app::App::feed_classifier`] from the row the feed came from.
    #[must_use]
    pub fn visible<'a>(&self, lines: &'a [TailLine], levels: &Classifier) -> Vec<&'a TailLine> {
        lines
            .iter()
            .filter(|line| self.filters.keeps(line, levels))
            .collect()
    }

    /// How many surviving lines back from the tail the view is scrolled.
    /// `0` is the newest surviving line. See the field's own doc for what
    /// this counts and why a stored value is never trusted on its own.
    #[must_use]
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Whether the view is pinned to the newest surviving line.
    #[must_use]
    pub fn following(&self) -> bool {
        self.following
    }

    /// The body rows last recorded through [`Self::set_rows`], for
    /// [`super::view::bleats_full::page_amount_up`] to size a page by.
    #[must_use]
    pub(crate) fn body_rows(&self) -> usize {
        self.body_rows
    }

    /// Records the body rows available, for [`super::view::bleats_full::page_amount_up`],
    /// which [`super::app::App::on_bleats_key`] reads before every
    /// `ctrl-u`/`ctrl-d`. Called from [`super::app::App::note_body_rows`]
    /// before every draw, the same way the config pane and settings screen
    /// already are.
    pub fn set_rows(&mut self, rows: usize) {
        self.body_rows = rows;
    }

    /// The area's own column width last recorded through [`Self::set_width`],
    /// or `0` before the first draw.
    #[must_use]
    pub(crate) fn width(&self) -> u16 {
        self.width
    }

    /// Records the area's column width, for [`super::view::bleats_full::page_amount_up`]
    /// to measure a wrapped line's row cost against. Called from
    /// [`super::app::App::note_body_width`] before every draw.
    pub fn set_width(&mut self, width: u16) {
        self.width = width;
    }

    /// Whether a long line wraps onto extra rows instead of truncating.
    #[must_use]
    pub fn wrapped(&self) -> bool {
        self.wrap
    }

    /// `w`: toggles wrapping.
    pub fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
    }

    /// Scrolls toward older lines by `amount`, and stops following the
    /// tail: any backward movement means the operator has taken over, or
    /// the next refresh would undo the keypress.
    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
        self.following = false;
    }

    /// Holds the offset at `ceiling`, the last value that changes the frame.
    ///
    /// Called by the reducer straight after a backward scroll, because the
    /// ceiling depends on the surviving lines and their wrapped heights and
    /// this type sees neither.
    pub fn clamp_scroll(&mut self, ceiling: usize) {
        self.scroll_offset = self.scroll_offset.min(ceiling);
    }

    /// Scrolls toward the newest line by `amount`. Does not restore
    /// following on its own, even if it lands back on the tail: only
    /// [`Self::jump_to_end`] and turning [`Self::toggle_follow`] on do that,
    /// because an operator who has taken over decides when to hand the view
    /// back, not the arithmetic.
    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    /// `ctrl-d`: the same, toward the newest line.
    pub fn page_down(&mut self, amount: usize) {
        self.scroll_down(amount.max(1));
    }

    /// `G`: jumps to the newest surviving line and resumes following, since
    /// that is what an operator means by "go to the end".
    pub fn jump_to_end(&mut self) {
        self.scroll_offset = 0;
        self.following = true;
    }

    /// `f`: toggles following explicitly. Turning it on also jumps to the
    /// tail, the same way [`Self::jump_to_end`] does, since "following"
    /// means pinned to the newest line, not pinned wherever the view
    /// happened to be.
    pub fn toggle_follow(&mut self) {
        self.following = !self.following;
        if self.following {
            self.scroll_offset = 0;
        }
    }

    /// `n`: steps one line toward the newest matching line. A no-op with no
    /// match axis set, rather than quietly becoming a line-movement key.
    ///
    /// Runs the matcher no second time: once the axis is set,
    /// [`Filters::keeps`] already dropped every non-matching line out of
    /// [`Self::visible`]. Clears the follow flag whenever it steps, and the
    /// no-axis guard clears nothing.
    pub fn match_next(&mut self) {
        if self.filters.match_text().is_none() {
            return;
        }
        self.scroll_down(1);
        self.following = false;
    }

    /// Drops the most recently set filter axis, returning whether one was
    /// there to drop.
    ///
    /// This is what `Escape` does while a chip is showing: backing out is
    /// one axis at a time, oldest survives longest. `false` once every
    /// axis is clear tells the caller there is nothing left to drop, which
    /// is the reducer's signal to close the pane instead.
    pub fn drop_newest_chip(&mut self) -> bool {
        let Some(axis) = self.filters.order.pop() else {
            return false;
        };
        match axis {
            Axis::Stream => self.filters.stream = None,
            Axis::Level => self.filters.min_level = None,
            Axis::Match => self.filters.clear_match(),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` steps toward the newest matching line only while the match axis
    /// is set. With it clear there is no "matching line" to step between, so
    /// the key must not quietly become a line-movement key.
    ///
    /// The guard reads the axis, so it is the match-axis state machine's
    /// last untested door: nothing else exercised the `None` arm.
    #[test]
    fn n_steps_only_while_the_match_axis_is_set() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));

        pane.match_next();
        assert!(pane.following(), "no axis set, so `n` spent no follow flag");
        assert_eq!(pane.scroll_offset(), 0, "and moved nothing");

        pane.scroll_up(3);
        pane.set_match("/po+l/".to_string());
        pane.match_next();
        assert_eq!(pane.scroll_offset(), 2, "the axis is set, so `n` stepped");
        assert!(!pane.following(), "a deliberate jump clears follow");
    }

    /// The axis dropping mid-session closes that door again, rather than
    /// leaving `n` live off a matcher that is gone.
    #[test]
    fn n_stops_stepping_once_the_match_chip_is_dropped() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("/po+l/".to_string());
        pane.scroll_up(3);
        assert!(pane.drop_newest_chip());

        pane.match_next();
        assert_eq!(
            pane.scroll_offset(),
            3,
            "the axis is gone, so `n` did nothing"
        );
    }
}
