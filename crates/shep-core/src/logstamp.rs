//! The timestamp every line in a sheep's or a dog's log file carries, and
//! how a reader takes it back off.
//!
//! Shared between the daemon, which writes the stamp, and every reader.
//! Readers are `shep bleats --no-follow`, `shep lookout`'s tail pane, and
//! `whistle`. The stamp lives on the line, not in the file's `mtime`.
//! `mtime` answers only for the whole file, and stops once anything touches it.
//!
//! [`strip`] never changes what a sheep is reported to have said.
//! `Bus::publish_log` carries its line verbatim, so a stripped line matches.

use core::fmt::{self, Write as _};

use chrono::{DateTime, TimeZone, Utc};

/// The `strftime` spelling of the stamp: `2026-09-02T14:22:31.412+02:00`.
///
/// Local time with the UTC offset, RFC 3339, to the millisecond: the offset
/// keeps local time unambiguous across a DST boundary, RFC 3339 sorts
/// lexicographically within one offset, and milliseconds resolve a dog's
/// handshake round trip.
pub const LOG_TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3f%:z";

/// How many bytes [`stamp_into`] writes: the stamp plus the separating space.
///
/// Fixed because every field in [`LOG_TIMESTAMP_FORMAT`] is zero-padded, and
/// `%:z` always renders `+HH:MM` (chrono truncates rarer sub-minute
/// offsets). The parts, in order: 10 date, 1 `T`, 8 time, 4 `.mmm`, 6
/// offset, 1 space.
pub const LOG_STAMP_BYTES: usize = 30;

/// [`LOG_TIMESTAMP_FORMAT`] up to the milliseconds.
const STAMP_HEAD: &str = "%Y-%m-%dT%H:%M:%S";

/// [`LOG_TIMESTAMP_FORMAT`] after the milliseconds.
const STAMP_TAIL: &str = "%:z";

/// Appends the current local time in [`LOG_TIMESTAMP_FORMAT`], plus the
/// separating space, to `buf`. Formats in full on every call: a writer
/// stamping more than one line holds a [`Stamper`] instead.
///
/// # Panics
/// Debug builds only, if the width diverges from [`LOG_STAMP_BYTES`].
#[track_caller]
pub fn stamp_into(buf: &mut String) {
    Stamper::default().stamp_into(buf);
}

/// Writes [`LOG_TIMESTAMP_FORMAT`] stamps, formatting the date, time and
/// offset once per second and only the milliseconds per line.
///
/// Exact, not approximate: a zone's offset changes only on a whole second.
/// A zone change reaches the stamp within a second, as it does
/// `chrono::Local` itself.
#[derive(Debug, Clone, Default)]
pub struct Stamper {
    /// The unix second `head` and `tail` were rendered for.
    second: Option<i64>,
    /// The stamp before its milliseconds: `2026-09-02T14:22:31`.
    head: String,
    /// The stamp after its milliseconds, plus the separating space.
    tail: String,
}

impl Stamper {
    /// Appends the current local time in [`LOG_TIMESTAMP_FORMAT`], plus the
    /// separating space, to `buf`.
    ///
    /// # Panics
    ///
    /// Debug builds only, when [`LOG_TIMESTAMP_FORMAT`]'s width diverges from
    /// [`LOG_STAMP_BYTES`].
    #[track_caller]
    pub fn stamp_into(&mut self, buf: &mut String) {
        self.stamp_at(Utc::now(), &chrono::Local, buf);
    }

    /// [`Self::stamp_into`] for a given instant and zone.
    #[track_caller]
    fn stamp_at<Tz>(&mut self, now: DateTime<Utc>, zone: &Tz, buf: &mut String)
    where
        Tz: TimeZone,
        Tz::Offset: fmt::Display,
    {
        let second = now.timestamp();
        if self.second != Some(second) {
            let local = now.with_timezone(zone);
            self.head.clear();
            self.tail.clear();
            // Infallible: `write!` to a `String` fails only if `Display`
            // does, and `DelayedFormat` fails only on a malformed format.
            let _ = write!(self.head, "{}", local.format(STAMP_HEAD));
            let _ = write!(self.tail, "{} ", local.format(STAMP_TAIL));
            self.second = Some(second);
        }
        let start = buf.len();
        buf.push_str(&self.head);
        let _ = write!(buf, ".{:03}", now.timestamp_subsec_millis());
        buf.push_str(&self.tail);
        debug_assert_eq!(
            buf.len() - start,
            LOG_STAMP_BYTES,
            "the stamp's width is fixed and readers strip it by count"
        );
    }
}

/// `line` with its stamp removed, or `line` unchanged if it does not carry
/// one.
///
/// Recognises the stamp by shape, not by parsing. It matches the fixed
/// layout [`stamp_into`] writes, field by field. The ranges are the
/// layout's, not the calendar's: `2026-04-31` is a stamp, and
/// `2026-99-99T99:99:99.999+99:99` is not.
#[must_use]
pub fn strip(line: &str) -> &str {
    let Some((stamp, rest)) = line.split_at_checked(LOG_STAMP_BYTES) else {
        return line;
    };
    if shaped_like_a_stamp(stamp.as_bytes()) {
        rest
    } else {
        line
    }
}

/// The stamp's shape, one entry per byte [`stamp_into`] writes: `#` is a
/// digit, `z` is the offset's sign, every other byte is a literal.
///
/// Its width is [`LOG_STAMP_BYTES`] by type, so a format whose width diverges
/// from the constant fails to compile rather than tripping a `debug_assert`.
const STAMP_SHAPE: [u8; LOG_STAMP_BYTES] = *b"####-##-##T##:##:##.###z##:## ";

/// Each two-digit field of the stamp: its byte offset, and the range chrono
/// renders it in. Second 60 is a leap second.
const STAMP_FIELDS: [(usize, u32, u32); 7] = [
    (5, 1, 12),  // month
    (8, 1, 31),  // day
    (11, 0, 23), // hour
    (14, 0, 59), // minute
    (17, 0, 60), // second
    (24, 0, 23), // offset hour
    (27, 0, 59), // offset minute
];

/// Whether `bytes` is the stamp [`stamp_into`] writes.
///
/// [`STAMP_FIELDS`] holds every offset this reads, each inside
/// [`LOG_STAMP_BYTES`].
fn shaped_like_a_stamp(bytes: &[u8]) -> bool {
    let Some(stamp) = bytes.first_chunk::<LOG_STAMP_BYTES>() else {
        return false;
    };
    let shape = |byte: u8, marker: u8| match marker {
        b'#' => byte.is_ascii_digit(),
        b'z' => byte == b'+' || byte == b'-',
        literal => byte == literal,
    };
    if !stamp
        .iter()
        .zip(&STAMP_SHAPE)
        .all(|(byte, marker)| shape(*byte, *marker))
    {
        return false;
    }
    let field = |at: usize| u32::from(stamp[at] - b'0') * 10 + u32::from(stamp[at + 1] - b'0');
    STAMP_FIELDS
        .iter()
        .all(|&(at, low, high)| (low..=high).contains(&field(at)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The zones the two stamper tests share.
    const ZONES: [chrono_tz::Tz; 4] = [
        chrono_tz::UTC,
        chrono_tz::America::New_York,
        chrono_tz::Asia::Kolkata,
        chrono_tz::Australia::Lord_Howe,
    ];

    /// The instants they share, in Unix milliseconds: before 1970, twice
    /// inside one second, and each side of New York's 2026 changes and Lord
    /// Howe's half-hour one.
    const INSTANTS: [i64; 9] = [
        -1_500,
        1_788_351_751_001,
        1_788_351_751_999,
        1_772_953_199_999,
        1_772_953_200_000,
        1_793_512_799_999,
        1_793_512_800_000,
        1_775_314_799_999,
        1_775_314_800_000,
    ];

    /// Every stamp [`INSTANTS`] produces in `zone`, one [`Stamper`] per zone
    /// so each instant after the first is either a reuse inside one second or
    /// a re-render across an offset change.
    fn stamps_written_in<Tz>(zone: &Tz) -> Vec<(i64, String)>
    where
        Tz: TimeZone,
        Tz::Offset: fmt::Display,
    {
        let mut stamper = Stamper::default();
        INSTANTS
            .iter()
            .map(|&millis| {
                let now = DateTime::from_timestamp_millis(millis).expect("in chrono's range");
                let mut written = String::new();
                stamper.stamp_at(now, zone, &mut written);
                (millis, written)
            })
            .collect()
    }

    #[test]
    fn a_stamped_line_comes_back_the_way_it_went_in() {
        let mut written = String::new();
        stamp_into(&mut written);
        written.push_str("the sheep said this");

        assert_eq!(written.len(), LOG_STAMP_BYTES + "the sheep said this".len());
        assert_eq!(strip(&written), "the sheep said this");
    }

    /// One stamper per zone, so each instant after the first is either a
    /// reuse inside one second or a re-render across an offset change.
    #[test]
    fn a_stamper_writes_what_the_whole_format_writes() {
        assert_eq!(
            format!("{STAMP_HEAD}%.3f{STAMP_TAIL}"),
            LOG_TIMESTAMP_FORMAT
        );
        for zone in ZONES {
            for (millis, written) in stamps_written_in(&zone) {
                let now = DateTime::from_timestamp_millis(millis).expect("in chrono's range");
                let whole = format!("{} ", now.with_timezone(&zone).format(LOG_TIMESTAMP_FORMAT));
                assert_eq!(written, whole, "{millis} in {zone}");
            }
        }
    }

    /// What [`strip`] must keep agreeing on: every stamp a [`Stamper`] writes
    /// is a prefix an RFC 3339 parse accepts, and [`strip`] takes it off.
    #[test]
    fn strip_agrees_with_the_rfc_3339_parse_for_every_stamp_a_stamper_writes() {
        const SAID: &str = "the sheep said this";
        // A negative offset and `+00:00`, spelled as fixed offsets rather
        // than read out of a zone's rules.
        let fixed = [
            chrono::FixedOffset::east_opt(-11 * 60 * 60).expect("an offset chrono accepts"),
            chrono::FixedOffset::east_opt(0).expect("an offset chrono accepts"),
        ];
        let mut written = Vec::new();
        for zone in ZONES {
            written.extend(stamps_written_in(&zone));
        }
        for zone in fixed {
            written.extend(stamps_written_in(&zone));
        }
        assert_eq!(written.len(), (ZONES.len() + fixed.len()) * INSTANTS.len());
        for (millis, stamp) in &written {
            let line = format!("{stamp}{SAID}");
            assert_eq!(stamp.len(), LOG_STAMP_BYTES, "{millis}: {stamp:?}");
            assert!(
                chrono::DateTime::parse_from_rfc3339(stamp.trim_end()).is_ok(),
                "{millis}: {stamp:?} is what the stamper wrote, so the parse must take it"
            );
            assert_eq!(strip(&line), SAID, "{millis}: {stamp:?}");
        }
    }

    /// The field ranges are what keep a stamp-shaped prefix that no clock
    /// writes unstripped.
    #[test]
    fn a_stamp_shaped_prefix_with_an_impossible_field_is_left_alone() {
        for line in [
            "2026-13-01T00:00:00.000+00:00 month 13",
            "2026-00-01T00:00:00.000+00:00 month 00",
            "2026-01-00T00:00:00.000+00:00 day 00",
            "2026-01-01T24:00:00.000+00:00 hour 24",
            "2026-01-01T00:60:00.000+00:00 minute 60",
            "2026-01-01T00:00:00.000+24:00 offset hour 24",
            "2026-01-01T00:00:00.000+00:60 offset minute 60",
        ] {
            assert_eq!(
                strip(line),
                line,
                "{line:?} is shaped like a stamp, and is not one"
            );
        }
    }

    /// The check is the layout and its field ranges, not the calendar.
    #[test]
    fn a_stamp_shaped_prefix_on_a_date_no_calendar_has_is_stripped() {
        const SAID: &str = "the sheep said this";
        for stamp in [
            "2026-04-31T00:00:00.000+00:00 ",
            "2026-02-30T00:00:00.000+00:00 ",
        ] {
            let line = format!("{stamp}{SAID}");
            assert_eq!(strip(&line), SAID, "{line:?}");
        }
    }

    /// Second 60 is the one value `chrono` renders that a stricter reading of
    /// the clock would refuse.
    #[test]
    fn a_leap_second_stamp_is_stripped() {
        const SAID: &str = "the sheep said this";
        let line = format!("2026-06-30T23:59:60.000+00:00 {SAID}");
        assert_eq!(strip(&line), SAID, "{line:?}");
    }

    /// `shaped_like_a_stamp` reads `STAMP_FIELDS` by index, so every offset
    /// it names has to be a two-digit field inside the width `strip` cuts.
    #[test]
    fn every_stamp_field_is_a_two_digit_field_inside_the_stamp_width() {
        for &(at, low, high) in &STAMP_FIELDS {
            assert!(low <= high, "{at} names an empty range");
            assert!(at + 1 < LOG_STAMP_BYTES, "{at} is past the stamp's width");
            assert_eq!(STAMP_SHAPE[at], b'#', "{at} is not a digit");
            assert_eq!(STAMP_SHAPE[at + 1], b'#', "{} is not a digit pair", at + 1);
        }
    }

    /// Covers a line from before this format existed and one appended by
    /// other tooling.
    #[test]
    fn an_unstamped_line_is_left_exactly_as_it_is() {
        for line in [
            "",
            "short",
            "an old line from before shep stamped anything at all",
            // Long enough to reach the width, with a space in the right
            // place, but not a real timestamp.
            "2026-99-99T99:99:99.999+99:99 nonsense in the shape of a stamp",
            "############################# looks like a prefix, parses as nothing",
        ] {
            assert_eq!(strip(line), line, "{line:?} carries no stamp to strip");
        }
    }

    /// `split_at_checked` returns `None` rather than panicking on a
    /// non-boundary index; `split_at` would panic here.
    #[test]
    fn a_line_split_mid_character_is_returned_whole() {
        let line = format!("{}x", "é".repeat(LOG_STAMP_BYTES));
        assert_eq!(strip(&line), line);
    }
}
