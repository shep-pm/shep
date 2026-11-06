//! One attached connection's view of a sheep's pty, as bus events.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use shep_core::protocol::{BusEvent, encode_frame};

/// A pty's raw output turned into [`BusEvent::PtyOut`] events, ending with
/// [`BusEvent::PtyEnd`].
///
/// Each chunk of a read goes out as it arrived, whole and undecoded, so a
/// subscriber sees what the master saw. Dropping the stream drops the
/// subscription.
#[derive(Debug)]
pub(crate) struct PtyStream {
    name: String,
    chunks: broadcast::Receiver<Bytes>,
    ending: Ending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// The terminal may still speak.
    Open,
    /// The terminal closed and the end has not been sent.
    Closed,
    /// The end was sent.
    Done,
}

impl PtyStream {
    /// Streams `chunks`, the output of the sheep called `name`.
    pub(crate) fn new(name: String, chunks: broadcast::Receiver<Bytes>) -> Self {
        Self {
            name,
            chunks,
            ending: Ending::Open,
        }
    }

    /// The next event, waiting for it, or `None` once the end was sent.
    ///
    /// Cancel safe: nothing awaits between taking a chunk and returning its event.
    pub(crate) async fn next(&mut self) -> Option<BusEvent> {
        loop {
            match self.ending {
                Ending::Done => return None,
                Ending::Closed => {
                    self.ending = Ending::Done;
                    return Some(BusEvent::PtyEnd {
                        name: self.name.clone(),
                    });
                }
                Ending::Open => {}
            }
            match self.chunks.recv().await {
                Ok(chunk) => return Some(self.out(chunk.to_vec())),
                Err(RecvError::Lagged(count)) => return Some(BusEvent::Dropped { count }),
                Err(RecvError::Closed) => self.ending = Ending::Closed,
            }
        }
    }

    /// Writes every event to `out` from a task of its own, until the end was
    /// sent or the connection stopped taking frames.
    ///
    /// Aborting the writer drops the stream, and with it the subscription.
    pub(crate) fn forward(mut self, out: mpsc::Sender<Bytes>) -> PtyWriter {
        let ended = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ended);
        let task = tokio::spawn(async move {
            while let Some(event) = self.next().await {
                // Before the frame is queued: the client can answer it, and
                // the answer must find the writer already over.
                if matches!(event, BusEvent::PtyEnd { .. }) {
                    flag.store(true, Ordering::Release);
                }
                let Ok(frame) = encode_frame(&event) else {
                    return;
                };
                if out.send(frame).await.is_err() {
                    return;
                }
            }
        });
        PtyWriter { task, ended }
    }

    fn out(&self, data: Vec<u8>) -> BusEvent {
        BusEvent::PtyOut {
            name: self.name.clone(),
            data,
        }
    }
}

/// The task writing one attach's events to its connection.
#[derive(Debug)]
pub(crate) struct PtyWriter {
    task: JoinHandle<()>,
    ended: Arc<AtomicBool>,
}

impl PtyWriter {
    /// Is the writer still sending, so that another attach would be a second
    /// stream on the connection?
    ///
    /// False from the moment `PtyEnd` is queued, not from the moment the task
    /// is next polled: a client that reads `PtyEnd` may attach at once.
    pub(crate) fn is_live(&self) -> bool {
        !self.ended.load(Ordering::Acquire) && !self.task.is_finished()
    }

    /// Waits for the writer to finish, which an ended one does as soon as
    /// `PtyEnd` is queued, so that nothing it wrote can land after what the
    /// caller queues next.
    pub(crate) async fn finish(self) {
        let _ = self.task.await;
    }

    /// Stops the writer and drops the subscription.
    pub(crate) fn abort(self) {
        self.task.abort();
    }
}
