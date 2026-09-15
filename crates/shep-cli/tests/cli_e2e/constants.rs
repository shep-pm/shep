//! The deadlines, poll intervals and fixture strings the cases here share.

use super::*;

/// Bound on every `shep` invocation here; `.output()` blocks unbounded
/// without it.
///
/// Must outlive [`shep_client::spawn::SPAWN_DEADLINE`]: the autostart path can
/// spend that whole budget before reporting `DaemonUnreachable`, and an equal
/// bound kills the process before it can report exit 5.
pub(crate) const CMD_TIMEOUT: Duration =
    Duration::from_secs(shep_client::spawn::SPAWN_DEADLINE.as_secs() + 15);

/// Bound on how long [`concurrent_cold_starts_produce_exactly_one_daemon`]
/// waits for one of its racers.
///
/// [`CMD_TIMEOUT`] bounds the process wait, not the reader threads after it:
/// those end on EOF, which waits for every copy of the write end, including
/// one a daemon inherited. Strictly longer than [`CMD_TIMEOUT`], so it fires
/// only on a stuck racer.
pub(crate) const RACER_DEADLINE: Duration = Duration::from_secs(CMD_TIMEOUT.as_secs() + 15);

/// How long [`bleats_no_follow_until_written`] keeps retrying.
pub(crate) const BLEATS_DEADLINE: Duration = Duration::from_secs(10);

/// Gap between [`bleats_no_follow_until_written`]'s retries.
pub(crate) const BLEATS_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a fixture sheep's script sleeps after writing whatever it writes.
///
/// Outlasts every case that uses it, and short enough that a sheep the
/// [`DaemonGuard`] sweep missed self-terminates. The real-clock cases use
/// [`SLOW_SCRIPT_SLEEP_SECS`].
pub(crate) const SCRIPT_SLEEP_SECS: u32 = 60;

/// [`SCRIPT_SLEEP_SECS`] for the two real-clock cases.
///
/// Twice [`CRON_DEADLINE`], the longest of their deadlines: a script that
/// could exit inside the observation window would make "the sheep restarted"
/// equally consistent with a crash loop.
pub(crate) const SLOW_SCRIPT_SLEEP_SECS: u32 = 300;

/// Basename, under a case's own `$SHEP_HOME`, of the file every fixture
/// script appends its own pid to. Written by [`record_pid_line`], read by
/// [`DaemonGuard`].
pub(crate) const FIXTURE_PIDS: &str = "fixture.pids";

/// How long [`DaemonGuard::drop`] keeps retrying for a parseable daemon pid.
///
/// `PidfileLock::acquire` creates the pidfile empty and `record` fills it only
/// once the control socket is bound, so a fresh `$SHEP_HOME` has an empty
/// pidfile for the whole bind.
pub(crate) const GUARD_PID_DEADLINE: Duration = Duration::from_secs(3);

/// Gap between [`GUARD_PID_DEADLINE`]'s and [`GUARD_SWEEP_WINDOW`]'s retries.
pub(crate) const GUARD_PID_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long [`sweep_flock`] keeps re-reading a case's recorded sheep pids.
/// Covers the gap between the spawn `shep start` reports as `Online` and the
/// script's first line, which is when the pid reaches disk.
pub(crate) const GUARD_SWEEP_WINDOW: Duration = Duration::from_secs(2);

/// How long [`poll_flock`] keeps asking before returning what it last saw.
///
/// One deadline for both directions: a case waiting for a restart and a case
/// proving none came must wait the same length. Sized against the 500ms
/// `DEFAULT_WATCH_DELAY` debounce plus a spawn and two RPC round trips, with
/// an order of magnitude of headroom.
pub(crate) const FLOCK_DEADLINE: Duration = Duration::from_secs(10);

/// Gap between [`poll_flock`]'s attempts.
pub(crate) const FLOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// `RequestError::Closed`: the reply a client loses when the image on the
/// other end of its request was replaced by a handover. An accepted
/// connection is `FD_CLOEXEC` and dies at the `execve`; the handover spec's
/// H2 table rules that the client sees the drop.
pub(crate) const DROPPED_REPLY: &str = "the connection closed before a reply arrived";

/// `ConnectError::HandshakeClosed`: the same exec, caught between the accept
/// and the `HelloReply`. A shepherd that is gone prints "could not connect"
/// instead, so neither is reachable from a dead one.
pub(crate) const DROPPED_HANDSHAKE: &str = "the daemon closed the connection during the handshake";

/// How long [`poll_metrics`] keeps retrying a `/metrics` scrape.
///
/// `shep enable metrics` returns once the `EnableDog` RPC is accepted, before
/// the daemon has exec'd `shep dog metrics` or that process has bound.
pub(crate) const METRICS_SCRAPE_DEADLINE: Duration = FLOCK_DEADLINE;

/// Gap between [`poll_metrics`]'s retries.
pub(crate) const METRICS_SCRAPE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Bound on a single scrape attempt's own I/O, inside [`poll_metrics`]'s retry
/// loop: a peer that connects and then never answers must not stall it past
/// its own deadline.
pub(crate) const METRICS_SCRAPE_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// How long [`poll_http_get`] keeps retrying a `shep serve` worker. `shep
/// serve` returning success means the sheep is registered, not that the worker
/// has bound its listener.
pub(crate) const SERVE_HTTP_DEADLINE: Duration = FLOCK_DEADLINE;

/// Gap between [`poll_http_get`]'s retries.
pub(crate) const SERVE_HTTP_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Bound on a single `shep serve` request's own I/O, inside
/// [`poll_http_get`]'s retry loop.
pub(crate) const SERVE_HTTP_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound [`a_served_sheep_stops_on_sigterm_rather_than_riding_the_ladder_to_sigkill`]
/// asserts `shep stop`'s wall clock against.
///
/// `Command::Stop` defers its reply until the sheep has exited, so elapsed
/// time reports which rung of the kill ladder answered: `SIGKILL` takes at
/// least `kill_timeout`, 1600ms; a handled `SIGTERM` takes tens of
/// milliseconds.
pub(crate) const SERVE_STOP_DEADLINE: Duration = Duration::from_millis(1000);

/// How long [`a_cron_occurrence_restarts_a_sheep_on_the_real_clock`] waits.
///
/// A `* * * * *` pattern armed at an arbitrary moment is up to 60s from its
/// first occurrence. Two and a half minutes covers two, so a loaded runner
/// that misses the first still has a second. The case costs 26s to 61s.
pub(crate) const CRON_DEADLINE: Duration = Duration::from_secs(150);

/// How long [`a_real_memory_breach_restarts_a_sheep`] waits for its breach.
///
/// The enforcer samples every `shep_daemon::limits::MEMORY_POLL_INTERVAL`
/// (15s), phased off daemon boot, so the worst wait is one whole interval plus
/// a kill ladder and a respawn. Four times that is headroom.
pub(crate) const BREACH_DEADLINE: Duration = Duration::from_secs(60);

/// How long a string [`write_ballooning_script`] grows, in bytes.
///
/// Growing a 16 MiB string takes a `/bin/sh` from about 1.2 MB resident to
/// about 166 MB: the doubling loop's intermediate allocations stay in its
/// heap. The string alone is twice [`BREACH_LIMIT`].
pub(crate) const BALLOON_BYTES: u64 = 16 * 1024 * 1024;

/// The `max_memory` the ballooning sheep is given: above a bare shell's 1.2 MB
/// resident set and half the string it grows, so it is under the ceiling
/// before and over it after.
pub(crate) const BREACH_LIMIT: &str = "8M";

/// The `listen_timeout` [`write_never_ready_flockfile`] gives its sheep.
///
/// Nothing races it: the sheep never signals, so this is a delay before a
/// certainty. The daemon takes a timed-out `wait_ready` sheep `Online`, so the
/// elapse shows in `shep flock` too.
pub(crate) const NEVER_READY_TIMEOUT: &str = "1s";

/// What [`write_rotating_script`]'s sheep prints before the rotation, and
/// what must end up in the renamed archive rather than in the recreated log.
pub(crate) const ROTATE_BEFORE: &str = "before-the-rotation";

/// What the same sheep prints after it. Its arrival in the recreated file is
/// the whole assertion.
pub(crate) const ROTATE_AFTER: &str = "after-the-rotation";

/// The daemon record the two log-plane cases read out of
/// `$SHEP_HOME/logs/shepd.err.log`, written at `WARN` by
/// `Actor::handle_ready_result`.
///
/// One owner for the string: the two cases assert opposite things about the
/// same record, and a drifted pair would keep passing while proving nothing.
pub(crate) const READINESS_RECORD: &str = "readiness deadline elapsed";

/// How long [`counting_lines`] waits for a counter sheep to reach a line
/// count, and [`wait_for_pid`] for a sheep to be online with a pid.
///
/// The counter emits five lines a second and the longest wait here is six
/// lines, so twenty seconds is a loaded runner's margin, not the sheep's pace.
#[cfg(unix)]
pub(crate) const HANDOVER_DEADLINE: Duration = Duration::from_secs(20);

/// Gap between [`counting_lines`]'s reads.
#[cfg(unix)]
pub(crate) const HANDOVER_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long [`a_sheep_owed_a_restart_still_gets_one_after_a_daemon_reload`]
/// waits for a re-armed backoff to fire.
///
/// The app's `restart_delay` is 8s and the successor re-arms with the same
/// figure, so five times that is a loaded runner's margin.
#[cfg(unix)]
pub(crate) const RESTARTED_DEADLINE: Duration = Duration::from_secs(40);
