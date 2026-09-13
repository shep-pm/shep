//! The full-screen bleats pane's own state: which sheep it is pinned to, and
//! the three filters an operator can stack over the feed it reads.

use core::fmt;

use regex::Regex;

use super::app::RowKey;
use super::level::{Classifier, Level};
use super::tail::{Stream, TailLine};

/// Which filter axis was most recently turned on.
///
/// Kept as its own small enum, in a `Vec` on [`Filters`], rather than
/// deriving "newest" from the three fields directly: nothing about a
/// `Stream`, a `Level` or a `String` says when it was set, so
/// [`BleatsPane::drop_newest_chip`] needs an explicit order to pop from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    /// The stream axis: `out`, `err`, or both.
    Stream,
    /// The minimum-level axis.
    Level,
    /// The text-match axis.
    Match,
}

/// What the match axis's typed text resolves to.
///
/// `/pattern/` (a leading and trailing slash, the same delimiter grep,
/// sed and this pane's own `filters` chip agree to read) compiles as a
/// regex; anything else is a plain substring. This is the deliberate
/// choice for distinguishing the two, since the spec says only "text or
/// regex" and not how an operator picks: trying to compile *every* typed
/// string as a regex and falling back to literal on a parse error was
/// rejected, because almost any short string a person types (`get
/// index.html`, `(unset)`) is *already* valid regex syntax with a
/// different meaning than its literal reading — `.` and `(` would
/// silently stop meaning themselves. An explicit delimiter costs two
/// characters and never reinterprets a search an operator meant literally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// Plain substring search: [`Filters::match_text`], verbatim.
    Literal,
    /// `/…/`-delimited text that compiled.
    Regex,
    /// `/…/`-delimited text that did not compile. Matches no line rather
    /// than every line or panicking; the filter row names this state so an
    /// operator sees a typo rather than a feed that has gone silent for no
    /// visible reason.
    Invalid,
}

/// The match axis's typed text, together with what it compiled to.
///
/// One value rather than a string beside a cached regex, because the two
/// have to agree and a single constructor is what makes disagreeing
/// impossible. [`Filters::set_match`] is the only way to build one.
///
/// Compiling at set time rather than at match time is the point:
/// [`Filters::match_ranges`] runs once per rendered line, so a matcher
/// parsed on demand recompiled the same pattern once per line on every
/// frame. Measured over a full 80-line feed at 160x50, `/\w+@\w+\.\w+/`
/// cost 27.0 ms a redraw that way, against [`super::MIN_REDRAW`]'s 33 ms
/// frame budget. Compiled once at set time it is 24.8 us, within noise of
/// the 21.1 us a literal axis costs: a redraw compiles nothing at all.
#[derive(Clone)]
struct Matcher {
    /// The text exactly as typed, delimiters and all. Also the needle a
    /// [`Compiled::Literal`] search runs with.
    text: String,
    /// What [`Self::text`] compiled to.
    compiled: Compiled,
}

/// What a [`Matcher`]'s text compiled to.
///
/// [`Self::Literal`] carries nothing: its needle is [`Matcher::text`]
/// itself, and a second copy of it could only ever drift.
#[derive(Clone)]
enum Compiled {
    /// Plain substring search.
    Literal,
    /// A compiled `/…/`-delimited regex.
    Regex(Regex),
    /// A `/…/`-delimited pattern that failed to compile.
    Invalid,
}

#[cfg(test)]
thread_local! {
    /// How many matchers this thread has built, for the tests that pin a
    /// redraw compiling none.
    ///
    /// Thread-local, not a global counter: the test harness gives each test
    /// its own thread, and a global one would count every other test's
    /// matchers alongside the one under test.
    static COMPILES: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

/// How many matchers this thread has built so far. Tests assert on the
/// difference between two readings, never on the value itself.
#[cfg(test)]
pub(crate) fn compiles() -> usize {
    COMPILES.with(core::cell::Cell::get)
}

impl Matcher {
    /// Compiles `text` per [`MatchKind`]'s rule.
    fn new(text: String) -> Self {
        #[cfg(test)]
        COMPILES.with(|count| count.set(count.get() + 1));
        let compiled = match delimited_regex(&text) {
            Some(pattern) => Regex::new(pattern).map_or(Compiled::Invalid, Compiled::Regex),
            None => Compiled::Literal,
        };
        Self { text, compiled }
    }

    /// This matcher's [`MatchKind`].
    fn kind(&self) -> MatchKind {
        match self.compiled {
            Compiled::Literal => MatchKind::Literal,
            Compiled::Regex(_) => MatchKind::Regex,
            Compiled::Invalid => MatchKind::Invalid,
        }
    }

    /// Whether `haystack` matches at all.
    fn is_match(&self, haystack: &str) -> bool {
        match &self.compiled {
            Compiled::Literal => haystack.contains(self.text.as_str()),
            Compiled::Regex(re) => re.is_match(haystack),
            Compiled::Invalid => false,
        }
    }

    /// Every byte range in `haystack` this matcher hits, oldest first, for
    /// highlighting. Empty for [`Compiled::Invalid`], which matches nothing,
    /// and for an empty literal pattern, which `str::match_indices` would
    /// otherwise report at every byte boundary.
    fn ranges(&self, haystack: &str) -> Vec<(usize, usize)> {
        match &self.compiled {
            Compiled::Literal if !self.text.is_empty() => haystack
                .match_indices(self.text.as_str())
                .map(|(start, matched)| (start, start + matched.len()))
                .collect(),
            Compiled::Literal | Compiled::Invalid => Vec::new(),
            Compiled::Regex(re) => re
                .find_iter(haystack)
                .map(|m| (m.start(), m.end()))
                .collect(),
        }
    }
}

/// Names the typed text and what it compiled to, never the compiled regex:
/// `regex::Regex` implements `Debug` by reprinting its own pattern, which
/// would print an operator's search text twice.
impl fmt::Debug for Matcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Matcher")
            .field("text", &self.text)
            .field("kind", &self.kind())
            .finish()
    }
}

/// The typed text alone. [`Matcher::compiled`] is a pure function of it, and
/// `regex::Regex` implements neither `PartialEq` nor `Eq`.
impl PartialEq for Matcher {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for Matcher {}

/// `text` if it is `/`-delimited on both ends with at least one byte between
/// them, stripped of both delimiters; `None` otherwise.
fn delimited_regex(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('/')?.strip_suffix('/')?;
    (!inner.is_empty()).then_some(inner)
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
    /// Keep only lines this matcher hits: a plain substring, or a
    /// `/…/`-delimited regex. See [`MatchKind`] for exactly how the two are
    /// told apart.
    ///
    /// Private, unlike the two axes above, because it carries a compiled
    /// regex that has to stay in step with the text it came from:
    /// [`Self::set_match`] and [`Self::clear_match`] are the only writers,
    /// and [`Self::match_text`], [`Self::match_kind`] and
    /// [`Self::match_ranges`] read it back.
    matcher: Option<Matcher>,
    /// The axes currently set, oldest first, so the newest is the last
    /// element.
    order: Vec<Axis>,
}

impl Filters {
    /// Records that `axis` just turned on or off, keeping [`Self::order`] in
    /// sync without letting an axis appear in it twice.
    fn note_axis(&mut self, axis: Axis, now_set: bool) {
        let already_set = self.order.contains(&axis);
        if now_set && !already_set {
            self.order.push(axis);
        } else if !now_set && already_set {
            self.order.retain(|set| *set != axis);
        }
    }

    /// Whether no axis is currently set.
    ///
    /// The filter row renders only when this is `false`: with nothing set,
    /// a row stating "all three must hold" and a survivor count equal to
    /// the total would say nothing an operator does not already see.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stream.is_none() && self.min_level.is_none() && self.matcher.is_none()
    }

    /// Sets the match axis to `text`, compiling it once, here, rather than
    /// once per line the pane later draws. An empty `text` clears the axis;
    /// see [`BleatsPane::set_match`] for why.
    fn set_match(&mut self, text: String) {
        let matcher = (!text.is_empty()).then(|| Matcher::new(text));
        self.note_axis(Axis::Match, matcher.is_some());
        self.matcher = matcher;
    }

    /// Clears the match axis, leaving [`Self::order`] to the caller.
    ///
    /// [`BleatsPane::drop_newest_chip`] has already popped the axis out of
    /// the order it is walking, so this must not touch it again.
    fn clear_match(&mut self) {
        self.matcher = None;
    }

    /// The match axis's text exactly as typed, delimiters and all, or `None`
    /// when the axis is not set.
    #[must_use]
    pub fn match_text(&self) -> Option<&str> {
        self.matcher.as_ref().map(|matcher| matcher.text.as_str())
    }

    /// What the match axis's typed text resolved to, or `None` when the
    /// axis is not set.
    #[must_use]
    pub fn match_kind(&self) -> Option<MatchKind> {
        self.matcher.as_ref().map(Matcher::kind)
    }

    /// Every byte range in `haystack` the match axis hits, for highlighting
    /// a rendered line. Empty when the axis is not set, and empty (never a
    /// panic) for an invalid regex.
    #[must_use]
    pub fn match_ranges(&self, haystack: &str) -> Vec<(usize, usize)> {
        self.matcher
            .as_ref()
            .map_or_else(Vec::new, |matcher| matcher.ranges(haystack))
    }

    /// Whether `line` survives every axis currently set.
    ///
    /// Each axis short-circuits the line out the moment it fails; an axis
    /// left `None` holds automatically, which is how the three compose
    /// with AND rather than needing a combinator. `levels` is this sheep's
    /// own reading of a level, built once by the caller ([`Self::visible`])
    /// rather than per line; the match axis needs no such hoist, holding a
    /// matcher already compiled.
    fn keeps(&self, line: &TailLine, levels: &Classifier) -> bool {
        if let Some(stream) = self.stream
            && line.stream != stream
        {
            return false;
        }
        if let Some(min) = self.min_level {
            // A line with no detectable level always passes: `None` means
            // "unclassifiable", the ordinary case for plain app output, not
            // "below the minimum". Treating it as a miss would make a bare
            // `println!` line vanish the moment an operator set any floor at
            // all, which is exactly the line the pane was opened to find.
            // True of an app's declared rules as much as of shep's own
            // reading: declaring rules narrows what gets a level, never what
            // gets shown.
            if let Some(level) = levels.level_of(&line.text)
                && level < min
            {
                return false;
            }
        }
        if let Some(matcher) = &self.matcher
            && !matcher.is_match(&line.text)
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
/// filters an operator has stacked on top of that sheep's feed, and, while
/// the match box is open, what the match axis held before the edit began.
///
/// `Debug` is derived. A row key, a filter set and a snapshot of typed
/// search text carry no env, no path and no argument vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleatsPane {
    sheep: RowKey,
    filters: Filters,
    /// `Some` while the match box owns `InputMode::Text`; the match axis's
    /// text as it stood before this edit started, so [`Self::abandon_match_edit`]
    /// can restore it. `None` the rest of the time.
    match_snapshot: Option<Option<String>>,
    /// How many surviving lines back from the newest one the view is
    /// scrolled, counting **filtered** lines (what [`Self::visible`]
    /// returns), not raw ones. `0` is the newest surviving line, the tail;
    /// it grows as the operator scrolls toward older lines. A stored value
    /// is never trusted past a filter change: [`super::view::bleats_full`]
    /// recomputes the window from this and the current survivor count on
    /// every draw, so a narrowing filter cannot leave it pointing past the
    /// end.
    scroll_offset: usize,
    /// Whether the view stays pinned to the newest surviving line as new
    /// ones arrive. `true` on open: an operator who has not scrolled wants
    /// the live tail. Any backward movement clears it;
    /// [`Self::jump_to_end`] and [`Self::toggle_follow`] (turning it on) both
    /// restore it and reset [`Self::scroll_offset`] to `0`.
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
    /// A thin alias for `filters().matcher.as_deref()`, `#[cfg(test)]` like
    /// [`super::app::App::bleats_pane_mut_for_tests`]: nothing in production
    /// reads a promoted pane's carried filter back out once `b` has set it,
    /// only `crate::lookout::app`'s own
    /// `b_promotes_the_feed_to_full_screen_with_its_filters`, which asserts
    /// that promotion carried it rather than dropping it.
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

    /// Opens the match box, remembering the match axis's current text so
    /// [`Self::abandon_match_edit`] can restore it.
    pub fn begin_match_edit(&mut self) {
        self.match_snapshot = Some(self.filters.match_text().map(str::to_string));
    }

    /// The match box's live buffer while it is open, or `None` when it is
    /// not.
    ///
    /// The status bar needs both facts and the buffer alone cannot carry
    /// them: an empty box and a closed box both read as an empty matcher.
    /// `Some("")` is a box open over nothing typed yet.
    #[must_use]
    pub fn match_editing(&self) -> Option<&str> {
        self.match_snapshot
            .as_ref()
            .map(|_| self.filters.match_text().unwrap_or(""))
    }

    /// Ends the match box, keeping whatever [`Self::set_match`] already
    /// applied on the way in: `TextChar` and `TextBackspace` narrow the
    /// axis live, so there is nothing left for this to write.
    pub fn commit_match_edit(&mut self) {
        self.match_snapshot = None;
    }

    /// Restores the match axis to what [`Self::begin_match_edit`] saw,
    /// discarding whatever was typed since. A no-op if the match box was
    /// never opened.
    pub fn abandon_match_edit(&mut self) {
        if let Some(previous) = self.match_snapshot.take() {
            self.set_match(previous.unwrap_or_default());
        }
    }

    /// The lines from `lines` that survive every filter axis currently set.
    ///
    /// Read by [`super::view::bleats_full::draw`], both to draw only the
    /// lines that pass and to count how many did, out of how many were in
    /// the window.
    ///
    /// `levels` is how this sheep's lines are read, which is the sheep's own
    /// business rather than the pane's: [`super::app::App::feed_classifier`]
    /// builds it from the row the feed came from.
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
    /// match axis set, rather than quietly becoming a line-movement key:
    /// with the axis off there is no "matching line" to step between at
    /// all.
    ///
    /// Runs the matcher no second time: once the axis is set,
    /// [`Filters::keeps`] already dropped every non-matching line out of
    /// [`Self::visible`], so every surviving line *is* a match, and
    /// stepping between matches is stepping between the lines already on
    /// screen. Clears the follow flag either way, like any other backward
    /// movement — `n` moves toward the newest line, not away from it, but
    /// the design still calls it a deliberate jump the next refresh must
    /// not undo.
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
            .visible(&lines, &Classifier::new(&[]))
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
        assert_eq!(pane.visible(&lines, &Classifier::new(&[])).len(), 1);
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

    /// `note_axis`'s "unset an axis already in `order`, and not the newest
    /// one" branch. Without the `retain`, `order` would still list `Stream`
    /// after this, and the second `drop_newest_chip` below would find it
    /// there and report `true` instead of `false`.
    #[test]
    fn clearing_an_older_axis_directly_removes_it_from_the_drop_order() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_stream(Some(Stream::Err));
        pane.set_min_level(Some(Level::Warn));
        pane.set_stream(None);

        assert!(pane.drop_newest_chip(), "level is the only chip left");
        assert!(pane.filters().min_level.is_none());
        assert!(
            !pane.drop_newest_chip(),
            "stream was cleared directly, not through drop_newest_chip, and \
             must not still be sitting in the order"
        );
    }

    /// `note_axis`'s "re-set an axis already set" branch: it must not move
    /// to the back of `order`. Without the `already_set` guard, `order`
    /// would gain a second `Stream` entry and the first
    /// `drop_newest_chip` below would pop that instead of `Level`.
    #[test]
    fn re_setting_an_already_set_axis_does_not_reorder_it() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_stream(Some(Stream::Err));
        pane.set_min_level(Some(Level::Warn));
        pane.set_stream(Some(Stream::Out));

        assert!(pane.drop_newest_chip(), "level is still the newest chip");
        assert!(pane.filters().min_level.is_none());
        assert!(
            pane.filters().stream.is_some(),
            "the re-set stream chip is still the older one"
        );
    }

    /// `/…/` compiles as a regex rather than a literal search for those two
    /// characters.
    #[test]
    fn a_slash_delimited_matcher_is_read_as_a_regex() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("/poo+l/".to_string());
        let lines = vec![
            line(Stream::Out, "pool exhausted"),
            line(Stream::Out, "pooool exhausted"),
            line(Stream::Out, "pol exhausted"),
            line(Stream::Out, "kennel"),
        ];
        let kept: Vec<&str> = pane
            .visible(&lines, &Classifier::new(&[]))
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(kept, vec!["pool exhausted", "pooool exhausted"]);
    }

    /// A pattern that does not compile matches nothing rather than
    /// panicking or matching every line, and the axis says so through
    /// `match_kind` for the filter row to render.
    #[test]
    fn an_invalid_regex_matches_no_line_and_reports_itself_invalid() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("/pool(/".to_string());
        let lines = vec![line(Stream::Out, "pool exhausted")];
        assert!(pane.visible(&lines, &Classifier::new(&[])).is_empty());
        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Invalid));
    }

    /// A plain string with no delimiters stays a literal search, even one
    /// that would parse as a different regex if it were read as one.
    #[test]
    fn a_plain_matcher_is_read_literally_not_as_a_regex() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        // `.` would match any character as a regex; read literally it must
        // only match a real dot.
        pane.set_match("get index.html".to_string());
        let lines = vec![
            line(Stream::Out, "GET get index.html 200"),
            line(Stream::Out, "GET get indexXhtml 200"),
        ];
        let kept: Vec<&str> = pane
            .visible(&lines, &Classifier::new(&[]))
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(kept, vec!["GET get index.html 200"]);
        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Literal));
    }

    /// The ranges a regex matcher reports are what the filter row highlights;
    /// pinned here directly rather than only through a render assertion.
    #[test]
    fn match_ranges_reports_every_hit_a_regex_matcher_finds() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("/po+l/".to_string());
        let ranges = pane.filters().match_ranges("pool then pol then pooool");
        let hits: Vec<&str> = ranges
            .iter()
            .map(|&(start, end)| &"pool then pol then pooool"[start..end])
            .collect();
        assert_eq!(hits, vec!["pool", "pol", "pooool"]);
    }

    /// The axis compiles its text once, when it is set, and never again:
    /// every read afterwards runs off the matcher already there.
    ///
    /// This is the whole point of holding a compiled matcher rather than
    /// parsing per call. `match_ranges` runs once per rendered line, so a
    /// matcher parsed on demand recompiled the same pattern once per line on
    /// every frame.
    #[test]
    fn setting_the_axis_compiles_once_and_reading_it_back_compiles_never() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));

        let before = compiles();
        pane.set_match("/po+l/".to_string());
        assert_eq!(compiles() - before, 1, "one compile, at set time");

        let after_set = compiles();
        let lines: Vec<TailLine> = (0..80)
            .map(|i| line(Stream::Out, &format!("pool acquire {i}")))
            .collect();
        for feed_line in pane.visible(&lines, &Classifier::new(&[])) {
            let _ = pane.filters().match_ranges(&feed_line.text);
            let _ = pane.filters().match_kind();
            let _ = pane.filters().match_text();
        }
        assert_eq!(
            compiles(),
            after_set,
            "reading the axis back over a full feed compiles nothing"
        );
    }

    /// Replacing a regex with a literal must not leave the regex behind:
    /// the new text is what matches, and it matches literally.
    #[test]
    fn replacing_a_regex_axis_with_a_literal_drops_the_compiled_regex() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("/po+l/".to_string());
        pane.set_match("po+l".to_string());

        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Literal));
        let lines = vec![line(Stream::Out, "pool"), line(Stream::Out, "po+l")];
        let kept: Vec<&str> = pane
            .visible(&lines, &Classifier::new(&[]))
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(
            kept,
            vec!["po+l"],
            "the literal matches, the regex does not"
        );
    }

    /// The mirror: a literal replaced by a regex compiles the new text.
    #[test]
    fn replacing_a_literal_axis_with_a_regex_compiles_the_new_text() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("pool".to_string());
        pane.set_match("/po+l/".to_string());

        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Regex));
        let lines = vec![line(Stream::Out, "pol"), line(Stream::Out, "kennel")];
        assert_eq!(
            pane.visible(&lines, &Classifier::new(&[])).len(),
            1,
            "the regex matches `pol`"
        );
    }

    /// An empty string clears the axis outright, matcher and all, rather
    /// than leaving the previous one in place behind an unset chip.
    #[test]
    fn setting_the_axis_to_nothing_clears_the_matcher_it_held() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("/po+l/".to_string());
        pane.set_match(String::new());

        assert_eq!(pane.filters().match_kind(), None);
        assert_eq!(pane.filters().match_text(), None);
        assert!(pane.filters().match_ranges("pool").is_empty());
        assert!(pane.filters().is_empty(), "no axis is set");
    }

    /// Abandoning an edit restores what the axis held before it, compiled:
    /// the regex that was there has to work again, not merely have its text
    /// back.
    #[test]
    fn abandoning_an_edit_restores_the_earlier_matcher_compiled() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("/po+l/".to_string());

        pane.begin_match_edit();
        pane.set_match("kennel".to_string());
        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Literal));

        pane.abandon_match_edit();
        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Regex));
        assert_eq!(pane.filters().match_text(), Some("/po+l/"));
        assert_eq!(
            pane.filters().match_ranges("pol and pool"),
            vec![(0, 3), (8, 12)],
            "the restored regex matches, rather than being a dead string"
        );
    }

    /// The mirror, and the one a stale matcher survives: abandoning back to
    /// a literal must not leave the regex typed during the edit compiled and
    /// still matching.
    #[test]
    fn abandoning_an_edit_back_to_a_literal_drops_the_regex_typed_during_it() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("po+l".to_string());

        pane.begin_match_edit();
        pane.set_match("/po+l/".to_string());
        pane.abandon_match_edit();

        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Literal));
        assert!(
            pane.filters().match_ranges("pool").is_empty(),
            "the abandoned regex must not still be matching"
        );
        assert_eq!(pane.filters().match_ranges("po+l"), vec![(0, 4)]);
    }

    /// Abandoning an edit that began with no axis set clears it again,
    /// rather than keeping whatever was typed before the escape.
    #[test]
    fn abandoning_an_edit_that_began_unset_leaves_the_axis_unset() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.begin_match_edit();
        pane.set_match("/po+l/".to_string());
        pane.abandon_match_edit();

        assert_eq!(pane.filters().match_text(), None);
        assert!(pane.filters().is_empty());
    }

    /// Abandon with no edit open is a no-op: the axis keeps what it has.
    #[test]
    fn abandoning_with_no_edit_open_leaves_the_axis_alone() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("/po+l/".to_string());
        pane.abandon_match_edit();

        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Regex));
        assert_eq!(pane.filters().match_ranges("pool"), vec![(0, 4)]);
    }

    /// Committing keeps what was typed during the edit, compiled as its own
    /// text rather than as the text the edit started from.
    #[test]
    fn committing_an_edit_keeps_the_matcher_typed_during_it() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("pool".to_string());

        pane.begin_match_edit();
        pane.set_match("/po+l/".to_string());
        pane.commit_match_edit();

        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Regex));
        assert_eq!(pane.filters().match_ranges("pol"), vec![(0, 3)]);
        assert_eq!(pane.match_editing(), None, "the box is closed");
    }

    /// esc spending the match chip clears the matcher, not only the chip:
    /// a dropped axis highlights nothing.
    #[test]
    fn dropping_the_match_chip_clears_the_matcher_it_held() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("/po+l/".to_string());

        assert!(pane.drop_newest_chip(), "the match chip was newest");
        assert_eq!(pane.filters().match_kind(), None);
        assert!(pane.filters().match_ranges("pool").is_empty());
        assert!(!pane.drop_newest_chip(), "nothing left to drop");
    }
}
