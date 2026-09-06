//! The queue between the app's threads and the one thread that writes.
//!
//! Two push policies. A dropped metric costs nothing: the shepherd only
//! logs it at debug level. A dropped `Ready` hangs `wait_ready`, and a
//! dropped reply costs an operator the whole `action_timeout`. So metrics
//! are lossy and never block; everything else waits for room.
//!
//! One queue holds both. A full queue gives up a metric, never a
//! `Ready` or an `ActionReply`.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, PoisonError};

use crate::{ChannelError, ChildMessage};

/// How many messages may wait for the writer before the policy applies.
///
/// 1024 is a starting guess, not a measurement. `ChildMessage` is 64
/// bytes on the stack. So a full queue's fixed cost is tens of
/// kilobytes plus whatever names and bodies heap-allocate.
pub(crate) const DEFAULT_CAPACITY: usize = 1024;

#[derive(Debug)]
struct Inner {
    queue: VecDeque<ChildMessage>,
    dropped: u64,
    closed: bool,
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
            }),
            capacity,
            queued: Condvar::new(),
            drained: Condvar::new(),
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
            self.drained.notify_one();
        }
        taken
    }

    /// Releases every waiter. Idempotent.
    pub(crate) fn close(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.closed = true;
        drop(inner);
        self.queued.notify_all();
        self.drained.notify_all();
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
}
