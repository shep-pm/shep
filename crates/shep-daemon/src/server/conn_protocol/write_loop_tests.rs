//! `write_loop` against a transport that counts flushes.

use core::time::Duration;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::StreamExt;
use shep_core::protocol::codec;
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};

use super::super::server_lifecycle::CONN_QUEUE;
use super::write_loop;

/// A transport that records what reaches it. It counts flushes, not write
/// syscalls: a flush is where the framed buffer is handed to the
/// transport, so it is the unit the loop controls.
#[derive(Clone, Default)]
struct Wire {
    flushes: Arc<AtomicUsize>,
    /// Bytes written so far, taken at each flush.
    flush_marks: Arc<Mutex<Vec<usize>>>,
    written: Arc<Mutex<Vec<u8>>>,
    fail_write: bool,
    fail_flush: bool,
}

impl Wire {
    fn flushes(&self) -> usize {
        self.flushes.load(Ordering::Relaxed)
    }

    fn flush_marks(&self) -> Vec<usize> {
        self.flush_marks
            .lock()
            .expect("no test panics holding it")
            .clone()
    }

    /// The payloads that reached the wire, in arrival order.
    async fn frames(&self) -> Vec<Bytes> {
        let written = self
            .written
            .lock()
            .expect("no test panics holding it")
            .clone();
        FramedRead::new(written.as_slice(), codec())
            .map(|frame| frame.expect("the loop writes whole frames").freeze())
            .collect()
            .await
    }
}

impl AsyncWrite for Wire {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.fail_write {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        self.written
            .lock()
            .expect("no test panics holding it")
            .extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        let written = self
            .written
            .lock()
            .expect("no test panics holding it")
            .len();
        self.flush_marks
            .lock()
            .expect("no test panics holding it")
            .push(written);
        if self.fail_flush {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// fails if each queued frame still costs its own flush. A followed log on
/// a chatty flock queues many lines behind the writer, and each flush is
/// a hand-off to the transport.
#[tokio::test(start_paused = true)]
async fn frames_already_queued_share_one_flush_and_keep_their_order() {
    let wire = Wire::default();
    let (tx, rx) = mpsc::channel::<Bytes>(CONN_QUEUE);
    let sent: Vec<Bytes> = (0..50).map(|n| Bytes::from(format!("line {n}"))).collect();
    for frame in &sent {
        tx.send(frame.clone()).await.expect("the queue has room");
    }
    drop(tx);

    write_loop(FramedWrite::new(wire.clone(), codec()), rx).await;

    assert_eq!(wire.flushes(), 1, "fifty queued frames, one flush");
    assert_eq!(wire.frames().await, sent);
}

/// fails if a frame fed after a batch is left in the buffer while the
/// loop waits for the next one: a quiet line must still reach the peer.
#[tokio::test(start_paused = true)]
async fn a_lone_frame_is_flushed_before_the_loop_waits_again() {
    let wire = Wire::default();
    let (tx, rx) = mpsc::channel::<Bytes>(CONN_QUEUE);
    let writer = tokio::spawn(write_loop(FramedWrite::new(wire.clone(), codec()), rx));

    tx.send(Bytes::from_static(b"quiet"))
        .await
        .expect("the writer is alive");
    // The paused clock only advances once every task is idle, so this
    // returns when the writer has parked on `recv`.
    tokio::time::sleep(Duration::from_millis(1)).await;

    assert_eq!(wire.frames().await, vec![Bytes::from_static(b"quiet")]);
    assert!(!writer.is_finished(), "the sender is still open");
}

/// fails if a dead peer no longer ends the loop. The flush is where the
/// error surfaces, and nothing is left to drain the queue after it.
#[tokio::test(start_paused = true)]
async fn a_failed_flush_ends_the_loop_with_the_sender_still_open() {
    let wire = Wire {
        fail_flush: true,
        ..Wire::default()
    };
    let (tx, rx) = mpsc::channel::<Bytes>(CONN_QUEUE);
    let writer = tokio::spawn(write_loop(FramedWrite::new(wire, codec()), rx));

    tx.send(Bytes::from_static(b"unheard"))
        .await
        .expect("the writer is alive");

    tokio::time::timeout(Duration::from_secs(5), writer)
        .await
        .expect("a failed flush must end the loop")
        .expect("the writer does not panic");
}

/// fails if a queue that never runs dry can hold the flush off. A steady
/// producer keeps `try_recv` returning frames, so only the cap ends the
/// batch; the frames are small enough that the buffer's backpressure
/// boundary never flushes for it.
#[tokio::test(start_paused = true)]
async fn a_queue_that_never_runs_dry_is_flushed_every_conn_queue_frames() {
    const WIRE_FRAME: usize = 10 + 4;
    const BATCHES: usize = 5;
    let wire = Wire::default();
    let (tx, rx) = mpsc::channel::<Bytes>(BATCHES * CONN_QUEUE);
    let sent: Vec<Bytes> = (0..BATCHES * CONN_QUEUE)
        .map(|n| Bytes::from(format!("line {n:05}")))
        .collect();
    for frame in &sent {
        tx.try_send(frame.clone()).expect("the queue has room");
    }
    let writer = tokio::spawn(write_loop(FramedWrite::new(wire.clone(), codec()), rx));

    // Returns once the writer has parked on `recv` with the queue empty.
    tokio::time::sleep(Duration::from_millis(1)).await;

    assert_eq!(
        wire.flush_marks(),
        (1..=BATCHES)
            .map(|batch| batch * CONN_QUEUE * WIRE_FRAME)
            .collect::<Vec<_>>(),
        "one flush per {CONN_QUEUE} frames fed"
    );
    assert_eq!(wire.frames().await, sent);
    assert!(!writer.is_finished(), "the sender is still open");
}

/// fails if a feed that errors mid-batch no longer ends the loop. Frames
/// past the buffer's backpressure boundary make `feed` write to the
/// transport, which is where a dead peer shows up before any flush.
#[tokio::test(start_paused = true)]
async fn a_failed_feed_mid_batch_ends_the_loop_and_drops_the_queue() {
    let wire = Wire {
        fail_write: true,
        ..Wire::default()
    };
    let (tx, rx) = mpsc::channel::<Bytes>(CONN_QUEUE);
    for _ in 0..20 {
        tx.try_send(Bytes::from(vec![b'x'; 1000]))
            .expect("the queue has room");
    }
    let writer = tokio::spawn(write_loop(FramedWrite::new(wire.clone(), codec()), rx));

    tokio::time::timeout(Duration::from_secs(5), writer)
        .await
        .expect("a failed feed must end the loop")
        .expect("the writer does not panic");

    assert_eq!(wire.flushes(), 0, "the flush is never reached");
    assert!(tx.is_closed(), "the loop dropped the queue it gave up on");
}
