//! The bleats pane's three filter axes, composed with AND, and the match
//! axis's typed text together with what it compiled to.

use core::fmt;

use regex::Regex;

use super::super::level::{Classifier, Level};
use super::super::tail::{Stream, TailLine};

/// Which filter axis was most recently turned on.
///
/// Kept as its own small enum, in a `Vec` on [`Filters`], rather than
/// deriving "newest" from the three fields directly: nothing about a
/// `Stream`, a `Level` or a `String` says when it was set, so
/// [`super::BleatsPane::drop_newest_chip`] needs an explicit order to pop from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Axis {
    /// The stream axis: `out`, `err`, or both.
    Stream,
    /// The minimum-level axis.
    Level,
    /// The text-match axis.
    Match,
}

/// What the match axis's typed text resolves to.
///
/// `/pattern/` (a leading and trailing slash, the same delimiter grep, sed
/// and this pane's own `filters` chip agree to read) compiles as a regex;
/// anything else is a plain substring. The delimiter is explicit because
/// almost any short string a person types is already valid regex syntax,
/// with a different meaning than its literal reading.
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

impl MatchKind {
    /// What a filter chip appends after the matcher's own text.
    ///
    /// Shared by both panes that draw a chip, so the regex states are
    /// worded in one place. Only the suffix is shared: `view::sheep`'s
    /// embedded feed words the stream and level axes shorter than
    /// `view::bleats_full` does, on purpose.
    #[must_use]
    pub const fn chip_suffix(self) -> &'static str {
        match self {
            Self::Literal => "",
            Self::Regex => " (regex)",
            Self::Invalid => " (invalid regex, matches nothing)",
        }
    }
}

/// The match axis's typed text, together with what it compiled to.
///
/// One value rather than a string beside a cached regex, because the two
/// have to agree and a single constructor is what makes disagreeing
/// impossible. [`Filters::set_match`] is the only way to build one.
///
/// Compiling at set time is the point: [`Filters::match_ranges`] runs per line.
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
    /// Compiles `text` per [`MatchKind`]'s rule, or `None` for empty text,
    /// which is an unset axis rather than a matcher hitting every line.
    ///
    /// The refusal lives here rather than in [`Filters::set_match`] because
    /// [`Self::is_match`] and [`Self::ranges`] disagree on an empty needle:
    /// `str::contains` answers true for one, `str::match_indices` hits at
    /// every byte boundary.
    fn new(text: String) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        #[cfg(test)]
        COMPILES.with(|count| count.set(count.get() + 1));
        let compiled = match delimited_regex(&text) {
            Some(pattern) => Regex::new(pattern).map_or(Compiled::Invalid, Compiled::Regex),
            None => Compiled::Literal,
        };
        Some(Self { text, compiled })
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
    /// highlighting. Empty for [`Compiled::Invalid`], which matches nothing.
    fn ranges(&self, haystack: &str) -> Vec<(usize, usize)> {
        match &self.compiled {
            Compiled::Literal => haystack
                .match_indices(self.text.as_str())
                .map(|(start, matched)| (start, start + matched.len()))
                .collect(),
            Compiled::Invalid => Vec::new(),
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
    /// [`Self::set_match`] and [`Self::clear_match`] are the only writers.
    matcher: Option<Matcher>,
    /// The axes currently set, oldest first, so the newest is the last
    /// element.
    pub(super) order: Vec<Axis>,
}

impl Filters {
    /// Records that `axis` just turned on or off, keeping [`Self::order`] in
    /// sync without letting an axis appear in it twice.
    pub(super) fn note_axis(&mut self, axis: Axis, now_set: bool) {
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
    /// see [`super::BleatsPane::set_match`] for why.
    pub(super) fn set_match(&mut self, text: String) {
        let matcher = Matcher::new(text);
        self.note_axis(Axis::Match, matcher.is_some());
        self.matcher = matcher;
    }

    /// Clears the match axis, leaving [`Self::order`] to the caller.
    ///
    /// [`super::BleatsPane::drop_newest_chip`] has already popped the axis out of
    /// the order it is walking, so this must not touch it again.
    pub(super) fn clear_match(&mut self) {
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
    /// Each axis short-circuits the line out the moment it fails; an axis left
    /// `None` holds automatically, which is how the three compose with AND
    /// rather than needing a combinator.
    ///
    /// `levels` is built once by the caller ([`super::BleatsPane::visible`]).
    pub(super) fn keeps(&self, line: &TailLine, levels: &Classifier) -> bool {
        if let Some(stream) = self.stream
            && line.stream != stream
        {
            return false;
        }
        if let Some(min) = self.min_level {
            // A line with no detectable level always passes: `None` means
            // "unclassifiable", not "below the minimum". A floor would
            // otherwise hide a bare `println!` line, the very line the pane
            // was opened to find.
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

#[cfg(test)]
mod tests {
    use super::super::{BleatsPane, RowKey};
    use super::*;

    fn line(stream: Stream, text: &str) -> TailLine {
        TailLine::new(stream, text)
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

    /// The three suffixes, exact. This pins the wording itself; each
    /// pane's own test pins that its call site adds nothing to it.
    #[test]
    fn only_the_two_regex_states_carry_a_chip_suffix() {
        assert_eq!(MatchKind::Literal.chip_suffix(), "");
        assert_eq!(MatchKind::Regex.chip_suffix(), " (regex)");
        assert_eq!(
            MatchKind::Invalid.chip_suffix(),
            " (invalid regex, matches nothing)"
        );
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

    /// Whitespace is a needle like any other: only a genuinely empty string
    /// clears the axis. Worth pinning because the chip for it renders as a
    /// near-blank `match   `, which reads like the empty-clears rule having
    /// misfired rather than like a filter doing what it was asked.
    #[test]
    fn whitespace_only_match_text_is_a_needle_not_a_cleared_axis() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));
        pane.set_match("  ".to_string());

        assert_eq!(pane.filters().match_kind(), Some(MatchKind::Literal));
        assert_eq!(pane.filters().match_text(), Some("  "));
        assert!(!pane.filters().is_empty(), "the axis is set, not cleared");

        let lines = vec![
            line(Stream::Out, "pool  exhausted"),
            line(Stream::Out, "kennel"),
        ];
        let kept: Vec<&str> = pane
            .visible(&lines, &Classifier::new(&[]))
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(kept, vec!["pool  exhausted"]);
    }

    /// `Matcher` hand-writes `Debug`, and that impl is half of why holding a
    /// compiled regex on `Filters` was possible at all: a derived one prints
    /// the compiled half as `Regex(Regex("po+l"))`.
    ///
    /// Exact-string, because nothing else pins it. `pane_sheep`'s own
    /// `Debug` test only ever sees `matcher: None`, so a later derive here
    /// would change this output and fail nothing.
    #[test]
    fn a_matcher_debug_names_its_kind_and_prints_its_text_once() {
        let mut pane = BleatsPane::new(RowKey::Sheep(9));

        for (text, expected) in [
            ("/po+l/", r#"Matcher { text: "/po+l/", kind: Regex }"#),
            ("pool", r#"Matcher { text: "pool", kind: Literal }"#),
            ("/pool(/", r#"Matcher { text: "/pool(/", kind: Invalid }"#),
        ] {
            pane.set_match(text.to_string());
            let matcher = pane
                .filters()
                .matcher
                .as_ref()
                .expect("set_match set the axis");
            assert_eq!(format!("{matcher:?}"), expected);
        }
    }

    /// The other half: `PartialEq` reads the typed text, since `regex::Regex`
    /// implements neither `PartialEq` nor `Eq` and the compiled form is a
    /// pure function of the text anyway.
    ///
    /// Worth pinning because an impl answering `true` unconditionally would
    /// also compile, and would make every `Filters` comparison in the suite
    /// pass while comparing nothing.
    #[test]
    fn filter_sets_compare_by_their_match_text() {
        let mut one = BleatsPane::new(RowKey::Sheep(9));
        let mut two = BleatsPane::new(RowKey::Sheep(9));
        one.set_match("/po+l/".to_string());
        two.set_match("/po+l/".to_string());
        assert_eq!(one.filters(), two.filters(), "same text, same axis");

        two.set_match("/po+ls/".to_string());
        assert_ne!(
            one.filters(),
            two.filters(),
            "different text, different axis"
        );
    }
}
