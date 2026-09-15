//! Fixtures and helpers shared by this module's tests.

use super::filter::{RootedFilter, WatchFilter};
use super::group::run_group;
use crate::bus::SharedEvent;
use crate::fake::{ProcScript, ScriptedRunner};
use crate::supervisor::SupervisorHandle;
use crate::supervisor::spawn_supervisor;
use crate::testing::test_paths;
use crate::watch::source::WatchBatch;
use core::time::Duration;
use shep_core::config::{AppConfig, normalize};
use shep_core::protocol::{BusEvent, ProcessEventKind, ProcessInfo};
use std::path::{Path, PathBuf};
use tokio::sync::broadcast;
use tokio::sync::mpsc;

// ------------------------------------------------------------------
// The group loop: paused clock, driven by a hand-fed channel.
// ------------------------------------------------------------------
/// Generous bound on how long a paused-clock test waits for a restart
/// before concluding the loop is broken. Costs no real wall-clock time:
/// auto-advance walks straight to it once nothing else is ready.
pub(super) const EVENT_WAIT: Duration = Duration::from_secs(30);

/// How many `tokio::task::yield_now` rounds [`settle`] spends: headroom
/// for the group loop, the actor and a sheep's task each needing a
/// scheduling turn. Never advances the paused clock itself.
const SETTLE_YIELDS: usize = 16;

pub(super) async fn settle() {
    for _ in 0..SETTLE_YIELDS {
        tokio::task::yield_now().await;
    }
}

/// A supervisor engine over a scripted runner, its bus receiver, and
/// tempdir.
pub(super) fn spawn_test_fixture(
    scripts: Vec<ProcScript>,
) -> (
    SupervisorHandle,
    broadcast::Receiver<SharedEvent>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let (events, rx) = crate::bus::test_bus(64);
    let runner = ScriptedRunner::new(scripts);
    let handle = spawn_supervisor(runner, test_paths(&dir), events);
    (handle, rx, dir)
}

pub(super) async fn start_app(
    handle: &SupervisorHandle,
    name: &str,
    instances: u32,
) -> Vec<ProcessInfo> {
    let mut app = AppConfig::minimal(name, "./srv");
    app.instances = instances;
    handle.start(vec![normalize(app).unwrap()]).await.unwrap()
}

/// Waits up to `deadline` for the next `Restart` for `name`; times out
/// rather than hanging.
pub(super) async fn expect_restart(
    rx: &mut broadcast::Receiver<SharedEvent>,
    name: &str,
    deadline: Duration,
) -> ProcessInfo {
    loop {
        match tokio::time::timeout(deadline, rx.recv())
            .await
            .map(|received| received.map(|event| event.to_event()))
        {
            Ok(Ok(BusEvent::Process {
                event: ProcessEventKind::Restart,
                info,
                ..
            })) if info.name == name => return info,
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(err)) => panic!("event stream closed before a restart of {name}: {err}"),
            Err(_) => panic!("timed out waiting for a watch-triggered restart of {name}"),
        }
    }
}

/// Waits up to `window` for a `Restart` for `name`, panicking if one
/// arrives. A real poll, not a bare `try_recv`: a restart working its
/// way through the loop, actor and sheep-task round trip needs the
/// scheduling rounds a bounded `recv` gives it.
pub(super) async fn assert_no_restart_within(
    rx: &mut broadcast::Receiver<SharedEvent>,
    name: &str,
    window: Duration,
) {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv())
            .await
            .map(|received| received.map(|event| event.to_event()))
        {
            Err(_) => return, // window elapsed with nothing matching, expected
            Ok(Ok(BusEvent::Process {
                event: ProcessEventKind::Restart,
                info,
                ..
            })) if info.name == name => {
                panic!(
                    "unexpected watch-triggered restart of {name} observed (restarts={})",
                    info.restarts
                );
            }
            Ok(Ok(_)) => continue,
            // A negative assertion cannot skip events: a dropped one
            // may be the very `Restart` this forbids, so it must fail
            // loudly instead of failing open.
            Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                panic!(
                    "event stream lagged by {skipped} while checking for no restart of \
                     {name}: a skipped event may have been the restart this forbids"
                )
            }
            Ok(Err(err)) => {
                panic!("event channel closed while checking for no restart of {name}: {err}")
            }
        }
    }
}

pub(super) fn matches_everything(root: PathBuf) -> RootedFilter {
    RootedFilter {
        root,
        filter: WatchFilter::new(&[], &[]).unwrap(),
    }
}

/// A watch group over `root` treating every path under it as a trigger,
/// with the sender its batches go in through.
///
/// The two cases that need a narrower filter build their own
/// [`RootedFilter`] and spawn inline.
pub(super) fn spawn_group_matching_everything(
    root: &Path,
    name: &str,
    handle: &SupervisorHandle,
) -> (
    mpsc::UnboundedSender<WatchBatch>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, group_rx) = mpsc::unbounded_channel();
    let group = tokio::spawn(run_group(
        name.to_string(),
        matches_everything(root.to_path_buf()),
        group_rx,
        handle.clone(),
    ));
    (tx, group)
}

/// Builds a batch with `rescan: false`.
pub(super) fn changed(paths: Vec<PathBuf>) -> WatchBatch {
    WatchBatch {
        paths,
        rescan: false,
    }
}

/// A rescan in its path-less (inotify) shape: notify dropped events and
/// wants the tree re-read. `source`'s own tests cover the macOS shape,
/// which carries the root alongside the flag.
pub(super) fn rescan_marker() -> WatchBatch {
    WatchBatch {
        paths: Vec::new(),
        rescan: true,
    }
}

// ------------------------------------------------------------------
// The single-flight property: the group loop against generated batch
// sequences and generated restart durations.
// ------------------------------------------------------------------
/// Most batches one generated case feeds the group.
pub(super) const MAX_BATCHES: usize = 8;

/// Scripted procs one generated case may spawn.
///
/// Sized against the maximum a broken loop can demand, not a correct
/// one: the worst case is one restart per batch, plus the initial
/// start. A pool sized to a correct run would swallow the extra
/// restarts this property exists to see.
pub(super) const SINGLE_FLIGHT_SCRIPTS: usize = MAX_BATCHES + 4;

/// One generated debounced batch: whether its path triggers a restart,
/// and how long after the previous send it arrives.
#[derive(Debug, Clone, Copy)]
pub(super) struct Batch {
    pub(super) triggers: bool,
    pub(super) gap: Duration,
}

fn gap_strategy() -> impl proptest::strategy::Strategy<Value = Duration> {
    use proptest::strategy::Strategy as _; // `prop_map` below
    // Zero (a send landing mid-restart) is drawn half the time. The
    // other two arms straddle the generated kill timeout's own
    // 200..2000ms range, so batches also arrive partway through a
    // ladder and well after one has finished.
    proptest::prop_oneof![
        4 => proptest::strategy::Just(Duration::ZERO),
        3 => (1u64..2_000u64).prop_map(Duration::from_millis),
        1 => (2_000u64..6_000u64).prop_map(Duration::from_millis),
    ]
}

pub(super) fn batch_strategy() -> impl proptest::strategy::Strategy<Value = Batch> {
    use proptest::strategy::Strategy as _; // `prop_map` below
    (proptest::bool::ANY, gap_strategy()).prop_map(|(triggers, gap)| Batch { triggers, gap })
}

/// When a correct group loop finishes each restart, given the instants
/// its batches arrive at and how long one restart takes.
///
/// A strictly sequential model: it holds no notion of two restarts
/// overlapping, because the loop it models has none. Every arrival
/// already queued when the loop next looks is folded into one check,
/// and a restart occupies the model for exactly `restart` from the
/// moment that check decided to run it.
pub(super) fn expected_restart_instants(batches: &[Batch], restart: Duration) -> Vec<Duration> {
    let mut arrivals = Vec::with_capacity(batches.len());
    let mut at = Duration::ZERO;
    for batch in batches {
        at += batch.gap;
        arrivals.push((at, batch.triggers));
    }

    let mut finished = Vec::new();
    let mut idle_since = Duration::ZERO;
    let mut i = 0;
    while i < arrivals.len() {
        // The loop is parked on `recv` and wakes at the first arrival it
        // has not seen, or, if that already happened while it was busy,
        // the moment it became free again.
        let woke_at = arrivals[i].0.max(idle_since);
        let mut triggers = arrivals[i].1;
        i += 1;
        // ...and drains everything else already queued at that instant.
        while i < arrivals.len() && arrivals[i].0 <= woke_at {
            triggers |= arrivals[i].1;
            i += 1;
        }
        idle_since = if triggers {
            let done = woke_at + restart;
            finished.push(done);
            done
        } else {
            woke_at
        };
    }
    finished
}
