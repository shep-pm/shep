//! Fixtures and helpers shared by this module's tests.

use crate::lookout::source::{HostSample, Local};
use crate::lookout::tail::Tail;
use shep_core::paths::ShepPaths;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The resolved layout for a given `home`, matching how the literal
/// `home`/`daemon_config`/`socket_default` triples below were derived by
/// hand: `SHEP_HOME` is `home` itself, so `field::resolve` appends no `.shep`.
pub(super) fn test_paths(home: &Path) -> ShepPaths {
    let home_str = home.to_string_lossy().into_owned();
    ShepPaths::resolve(&|key| (key == "SHEP_HOME").then(|| home_str.clone()), home)
}

/// A `source::Local` that touches no disk: a fixed sample, a fixed tail, and a
/// count of each call. `Arc`, since `ui_event_loop::run_ui` takes the reader by value.
#[derive(Clone, Default)]
pub(super) struct FakeLocal {
    pub(super) sample: Option<HostSample>,
    pub(super) hosts: Arc<AtomicUsize>,
    pub(super) tails: Arc<AtomicUsize>,
}

impl Local for FakeLocal {
    fn host(&mut self) -> Option<HostSample> {
        self.hosts.fetch_add(1, Ordering::Relaxed);
        self.sample
    }

    fn tail(&mut self, _out: Option<&Path>, _err: Option<&Path>) -> Tail {
        self.tails.fetch_add(1, Ordering::Relaxed);
        Tail::default()
    }
}
