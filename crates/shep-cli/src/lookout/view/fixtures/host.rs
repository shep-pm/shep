//! The host strip's own readings.

use shep_core::protocol::ProcessInfo;

use crate::lookout::app::{App, Msg};
use crate::lookout::source::HostSample;

use super::flock::app_with;
use super::palette::plain;

/// One plausible host reading: the same numbers the gallery's scenes use, so
/// a failure here and a frame under review name the same figures.
pub fn sample() -> HostSample {
    HostSample {
        load: (2.31, 4.10, 3.88),
        cores: Some(10),
        memory_total_bytes: 32 << 30,
        memory_used_bytes: 12 * (1 << 30) + (410 << 20),
        uptime_seconds: 6 * 86_400 + 3 * 3_600,
    }
}

/// A dashboard that has had one host sample applied.
pub fn with_host(sample: HostSample, flock: Vec<ProcessInfo>) -> App {
    let mut app = app_with(flock, plain());
    app.update(Msg::Host {
        sample: Some(sample),
    });
    app
}

/// A dashboard with no host reading. The two ways of having none are not the
/// same state: `unsupported: true` applies `Msg::Host { sample: None }`, the
/// signal a `sysinfo` that does not support the platform produces, and the
/// strip says so. `unsupported: false` applies no `Msg::Host` at all, the
/// state before the first heartbeat, and the strip says `not read yet`
/// instead.
pub fn with_host_none(flock: Vec<ProcessInfo>, unsupported: bool) -> App {
    let mut app = app_with(flock, plain());
    if unsupported {
        app.update(Msg::Host { sample: None });
    }
    app
}
