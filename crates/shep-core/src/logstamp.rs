//! The timestamp every line in a sheep's or a dog's log file carries, and
//! how a reader takes it back off.
//!
//! Shared between the daemon, which writes the stamp, and every reader:
//! `shep bleats --no-follow`, `shep lookout`'s tail pane, and the `whistle`
//! tool. The stamp lives on the line rather than the file's `mtime`, which
//! answers only for the whole file and stops answering once anything
//! touches it, log rotation included.
//!
//! [`strip`] never changes what a sheep is reported to have said:
//! `Bus::publish_log` carries its line verbatim, so a stripped line matches
//! what `--follow` and `log.*` subscribers already saw.

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
/// offsets). 10 date + 1 `T` + 8 time + 4 `.mmm` + 6 offset + 1 space.
pub const LOG_STAMP_BYTES: usize = 30;

/// [`LOG_TIMESTAMP_FORMAT`] up to the milliseconds.
const STAMP_HEAD: &str = "%Y-%m-%dT%H:%M:%S";

/// [`LOG_TIMESTAMP_FORMAT`] after the milliseconds.
const STAMP_TAIL: &str = "%:z";

/// Appends the current local time in [`LOG_TIMESTAMP_FORMAT`], plus the
/// separating space, to `buf`.
///
/// Formats in full on every call. A writer stamping more than one line
/// holds a [`Stamper`] instead.
///
/// # Panics
///
/// Debug builds only, when [`LOG_TIMESTAMP_FORMAT`]'s width diverges from
/// [`LOG_STAMP_BYTES`].
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
/// Recognises the stamp by parsing the first [`LOG_STAMP_BYTES`] as RFC
/// 3339, rather than blindly cutting a fixed prefix: a line predating this
/// format, or one appended by other tooling, keeps its first 30 characters
/// instead of losing them.
///
/// Cheap on the common path: a line too short or missing the separator
/// space is rejected before parsing runs.
#[must_use]
pub fn strip(line: &str) -> &str {
    let Some((stamp, rest)) = line.split_at_checked(LOG_STAMP_BYTES) else {
        return line;
    };
    let Some(stamp) = stamp.strip_suffix(' ') else {
        return line;
    };
    if chrono::DateTime::parse_from_rfc3339(stamp).is_ok() {
        rest
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let zones = [
            chrono_tz::UTC,
            chrono_tz::America::New_York,
            chrono_tz::Asia::Kolkata,
            chrono_tz::Australia::Lord_Howe,
        ];
        // Unix millis: before 1970, twice inside one second, and each side
        // of New York's 2026 changes and Lord Howe's half-hour one.
        let instants = [
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
        for zone in zones {
            let mut stamper = Stamper::default();
            for millis in instants {
                let now = DateTime::from_timestamp_millis(millis).expect("in chrono's range");
                let mut written = String::new();
                stamper.stamp_at(now, &zone, &mut written);
                let whole = format!("{} ", now.with_timezone(&zone).format(LOG_TIMESTAMP_FORMAT));
                assert_eq!(written, whole, "{millis} in {zone}");
            }
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
