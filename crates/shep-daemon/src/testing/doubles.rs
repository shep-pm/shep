// The scripted doubles the fixture harnesses run on: a sampler, a prober, a
// clock and an enforcer that replay what a test hands them, plus the readings
// and configs built from them. Re-exported through `crate::testing`.
use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};
use shep_core::config::{ProbeConfig, ProbeKind, ProbeTarget};
use shep_core::values::{MemSize, UpDuration};

use crate::cron::Clock;
use crate::limits::LimitEnforcer;
use crate::limits::sample::{MemorySampler, ProcessIdentity, ProcessRss};
use crate::probes::{ProbeFailure, Prober};
use crate::rpc::PidSignals;
use crate::runner::RunnerError;
use shep_core::signals::OperatorSignal;

// Records rather than delivers: a test asserts that a refused lamb was sent
// nothing, which a real `kill` on a scripted pid could not show.
#[derive(Debug, Default)]
pub(crate) struct RecordingPidSignals {
    sent: Mutex<Vec<(u32, OperatorSignal)>>,
}

impl RecordingPidSignals {
    /// Every [`PidSignals::send`] call so far, in order.
    pub(crate) fn sent(&self) -> Vec<(u32, OperatorSignal)> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl PidSignals for RecordingPidSignals {
    fn send(&self, pid: u32, sig: OperatorSignal) -> Result<(), RunnerError> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((pid, sig));
        Ok(())
    }
}

/// One [`LimitEnforcer::arm`] call, exactly as the registry made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArmCall {
    /// The sheep's id.
    pub(crate) id: u32,
    /// The pid the arming was made against.
    pub(crate) root_pid: u32,
    /// The ceiling it was armed with.
    pub(crate) limit: MemSize,
}

// A recording fake rather than a `PollingEnforcer` over a scripted sampler:
// the registry tests assert on the arguments an arming was made with, the pid
// above all, and a real enforcer only reports the consequence of a reading.
#[derive(Debug, Default)]
pub(crate) struct RecordingEnforcer {
    arms: Mutex<Vec<ArmCall>>,
    disarms: Mutex<Vec<u32>>,
}

impl RecordingEnforcer {
    /// Every [`LimitEnforcer::arm`] call so far, in order.
    pub(crate) fn arms(&self) -> Vec<ArmCall> {
        self.arms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Every id [`LimitEnforcer::disarm`] was called with, in order.
    pub(crate) fn disarms(&self) -> Vec<u32> {
        self.disarms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl LimitEnforcer for RecordingEnforcer {
    fn arm(&self, id: u32, root_pid: u32, limit: MemSize) {
        self.arms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(ArmCall {
                id,
                root_pid,
                limit,
            });
    }

    fn disarm(&self, id: u32) {
        self.disarms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(id);
    }
}

// `start_paused = true` freezes `tokio::time`, but `chrono::Utc::now()` keeps
// reading the real system clock. Deriving wall time as `epoch + elapsed` makes
// `tokio::time::advance` move both by the same amount, so a whole day of
// schedule fits in a test that takes microseconds.
pub(crate) struct TestClock {
    epoch: DateTime<Utc>,
    started: tokio::time::Instant,
    // Counts `now_utc` calls. The only observable difference between two
    // `max_sleep` values is how often the loop wakes, and on a paused clock a
    // wakeup leaves no other trace.
    reads: AtomicUsize,
}

impl TestClock {
    /// A clock that reads `epoch` at construction and advances in lockstep
    /// with `tokio::time` from there.
    pub(crate) fn starting_at(epoch: DateTime<Utc>) -> Self {
        Self {
            epoch,
            started: tokio::time::Instant::now(),
            reads: AtomicUsize::new(0),
        }
    }

    /// How many times [`Clock::now_utc`] has been called.
    pub(crate) fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }
}

impl Clock for TestClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        // `chrono::Duration::from_std` is fallible over its full range, but a
        // test clock cannot run long enough to overflow it, so this saturates
        // rather than panicking.
        let elapsed =
            chrono::Duration::from_std(self.started.elapsed()).unwrap_or(chrono::Duration::MAX);
        self.epoch + elapsed
    }
}

/// A reading with no CPU time on it, for the memory cases.
pub(crate) fn rss(pid: u32, parent: Option<u32>, bytes: u64) -> ProcessRss {
    rss_cpu(pid, parent, bytes, 0)
}

/// A reading carrying both quantities, for the CPU cases.
pub(crate) fn rss_cpu(pid: u32, parent: Option<u32>, bytes: u64, cpu_ms: u64) -> ProcessRss {
    ProcessRss {
        pid,
        parent,
        bytes,
        cpu_ms,
    }
}

// A scripted sequence rather than one fixed table: the polling memory-limit
// enforcer's tests need the reading to change between polls, such as a tree
// that crosses its limit only on the third tick.
pub(crate) struct ScriptedSampler {
    readings: Vec<Vec<ProcessRss>>,
    calls: AtomicUsize,
    // Empty for every sampler built through `new`, which is what makes those
    // samplers behave like the trait's default `identify`: an empty table is
    // returned on every call rather than replayed against `identities`.
    identities: Vec<Vec<ProcessIdentity>>,
    identify_calls: AtomicUsize,
}

impl ScriptedSampler {
    /// A sampler that replays `readings` in order, one per [`MemorySampler::sample`]
    /// call, repeating the last reading once the script is exhausted.
    ///
    /// `identify` is left unscripted; see [`Self::identifying`] for a sampler
    /// that answers it too.
    pub(crate) fn new(readings: Vec<Vec<ProcessRss>>) -> Self {
        // A script with nothing to replay is a fixture bug: failing here beats
        // an index-out-of-bounds panic three frames away inside `sample`.
        assert!(
            !readings.is_empty(),
            "ScriptedSampler needs at least one reading to replay"
        );
        Self {
            readings,
            calls: AtomicUsize::new(0),
            identities: Vec::new(),
            identify_calls: AtomicUsize::new(0),
        }
    }

    /// A sampler that answers `identify` from `tables`, one per call, and
    /// `sample` from an empty machine.
    ///
    /// A separate constructor because the two halves are scripted
    /// independently and every caller of `new` wants the default `identify`.
    pub(crate) fn identifying(tables: Vec<Vec<ProcessIdentity>>) -> Self {
        assert!(
            !tables.is_empty(),
            "ScriptedSampler::identifying needs at least one table to replay"
        );
        Self {
            readings: vec![vec![]],
            calls: AtomicUsize::new(0),
            identities: tables,
            identify_calls: AtomicUsize::new(0),
        }
    }

    /// How many times [`MemorySampler::sample`] has been called.
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl MemorySampler for ScriptedSampler {
    fn sample(&self) -> Vec<ProcessRss> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let index = call.min(self.readings.len() - 1);
        self.readings[index].clone()
    }

    fn identify(&self) -> Vec<ProcessIdentity> {
        // A sampler built through `new` has nothing scripted here, and reads
        // exactly as the trait's own default would.
        if self.identities.is_empty() {
            return Vec::new();
        }
        let call = self.identify_calls.fetch_add(1, Ordering::Relaxed);
        let index = call.min(self.identities.len() - 1);
        self.identities[index].clone()
    }
}

// A scripted sequence rather than one fixed outcome: the liveness loop's tests
// need pass/fail to change between polls. Unlike `ScriptedSampler`, an empty
// script is not a fixture bug here; `new(vec![])` means never fails, the
// neutral value for a prober nobody scripted.
pub(crate) struct ScriptedProber {
    script: Vec<Result<(), ProbeFailure>>,
    calls: AtomicUsize,
    delay: Duration,
    // The `timeout` argument of the most recent `probe()` call, in
    // milliseconds. `probe()` ignores the parameter itself, so recording it is
    // the only way a caller wiring `interval` where `timeout` belongs fails.
    last_timeout_ms: AtomicU64,
}

impl ScriptedProber {
    /// A prober that replays `script` in order, one outcome per
    /// [`Prober::probe`] call, repeating the last outcome once the script is
    /// exhausted. `script: vec![]` returns `Ok(())` forever.
    pub(crate) fn new(script: Vec<Result<(), ProbeFailure>>) -> Self {
        Self {
            script,
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            last_timeout_ms: AtomicU64::new(0),
        }
    }

    /// Every subsequent `probe()` call sleeps `delay` on the (paused) tokio
    /// clock before returning its scripted outcome.
    ///
    /// The delay is honoured even when it exceeds a `probe()` call's own
    /// `timeout`, which this fake ignores: a case reaching for `with_delay`
    /// wants a probe that passes or fails slowly, not one that times out.
    pub(crate) fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// How many times [`Prober::probe`] has been called.
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    /// The `timeout` argument passed to the most recently started
    /// [`Prober::probe`] call. `Duration::ZERO` before the first call.
    pub(crate) fn last_timeout(&self) -> Duration {
        Duration::from_millis(self.last_timeout_ms.load(Ordering::Relaxed))
    }
}

impl Prober for ScriptedProber {
    fn probe<'a>(
        &'a self,
        _target: &'a ProbeTarget,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), ProbeFailure>> + Send + 'a>> {
        Box::pin(async move {
            // Counted at call start, not completion: a count advancing only
            // after `with_delay`'s sleep would make "no further calls" and
            // "one call in flight" indistinguishable.
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            // Test timeouts are single-digit seconds, so this cast cannot
            // truncate, and a wrong value here only breaks an assertion.
            self.last_timeout_ms
                .store(timeout.as_millis() as u64, Ordering::Relaxed);
            tokio::time::sleep(self.delay).await;
            if self.script.is_empty() {
                return Ok(());
            }
            let index = call.min(self.script.len() - 1);
            self.script[index].clone()
        })
    }
}

/// Builds a `ProbeConfig` with fixture-friendly `interval`/`timeout`
/// (production defaults: 10s/5s) and `failure_threshold` at its production
/// default of 3.
pub(crate) fn probe_config(kind: ProbeKind, target: &str) -> ProbeConfig {
    ProbeConfig {
        kind,
        target: target.to_string(),
        interval: UpDuration::from_millis(10_000),
        timeout: UpDuration::from_millis(5_000),
        failure_threshold: 3,
    }
}
