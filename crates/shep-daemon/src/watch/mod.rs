//! The filesystem-watch subsystem (spec §4).
//!
//! [`source`] bridges notify's debounced events onto a tokio channel.
//! [`WatchFilter`] decides which delivered paths trigger a restart, and
//! [`spawn_watch_group`] runs one name-group's restart loop over them,
//! single-flighted like [`crate::cron`]'s.
//!
//! A triggering change restarts every instance of the name, stopped ones
//! included: disarming a sheep's watch, not filtering the restart, is what
//! keeps it down. A rescan bypasses both glob sets and always restarts.
//! `watch = true` requires `cwd`; the debounce runs on notify's own OS
//! thread, so a paused clock in tests never moves it.

mod filter;
mod group;
pub mod source;
#[cfg(test)]
mod testing;
pub use filter::WatchFilter;
pub(crate) use filter::own_log_ignores;
pub use group::spawn_watch_group;
pub(crate) use group::{DEFAULT_WATCH_DELAY, MIN_WATCH_DELAY};

/// The real-time constants shared by every real-filesystem test suite
/// in this crate: [`source`]'s smoke tests, this module's own case for
/// [`spawn_watch_group`], and the extras registry's arm/disarm case.
///
/// One owner rather than a copy per suite, since every one of them
/// drives the same debouncer at the same delay. `TEST_DELAY` and
/// `NO_EVENT_WINDOW` are load-bearing together; see the assertion at the
/// top of `source`'s tests.
#[cfg(test)]
pub(crate) mod real_time {
    use core::time::Duration;

    /// Debounce window for every real-filesystem test in this crate: tens of
    /// milliseconds, so a real save-to-batch round trip finishes fast without
    /// accidentally coalescing writes a test means to keep distinct.
    pub(crate) const TEST_DELAY: Duration = Duration::from_millis(50);

    /// How long a test waits for something expected to arrive: a delivered
    /// batch, or a watch-triggered restart. Generous enough that a loaded
    /// CI runner's real inotify/FSEvents latency cannot turn a genuine pass
    /// into a flaky timeout.
    pub(crate) const SMOKE_DEADLINE: Duration = Duration::from_secs(5);

    /// How long a test waits for something that must not arrive. Short on
    /// purpose: this window is a cost every passing run of such a test
    /// pays, and it exists only to prove a negative. Generous enough that a
    /// real event has time to land; short enough not to make a green suite
    /// slow.
    pub(crate) const NO_EVENT_WINDOW: Duration = Duration::from_millis(500);
}
