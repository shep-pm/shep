/// A log level, ordered so a minimum can be compared against.
///
/// `Ord` is derived and the declaration order is the ordering: `Trace` is
/// the lowest and `Error` the highest, so `level >= minimum` reads the way
/// an operator setting `level >= warn` expects.
///
/// `Debug` is derived and is operator-facing rather than diagnostic: the
/// bleats filter row renders the chip through `format!("{min:?}")`
/// lowercased, so these variant names are the words on screen. Renaming one
/// changes what an operator reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// The lowest, and the only one an app has to opt into emitting.
    Trace,
    /// Below `Info`, and the usual floor for an app's own noise.
    Debug,
    /// The default an operator reads when nothing is filtered.
    Info,
    /// Something an operator should look at, without the sheep being broken.
    Warn,
    /// The highest. `fatal` parses to this too, since a line saying it is
    /// fatal is not saying something milder than one saying error.
    Error,
}

/// The keys a `key=value` pair may announce a level under.
///
/// A closed set. `level` is Go's `log/slog` text handler and most logfmt
/// writers, `lvl` is log15 and its descendants, `severity` the syslog
/// spelling.
const LEVEL_KEYS: [&str; 3] = ["level", "lvl", "severity"];

/// The level a line announces, or `None` when it announces none.
///
/// Scans the first 4 whitespace-separated words, since a timestamp before
/// the level is the common shape. A word counts as a bare level, or as a
/// `key=value` pair keyed by `LEVEL_KEYS`. Either way the level must be a
/// whole word once surrounding punctuation is stripped. A touching digit
/// blocks it, so `/error404` and `info2` announce nothing.
///
/// `None` is the ordinary answer for app output. Callers must not treat it
/// as "below the minimum": see the spec's decision 3.
#[must_use]
pub fn level_of(line: &str) -> Option<Level> {
    line.split_whitespace().take(4).find_map(|word| {
        match level_word(word)
            .trim_matches(|c: char| !c.is_ascii_alphabetic() && !c.is_ascii_digit())
            .to_ascii_lowercase()
            .as_str()
        {
            "trace" => Some(Level::Trace),
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" | "fatal" => Some(Level::Error),
            _ => None,
        }
    })
}

/// The part of `word` that may name a level: a pair's value when its key
/// is in `LEVEL_KEYS`, the whole word otherwise.
///
/// The key is matched whole, with no punctuation trimmed, so an access
/// log's `GET /search?level=error` is not read as a pair.
fn level_word(word: &str) -> &str {
    match word.split_once('=') {
        Some((key, value)) if LEVEL_KEYS.iter().any(|k| k.eq_ignore_ascii_case(key)) => value,
        _ => word,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_level_word_is_found_whatever_its_case() {
        assert_eq!(level_of("WARN pool exhausted"), Some(Level::Warn));
        assert_eq!(level_of("warn pool exhausted"), Some(Level::Warn));
        assert_eq!(level_of("Error: connection refused"), Some(Level::Error));
    }

    /// A timestamp before the level is the common shape, so the search looks
    /// past a prefix rather than only at the first word.
    #[test]
    fn a_level_after_a_timestamp_is_still_found() {
        assert_eq!(
            level_of("2026-09-08T11:02:03Z INFO listening on 8080"),
            Some(Level::Info)
        );
    }

    /// The whole reason the filter shows unclassifiable lines: most app
    /// output looks like this.
    #[test]
    fn a_line_with_no_level_word_has_no_level() {
        assert_eq!(level_of("listening on 8080"), None);
        assert_eq!(level_of(""), None);
    }

    /// A word that merely contains a level name is not a level. Without
    /// this, "information" and "errors:" both read as levels.
    #[test]
    fn a_level_name_inside_a_longer_word_is_not_a_level() {
        assert_eq!(level_of("information about the pool"), None);
        assert_eq!(level_of("warnings are disabled"), None);
    }

    /// The full extra-word variants of warn/error, not just the short
    /// forms. Dropping either from the match still passes every other
    /// test in this file, so each needs its own pin.
    ///
    /// Neither line may contain a second level word. `a fatal error
    /// occurred` pins nothing: drop the `fatal` arm and `find_map` reaches
    /// `error` on the next word for the same answer.
    #[test]
    fn the_long_forms_of_warn_and_error_are_recognised() {
        assert_eq!(level_of("a warning was logged"), Some(Level::Warn));
        assert_eq!(level_of("a fatal crash occurred"), Some(Level::Error));
    }

    /// The two arms nothing else reaches. Delete either and every other
    /// test in this file still passes.
    #[test]
    fn the_lowest_two_levels_are_recognised() {
        assert_eq!(level_of("TRACE entering the pool"), Some(Level::Trace));
        assert_eq!(level_of("debug pool has 4 idle"), Some(Level::Debug));
    }

    /// A digit touching the word means it was never a level token: an
    /// HTTP status glued onto a route, or a word that merely ends in a
    /// digit. Stripping through the digit would invent a level the line
    /// never announced (decision 3 forbids exactly this).
    #[test]
    fn a_digit_touching_the_word_blocks_the_match() {
        assert_eq!(level_of("/error404 requested"), None);
        assert_eq!(level_of("info2 and counting"), None);
        assert_eq!(level_of("GET /info2 200"), None);
    }

    /// Punctuation still strips cleanly on both sides; only digits are
    /// excluded from what trims.
    #[test]
    fn surrounding_punctuation_still_trims() {
        assert_eq!(level_of("[WARN] pool exhausted"), Some(Level::Warn));
        assert_eq!(level_of("Error: connection refused"), Some(Level::Error));
    }

    /// The scan window is exactly 4 words: a level at the fourth word is
    /// found, one at the fifth is not.
    #[test]
    fn the_scan_window_is_exactly_four_words() {
        assert_eq!(level_of("a b c warn"), Some(Level::Warn));
        assert_eq!(level_of("a b c d warn"), None);
    }

    /// The shape this reads for: `log/slog`'s text handler, and the many
    /// Go and Rust services writing the same pairs.
    #[test]
    fn a_logfmt_line_announces_its_level() {
        assert_eq!(
            level_of(r#"time=2026-09-08T11:02:03.000Z level=ERROR msg="connection refused""#),
            Some(Level::Error)
        );
        assert_eq!(
            level_of("ts=2026-09-08T11:02:03Z caller=main.go:42 level=info msg=listening"),
            Some(Level::Info)
        );
    }

    /// Each accepted key, since each is the only thing pinning itself.
    #[test]
    fn every_accepted_key_is_read() {
        assert_eq!(level_of("level=warn pool exhausted"), Some(Level::Warn));
        assert_eq!(level_of("lvl=warn pool exhausted"), Some(Level::Warn));
        assert_eq!(level_of("severity=warn pool exhausted"), Some(Level::Warn));
    }

    #[test]
    fn a_key_matches_whatever_its_case() {
        assert_eq!(
            level_of("LEVEL=Error connection refused"),
            Some(Level::Error)
        );
        assert_eq!(level_of("Lvl=FATAL connection refused"), Some(Level::Error));
    }

    /// A pair's value goes through the same rules as a bare word: quotes
    /// strip, a touching digit blocks, an empty value announces nothing.
    #[test]
    fn a_pair_value_follows_the_bare_word_rules() {
        assert_eq!(level_of(r#"level="error" msg=x"#), Some(Level::Error));
        assert_eq!(level_of("level=error404 msg=x"), None);
        assert_eq!(level_of("level= msg=x"), None);
    }

    /// The key set is closed rather than "anything before an `=`". Each of
    /// these announces a level to a reader and none to the filter.
    #[test]
    fn a_key_outside_the_set_announces_nothing() {
        assert_eq!(level_of("l=error connection refused"), None);
        assert_eq!(level_of("loglevel=error connection refused"), None);
        assert_eq!(level_of("status=error connection refused"), None);
    }

    /// The key is matched whole, which is what stops an access log's query
    /// string from reading as a pair.
    #[test]
    fn a_query_string_is_not_a_level_pair() {
        assert_eq!(level_of("GET /search?level=error 200"), None);
    }

    #[test]
    fn levels_order_from_trace_up_to_error() {
        assert!(Level::Trace < Level::Debug);
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
    }
}
