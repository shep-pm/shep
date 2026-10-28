//! `ReadinessSource` and `await_ready`: the gate between `starting` and
//! `online` (spec §7).
//!
//! A sheep whose app configures `wait_ready` or `readiness_probe` holds at
//! `starting` until [`await_ready`] resolves. `ReadinessSource::Heuristic`
//! is reachable from reload's `AwaitReady` state, and from an ordinary
//! `start` when the app's name is in a boot stage's gate set even though it
//! configures no signal of its own.
//!
//! A probe wait sleeps at most a quarter of its deadline between looks.

use core::future::Future;
use core::time::Duration;

use std::sync::Arc;

use tokio::sync::oneshot;

use shep_core::config::{AppConfig, ProbeConfig, ProbeTarget, ProbeTargetError};

use super::Prober;

/// Where a sheep's readiness signal comes from (spec §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessSource {
    /// `wait_ready = true`: the shepherd channel's `{"kind":"ready"}`.
    Channel,
    /// `readiness_probe` is set: the first passing probe.
    Probe(ProbeConfig, ProbeTarget),
    /// Neither is configured: readiness is the deadline elapsing.
    Heuristic,
}

impl ReadinessSource {
    /// Derives the source from an app's configuration.
    ///
    /// `wait_ready` wins over `readiness_probe` when both are set: the
    /// channel is the app telling us directly.
    ///
    /// # Errors
    ///
    /// Whatever [`ProbeTarget::parse`] returns for the app's
    /// `readiness_probe` target. `normalize` already rejects every case in
    /// practice, so an `Err` here means the caller adopted an app that
    /// never went through it.
    pub fn of(config: &AppConfig) -> Result<Self, ProbeTargetError> {
        if config.wait_ready {
            return Ok(Self::Channel);
        }
        match &config.readiness_probe {
            Some(probe) => Ok(Self::Probe(probe.clone(), ProbeTarget::parse(probe)?)),
            None => Ok(Self::Heuristic),
        }
    }

    /// [`Self::of`] on a config that has already been through `normalize`.
    ///
    /// Every supervisor site reads the source off a `ResolvedApp`, which
    /// `normalize` built by running [`ProbeTarget::parse`] over its
    /// `readiness_probe`.
    /// # Panics
    /// Never for a `ResolvedApp`. A panic means an app reached the supervisor without `normalize`.
    #[track_caller]
    pub(crate) fn of_normalized(config: &AppConfig) -> Self {
        Self::of(config).expect("ResolvedApp already passed ProbeTarget::parse in normalize")
    }
}

/// How many probe gaps a readiness wait splits its deadline into.
///
/// Four is the fewest that covers a process taking most of the deadline to
/// bind: at the 3000ms `listen_timeout` default the sleep between looks is
/// 750ms, so a prober that answers at once looks at 0, 750ms, 1500ms and
/// 2250ms. Fewer gaps leaves a wider window in which a process that has
/// bound nothing goes unlooked at.
const READINESS_PROBE_GAPS: u32 = 4;

/// Floor on the gap between readiness probes.
///
/// `deadline / READINESS_PROBE_GAPS` is zero for any deadline under
/// four milliseconds, and a zero sleep would spin the loop for as long as the
/// deadline lasts. `shep-core`'s `normalize` floors a probe's own `interval`
/// at 1ms and says nothing about `listen_timeout`, so a tiny deadline is
/// reachable.
const MIN_READINESS_POLL: Duration = Duration::from_millis(1);

/// How a readiness wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// The signal arrived inside the deadline.
    Ready,
    /// The deadline elapsed with no signal.
    TimedOut,
}

/// Waits for `source`'s readiness signal, giving up after `deadline`.
///
/// `channel` carries the shepherd channel's ready notification and is read
/// only by [`ReadinessSource::Channel`]; a `Probe`/`Heuristic` wait ignores
/// it entirely, so a `{"kind":"ready"}` write against a probe- or
/// heuristic-gated sheep has nothing here to wake.
pub async fn await_ready(
    source: &ReadinessSource,
    deadline: Duration,
    channel: oneshot::Receiver<()>,
    prober: Arc<dyn Prober>,
) -> Readiness {
    // No `_` arm: a fourth source added later fails `cargo check` right
    // here instead of silently taking a default this function never chose.
    match source {
        ReadinessSource::Channel => {
            // A dropped sender must not resolve this wait early, since
            // deciding here would race the exit path. A closed channel
            // switches to a future that never resolves, leaving the deadline
            // in `within_deadline` as the only thing that ends the wait.
            let wait_for_signal = async {
                if channel.await.is_err() {
                    core::future::pending::<()>().await;
                }
            };
            within_deadline(deadline, wait_for_signal).await
        }
        ReadinessSource::Probe(config, target) => {
            // Not floored at `MIN_PROBE_INTERVAL` like `spawn_liveness_task`:
            // this loop is bounded by `deadline`, not infinite, and
            // flooring it would hold a fast, already-answering app at
            // `starting` for a needless second.
            let interval = readiness_poll_interval(config, deadline);
            let timeout = config.timeout.as_duration();
            // Probe first, sleep after: the first probe lands at t=0, and
            // each sleep runs from the end of the probe before it.
            // `failure_threshold` is never read here: it is a liveness
            // concept, not a readiness one.
            let poll = async {
                loop {
                    if prober.probe(target, timeout).await.is_ok() {
                        return;
                    }
                    tokio::time::sleep(interval).await;
                }
            };
            within_deadline(deadline, poll).await
        }
        ReadinessSource::Heuristic => {
            // The elapse is the signal here: the one branch that returns
            // `Ready` from a deadline racing nothing, not an event beating
            // one.
            tokio::time::sleep(deadline).await;
            Readiness::Ready
        }
    }
}

/// Runs `fut` against `deadline`: `Ready` if it finishes inside, `TimedOut`
/// if the deadline elapses first.
///
/// The one place a readiness deadline becomes a [`Readiness`]. A future that
/// resolves only by the deadline elapsing (a closed channel switched to
/// `pending`) ends here as `TimedOut`, never early.
async fn within_deadline<F: Future<Output = ()>>(deadline: Duration, fut: F) -> Readiness {
    match tokio::time::timeout(deadline, fut).await {
        Ok(()) => Readiness::Ready,
        Err(_elapsed) => Readiness::TimedOut,
    }
}

/// The gap [`await_ready`] sleeps after each probe of `config` under `deadline`.
///
/// `config.interval` as written, shortened only where following it would
/// spend the whole wait on the probe that lands at t=0. A shorter interval
/// is never raised: an app polled every 20ms leaves `starting` 20ms after it
/// binds, whatever its deadline. A probe that takes time to answer moves the
/// looks after it later, not the gap.
fn readiness_poll_interval(config: &ProbeConfig, deadline: Duration) -> Duration {
    config
        .interval
        .as_duration()
        .min(deadline / READINESS_PROBE_GAPS)
        .max(MIN_READINESS_POLL)
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use std::sync::Arc;

    use tokio::sync::oneshot;

    use super::*;
    use crate::probes::ProbeFailure;
    use crate::testing::{ScriptedProber, probe_config};
    use shep_core::config::ProbeKind;
    use shep_core::values::UpDuration;

    /// A prober `Channel`/`Heuristic` waits never call: an empty script
    /// would panic on the first `probe()` call, proving those arms never
    /// reach the prober.
    fn unused_prober() -> Arc<dyn Prober> {
        Arc::new(ScriptedProber::new(vec![]))
    }

    fn tcp_source(interval_ms: u64) -> ReadinessSource {
        let config = ProbeConfig {
            interval: UpDuration::from_millis(interval_ms),
            ..probe_config(ProbeKind::Tcp, "localhost:5432")
        };
        let target = ProbeTarget::parse(&config).expect("a valid tcp target parses");
        ReadinessSource::Probe(config, target)
    }

    #[tokio::test(start_paused = true)]
    async fn channel_ready_before_the_deadline_resolves_at_signal_time() {
        let (tx, rx) = oneshot::channel();
        let start = tokio::time::Instant::now();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = tx.send(());
        });

        let readiness = await_ready(
            &ReadinessSource::Channel,
            Duration::from_secs(3),
            rx,
            unused_prober(),
        )
        .await;

        assert_eq!(readiness, Readiness::Ready);
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(500)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn channel_with_no_signal_times_out_at_exactly_the_deadline() {
        let (_tx, rx) = oneshot::channel(); // held: never signals, never drops
        let start = tokio::time::Instant::now();
        let deadline = Duration::from_secs(3);

        let readiness = await_ready(&ReadinessSource::Channel, deadline, rx, unused_prober()).await;

        assert_eq!(readiness, Readiness::TimedOut);
        assert_eq!(tokio::time::Instant::now() - start, deadline);
    }

    #[tokio::test(start_paused = true)]
    async fn channel_dropped_without_signalling_times_out_at_the_deadline_not_immediately() {
        let (tx, rx) = oneshot::channel();
        drop(tx);
        let start = tokio::time::Instant::now();
        let deadline = Duration::from_secs(3);

        let readiness = await_ready(&ReadinessSource::Channel, deadline, rx, unused_prober()).await;

        assert_eq!(readiness, Readiness::TimedOut);
        assert_eq!(tokio::time::Instant::now() - start, deadline);
    }

    #[tokio::test(start_paused = true)]
    async fn probe_becomes_ready_two_failures_then_a_pass_at_two_intervals() {
        let source = tcp_source(1_000);
        let (_tx, rx) = oneshot::channel();
        let prober = Arc::new(ScriptedProber::new(vec![
            Err(ProbeFailure::Timeout),
            Err(ProbeFailure::Timeout),
            Ok(()),
        ]));
        let start = tokio::time::Instant::now();

        let readiness = await_ready(&source, Duration::from_secs(10), rx, prober).await;

        assert_eq!(readiness, Readiness::Ready);
        assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn probe_ready_immediately_when_the_first_probe_passes() {
        let source = tcp_source(1_000);
        let (_tx, rx) = oneshot::channel();
        let prober = Arc::new(ScriptedProber::new(vec![Ok(())]));
        let start = tokio::time::Instant::now();

        let readiness = await_ready(&source, Duration::from_secs(10), rx, prober).await;

        assert_eq!(readiness, Readiness::Ready);
        assert_eq!(
            tokio::time::Instant::now(),
            start,
            "the first probe must run at t=0, not after one interval"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn probe_that_always_fails_times_out_at_the_deadline() {
        let source = tcp_source(1_000);
        let (_tx, rx) = oneshot::channel();
        let prober = Arc::new(ScriptedProber::new(vec![Err(ProbeFailure::Timeout)]));
        let start = tokio::time::Instant::now();
        let deadline = Duration::from_secs(3);

        let readiness = await_ready(&source, deadline, rx, prober).await;

        assert_eq!(readiness, Readiness::TimedOut);
        assert_eq!(tokio::time::Instant::now() - start, deadline);
    }

    // Both defaults at once: a 10s `interval` under a 3000ms deadline. The
    // first probe lands at t=0, where nothing has bound yet, so a wait that
    // follows `interval` here can only ever time out.
    #[tokio::test(start_paused = true)]
    async fn a_probe_interval_past_the_deadline_still_polls_inside_it() {
        let source = tcp_source(10_000);
        let (_tx, rx) = oneshot::channel();
        let prober = Arc::new(ScriptedProber::new(vec![
            Err(ProbeFailure::Transport("connection refused".to_string())),
            Ok(()),
        ]));
        let start = tokio::time::Instant::now();

        let readiness = await_ready(&source, Duration::from_secs(3), rx, prober).await;

        assert_eq!(readiness, Readiness::Ready);
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(750),
            "the second probe must land a quarter of the deadline in, not one 10s interval in"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_holds_a_probe_every_quarter_of_it_at_the_default_interval() {
        let source = tcp_source(10_000);
        let (_tx, rx) = oneshot::channel();
        let prober = Arc::new(ScriptedProber::new(vec![Err(ProbeFailure::Timeout)]));
        let start = tokio::time::Instant::now();
        let deadline = Duration::from_secs(3);

        let dyn_prober = Arc::clone(&prober) as Arc<dyn Prober>;
        let readiness = await_ready(&source, deadline, rx, dyn_prober).await;

        assert_eq!(readiness, Readiness::TimedOut);
        assert_eq!(
            tokio::time::Instant::now() - start,
            deadline,
            "the extra probes must not end the wait early"
        );
        assert!(
            prober.calls() >= 4,
            "a 3s deadline split into 750ms gaps looks at 0ms, 750ms, 1500ms and 2250ms; \
             following the 10s interval instead gives one probe and no more"
        );
    }

    // The cap bounds the sleep, not the landing times. A 1s probe plus a
    // 750ms sleep fits twice inside a 3s deadline and leaves no room for a
    // third look, where two fast probes plus two sleeps fit four.
    #[tokio::test(start_paused = true)]
    async fn a_probe_that_takes_a_second_to_answer_shifts_the_looks_after_it() {
        let source = tcp_source(10_000);
        let (_tx, rx) = oneshot::channel();
        let prober = Arc::new(
            ScriptedProber::new(vec![Err(ProbeFailure::Timeout)])
                .with_delay(Duration::from_secs(1)),
        );
        let start = tokio::time::Instant::now();
        let deadline = Duration::from_secs(3);

        let dyn_prober = Arc::clone(&prober) as Arc<dyn Prober>;
        let readiness = await_ready(&source, deadline, rx, dyn_prober).await;

        assert_eq!(readiness, Readiness::TimedOut);
        assert_eq!(tokio::time::Instant::now() - start, deadline);
        assert_eq!(
            prober.calls(),
            2,
            "looks start at 0 and 1.75s, each sleep running from the end of the probe before it"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_interval_under_the_cap_is_never_raised_to_it() {
        let source = tcp_source(20);
        let (_tx, rx) = oneshot::channel();
        let prober = Arc::new(ScriptedProber::new(vec![
            Err(ProbeFailure::Transport("connection refused".to_string())),
            Ok(()),
        ]));
        let start = tokio::time::Instant::now();

        let readiness = await_ready(&source, Duration::from_secs(3), rx, prober).await;

        assert_eq!(readiness, Readiness::Ready);
        assert_eq!(
            tokio::time::Instant::now() - start,
            Duration::from_millis(20),
            "a 20ms poller leaves `starting` 20ms after it answers, not 750ms after"
        );
    }

    #[test]
    fn a_deadline_under_four_milliseconds_still_polls_at_one_ms() {
        let config = probe_config(ProbeKind::Tcp, "localhost:5432");

        assert_eq!(
            readiness_poll_interval(&config, Duration::from_millis(3)),
            Duration::from_millis(1),
            "3ms / 4 is zero, and a zero sleep spins the loop for the whole deadline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn heuristic_becomes_ready_when_its_deadline_elapses() {
        let (_tx, rx) = oneshot::channel();
        let start = tokio::time::Instant::now();
        let deadline = Duration::from_secs(3);

        let readiness =
            await_ready(&ReadinessSource::Heuristic, deadline, rx, unused_prober()).await;

        assert_eq!(readiness, Readiness::Ready);
        assert_eq!(tokio::time::Instant::now() - start, deadline);
    }

    // --- ReadinessSource::of ---

    #[test]
    fn wait_ready_wins_over_a_configured_probe() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.wait_ready = true;
        app.readiness_probe = Some(probe_config(ProbeKind::Tcp, "localhost:5432"));

        assert_eq!(ReadinessSource::of(&app), Ok(ReadinessSource::Channel));
    }

    #[test]
    fn a_configured_probe_is_used_when_wait_ready_is_unset() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.readiness_probe = Some(probe_config(ProbeKind::Tcp, "localhost:5432"));
        let target = ProbeTarget::parse(app.readiness_probe.as_ref().unwrap()).unwrap();

        assert_eq!(
            ReadinessSource::of(&app),
            Ok(ReadinessSource::Probe(
                app.readiness_probe.clone().unwrap(),
                target
            ))
        );
    }

    #[test]
    fn neither_configured_is_heuristic() {
        let app = AppConfig::minimal("web", "./srv");
        assert_eq!(ReadinessSource::of(&app), Ok(ReadinessSource::Heuristic));
    }

    #[test]
    fn of_normalized_agrees_with_of_for_channel_probe_and_heuristic() {
        let probe_cfg = probe_config(ProbeKind::Tcp, "localhost:5432");
        let target = ProbeTarget::parse(&probe_cfg).expect("a valid tcp target parses");

        let mut channel = AppConfig::minimal("web", "./srv");
        channel.wait_ready = true;
        channel.readiness_probe = Some(probe_cfg.clone());
        let mut probe = AppConfig::minimal("web", "./srv");
        probe.readiness_probe = Some(probe_cfg.clone());
        let heuristic = AppConfig::minimal("web", "./srv");

        let cases = [
            (&channel, ReadinessSource::Channel),
            (&probe, ReadinessSource::Probe(probe_cfg, target)),
            (&heuristic, ReadinessSource::Heuristic),
        ];
        for (app, expected) in cases {
            let source = ReadinessSource::of_normalized(app);
            assert_eq!(source, expected, "each signal still decides on its own");
            assert_eq!(source, ReadinessSource::of(app).unwrap(), "and with `of`");
        }
    }

    #[test]
    fn a_malformed_probe_target_is_rejected() {
        let mut app = AppConfig::minimal("web", "./srv");
        let bad_probe = probe_config(ProbeKind::Http, "not-a-url");
        app.readiness_probe = Some(bad_probe.clone());

        let expected = ProbeTarget::parse(&bad_probe).unwrap_err();
        assert_eq!(ReadinessSource::of(&app), Err(expected));
    }
}
