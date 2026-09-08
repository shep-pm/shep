//! The full-screen bleats pane's own state: which sheep it is pinned to, and
//! the three filters an operator can stack over the feed it reads.

use super::app::RowKey;
use super::level::{Level, level_of};
use super::tail::{Stream, TailLine};

/// Which filter axis was most recently turned on.
///
/// Kept as its own small enum, in a `Vec` on [`Filters`], rather than
/// deriving "newest" from the three fields directly: nothing about a
/// `Stream`, a `Level` or a `String` says when it was set, so
/// [`BleatsPane::drop_newest_chip`] needs an explicit order to pop from.
///
/// No non-test caller for the setters that construct a variant yet:
/// `#[allow(dead_code)]` on them says so rather than inventing one. Task 4
/// wires the filter row's keys into [`BleatsPane::set_stream`],
/// [`BleatsPane::set_min_level`] and [`BleatsPane::set_match`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Axis {
    /// The stream axis: `out`, `err`, or both.
    Stream,
    /// The minimum-level axis.
    Level,
    /// The text-match axis.
    Match,
}

/// The bleats pane's three filter axes, composed with AND.
///
/// `Debug` is derived. A stream tag, a level and an operator-typed search
/// string carry no env, no path and no argument vector.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    /// Keep only lines from this stream, or both when `None`.
    pub stream: Option<Stream>,
    /// Keep a line whose level meets this floor, or every line when `None`.
    ///
    /// A line with no detectable level always passes regardless of this
    /// field: see [`Filters::keeps`].
    pub min_level: Option<Level>,
    /// Keep only lines whose text contains this substring.
    pub matcher: Option<String>,
    /// The axes currently set, oldest first, so the newest is the last
    /// element.
    order: Vec<Axis>,
}

impl Filters {
    /// Records that `axis` just turned on or off, keeping [`Self::order`] in
    /// sync without letting an axis appear in it twice.
    ///
    /// No non-test caller yet: `#[allow(dead_code)]` says so rather than
    /// inventing one. Task 4 wires the filter row's keys into the setters
    /// that call this.
    #[allow(dead_code)]
    fn note_axis(&mut self, axis: Axis, now_set: bool) {
        let already_set = self.order.contains(&axis);
        if now_set && !already_set {
            self.order.push(axis);
        } else if !now_set && already_set {
            self.order.retain(|set| *set != axis);
        }
    }

    /// Whether `line` survives every axis currently set.
    ///
    /// Each axis short-circuits the line out the moment it fails; an axis
    /// left `None` holds automatically, which is how the three compose
    /// with AND rather than needing a combinator.
    ///
    /// No non-test caller yet: `#[allow(dead_code)]` says so rather than
    /// inventing one. Task 5 reaches this through [`BleatsPane::visible`]
    /// on every poll.
    #[allow(dead_code)]
    fn keeps(&self, line: &TailLine) -> bool {
        if let Some(stream) = self.stream
            && line.stream != stream
        {
            return false;
        }
        if let Some(min) = self.min_level {
            // A line with no detectable level always passes: `level_of`
            // returning `None` means "unclassifiable", the ordinary case
            // for plain app output, not "below the minimum". Treating it
            // as a miss would make a bare `println!` line vanish the
            // moment an operator set any floor at all, which is exactly
            // the line the pane was opened to find.
            if let Some(level) = level_of(&line.text)
                && level < min
            {
                return false;
            }
        }
        if let Some(text) = &self.matcher
            && !line.text.contains(text.as_str())
        {
            return false;
        }
        true
    }
}

/// The full-screen bleats pane's own state.
///
/// Holds the sheep it opened on rather than reading the dashboard's
/// selection: full screen leaves no table on which to change one, so the
/// pane describes a single sheep for as long as it is open. Also holds the
/// filters an operator has stacked on top of that sheep's feed.
///
/// `Debug` is derived. A row key, a filter set and a cursor position carry
/// no env, no path and no argument vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleatsPane {
    sheep: RowKey,
    filters: Filters,
}

impl BleatsPane {
    /// Opens the pane on one sheep, with no filters set.
    #[must_use]
    pub fn new(sheep: RowKey) -> Self {
        Self {
            sheep,
            filters: Filters::default(),
        }
    }

    /// The sheep this pane describes, fixed for its lifetime.
    #[must_use]
    pub fn sheep(&self) -> &RowKey {
        &self.sheep
    }

    /// The filters currently stacked on this pane's feed.
    ///
    /// No non-test caller yet: `#[allow(dead_code)]` says so rather than
    /// inventing one. Task 4 reads this to draw the filter row's chips.
    #[must_use]
    #[allow(dead_code)]
    pub fn filters(&self) -> &Filters {
        &self.filters
    }

    /// Restricts the feed to one stream, or both when `stream` is `None`.
    ///
    /// No non-test caller yet: `#[allow(dead_code)]` says so rather than
    /// inventing one. Task 4 calls this from the filter row's stream key.
    #[allow(dead_code)]
    pub fn set_stream(&mut self, stream: Option<Stream>) {
        self.filters.note_axis(Axis::Stream, stream.is_some());
        self.filters.stream = stream;
    }

    /// Sets the minimum level a line must meet to show, or clears the floor
    /// when `level` is `None`.
    ///
    /// No non-test caller yet: `#[allow(dead_code)]` says so rather than
    /// inventing one. Task 4 calls this from the filter row's level key.
    #[allow(dead_code)]
    pub fn set_min_level(&mut self, level: Option<Level>) {
        self.filters.note_axis(Axis::Level, level.is_some());
        self.filters.min_level = level;
    }

    /// Sets the text a line's body must contain to show.
    ///
    /// An empty string clears the axis rather than matching every line:
    /// there is no chip an operator can point at for "match nothing
    /// typed yet", so an empty search box behaves as if the axis were
    /// never set.
    ///
    /// No non-test caller yet: `#[allow(dead_code)]` says so rather than
    /// inventing one. Task 4 calls this from the filter row's match box.
    #[allow(dead_code)]
    pub fn set_match(&mut self, text: String) {
        let matcher = (!text.is_empty()).then_some(text);
        self.filters.note_axis(Axis::Match, matcher.is_some());
        self.filters.matcher = matcher;
    }

    /// The lines from `lines` that survive every filter axis currently set.
    ///
    /// No non-test caller yet: `#[allow(dead_code)]` says so rather than
    /// inventing one. Task 5 calls this on every poll to draw the feed.
    #[must_use]
    #[allow(dead_code)]
    pub fn visible<'a>(&self, lines: &'a [TailLine]) -> Vec<&'a TailLine> {
        lines
            .iter()
            .filter(|line| self.filters.keeps(line))
            .collect()
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
            Axis::Match => self.filters.matcher = None,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(stream: Stream, text: &str) -> TailLine {
        TailLine {
            stream,
            text: text.to_string(),
        }
    }

    /// The decision most likely to be quietly reversed. An app printing bare
    /// text must not vanish because somebody asked for warnings.
    #[test]
    fn a_line_with_no_level_survives_a_minimum() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_min_level(Some(Level::Warn));
        let lines = vec![
            line(Stream::Out, "listening on 8080"),
            line(Stream::Out, "INFO routine chatter"),
            line(Stream::Out, "ERROR pool exhausted"),
        ];
        let kept: Vec<&str> = pane
            .visible(&lines)
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(kept, vec!["listening on 8080", "ERROR pool exhausted"]);
    }

    #[test]
    fn the_three_axes_compose_with_and() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_stream(Some(Stream::Err));
        pane.set_min_level(Some(Level::Warn));
        pane.set_match("pool".to_string());
        let lines = vec![
            line(Stream::Err, "ERROR pool exhausted"), // all three hold
            line(Stream::Out, "ERROR pool exhausted"), // wrong stream
            line(Stream::Err, "INFO pool warming"),    // below the minimum
            line(Stream::Err, "ERROR disk full"),      // no match
        ];
        assert_eq!(pane.visible(&lines).len(), 1);
    }

    /// esc removes the newest chip rather than clearing every filter, so
    /// backing out of a filter is one key at a time.
    #[test]
    fn esc_drops_the_newest_chip_then_reports_there_are_none_left() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_stream(Some(Stream::Err));
        pane.set_min_level(Some(Level::Warn));

        assert!(pane.drop_newest_chip(), "the level chip was newest");
        assert!(pane.filters().min_level.is_none());
        assert!(pane.filters().stream.is_some(), "the older chip stays");

        assert!(pane.drop_newest_chip(), "the stream chip goes next");
        assert!(!pane.drop_newest_chip(), "nothing left to drop");
    }
}
