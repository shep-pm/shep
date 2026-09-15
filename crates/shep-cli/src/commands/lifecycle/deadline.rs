//! How long the client waits for a staged start.
//!
//! Budget arithmetic over the per-app `listen_timeout`s a staged start
//! sums, mirroring the daemon's own per-stage slack.

use std::time::Duration;

use shep_client::START_DEADLINE;
use shep_core::config::AppConfig;

/// Slack the client allows PER APP over the summed readiness deadlines of a
/// staged start.
///
/// Mirrors the daemon's own `boot_order::STAGE_SLACK`, which is 5s and is
/// spent per stage, not per start: the daemon's worst case over N stages is
/// the summed `listen_timeout`s plus N times that. A flat 10s therefore made
/// the client give up BEFORE the daemon from three stages on, which is the
/// opposite of what it was for. [`staged_start_deadline`] already sums the
/// timeouts under the worst case of one app per stage, so multiplying the
/// slack the same way keeps the two halves reading the same graph.
///
/// The round trip needs nothing here: `Client::request_with_deadline` waits
/// the deadline it asked for plus `DEADLINE_GRACE` before calling it a
/// timeout of its own.
const STAGED_START_SLACK: Duration = Duration::from_secs(5);

/// The deadline a staged start needs.
///
/// `foreground` reaches this too, for `shep runtime` and `shep dev`: both
/// start a whole Flockfile in one request, and a flat budget there takes a
/// container's flock down over a chain that was doing what it was asked.
///
/// The daemon runs a `Request::Start` batch in dependency order and holds
/// each stage until its members settle, so the reply lands only after the
/// last one. The worst case is every app in its own stage, each held for its
/// own `listen_timeout`, and the sum of those is the bound. Never under
/// [`START_DEADLINE`], which is what a one-stage batch of cold spawns already
/// needs.
///
/// The CLI computes it rather than asking the daemon because the CLI is what
/// holds every [`AppConfig`] going out. `shep bleats -f` asks for a longer
/// deadline the same way.
///
/// The daemon clamps anything past its own `MAX_DEADLINE_MS` (60s), so a
/// batch whose stages sum past that is bounded there whatever this returns;
/// asking for more only makes the client outlast the daemon rather than the
/// other way round.
///
/// Which is why this is not clamped to 60s here. Past that line the daemon
/// answers `DeadlineExceeded` while the start carries on running: the budget
/// bounds the reply, not the actor's work. A client that gave up at the same
/// moment would race that answer and print a local timeout instead of the
/// daemon's own, and the operator reconciles with `shep flock` either way.
/// The slack is spent per app as well, so at the default 3s
/// `listen_timeout` each app costs 8s and the ceiling is crossed at eight of
/// them, which is an ordinary flock rather than a pathological one.
pub(crate) fn staged_start_deadline(apps: &[AppConfig]) -> Duration {
    let stages: Duration = apps
        .iter()
        .map(|app| app.listen_timeout.as_duration())
        .sum();
    let slack = STAGED_START_SLACK * u32::try_from(apps.len()).unwrap_or(u32::MAX);
    (stages + slack).max(START_DEADLINE)
}

