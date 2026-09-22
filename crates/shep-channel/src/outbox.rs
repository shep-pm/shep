//! The queue between the app's threads and the one thread that writes.
//!
//! Two push policies. A dropped metric costs nothing: the shepherd only
//! logs it at debug level. A dropped `Ready` hangs `wait_ready`, and a
//! dropped reply costs an operator the whole `action_timeout`. So metrics
//! are lossy and never block; everything else waits for room.
//!
//! One queue holds both. A full queue gives up a metric, never a
//! `Ready` or an `ActionReply`.
//!
//! An app that wants to exit without losing what it queued waits on
//! `drain`. A message the writer has taken is not yet a message the
//! shepherd has, so the count that matters spans both the queue and the
//! one write in progress.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::{ChannelError, ChildMessage};

/// How many messages may wait for the writer before the policy applies.
///
/// 1024 is a starting guess, not a measurement. `ChildMessage` is 64
/// bytes on the stack. So a full queue's fixed cost is tens of
/// kilobytes plus whatever names and bodies heap-allocate.
pub(crate) const DEFAULT_CAPACITY: usize = 1024;

/// How a wait on [`Outbox::drain`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drain {
    /// Nothing is queued and nothing is part-written. Everything the
    /// outbox took has reached the transport.
    Empty,
    /// The writer returned with messages still unwritten. They are gone.
    Stopped,
    /// The budget ran out with messages still waiting.
    TimedOut,
}

#[derive(Debug)]
struct Inner {
    queue: VecDeque<ChildMessage>,
    dropped: u64,
    closed: bool,
    /// Messages [`Outbox::pop`] handed the writer that it has not
    /// reported written. At most one while a single writer runs, and
    /// the whole reason `drain` cannot just read `queue.is_empty()`.
    in_flight: usize,
    /// The writer loop has returned. Nothing still held will ever be
    /// written. Distinct from `closed`, which the reader also sets and
    /// which the writer keeps draining through.
    stopped: bool,
}

/// The bounded queue the writer thread drains.
#[derive(Debug)]
pub(crate) struct Outbox {
    inner: Mutex<Inner>,
    capacity: usize,
    /// Signalled when a message is queued, or the outbox closes.
    queued: Condvar,
    /// Signalled when a message leaves, or the outbox closes.
    drained: Condvar,
    /// Signalled when nothing is left to write, or the writer stops.
    ///
    /// Its own condvar rather than `drained`: that one wakes a push
    /// waiting for room, and `notify_one` between two kinds of waiter
    /// wakes the wrong one.
    emptied: Condvar,
}

impl Outbox {
    /// `capacity` bounds how many messages `push_lossy` will hold before it
    /// starts discarding. Zero is legal: nothing is ever retained, so every
    /// lossy push is counted as a drop.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                dropped: 0,
                closed: false,
                in_flight: 0,
                stopped: false,
            }),
            capacity,
            queued: Condvar::new(),
            drained: Condvar::new(),
            emptied: Condvar::new(),
        }
    }

    /// Queues a message that may be dropped. Never blocks, never fails.
    ///
    /// A full queue discards its oldest metric, never a `Ready` or an
    /// `ActionReply`, and counts it. Newer samples are worth more, so the
    /// older one goes. With nothing to evict, the incoming message is the
    /// one dropped instead.
    pub(crate) fn push_lossy(&self, message: ChildMessage) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.closed {
            // The sample is really gone, so count it. `dropped()` must keep
            // moving after the shepherd leaves. Otherwise an app cannot tell
            // why its samples stopped.
            inner.dropped = inner.dropped.saturating_add(1);
            return;
        }
        if self.capacity == 0 {
            // Nothing is ever retained at zero capacity, so the message being
            // pushed is what gets dropped. Count it and stop, rather than
            // evicting nothing and queueing past capacity.
            inner.dropped = inner.dropped.saturating_add(1);
            return;
        }
        if inner.queue.len() >= self.capacity {
            // Scans for a metric instead of taking the head. A droppable message
            // must never displace a `Ready` or an `ActionReply`. Evicting either
            // one hangs `wait_ready` or costs a full `action_timeout`. Only a
            // full queue pays for the scan.
            let Some(oldest_metric) = inner
                .queue
                .iter()
                .position(|queued| matches!(queued, ChildMessage::Metric { .. }))
            else {
                // Nothing in here may be given up, so the incoming metric is
                // what goes.
                inner.dropped = inner.dropped.saturating_add(1);
                return;
            };
            inner.queue.remove(oldest_metric);
            inner.dropped = inner.dropped.saturating_add(1);
        }
        inner.queue.push_back(message);
        self.queued.notify_one();
    }

    /// Queues a message that must not be lost, waiting for room.
    ///
    /// # Errors
    ///
    /// [`ChannelError::Closed`] when the outbox closes while waiting, which
    /// is the shepherd having gone away.
    pub(crate) fn push_blocking(&self, message: ChildMessage) -> Result<(), ChannelError> {
        if self.capacity == 0 {
            // Nothing is ever retained, so this wait would never end. `len >=
            // capacity` is `0 >= 0`, which no drain can falsify. A message that
            // must not be dropped has no honest outcome here but a refusal.
            // `ready()` calls straight into this.
            return Err(ChannelError::Closed);
        }
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        while !inner.closed && inner.queue.len() >= self.capacity {
            inner = self
                .drained
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if inner.closed {
            return Err(ChannelError::Closed);
        }
        inner.queue.push_back(message);
        self.queued.notify_one();
        Ok(())
    }

    /// Takes the next message, waiting for one. `None` once closed and empty.
    ///
    /// A taken message counts as in flight until [`Outbox::wrote`], so a
    /// `drain` in progress keeps waiting for it.
    pub(crate) fn pop(&self) -> Option<ChildMessage> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        while inner.queue.is_empty() && !inner.closed {
            inner = self
                .queued
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
        let taken = inner.queue.pop_front();
        if taken.is_some() {
            inner.in_flight = inner.in_flight.saturating_add(1);
            self.drained.notify_one();
        }
        taken
    }

    /// Reports that the message the writer last took reached the
    /// transport.
    ///
    /// Called only after a successful write. A failed one leaves the
    /// message in flight so the [`Outbox::stop`] that follows reaches a
    /// `drain` as [`Drain::Stopped`] rather than [`Drain::Empty`].
    pub(crate) fn wrote(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.in_flight = inner.in_flight.saturating_sub(1);
        let idle = inner.in_flight == 0 && inner.queue.is_empty();
        drop(inner);
        if idle {
            self.emptied.notify_all();
        }
    }

    /// Releases every waiter. Idempotent.
    pub(crate) fn close(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.closed = true;
        drop(inner);
        self.wake_waiters();
    }

    /// Records that the writer has returned, and closes. Idempotent.
    ///
    /// [`Outbox::close`] on its own does not mean a queued message is
    /// lost: the writer drains what is already there before it stops.
    /// This is the point after which nothing left will ever be written.
    ///
    /// Both flags move under one lock, and that is load-bearing rather
    /// than tidy. Setting `stopped` and then calling `close` for the
    /// other leaves a window where `stopped` holds and `closed` does
    /// not. `push_blocking` reads only `closed`, so a push landing in
    /// that window is told `Ok` for a message nothing will ever write,
    /// and `ready()` calls straight into it. `writer_loop` ends here,
    /// so the window would open on exactly the path that means the
    /// shepherd has gone.
    pub(crate) fn stop(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.stopped = true;
        inner.closed = true;
        drop(inner);
        self.wake_waiters();
    }

    /// Wakes every kind of waiter. Called after a flag changes and
    /// never with the lock held.
    fn wake_waiters(&self) {
        self.queued.notify_all();
        self.drained.notify_all();
        self.emptied.notify_all();
    }

    /// Waits for everything held right now to reach the transport,
    /// giving up after `timeout`.
    ///
    /// Waits on the outbox being empty rather than on the messages that
    /// were there at the call, so a thread still emitting can hold it
    /// open. `timeout` bounds that either way, and a zero one is a
    /// non-blocking probe.
    pub(crate) fn drain(&self, timeout: Duration) -> Drain {
        let started = Instant::now();
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            // Before `stopped`, so a writer that emptied the outbox and
            // then returned reads as the success it is.
            if inner.queue.is_empty() && inner.in_flight == 0 {
                return Drain::Empty;
            }
            if inner.stopped {
                return Drain::Stopped;
            }
            let Some(left) = timeout.checked_sub(started.elapsed()) else {
                return Drain::TimedOut;
            };
            if left.is_zero() {
                return Drain::TimedOut;
            }
            inner = self
                .emptied
                .wait_timeout(inner, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// How many messages are waiting for the transport, counting the one
    /// the writer is part-way through.
    pub(crate) fn pending(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.queue.len() + inner.in_flight
    }

    /// Whether the writer has stopped, which is the shepherd having gone
    /// away. Nothing is ever queued again once this is true.
    pub(crate) fn is_closed(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed
    }

    /// How many messages `push_lossy` has discarded.
    pub(crate) fn dropped(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dropped
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Every wait in this module's tests is bounded by this. A working
    /// outbox answers in microseconds; this is slack for a loaded runner.
    const DEADLINE: Duration = Duration::from_secs(5);

    /// Runs `drain(timeout)` on its own thread, bounded from outside the
    /// call.
    ///
    /// The `timeout` handed to `drain` is that function's input, not this
    /// test's forcing mechanism (IR-46). A drain called straight from the
    /// test body and given its own bound to hold parks the whole binary
    /// when it regresses, which reports as a harness timeout naming
    /// nothing. Measured: with `drain` stubbed to never return, five of
    /// the six tests below hung and only the one already spawning a
    /// thread failed. `DEADLINE` is this side's bound, so a regression
    /// fails here by name.
    #[track_caller]
    fn drain_bounded(outbox: &Arc<Outbox>, timeout: Duration) -> Drain {
        let (tx, rx) = mpsc::channel();
        let draining = Arc::clone(outbox);
        // Never joined. A parked drain holds this thread, and joining it
        // would put back the hang this exists to avoid.
        std::thread::spawn(move || {
            let _ = tx.send(draining.drain(timeout));
        });
        rx.recv_timeout(DEADLINE).expect("drain never returned")
    }

    fn metric(value: f64) -> ChildMessage {
        ChildMessage::Metric {
            name: "rps".into(),
            value,
        }
    }

    #[test]
    fn a_full_outbox_drops_the_oldest_metric_and_counts_it() {
        let outbox = Outbox::new(2);
        outbox.push_lossy(metric(1.0));
        outbox.push_lossy(metric(2.0));
        outbox.push_lossy(metric(3.0));

        assert_eq!(outbox.dropped(), 1);
        assert_eq!(outbox.pop(), Some(metric(2.0)));
        assert_eq!(outbox.pop(), Some(metric(3.0)));
    }

    #[test]
    fn a_full_outbox_evicts_a_metric_rather_than_a_readiness_signal() {
        let outbox = Outbox::new(3);
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for readiness");
        outbox.push_lossy(metric(1.0));
        outbox.push_lossy(metric(2.0));

        outbox.push_lossy(metric(3.0));

        assert_eq!(outbox.dropped(), 1);
        assert_eq!(
            outbox.pop(),
            Some(ChildMessage::Ready),
            "readiness was evicted by a metric"
        );
        assert_eq!(outbox.pop(), Some(metric(2.0)));
        assert_eq!(outbox.pop(), Some(metric(3.0)));
    }

    #[test]
    fn a_full_outbox_with_no_metric_to_evict_drops_the_incoming_one() {
        let reply = ChildMessage::ActionReply {
            action: "gc".to_string(),
            body: "ok".to_string(),
            id: Some(1),
        };
        let outbox = Outbox::new(2);
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for readiness");
        outbox
            .push_blocking(reply.clone())
            .expect("room for the reply");

        outbox.push_lossy(metric(1.0));

        assert_eq!(outbox.dropped(), 1);
        assert_eq!(outbox.pop(), Some(ChildMessage::Ready));
        assert_eq!(outbox.pop(), Some(reply));
        outbox.close();
        assert_eq!(
            outbox.pop(),
            None,
            "the incoming metric was queued past capacity"
        );
    }

    /// The pusher reports only after `push_blocking` returns, so a
    /// `recv_timeout` timeout proves it is still waiting.
    #[test]
    fn a_must_deliver_push_waits_for_room_and_then_proceeds() {
        let outbox = Arc::new(Outbox::new(1));
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("first fits");

        let (tx, rx) = mpsc::channel();
        let pusher = Arc::clone(&outbox);
        let handle = std::thread::spawn(move || {
            let outcome = pusher.push_blocking(ChildMessage::Ready);
            tx.send(outcome).expect("report");
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "push_blocking returned while the outbox was full"
        );

        assert_eq!(outbox.pop(), Some(ChildMessage::Ready));
        rx.recv_timeout(DEADLINE)
            .expect("pusher did not proceed")
            .expect("push after room");
        handle.join().expect("pusher panicked");
    }

    /// Without this, an app whose shepherd went away hangs on `ready()`.
    #[test]
    fn closing_releases_a_blocked_push_with_an_error() {
        let outbox = Arc::new(Outbox::new(1));
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("first fits");

        let (tx, rx) = mpsc::channel();
        let pusher = Arc::clone(&outbox);
        let handle = std::thread::spawn(move || {
            tx.send(pusher.push_blocking(ChildMessage::Ready))
                .expect("report");
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "returned too early"
        );
        outbox.close();

        let outcome = rx.recv_timeout(DEADLINE).expect("still parked after close");
        assert!(matches!(outcome, Err(ChannelError::Closed)));
        handle.join().expect("pusher panicked");
    }

    /// `len >= capacity` at capacity 0 is `0 >= 0`, which no drain can
    /// falsify. So a regression hangs instead of failing. `DEADLINE` bounds
    /// the wait, so a regression fails loudly instead of parking the suite.
    #[test]
    fn a_must_deliver_push_refuses_a_zero_capacity_outbox_rather_than_parking() {
        let outbox = Arc::new(Outbox::new(0));
        let (tx, rx) = mpsc::channel();
        let pusher = Arc::clone(&outbox);
        let handle = std::thread::spawn(move || {
            tx.send(pusher.push_blocking(ChildMessage::Ready))
                .expect("report");
        });

        let outcome = rx
            .recv_timeout(DEADLINE)
            .expect("push_blocking parked on a zero-capacity outbox");
        assert!(matches!(outcome, Err(ChannelError::Closed)));
        handle.join().expect("pusher panicked");
    }

    /// Otherwise the writer thread is unjoinable at shutdown.
    #[test]
    fn pop_returns_none_once_closed_and_empty() {
        let outbox = Outbox::new(4);
        outbox.close();
        assert_eq!(outbox.pop(), None);
    }

    /// Emitting a metric after shutdown is ordinary, not an error, but the
    /// sample is really gone. `dropped()` is how an app notices that.
    #[test]
    fn a_lossy_push_after_close_counts_the_drop_and_queues_nothing() {
        let outbox = Outbox::new(4);
        outbox.close();
        outbox.push_lossy(metric(1.0));
        assert_eq!(outbox.pop(), None);
        assert_eq!(outbox.dropped(), 1);
    }

    /// Closes before `pop()` so the test does not block on an empty, open
    /// outbox.
    #[test]
    fn a_zero_capacity_outbox_counts_the_drop_and_retains_nothing() {
        let outbox = Outbox::new(0);
        outbox.push_lossy(metric(1.0));
        assert_eq!(outbox.dropped(), 1);

        outbox.close();
        assert_eq!(outbox.pop(), None);
    }

    /// Pins that `stop` closes as well as stops, which is what keeps a
    /// push from being told `Ok` after the writer has gone.
    ///
    /// It does not prove the two flags move together, and no test from
    /// outside this type can: the window is only observable from a
    /// thread holding the lock between them. `stop`'s own doc carries
    /// that reason. What this catches is the flag being dropped
    /// outright, which is the regression a later edit would make.
    #[test]
    fn a_must_deliver_push_is_refused_once_the_writer_has_stopped() {
        let outbox = Outbox::new(4);
        outbox.stop();

        assert!(matches!(
            outbox.push_blocking(ChildMessage::Ready),
            Err(ChannelError::Closed)
        ));
        assert!(outbox.is_closed(), "stop left the outbox open to pushes");
    }

    /// The whole point of the in-flight count, and the one thing an
    /// empty-queue check gets wrong. The test is the writer here: it
    /// takes the message and never writes it, which is the window a
    /// process exiting from `on_shutdown` falls into.
    ///
    /// Forcing mechanism in both directions. The `recv_timeout` that
    /// must expire proves the drain is still waiting; the one bounded by
    /// `DEADLINE` proves `wrote()` releases it. Drop the `in_flight`
    /// bookkeeping and the first assertion fails in 200ms.
    #[test]
    fn a_drain_waits_for_a_message_the_writer_took_but_has_not_written() {
        let outbox = Arc::new(Outbox::new(4));
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for readiness");
        assert_eq!(outbox.pop(), Some(ChildMessage::Ready));

        let (tx, rx) = mpsc::channel();
        let draining = Arc::clone(&outbox);
        let handle = std::thread::spawn(move || {
            tx.send(draining.drain(DEADLINE)).expect("report");
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "drain reported an empty outbox while the writer still held a message"
        );
        assert_eq!(outbox.pending(), 1, "an in-flight message is still pending");

        outbox.wrote();

        assert_eq!(
            rx.recv_timeout(DEADLINE).expect("drain never returned"),
            Drain::Empty
        );
        assert_eq!(outbox.pending(), 0);
        handle.join().expect("drainer panicked");
    }

    /// `DEADLINE` bounds a regression that would otherwise park here
    /// until the harness kills the whole binary.
    #[test]
    fn a_drain_reports_a_writer_that_stopped_with_a_message_unwritten() {
        let outbox = Arc::new(Outbox::new(4));
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for readiness");

        outbox.stop();

        assert_eq!(drain_bounded(&outbox, DEADLINE), Drain::Stopped);
        assert_eq!(
            outbox.pending(),
            1,
            "the unwritten message is what was lost"
        );
    }

    /// A writer that emptied the outbox and then returned delivered
    /// everything, so this is a success and not a `Stopped`.
    #[test]
    fn a_drain_of_an_outbox_a_stopped_writer_had_emptied_is_a_success() {
        let outbox = Arc::new(Outbox::new(4));
        outbox.stop();

        assert_eq!(drain_bounded(&outbox, Duration::ZERO), Drain::Empty);
    }

    /// The timeout is the test's own bound: nothing is draining this
    /// outbox, so a regression that waits for a drainer fails here in
    /// 50ms rather than hanging.
    #[test]
    fn a_drain_gives_up_on_a_queue_nothing_is_draining() {
        let outbox = Arc::new(Outbox::new(4));
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for readiness");

        assert_eq!(
            drain_bounded(&outbox, Duration::from_millis(50)),
            Drain::TimedOut
        );
    }

    /// A zero timeout is a probe, so it must answer both ways without
    /// waiting rather than always reporting a timeout.
    #[test]
    fn a_zero_timeout_drain_answers_without_waiting() {
        let outbox = Arc::new(Outbox::new(4));
        assert_eq!(drain_bounded(&outbox, Duration::ZERO), Drain::Empty);

        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for readiness");
        assert_eq!(drain_bounded(&outbox, Duration::ZERO), Drain::TimedOut);
    }

    /// A closed outbox is not a stopped one: the writer keeps draining
    /// what is already queued, and a drain has to wait for that rather
    /// than call the messages lost.
    #[test]
    fn closing_alone_does_not_end_a_drain() {
        let outbox = Arc::new(Outbox::new(4));
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for readiness");

        outbox.close();

        assert_eq!(
            drain_bounded(&outbox, Duration::from_millis(50)),
            Drain::TimedOut,
            "a close with the writer still draining was read as a loss"
        );

        // Now drain it the way the writer would, and the same wait succeeds.
        assert_eq!(outbox.pop(), Some(ChildMessage::Ready));
        outbox.wrote();
        assert_eq!(drain_bounded(&outbox, Duration::ZERO), Drain::Empty);
    }
}
