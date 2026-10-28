//! The bark dog's wall clock: the one reader of `SystemTime`, and what a
//! reading before the Unix epoch does instead of stamping zero. Every
//! `at_ms` the dog puts on a bark comes from here.
//!
//! A failed reading is a real state, not an impossible one: a board with a
//! dead RTC boots at 1970 and stays there until NTP corrects it. While that
//! holds, [`Clock::now_ms`] carries the last good stamp forward by the
//! monotonic time since it, which keeps a rule's per-subject debounce
//! working, and names the clock as the cause once for the whole run. Only a
//! clock that never had a good reading stamps 0.

use std::fmt;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// What a clock that cannot fall back to an earlier reading tells the
/// operator, naming the stamp it is producing.
const NOTHING_REMEMBERED: &str = "the system clock reads before the Unix epoch and no earlier reading is remembered; barks are stamped 0 until it recovers";

/// What a clock that can fall back tells the operator instead.
const LAST_GOOD_FALLBACK: &str = "the system clock reads before the Unix epoch; barks are stamped from the last good reading until it recovers";

/// What ends a run of failed readings.
const CLOCK_RECOVERED: &str = "the system clock reads normally again; barks are stamped from it";

/// The bark dog's clock, and its fallback while the wall clock reads before
/// the Unix epoch.
pub(super) struct Clock {
    source: Box<dyn FnMut() -> SystemTime + Send>,
    lines: Box<dyn FnMut(&str) + Send>,
    /// The last reading that was not before the epoch, and the monotonic
    /// instant it was taken at.
    last_good: Option<(u64, Instant)>,
    /// Whether the run of readings before the epoch in progress has been
    /// reported.
    failure_reported: bool,
}

/// Manual: the two closures have no `Debug` of their own, and printing what
/// a caller closed over is what a `derive` would try. What is printed here
/// holds no secret: a stamp, an instant, and a flag.
impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Clock")
            .field("last_good_ms", &self.last_good.map(|(ms, _)| ms))
            .field("failure_reported", &self.failure_reported)
            .finish()
    }
}

impl Clock {
    /// A clock reading the wall clock through `source` and reporting through
    /// `lines`.
    ///
    /// `lines` takes one operator-facing line at a time, without the dog's
    /// stderr prefix: [`Clock::system`] wraps it in that prefix, and a test
    /// reads the lines themselves.
    pub(super) fn new(
        source: impl FnMut() -> SystemTime + Send + 'static,
        lines: impl FnMut(&str) + Send + 'static,
    ) -> Self {
        Self {
            source: Box::new(source),
            lines: Box::new(lines),
            last_good: None,
            failure_reported: false,
        }
    }

    /// The clock the dog runs on in production: `SystemTime::now`, reported
    /// on stderr with the dog's own prefix.
    pub(super) fn system() -> Self {
        Self::new(SystemTime::now, |line| eprintln!("shep dog bark: {line}"))
    }

    /// Wall-clock milliseconds since the Unix epoch.
    ///
    /// A reading before the epoch falls back to the last good stamp plus the
    /// monotonic time since it, so stamps keep moving and a debounce keeps
    /// its window. With no good reading ever taken, the stamp is 0.
    #[must_use]
    pub(super) fn now_ms(&mut self) -> u64 {
        match (self.source)().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => {
                let stamp = elapsed.as_millis() as u64;
                self.last_good = Some((stamp, Instant::now()));
                if self.failure_reported {
                    self.failure_reported = false;
                    (self.lines)(CLOCK_RECOVERED);
                }
                stamp
            }
            Err(_) => {
                let stamp = match self.last_good {
                    Some((last, at)) => last.saturating_add(at.elapsed().as_millis() as u64),
                    None => 0,
                };
                if !self.failure_reported {
                    self.failure_reported = true;
                    (self.lines)(match self.last_good {
                        Some(_) => LAST_GOOD_FALLBACK,
                        None => NOTHING_REMEMBERED,
                    });
                }
                stamp
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    /// What a clock reported, one line per report.
    type Lines = Arc<Mutex<Vec<String>>>;

    /// A clock whose wall-clock source answers `source`, with every line it
    /// reports collected in the returned [`Lines`].
    fn clock_with(source: impl FnMut() -> SystemTime + Send + 'static) -> (Clock, Lines) {
        let lines = Lines::default();
        let reported = Arc::clone(&lines);
        let sink = move |line: &str| reported.lock().unwrap().push(line.to_owned());
        (Clock::new(source, sink), lines)
    }

    /// 2024-11-15T18:13:20.123Z, as milliseconds since the Unix epoch.
    const GOOD_MS: u64 = 1_731_691_999_123;

    #[test]
    fn a_good_read_returns_milliseconds_since_the_epoch() {
        let (mut clock, _lines) = clock_with(|| UNIX_EPOCH + Duration::from_millis(GOOD_MS));

        assert_eq!(clock.now_ms(), GOOD_MS);
    }

    /// Fails if a reading before the epoch stamps the firing at zero: the
    /// stamp must keep moving, or a rule's debounce reads every repeat as
    /// too soon.
    #[test]
    fn a_pre_epoch_read_after_a_good_one_keeps_the_last_stamp_moving() {
        let mut good_first = true;
        let (mut clock, _lines) = clock_with(move || {
            if good_first {
                good_first = false;
                UNIX_EPOCH + Duration::from_millis(GOOD_MS)
            } else {
                UNIX_EPOCH - Duration::from_secs(3_600)
            }
        });

        assert_eq!(clock.now_ms(), GOOD_MS, "the good read is remembered");

        // Real elapsed time, with a floor the OS guarantees: a sleep never
        // returns early, so the gap after it is at least this long.
        std::thread::sleep(Duration::from_millis(20));
        let stamp = clock.now_ms();
        assert!(
            stamp >= GOOD_MS + 20,
            "a pre-epoch read must carry the last good stamp forward by the time since it, got {stamp}"
        );
        assert!(
            clock.now_ms() >= stamp,
            "and the next failed read must not fall back to the last good one"
        );
    }

    /// Fails if a clock with nothing to fall back to stamps quietly: the
    /// operator has to be told the clock is what made the stamp zero.
    #[test]
    fn a_pre_epoch_read_with_no_good_one_returns_zero_and_says_so() {
        let (mut clock, lines) = clock_with(|| UNIX_EPOCH - Duration::from_secs(3_600));

        assert_eq!(clock.now_ms(), 0);
        assert_eq!(
            lines.lock().unwrap().clone(),
            [
                "the system clock reads before the Unix epoch and no earlier reading is remembered; barks are stamped 0 until it recovers"
            ],
            "a stamp of 0 must be reported as the clock's doing"
        );
    }

    /// Fails if every failed read of one episode gets its own line: a busy
    /// flock reports a clock fault once per firing, and the recovery is
    /// lost in them.
    #[test]
    fn a_run_of_failed_reads_prints_one_line_and_recovery_prints_one_more() {
        let mut reads = 0;
        let (mut clock, lines) = clock_with(move || {
            reads += 1;
            if reads <= 3 {
                UNIX_EPOCH - Duration::from_secs(3_600)
            } else {
                UNIX_EPOCH + Duration::from_millis(GOOD_MS)
            }
        });

        let stamps: Vec<u64> = (0..4).map(|_| clock.now_ms()).collect();
        assert_eq!(stamps, [0, 0, 0, GOOD_MS]);
        assert_eq!(
            lines.lock().unwrap().clone(),
            [
                "the system clock reads before the Unix epoch and no earlier reading is remembered; barks are stamped 0 until it recovers",
                "the system clock reads normally again; barks are stamped from it",
            ],
            "one line for the episode, one for its end"
        );
    }

    /// Fails if a recovery leaves the episode open: the next pre-epoch read
    /// would be the same fault, reported as if it were already known.
    #[test]
    fn a_second_failure_after_recovery_prints_again() {
        let mut reads = 0;
        let (mut clock, lines) = clock_with(move || {
            reads += 1;
            if reads % 2 == 1 {
                UNIX_EPOCH + Duration::from_millis(GOOD_MS)
            } else {
                UNIX_EPOCH - Duration::from_secs(3_600)
            }
        });

        for read in 0..4 {
            assert!(
                clock.now_ms() >= GOOD_MS,
                "read {read} must carry a stamp, whether the clock answered it or the fallback did"
            );
        }
        assert_eq!(
            lines.lock().unwrap().clone(),
            [
                "the system clock reads before the Unix epoch; barks are stamped from the last good reading until it recovers",
                "the system clock reads normally again; barks are stamped from it",
                "the system clock reads before the Unix epoch; barks are stamped from the last good reading until it recovers",
            ],
            "a fault that comes back is reported again"
        );
    }

    /// Fails if a lazy `derive(Debug)` prints whatever the closures capture.
    #[test]
    fn the_clock_debug_names_its_state_and_not_its_closures() {
        let (clock, _lines) = clock_with(|| UNIX_EPOCH + Duration::from_millis(GOOD_MS));

        assert_eq!(
            format!("{clock:?}"),
            "Clock { last_good_ms: None, failure_reported: false }"
        );
    }
}
