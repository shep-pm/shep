//! A request to stop, and the signals that make one.
//!
//! A dog's loop watches a [`Stop`]; a [`StopRequest`] is what asks. Which
//! signals ask is the dog's call. [`Stop::on_interrupt`] suits a dog the
//! shepherd may kill outright, [`Stop::on_stop_signals`] one that must
//! finish a step before it goes.
//!
//! Both install their handlers before returning, so a signal arriving
//! during startup is not lost to its default disposition.

use core::future::Future;
use core::pin::Pin;
use std::time::Duration;

use tokio::sync::watch;

/// Where a dog reads a request to stop.
///
/// Cloning shares the one request, so two loops can watch it.
#[derive(Debug, Clone)]
pub struct Stop(watch::Receiver<bool>);

/// Where a request to stop is made.
///
/// Dropping it without asking means no request ever comes.
#[derive(Debug)]
pub struct StopRequest(watch::Sender<bool>);

/// Whether [`Stop::sleep`] ended on a stop rather than on the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupted {
    /// The whole period elapsed.
    No,
    /// A stop was requested first, or already had been.
    Yes,
}

/// One installed signal handler, waiting for its signal.
///
/// Resolves `None` if the stream closes, which is not a request to stop.
type Listener = Pin<Box<dyn Future<Output = Option<()>> + Send>>;

impl Stop {
    /// A stop and the handle that requests it.
    #[must_use]
    pub fn new() -> (Self, StopRequest) {
        let (sender, receiver) = watch::channel(false);
        (Self(receiver), StopRequest(sender))
    }

    /// A stop nothing will ever request.
    #[must_use]
    pub fn never() -> Self {
        Self::new().0
    }

    /// A stop the operator's Ctrl+C requests: `SIGINT` on unix, the
    /// console's Ctrl+C on Windows.
    ///
    /// `SIGTERM` keeps its default, so a supervised stop ends the process
    /// at once. A handler the OS refuses is skipped, and the dog runs on.
    ///
    /// # Panics
    ///
    /// Outside a Tokio runtime, as [`tokio::spawn`] does.
    #[track_caller]
    #[must_use]
    pub fn on_interrupt() -> Self {
        Self::listening(interrupt_listeners())
    }

    /// A stop the shepherd's `SIGTERM` or the operator's `SIGINT` requests.
    ///
    /// On Windows, any console control event: Ctrl+C, Ctrl+Break, the
    /// console closing, logoff and shutdown. A handler the OS refuses is
    /// skipped, and the dog runs on.
    ///
    /// # Panics
    ///
    /// Outside a Tokio runtime, as [`tokio::spawn`] does.
    #[track_caller]
    #[must_use]
    pub fn on_stop_signals() -> Self {
        Self::listening(stop_listeners())
    }

    /// A stop each of `listeners` requests when its signal arrives.
    ///
    /// With no listener at all every sender is dropped here, which is the
    /// stop that never comes rather than one that fires.
    #[track_caller]
    fn listening(listeners: Vec<Listener>) -> Self {
        let (stop, request) = Self::new();
        for listener in listeners {
            let sender = request.0.clone();
            tokio::spawn(async move {
                if listener.await.is_some() {
                    sender.send_replace(true);
                }
            });
        }
        stop
    }

    /// Whether a stop has been requested.
    #[must_use]
    pub fn requested(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once a stop is requested, at once if it already was, and
    /// never if the [`StopRequest`] was dropped without one.
    ///
    /// # Cancellation safety
    ///
    /// Safe: a request is a stored value rather than an event.
    pub async fn wait(&mut self) {
        // `wait_for` reads the value before it checks for a dropped sender,
        // so an error here means nobody asked and nobody ever will.
        if self.0.wait_for(|&requested| requested).await.is_err() {
            core::future::pending::<()>().await;
        }
    }

    /// Sleeps for `period`, or until a stop is requested, and says which.
    ///
    /// A stop already requested wins over a period that has also run out.
    ///
    /// # Cancellation safety
    ///
    /// Safe, for the reason [`Self::wait`] is.
    pub async fn sleep(&mut self, period: Duration) -> Interrupted {
        tokio::select! {
            biased;
            () = self.wait() => Interrupted::Yes,
            () = tokio::time::sleep(period) => Interrupted::No,
        }
    }
}

impl StopRequest {
    /// Asks every [`Stop`] made with this handle to stop. Asking twice is
    /// harmless.
    pub fn request(&self) {
        self.0.send_replace(true);
    }
}

#[cfg(unix)]
fn interrupt_listeners() -> Vec<Listener> {
    use tokio::signal::unix::SignalKind;
    unix_listener(SignalKind::interrupt()).into_iter().collect()
}

#[cfg(unix)]
fn stop_listeners() -> Vec<Listener> {
    use tokio::signal::unix::SignalKind;
    [SignalKind::terminate(), SignalKind::interrupt()]
        .into_iter()
        .filter_map(unix_listener)
        .collect()
}

/// `kind`'s handler, installed now, or `None` when the OS refuses it.
#[cfg(unix)]
#[track_caller]
fn unix_listener(kind: tokio::signal::unix::SignalKind) -> Option<Listener> {
    let mut signal = tokio::signal::unix::signal(kind).ok()?;
    Some(Box::pin(async move { signal.recv().await }))
}

#[cfg(windows)]
#[track_caller]
fn interrupt_listeners() -> Vec<Listener> {
    let mut listeners: Vec<Listener> = Vec::new();
    if let Ok(mut ctrl_c) = tokio::signal::windows::ctrl_c() {
        listeners.push(Box::pin(async move { ctrl_c.recv().await }));
    }
    listeners
}

#[cfg(windows)]
#[track_caller]
fn stop_listeners() -> Vec<Listener> {
    use tokio::signal::windows;
    let mut listeners = interrupt_listeners();
    if let Ok(mut ctrl_break) = windows::ctrl_break() {
        listeners.push(Box::pin(async move { ctrl_break.recv().await }));
    }
    if let Ok(mut ctrl_close) = windows::ctrl_close() {
        listeners.push(Box::pin(async move { ctrl_close.recv().await }));
    }
    if let Ok(mut ctrl_logoff) = windows::ctrl_logoff() {
        listeners.push(Box::pin(async move { ctrl_logoff.recv().await }));
    }
    if let Ok(mut ctrl_shutdown) = windows::ctrl_shutdown() {
        listeners.push(Box::pin(async move { ctrl_shutdown.recv().await }));
    }
    listeners
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;

    #[tokio::test(start_paused = true)]
    async fn a_request_wakes_a_waiter() {
        let (mut stop, request) = Stop::new();
        assert!(!stop.requested());
        let waiter = tokio::spawn(async move {
            stop.wait().await;
            stop.requested()
        });
        request.request();
        let woke = timeout(Duration::from_secs(5), waiter)
            .await
            .expect("a request wakes the waiter")
            .expect("the waiter did not panic");
        assert!(woke);
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_made_before_the_wait_still_wakes_it() {
        let (mut stop, request) = Stop::new();
        request.request();
        drop(request);
        assert!(stop.requested());
        timeout(Duration::from_secs(5), stop.wait())
            .await
            .expect("a request already made resolves the wait at once");
    }

    /// Including once the request handle is gone: dropped is "nobody will
    /// ever ask", not "asked".
    #[tokio::test(start_paused = true)]
    async fn no_request_never_wakes() {
        let mut stop = Stop::never();
        assert!(
            timeout(Duration::from_secs(60), stop.wait()).await.is_err(),
            "nothing requested a stop, so the wait must not resolve"
        );
        assert!(!stop.requested());
    }

    #[tokio::test(start_paused = true)]
    async fn a_clone_hears_the_request_made_to_the_original() {
        let (stop, request) = Stop::new();
        let mut clone = stop.clone();
        request.request();
        timeout(Duration::from_secs(5), clone.wait())
            .await
            .expect("both ends share one request");
    }

    #[tokio::test(start_paused = true)]
    async fn a_requested_stop_wins_over_a_period_already_over() {
        let (mut stop, request) = Stop::new();
        request.request();
        assert_eq!(stop.sleep(Duration::ZERO).await, Interrupted::Yes);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unrequested_sleep_runs_its_whole_period() {
        let (mut stop, _request) = Stop::new();
        let started = tokio::time::Instant::now();
        assert_eq!(stop.sleep(Duration::from_secs(30)).await, Interrupted::No);
        assert_eq!(started.elapsed(), Duration::from_secs(30));
    }

    /// A mis-wired listener would stop every dog the moment it started.
    #[tokio::test(start_paused = true)]
    async fn installed_listeners_do_not_fire_on_their_own() {
        for mut stop in [Stop::on_interrupt(), Stop::on_stop_signals()] {
            assert!(
                timeout(Duration::from_secs(60), stop.wait()).await.is_err(),
                "no signal was sent, so no stop was requested"
            );
        }
    }

    /// Real signals, so a real process: raising one here would reach every
    /// listener in this test binary.
    #[cfg(unix)]
    mod signals {
        use std::io::{BufRead as _, BufReader};
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::{Command, ExitStatus, Stdio};
        use std::sync::mpsc;

        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;

        use super::*;

        /// Names which constructor the child listens with.
        const LISTEN_VAR: &str = "SHEP_CLIENT_STOP_CHILD";
        /// What the child prints once its handlers are installed.
        const READY: &str = "shep-client stop child: listening";
        /// The child test's path, as `--exact` wants it.
        const CHILD: &str = "dogs::stop::tests::signals::signal_child";
        /// Generous for a debug build starting a harness under load.
        const BUDGET: Duration = Duration::from_secs(30);

        /// Child half of the tests below. Real time: it waits on a signal
        /// another process sends, which a paused clock cannot deliver.
        #[tokio::test]
        #[ignore = "child process of the signal tests in this module"]
        async fn signal_child() {
            let mut stop = match std::env::var(LISTEN_VAR).as_deref() {
                Ok("interrupt") => Stop::on_interrupt(),
                Ok("stop_signals") => Stop::on_stop_signals(),
                other => panic!("{LISTEN_VAR} is {other:?}; this test only runs as a child"),
            };
            println!("{READY}");
            timeout(BUDGET, stop.wait())
                .await
                .expect("the parent's signal never became a stop");
        }

        /// Starts the child listening with `listen`, sends it `signal`, and
        /// returns how it ended.
        fn signalled(listen: &str, signal: Signal) -> ExitStatus {
            let mut child = Command::new(std::env::current_exe().expect("test binary path"))
                .args(["--exact", "--ignored", "--nocapture", CHILD])
                .env(LISTEN_VAR, listen)
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn the child harness");

            let stdout = child.stdout.take().expect("stdout is piped");
            let (ready, listening) = mpsc::channel();
            std::thread::spawn(move || {
                let mut lines = BufReader::new(stdout).lines();
                if lines.any(|line| line.is_ok_and(|line| line == READY)) {
                    let _ = ready.send(());
                    // Drained so a chatty harness never blocks on a full pipe.
                    lines.for_each(drop);
                }
            });
            listening.recv_timeout(BUDGET).unwrap_or_else(|why| {
                panic!("the child never said it was listening ({why}); is `{CHILD}` stale?")
            });

            let pid = Pid::from_raw(i32::try_from(child.id()).expect("a pid fits an i32"));
            kill(pid, signal).expect("signal the child");

            let (done, exited) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = done.send(child.wait());
            });
            exited
                .recv_timeout(BUDGET)
                .expect("the child outlived its budget")
                .expect("wait on the child")
        }

        /// shep-pm/shep-log-rotate#34: a supervised stop is `SIGTERM`.
        #[test]
        fn sigterm_ends_a_stop_signals_dog_cleanly() {
            let status = signalled("stop_signals", Signal::SIGTERM);
            assert_eq!(status.code(), Some(0), "{status:?}");
        }

        #[test]
        fn sigint_ends_a_stop_signals_dog_cleanly() {
            let status = signalled("stop_signals", Signal::SIGINT);
            assert_eq!(status.code(), Some(0), "{status:?}");
        }

        #[test]
        fn sigint_ends_an_interrupt_dog_cleanly() {
            let status = signalled("interrupt", Signal::SIGINT);
            assert_eq!(status.code(), Some(0), "{status:?}");
        }

        /// The shepherd's kill stays a kill for a dog that asked for Ctrl+C
        /// alone.
        #[test]
        fn sigterm_keeps_its_default_for_an_interrupt_dog() {
            let status = signalled("interrupt", Signal::SIGTERM);
            assert_eq!(status.signal(), Some(Signal::SIGTERM as i32), "{status:?}");
        }
    }
}
