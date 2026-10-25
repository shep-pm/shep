//! The `Range` grammar `shep serve` reads, the validators a served file
//! carries, and the status a request's own headers answer with.
//!
//! Pure: it reads the request's header map and the metadata the open handle
//! already gave `worker::answer`, and decides between a 200, a 206, a 304 and
//! a 416. Writing any of them is `worker::response`'s.
//!
//! Two deliberate limits. A header naming several ranges is ignored and
//! answered as a full 200, which RFC 9110 §14.1.2 permits and which saves a
//! multipart encoder. And every ETag here is weak, built from metadata
//! rather than from the file's contents: RFC 9110 §14.2.1 requires strong
//! comparison for an `If-Range` carrying an entity-tag, so a client that
//! echoes this server's own ETag back in `If-Range` gets the full 200, while
//! `If-Range` carrying the file's own `Last-Modified` does apply the range.

use std::collections::BTreeMap;
use std::time::SystemTime;

use chrono::{DateTime, Utc};

/// The `HTTP-date` this server writes: IMF-fixdate (RFC 9110 §5.5.2), with
/// the literal `GMT` for its UTC offset.
const IMF_FIXDATE: &str = "%a, %d %b %Y %H:%M:%S GMT";

/// The `HTTP-date` forms this server reads: the one it writes, and the two
/// obsolete forms RFC 9110 §5.5.1 says a recipient must still be able to
/// read. All three are read as UTC, which is all an `HTTP-date` can name.
const OBSOLETE_HTTP_DATES: [&str; 2] = ["%a, %d-%b-%y %H:%M:%S GMT", "%a %b %e %H:%M:%S %Y"];

/// The slice a `Range` header asks for, checked against a file `len` bytes
/// long.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Range {
    /// Answer the whole file: no header at all, or one this server does not
    /// read as a single byte range (several ranges, another unit, syntax
    /// outside [`range_of`]'s grammar).
    Whole,
    /// Answer bytes `start..=end`, both ends inclusive.
    Slice { start: u64, end: u64 },
    /// A well-formed byte range no part of which exists in a file `len`
    /// bytes long.
    Unsatisfiable,
}

/// Reads one `Range` header against a file `len` bytes long.
///
/// The grammar is the three single-range forms RFC 9110 §14.1.2 names:
/// `bytes=<first>-<last>`, `bytes=<first>-` and `bytes=-<suffix>`. Anything
/// else, a comma included, is ignored rather than refused: §14.1.2 lets a
/// server ignore a `Range` header it does not support, and what an ignored
/// header costs the client is the full body it would have got anyway.
pub(super) fn range_of(header: Option<&String>, len: u64) -> Range {
    let Some(value) = header else {
        return Range::Whole;
    };
    match spec_of(value) {
        Spec::Ignored => Range::Whole,
        // No byte range of an empty file is satisfiable: a `first-byte-pos`
        // has to be less than the length.
        _ if len == 0 => Range::Unsatisfiable,
        Spec::Suffix(0) => Range::Unsatisfiable,
        Spec::Suffix(n) => Range::Slice {
            start: len - n.min(len),
            end: len - 1,
        },
        Spec::From(first) if first >= len => Range::Unsatisfiable,
        Spec::From(first) => Range::Slice {
            start: first,
            end: len - 1,
        },
        Spec::Span { first, last } if first >= len || last < first => Range::Unsatisfiable,
        Spec::Span { first, last } => Range::Slice {
            start: first,
            end: last.min(len - 1),
        },
    }
}

/// A `Range` header's value once its syntax is read, before it is checked
/// against any file.
enum Spec {
    /// Outside the grammar, or naming more than one range.
    Ignored,
    /// `bytes=-<n>`: the last `n` bytes.
    Suffix(u64),
    /// `bytes=<first>-`: everything from `first` to the end.
    From(u64),
    /// `bytes=<first>-<last>`.
    Span { first: u64, last: u64 },
}

/// [`range_of`]'s syntax pass. Splitting on the one `-` is what makes a
/// multi-range value fall out as `Ignored`: `"0-1,4-5"` splits into `"0"`
/// and `"1,4-5"`, and the latter is no number.
fn spec_of(value: &str) -> Spec {
    let Some(spec) = value.strip_prefix("bytes=") else {
        return Spec::Ignored;
    };
    let Some((first, last)) = spec.split_once('-') else {
        return Spec::Ignored;
    };
    match (first.is_empty(), last.is_empty()) {
        (true, true) => Spec::Ignored,
        (true, false) => digits(last).map_or(Spec::Ignored, Spec::Suffix),
        (false, true) => digits(first).map_or(Spec::Ignored, Spec::From),
        (false, false) => match (digits(first), digits(last)) {
            (Some(first), Some(last)) => Spec::Span { first, last },
            _ => Spec::Ignored,
        },
    }
}

/// `1*DIGIT`, the grammar RFC 9110 §14.1.2 gives a byte position. Reading a
/// position with `u64::parse` alone would take a leading `+`, which no
/// client sends and no range means.
fn digits(token: &str) -> Option<u64> {
    if token.is_empty() || !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
}

/// The validators a served file carries, both read from the `fstat` that
/// opened it: a weak ETag over length and modification time, and the
/// modification time itself in whole seconds, which is the finest grain an
/// `HTTP-date` can carry.
#[derive(Debug)]
pub(super) struct Validators {
    etag: String,
    /// Whole seconds since the Unix epoch. `None` where the platform will
    /// not report a modification time, which leaves the length alone.
    modified: Option<i64>,
}

impl Validators {
    /// Builds the pair from a file's length and the modification time its own
    /// open handle reported.
    pub(super) fn new(len: u64, modified: Option<SystemTime>) -> Self {
        let modified = modified.map(|when| DateTime::<Utc>::from(when).timestamp());
        let etag = match modified {
            Some(secs) => format!("W/\"{len}-{secs}\""),
            None => format!("W/\"{len}\""),
        };
        Self { etag, modified }
    }

    /// The `ETag` header value. Weak (`W/`): it is built from metadata, and
    /// a file rewritten with the same length and the same mtime would keep
    /// it while its contents changed.
    pub(super) fn etag(&self) -> &str {
        &self.etag
    }

    /// The `Last-Modified` header value, or `None` where the platform will
    /// not report a modification time.
    pub(super) fn last_modified(&self) -> Option<String> {
        self.modified
            .and_then(|secs| DateTime::from_timestamp(secs, 0))
            .map(|when| when.format(IMF_FIXDATE).to_string())
    }

    /// Whether an `If-None-Match` names this file, by weak comparison
    /// (RFC 9110 §8.8.3.2): the `W/` marker is dropped from both sides and
    /// the opaque value compared. `*` names whatever is there.
    pub(super) fn none_match_agrees(&self, header: &str) -> bool {
        header.split(',').any(|item| {
            let item = item.trim();
            item == "*" || strong_tag_of(item) == strong_tag_of(&self.etag)
        })
    }

    /// Whether an `If-Modified-Since` says this file is no newer than the
    /// copy the client holds (RFC 9110 §13.1.3). A value that is not an
    /// `HTTP-date` is no validator, and neither is a file whose modification
    /// time the platform will not report.
    pub(super) fn not_newer_than(&self, header: &str) -> bool {
        match (self.modified, parse_http_date(header)) {
            (Some(secs), Some(since)) => secs <= since,
            _ => false,
        }
    }

    /// Whether an `If-Range` still names this file, which is what decides
    /// whether its `Range` is honoured at all (RFC 9110 §14.2.1). An
    /// entity-tag is compared strongly, and every tag this server writes is
    /// weak, so only an `HTTP-date` that equals this file's own
    /// `Last-Modified` ever agrees.
    pub(super) fn if_range_agrees(&self, header: &str) -> bool {
        let header = header.trim();
        if header.contains('"') {
            return false;
        }
        match (self.modified, parse_http_date(header)) {
            (Some(secs), Some(if_range)) => secs == if_range,
            _ => false,
        }
    }
}

/// An entity-tag with its `W/` weakness marker removed, which is the whole
/// of weak comparison.
fn strong_tag_of(tag: &str) -> &str {
    tag.strip_prefix("W/").unwrap_or(tag)
}

/// Reads an `HTTP-date` into whole seconds since the Unix epoch.
fn parse_http_date(value: &str) -> Option<i64> {
    let value = value.trim();
    std::iter::once(IMF_FIXDATE)
        .chain(OBSOLETE_HTTP_DATES)
        .find_map(|format| chrono::NaiveDateTime::parse_from_str(value, format).ok())
        .map(|when| when.and_utc().timestamp())
}

/// What a request's conditional and range headers answer with, for a file
/// `len` bytes long carrying `validators`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// 200, the whole file.
    Whole,
    /// 206, bytes `start..=end`.
    Partial { start: u64, end: u64 },
    /// 416, no part of the range asked for exists in the file.
    Unsatisfiable,
    /// 304, the validator the client sent still names this file.
    NotModified,
}

/// The order is RFC 9110's. `If-None-Match` is evaluated first and, when it
/// is present, outranks `If-Modified-Since` (§13.2.2): a request whose
/// `If-None-Match` does not match gets the body even if its
/// `If-Modified-Since` would have matched. Then `If-Range`, which decides
/// whether a `Range` is honoured at all (§14.2.1). Then the range.
pub(super) fn decide(
    headers: &BTreeMap<String, String>,
    len: u64,
    validators: &Validators,
) -> Outcome {
    if let Some(if_none_match) = headers.get("if-none-match") {
        if validators.none_match_agrees(if_none_match) {
            return Outcome::NotModified;
        }
    } else if let Some(if_modified_since) = headers.get("if-modified-since")
        && validators.not_newer_than(if_modified_since)
    {
        return Outcome::NotModified;
    }

    let range = match headers.get("if-range") {
        Some(if_range) if !validators.if_range_agrees(if_range) => Range::Whole,
        _ => range_of(headers.get("range"), len),
    };
    match range {
        Range::Whole => Outcome::Whole,
        Range::Slice { start, end } => Outcome::Partial { start, end },
        Range::Unsatisfiable => Outcome::Unsatisfiable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn header(value: &str) -> String {
        value.to_string()
    }

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    /// 1994-11-06T08:49:37Z, the example timestamp RFC 9110 §5.5.2 uses.
    const EXAMPLE_SECS: i64 = 784_111_777;

    fn example_mtime() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(EXAMPLE_SECS).unwrap())
    }

    /// fails if a single range is answered with the whole file, or if an
    /// inclusive end is read as exclusive.
    #[test]
    fn a_closed_range_is_the_slice_it_names_inclusive_of_both_ends() {
        assert_eq!(
            range_of(Some(&header("bytes=2-5")), 10),
            Range::Slice { start: 2, end: 5 }
        );
        assert_eq!(
            range_of(Some(&header("bytes=0-0")), 10),
            Range::Slice { start: 0, end: 0 }
        );
    }

    /// fails if an open-ended range loses its end.
    #[test]
    fn an_open_range_runs_to_the_last_byte() {
        assert_eq!(
            range_of(Some(&header("bytes=9-")), 10),
            Range::Slice { start: 9, end: 9 }
        );
    }

    /// fails if a suffix range is read from the front.
    #[test]
    fn a_suffix_range_counts_back_from_the_end() {
        assert_eq!(
            range_of(Some(&header("bytes=-3")), 10),
            Range::Slice { start: 7, end: 9 }
        );
    }

    /// fails if a range running past the end is refused instead of clamped,
    /// which is what a client seeking a media file relies on.
    #[test]
    fn a_last_byte_past_the_end_is_clamped_to_the_file() {
        assert_eq!(
            range_of(Some(&header("bytes=5-900")), 10),
            Range::Slice { start: 5, end: 9 }
        );
        assert_eq!(
            range_of(Some(&header("bytes=-900")), 10),
            Range::Slice { start: 0, end: 9 }
        );
    }

    /// fails if a range past the end, an inverted range, or a zero-length
    /// suffix is answered with a 206 of nothing.
    #[test]
    fn a_range_no_part_of_which_exists_is_unsatisfiable() {
        for value in ["bytes=999-", "bytes=10-20", "bytes=5-2", "bytes=-0"] {
            assert_eq!(
                range_of(Some(&header(value)), 10),
                Range::Unsatisfiable,
                "{value}"
            );
        }
        assert_eq!(
            range_of(Some(&header("bytes=0-9")), 0),
            Range::Unsatisfiable,
            "no range of an empty file exists"
        );
    }

    /// fails if a multi-range or malformed header is refused rather than
    /// ignored, or if ignoring one is mistaken for honouring it.
    #[test]
    fn a_header_outside_the_single_range_grammar_is_ignored() {
        for value in [
            "bytes=0-1,4-5",
            "bytes=0-1, 4-5",
            "bytes=",
            "bytes=-",
            "bytes=abc-5",
            "bytes=0-1-2",
            "items=0-1",
            "0-1",
            "bytes =0-1",
            "bytes=0 - 1",
            "bytes=+2-5",
            "bytes=0-18446744073709551616",
        ] {
            assert_eq!(range_of(Some(&header(value)), 10), Range::Whole, "{value}");
        }
        assert_eq!(range_of(None, 10), Range::Whole);
    }

    /// fails if the ETag is not weak, or if it stops changing with either
    /// half of the metadata it is built from.
    #[test]
    fn the_etag_is_weak_and_carries_both_halves_of_the_metadata() {
        let built = Validators::new(13, Some(example_mtime()));
        assert_eq!(built.etag(), format!("W/\"13-{EXAMPLE_SECS}\""));
        assert_eq!(
            Validators::new(14, Some(example_mtime())).etag(),
            format!("W/\"14-{EXAMPLE_SECS}\"")
        );
        assert_ne!(
            Validators::new(13, Some(example_mtime() + Duration::from_secs(1))).etag(),
            built.etag()
        );
        assert_eq!(Validators::new(13, None).etag(), "W/\"13\"");
    }

    /// fails if the date written is not IMF-fixdate, which is the one form
    /// RFC 9110 §5.5.1 says every recipient must read.
    #[test]
    fn last_modified_is_an_imf_fixdate() {
        assert_eq!(
            Validators::new(13, Some(example_mtime()))
                .last_modified()
                .as_deref(),
            Some("Sun, 06 Nov 1994 08:49:37 GMT")
        );
        assert_eq!(Validators::new(13, None).last_modified(), None);
    }

    /// fails if a date this server writes cannot be read back, or if an
    /// obsolete form or a non-date is read as a validator.
    #[test]
    fn an_http_date_is_read_in_the_three_forms_the_spec_names() {
        for value in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sun, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(parse_http_date(value), Some(EXAMPLE_SECS), "{value}");
        }
        for value in [
            "Sun, 06 Nov 1994 08:49:37 +0000",
            "1994-11-06T08:49:37Z",
            "0",
            "",
            "Sun, 06 Nov 1994",
        ] {
            assert_eq!(parse_http_date(value), None, "{value}");
        }
    }

    /// fails if `If-None-Match` is compared strongly, which would never
    /// match a `W/` tag, or if a list of tags is not read as a list.
    #[test]
    fn if_none_match_compares_weakly_and_reads_a_list() {
        let built = Validators::new(13, Some(example_mtime()));
        assert!(built.none_match_agrees("W/\"13-784111777\""));
        assert!(
            built.none_match_agrees("\"13-784111777\""),
            "a strong tag matches a weak one by weak comparison"
        );
        assert!(built.none_match_agrees("W/\"other\", W/\"13-784111777\""));
        assert!(built.none_match_agrees("*"));
        assert!(!built.none_match_agrees("W/\"13-784111778\""));
        assert!(!built.none_match_agrees("\"13\""));
    }

    /// fails if a file whose mtime equals the one the client holds is read
    /// as newer, which is what makes a reload re-read the file.
    #[test]
    fn if_modified_since_matches_the_second_the_file_was_written_in() {
        let built = Validators::new(13, Some(example_mtime()));
        assert!(built.not_newer_than("Sun, 06 Nov 1994 08:49:37 GMT"));
        assert!(
            built.not_newer_than("Sun, 06 Nov 1994 08:49:38 GMT"),
            "a client holding a copy from a second later"
        );
        assert!(!built.not_newer_than("Sun, 06 Nov 1994 08:49:36 GMT"));
        assert!(!built.not_newer_than("not a date"));
        assert!(!Validators::new(13, None).not_newer_than("Sun, 06 Nov 1994 08:49:37 GMT"));
    }

    /// fails if `If-Range` is read as a weak comparison, which would let a
    /// client's stale copy pick the slice, or if its date form is ignored.
    #[test]
    fn if_range_agrees_only_with_the_files_own_last_modified() {
        let built = Validators::new(13, Some(example_mtime()));
        assert!(built.if_range_agrees("Sun, 06 Nov 1994 08:49:37 GMT"));
        assert!(!built.if_range_agrees("Sun, 06 Nov 1994 08:49:36 GMT"));
        assert!(!built.if_range_agrees("W/\"13-784111777\""), "a weak tag");
        assert!(!built.if_range_agrees("\"13-784111777\""));
        assert!(!built.if_range_agrees("nonsense"));
    }

    /// fails if a 206 is picked for a file the client already has whole.
    #[test]
    fn a_matching_validator_answers_304_in_place_of_a_range() {
        let built = Validators::new(10, Some(example_mtime()));
        assert_eq!(
            decide(
                &headers(&[
                    ("if-none-match", "W/\"10-784111777\""),
                    ("range", "bytes=2-5"),
                ]),
                10,
                &built
            ),
            Outcome::NotModified
        );
        assert_eq!(
            decide(
                &headers(&[
                    ("if-modified-since", "Sun, 06 Nov 1994 08:49:37 GMT"),
                    ("range", "bytes=2-5"),
                ]),
                10,
                &built
            ),
            Outcome::NotModified
        );
    }

    /// fails if `If-Modified-Since` is consulted when `If-None-Match` is
    /// present and did not match (RFC 9110 §13.2.2).
    #[test]
    fn if_none_match_outranks_if_modified_since() {
        let built = Validators::new(10, Some(example_mtime()));
        assert_eq!(
            decide(
                &headers(&[
                    ("if-none-match", "W/\"other-tag\""),
                    ("if-modified-since", "Sun, 06 Nov 1994 08:49:37 GMT"),
                ]),
                10,
                &built
            ),
            Outcome::Whole,
            "the newer validator is the one that answers"
        );
    }

    /// fails if a stale `If-Range` still gets its slice.
    #[test]
    fn a_stale_if_range_sends_the_whole_file() {
        let built = Validators::new(10, Some(example_mtime()));
        assert_eq!(
            decide(
                &headers(&[
                    ("if-range", "Sun, 01 Jan 1970 00:00:01 GMT"),
                    ("range", "bytes=2-5")
                ]),
                10,
                &built
            ),
            Outcome::Whole
        );
        assert_eq!(
            decide(
                &headers(&[("if-range", "W/\"10-784111777\""), ("range", "bytes=2-5")]),
                10,
                &built
            ),
            Outcome::Whole
        );
        assert_eq!(
            decide(
                &headers(&[
                    ("if-range", "Sun, 06 Nov 1994 08:49:37 GMT"),
                    ("range", "bytes=2-5"),
                ]),
                10,
                &built
            ),
            Outcome::Partial { start: 2, end: 5 },
            "the matching one is the one that does apply it"
        );
    }

    /// fails if an unsatisfiable range is answered with a 200 of the whole
    /// file, or a plain request with anything but a 200.
    #[test]
    fn a_request_with_no_conditional_headers_gets_the_whole_file() {
        let built = Validators::new(10, Some(example_mtime()));
        assert_eq!(decide(&headers(&[]), 10, &built), Outcome::Whole);
        assert_eq!(
            decide(&headers(&[("range", "bytes=999-")]), 10, &built),
            Outcome::Unsatisfiable
        );
        assert_eq!(
            decide(&headers(&[("range", "bytes=0-1,4-5")]), 10, &built),
            Outcome::Whole
        );
    }
}
