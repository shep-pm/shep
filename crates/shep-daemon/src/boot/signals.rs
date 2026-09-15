//! The signal listeners, and the guard that stops them
//!
//! [`install_signals`] runs before anything observable exists, because SIGUSR2
//! and SIGHUP both terminate by default and a boot that died between the two
//! would leave the operator neither. Each listener loops for the process's
//! life rather than returning after one delivery, so a second SIGTERM during a
//! slow teardown still has somewhere to go. [`SignalTasks`] is what ends them:
//! its [`Drop`] aborts, where a bare `JoinHandle` would detach.

use std::sync::Arc;

use shep_core::paths::ShepPaths;
#[cfg(unix)]
use shep_core::selector::ProcessSelector;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use super::BootError;
#[cfg(unix)]
use super::handover::{HandoverSeam, hand_over_now};
use crate::supervisor::SupervisorHandle;
// unix only: read by the SIGUSR2 log-reopen handler, which Windows has none of.
#[cfg(unix)]
use crate::supervisor::SupervisorError;

/// Live signal-listener tasks [`install_signals`] spawned, held so its
/// [`Drop`] stops them rather than detaching them. Covers an early `?`-return
/// from a later step inside [`boot`](super::boot), which must not leak a task per boot
/// attempt.
#[derive(Debug)]
pub(super) struct SignalTasks {
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for SignalTasks {
    fn drop(&mut self) {
        // `JoinHandle::drop` detaches rather than stopping.
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Installs SIGTERM/SIGINT/SIGQUIT (graceful shutdown) and SIGUSR2 (reopen)
///
/// SIGUSR2's default disposition is to terminate, so this handler is what
/// keeps a logrotate `postrotate` stanza from killing the daemon; carrying no
/// selector, it reopens [`ProcessSelector::All`]. The returned sender hands
/// that listener its [`SupervisorHandle`], absent until [`boot`](super::boot)'s step 4;
/// tokio coalesces anything raised in that window into the first `recv()`.
/// Each listener then loops for life, or a second SIGTERM during a slow
/// teardown would have nowhere to go and no default disposition left.
///
/// # Errors
/// - [`BootError::Io`] if the OS refused to register a signal handler.
#[cfg(windows)]
pub(super) fn install_signals(
    shutdown: Arc<watch::Sender<bool>>,
    paths: ShepPaths,
) -> Result<(SignalTasks, oneshot::Sender<SupervisorHandle>), BootError> {
    use tokio::signal::windows;

    let mut signals = SignalTasks {
        tasks: Vec::with_capacity(4),
    };

    // A macro rather than the unix arm's loop: each console control event is
    // its own tokio type with its own `recv`, so there is nothing to iterate.
    macro_rules! listen {
        ($ctor:path, $name:literal) => {{
            let mut stream = $ctor().map_err(|source| BootError::Io {
                path: paths.home.clone(),
                source,
            })?;
            let shutdown = Arc::clone(&shutdown);
            signals.tasks.push(tokio::spawn(async move {
                // Looped: a single await leaves a second event during a
                // slow teardown with nowhere to go.
                let mut already_shutting_down = false;
                while stream.recv().await.is_some() {
                    if already_shutting_down {
                        tracing::warn!(
                            signal = $name,
                            "received a repeat shutdown event while teardown is already \
                             underway; teardown continues unchanged"
                        );
                    } else {
                        already_shutting_down = true;
                    }
                    let _ = shutdown.send(true);
                }
            }));
        }};
    }

    // CTRL_CLOSE and CTRL_SHUTDOWN carry a hard OS deadline shorter than any
    // teardown shep can promise: Windows terminates the process about five
    // seconds after the handler returns, so a flock slower than that loses the
    // tail of its kill ladder. Only an SCM service can negotiate longer.
    listen!(windows::ctrl_c, "CTRL_C");
    listen!(windows::ctrl_break, "CTRL_BREAK");
    listen!(windows::ctrl_close, "CTRL_CLOSE");
    listen!(windows::ctrl_shutdown, "CTRL_SHUTDOWN");

    // No SIGUSR2 counterpart: Windows has no user-defined console control
    // event, so the signal-driven log reopen has no trigger. Rotation works
    // anyway through `tokio_runner`'s `open_append`. The channel is created
    // and dropped so the caller wiring is one shape on both platforms.
    let (connect_supervisor, _supervisor_rx) = oneshot::channel::<SupervisorHandle>();
    Ok((signals, connect_supervisor))
}

#[cfg(unix)]
pub(super) fn install_signals(
    shutdown: Arc<watch::Sender<bool>>,
    paths: ShepPaths,
) -> Result<InstalledSignals, BootError> {
    let mut signals = SignalTasks {
        tasks: Vec::with_capacity(4),
    };

    for kind in [
        SignalKind::terminate(),
        SignalKind::interrupt(),
        SignalKind::quit(),
    ] {
        // An early return drops `signals`, whose `Drop` aborts every task
        // already pushed.
        let mut stream = signal(kind).map_err(|source| BootError::Io {
            path: paths.home.clone(),
            source,
        })?;
        let shutdown = Arc::clone(&shutdown);
        signals.tasks.push(tokio::spawn(async move {
            // `None` means the stream itself closed, leaving this task
            // nothing to listen for.
            let mut already_shutting_down = false;
            while stream.recv().await.is_some() {
                if already_shutting_down {
                    // Observable, but teardown is already unconditional and
                    // already running. `SIGKILL` is the only faster exit,
                    // and no handler here can intercept it.
                    tracing::warn!(
                        ?kind,
                        "received a repeat shutdown signal while teardown is already \
                         underway; teardown continues unchanged (SIGKILL forces an \
                         immediate exit)"
                    );
                } else {
                    already_shutting_down = true;
                }
                let _ = shutdown.send(true);
            }
        }));
    }

    // SIGHUP is the handover trigger, a signal rather than a request because
    // the case that most needs a reload is a daemon refusing the client at the
    // handshake. Its own task, since it replaces this daemon where the loop
    // above stops it; a refused handover falls back to the graceful stop.
    let mut hup = signal(SignalKind::hangup()).map_err(|source| BootError::Io {
        path: paths.home.clone(),
        source,
    })?;
    let (connect_handover, handover_rx) = oneshot::channel::<Option<HandoverSeam>>();
    let hup_shutdown = Arc::clone(&shutdown);
    signals.tasks.push(tokio::spawn(async move {
        // Parked until `boot` reaches step 4: the descriptors and supervisor a
        // handover needs do not exist yet, and the stream registered above
        // buffers anything raised meanwhile. `None` (not armed) and `Err`
        // (boot never got that far) still answer SIGHUP, whose default kills.
        let seam = handover_rx.await.ok().flatten();
        // `if`, not `while`: at most one SIGHUP. On the success arm there is
        // no image left to loop in, and every other arm is now stopping.
        if hup.recv().await.is_some() {
            let refusal = match &seam {
                Some(seam) => match hand_over_now(seam).await {
                    // No successor image runs this code.
                    Ok(never) => match never {},
                    Err(refusal) => refusal,
                },
                None => "this shepherd was not booted with the handover armed".to_string(),
            };
            tracing::warn!(
                %refusal,
                "SIGHUP: this flock could not be handed to a successor; stopping gracefully \
                 instead. This line may be the only record of the reason: a signal carries no \
                 sender, and the case this gate exists for is a flock that changed between a \
                 client's question and the signal, where that client was told nothing"
            );
            let _ = hup_shutdown.send(true);
        }
    }));

    let mut usr2 = signal(SignalKind::user_defined2()).map_err(|source| BootError::Io {
        path: paths.home.clone(),
        source,
    })?;
    let (connect_supervisor, supervisor_rx) = oneshot::channel::<SupervisorHandle>();
    signals.tasks.push(tokio::spawn(async move {
        // Parked until `boot` reaches step 4; the wait loses no signal.
        let Ok(supervisor) = supervisor_rx.await else {
            return;
        };
        while usr2.recv().await.is_some() {
            // A rotator that moved the whole log directory gets it back at
            // `DIR_MODE` from the pump's own open (see `open_append`).
            // Recreating it here would be a second owner of that guarantee.
            match supervisor.reopen(ProcessSelector::All).await {
                Ok(reopened) => tracing::info!(
                    reopened = reopened.len(),
                    "SIGUSR2: every sheep's log files reopened"
                ),
                // An empty flock is an idle daemon's ordinary state, not
                // something a nightly `postrotate` should warn about.
                Err(SupervisorError::NotFound) => {
                    tracing::info!("SIGUSR2: no sheep to reopen");
                }
                // A signal carries no reply channel, so this log is the
                // whole report.
                Err(err) => tracing::warn!(%err, "SIGUSR2: log reopen failed"),
            }
        }
    }));

    Ok((signals, connect_supervisor, connect_handover))
}

/// What [`install_signals`] hands back: the live listener tasks, and the two
/// senders that connect them to state `boot` has not built yet.
///
/// The SIGUSR2 task needs a [`SupervisorHandle`], the SIGHUP task a
/// [`HandoverSeam`] or the `None` saying this boot did not arm one.
#[cfg(unix)]
type InstalledSignals = (
    SignalTasks,
    oneshot::Sender<SupervisorHandle>,
    oneshot::Sender<Option<HandoverSeam>>,
);

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::boot::{BootOptions, SIGNAL_TEST_LOCK, boot, init_dirs};
    use crate::fake::{ProcScript, ScriptedRunner};
    use crate::testing::{SharedRunner, test_paths};
    use shep_core::config::{AppConfig, normalize};
    use std::time::Duration;

    #[tokio::test]
    async fn sigterm_triggers_the_same_graceful_shutdown() {
        // Real time and a real signal, safe to raise only because the handler
        // is installed first: SIGTERM's default action would kill the test
        // binary. The raise is process-wide, hence SIGNAL_TEST_LOCK.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        let daemon = boot(
            ScriptedRunner::new(vec![]),
            paths.clone(),
            BootOptions::default(),
        )
        .await
        .unwrap();
        // No sleep: the handlers are installed inside `boot`, which this call
        // already awaited, so they are live before `run()` is ever polled.
        let run = tokio::spawn(daemon.run());
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).unwrap();
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!paths.socket.exists());
    }

    #[tokio::test]
    async fn sighup_triggers_the_same_graceful_shutdown() {
        // SIGHUP's default disposition is to terminate, and this handler
        // replaces it. SIGHUP is the handover trigger, but a boot that has not
        // set `BootOptions::handover` has no successor to become, and must
        // still walk SIGTERM's graceful path rather than drop the flock's pipes.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        // `handover` left `false`, or `exec_target` would replace the test
        // binary with a fresh copy of itself.
        let daemon = boot(
            ScriptedRunner::new(vec![]),
            paths.clone(),
            BootOptions::default(),
        )
        .await
        .unwrap();
        let run = tokio::spawn(daemon.run());
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGHUP).unwrap();
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!paths.socket.exists());
    }

    #[tokio::test]
    async fn a_repeat_sigterm_is_observed_not_swallowed() {
        // Each listener `install_signals` spawns stays armed for the process's
        // life instead of returning after one `recv()`. Drives
        // `install_signals` directly: the loop is the whole subject. Real time
        // and real signals, and the raise is process-wide, hence the lock.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let shutdown = Arc::new(shutdown);
        // The SIGUSR2 and SIGHUP senders are dropped unused: that ends the
        // task parked on each receiver without disturbing the three below.
        let (signals, _connect_supervisor, _connect_handover) =
            install_signals(shutdown, paths).unwrap();

        // First SIGTERM: starts shutdown, exactly as before this decision.
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).unwrap();
        tokio::time::timeout(Duration::from_secs(5), shutdown_rx.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(*shutdown_rx.borrow());

        // Looped, the listener never finishes on its own; only
        // `SignalTasks::drop`'s `abort()` stops it.
        assert!(
            !signals.tasks[0].is_finished(),
            "the SIGTERM listener must still be polling after its first signal, not have exited"
        );

        // A second SIGTERM into the already-shutting-down state a slow
        // teardown would be in. `watch::Sender::send` marks its channel
        // changed on every call whether or not the value differs, so a second
        // `changed()` on a value already `true` proves the loop delivered it.
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).unwrap();
        tokio::time::timeout(Duration::from_secs(5), shutdown_rx.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !signals.tasks[0].is_finished(),
            "the SIGTERM listener must still be armed after a SECOND signal too"
        );

        drop(signals); // aborts the listener tasks (SignalTasks::drop)
    }

    // The only case driving the seam where `boot` hands the SIGUSR2 listener
    // its supervisor; `shep reopen` reaches the same supervisor over the
    // socket, so the RPC tier stays green with the signal path dead. Both
    // instances are asserted, since `ProcessSelector::All` is the claim.
    #[tokio::test]
    async fn sigusr2_reopens_every_sheeps_log_files() {
        // Real time and a real signal, raised process-wide, hence the lock.
        // Safe only because `boot` below has already replaced SIGUSR2's
        // default disposition, which would kill the test binary.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);

        // Two scripts for two instances: `ScriptedRunner` answers
        // `SpawnFailed("script exhausted")` once it runs out, landing that
        // sheep `Errored` with no pump, which this case could not tell apart
        // from a pump nobody reopened. `log_ctl_live` below is the other half.
        let runner = Arc::new(ScriptedRunner::new(vec![ProcScript::never_exits(); 2]));
        let daemon = boot(
            SharedRunner(Arc::clone(&runner)),
            paths.clone(),
            BootOptions::default(),
        )
        .await
        .unwrap();
        let ctx = daemon.context();
        let run = tokio::spawn(daemon.run());

        let mut web = AppConfig::minimal("web", "./srv");
        web.instances = 2;
        ctx.supervisor
            .start(vec![normalize(web).unwrap()])
            .await
            .unwrap();
        for instance in 0..2 {
            assert!(
                runner.log_ctl_live(instance),
                "instance {instance} must have a live log pump before the signal, or this \
                 case proves nothing"
            );
            assert_eq!(
                runner.reopens(instance),
                0,
                "instance {instance} must not have been reopened before the signal"
            );
        }

        nix::sys::signal::raise(nix::sys::signal::Signal::SIGUSR2).unwrap();

        // Polled: a signal has no reply channel, so the counters are the only
        // place a reopen becomes visible. Bounded, so a listener that never
        // reaches a pump fails here instead of hanging.
        let both_reopened = async {
            while runner.reopens(0) == 0 || runner.reopens(1) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), both_reopened)
            .await
            .expect("SIGUSR2 must reopen every sheep's log files");

        ctx.shutdown();
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
