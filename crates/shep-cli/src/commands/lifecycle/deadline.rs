//! How long the client waits for a staged start.
//!
//! Budget arithmetic over the per-app `listen_timeout`s a staged start
//! sums, mirroring the daemon's own per-stage slack.

use std::time::Duration;

use shep_client::START_DEADLINE;
use shep_core::config::AppConfig;

/// Slack the client allows per app over the summed readiness deadlines of a
/// staged start.
///
/// Mirrors the daemon's own `boot_order::STAGE_SLACK`, which is spent per
/// stage: the daemon's worst case over N stages is the summed
/// `listen_timeout`s plus N times that. [`staged_start_deadline`] sums the
/// timeouts the same way.
const STAGED_START_SLACK: Duration = Duration::from_secs(5);

/// The deadline a staged start needs.
///
/// The daemon runs a `Request::Start` batch in dependency order and holds
/// each stage until its members settle, so the worst case is every app in
/// its own stage, each held for its own `listen_timeout`.
///
/// Not clamped to the daemon's 60s ceiling: past that it answers `DeadlineExceeded`.
pub(crate) fn staged_start_deadline(apps: &[AppConfig]) -> Duration {
    let stages: Duration = apps
        .iter()
        .map(|app| app.listen_timeout.as_duration())
        .sum();
    let slack = STAGED_START_SLACK * u32::try_from(apps.len()).unwrap_or(u32::MAX);
    (stages + slack).max(START_DEADLINE)
}
