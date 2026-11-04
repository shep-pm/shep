//! Fixtures and helpers shared by this module's tests.

use crate::lookout::app::{App, Control};
use crate::lookout::source::{HostSample, Local};
use crate::lookout::tail::Tail;
use crate::lookout::theme::Palette;
use shep_core::paths::ShepPaths;
use shep_core::protocol::ProcessInfo;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// The resolved layout for a given `home`, matching how the literal
/// `home`/`daemon_config`/`socket_default` triples below were derived by
/// hand: `SHEP_HOME` is `home` itself, so `field::resolve` appends no `.shep`.
pub(super) fn test_paths(home: &Path) -> ShepPaths {
    let home_str = home.to_string_lossy().into_owned();
    ShepPaths::resolve(&|key| (key == "SHEP_HOME").then(|| home_str.clone()), home)
}

/// [`test_paths`] under the home most tests share, which no test writes to.
pub(super) fn shared_test_paths() -> ShepPaths {
    test_paths(Path::new("/tmp/shep-lookout-tests"))
}

/// A read-only `App` on the default palette, for a test that drives the loop
/// and does not care what the app starts with.
pub(super) fn read_only_app() -> App {
    App::new(
        Palette::detect(None, None, None),
        Control::ReadOnly,
        "/tmp/shep".to_string(),
        Instant::now(),
    )
}

/// A `source::Local` that touches no disk: a fixed sample, a fixed tail, and a
/// count of each call. `Arc`, since `ui_event_loop::run_ui` takes the reader by value.
#[derive(Clone, Default)]
pub(super) struct FakeLocal {
    pub(super) sample: Option<HostSample>,
    pub(super) hosts: Arc<AtomicUsize>,
    pub(super) tails: Arc<AtomicUsize>,
    pub(super) log_sizes: Arc<AtomicUsize>,
    pub(super) hint: Option<String>,
    pub(super) hints: Arc<AtomicUsize>,
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

    fn log_sizes(&mut self, _out: Option<&Path>, _err: Option<&Path>) -> Option<u64> {
        self.log_sizes.fetch_add(1, Ordering::Relaxed);
        None
    }

    fn hint(&mut self, _flock: &[ProcessInfo]) -> Option<String> {
        self.hints.fetch_add(1, Ordering::Relaxed);
        self.hint.clone()
    }
}

/// A `cargo` that records its own pid and then replaces the shell with a
/// `sleep`, so the recorded pid is the one that is still building when the
/// operator quits.
#[cfg(unix)]
pub(super) fn sleeping_cargo(pid_file: &Path) -> String {
    format!(
        "#!/bin/sh\necho $$ > {}\nexec sleep 30\n",
        pid_file.display()
    )
}

/// Whether `pid` is no longer running: reaped already (`ECHILD`), or reaped
/// here. A child still alive is the thing a test is looking for.
///
/// `WNOHANG` rather than `kill(pid, 0)`: a killed child tokio never reaped
/// is still a pid, and a zombie is not a running `cargo`.
#[cfg(unix)]
#[track_caller]
pub(super) fn is_gone(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;

    match waitpid(Pid::from_raw(cast_pid(pid)), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive) => false,
        Ok(_) | Err(Errno::ECHILD) => true,
        Err(err) => panic!("waitpid({pid}): {err}"),
    }
}

#[cfg(unix)]
fn cast_pid(pid: u32) -> i32 {
    i32::try_from(pid).expect("a pid fits an i32")
}

/// A recorded copy of shep-kelpie's `--schema` answer, as `kelpie-schema.json`
/// holds it.
pub(super) fn kelpie_answer() -> serde_json::Value {
    serde_json::from_str(include_str!("kelpie-schema.json")).expect("the recording is JSON")
}

/// The part of [`kelpie_answer`] a sheep's `kelpie` table is edited by.
pub(super) fn kelpie_sheep_schema() -> serde_json::Value {
    super::probe_sheep_dogs::sheep_schema(&kelpie_answer()).expect("the answer names x-shep-sheep")
}
