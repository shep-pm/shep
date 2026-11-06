//! One attached connection's view of a sheep's pty, as bus events.

use bytes::{Bytes, BytesMut};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use shep_core::protocol::{BusEvent, encode_frame};

/// A pty's raw output turned into [`BusEvent::PtyOut`] text, ending with
/// [`BusEvent::PtyEnd`].
///
/// A read of the master can stop inside a multibyte character. The bytes of
/// that partial character wait for the rest, so no event carries half of one.
/// Dropping the stream drops the subscription.
#[derive(Debug)]
pub(crate) struct PtyStream {
    name: String,
    chunks: broadcast::Receiver<Bytes>,
    /// The incomplete character at the end of the last chunk, at most three bytes.
    held: BytesMut,
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
            held: BytesMut::new(),
            ending: Ending::Open,
        }
    }

    /// The next event, waiting for it, or `None` once the end was sent.
    ///
    /// Cancel safe: nothing awaits between taking a chunk and returning its text.
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
                Ok(chunk) => {
                    self.held.extend_from_slice(&chunk);
                    let data = self.take_whole_characters();
                    if !data.is_empty() {
                        return Some(self.out(data));
                    }
                }
                // Chunks were lost, so the held bytes no longer lead into
                // what comes next.
                Err(RecvError::Lagged(count)) => {
                    self.held.clear();
                    return Some(BusEvent::Dropped { count });
                }
                Err(RecvError::Closed) => {
                    self.ending = Ending::Closed;
                    // What was held never completes.
                    let rest = String::from_utf8_lossy(&self.held).into_owned();
                    self.held.clear();
                    if !rest.is_empty() {
                        return Some(self.out(rest));
                    }
                }
            }
        }
    }

    /// Writes every event to `out` from a task of its own, until the end was
    /// sent or the connection stopped taking frames.
    ///
    /// Aborting the task drops the stream, and with it the subscription.
    pub(crate) fn forward(mut self, out: mpsc::Sender<Bytes>) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = self.next().await {
                let Ok(frame) = encode_frame(&event) else {
                    return;
                };
                if out.send(frame).await.is_err() {
                    return;
                }
            }
        })
    }

    fn out(&self, data: String) -> BusEvent {
        BusEvent::PtyOut {
            name: self.name.clone(),
            data,
        }
    }

    /// Takes the text off the front of `held`, leaving only a character that
    /// is cut short at its end. A byte that can start no character becomes
    /// U+FFFD.
    fn take_whole_characters(&mut self) -> String {
        let mut text = String::new();
        loop {
            match core::str::from_utf8(&self.held) {
                Ok(valid) => {
                    text.push_str(valid);
                    self.held.clear();
                    return text;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    let head = self.held.split_to(valid);
                    // `valid_up_to` is the length of the valid prefix.
                    text.push_str(&String::from_utf8_lossy(&head));
                    let Some(bad) = error.error_len() else {
                        return text;
                    };
                    let _ = self.held.split_to(bad);
                    text.push(char::REPLACEMENT_CHARACTER);
                }
            }
        }
    }
}
