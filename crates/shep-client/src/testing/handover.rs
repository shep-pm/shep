use super::*;
use futures_util::{SinkExt, StreamExt};
use shep_core::protocol::{
    Envelope, Hello, HelloAck, HelloReply, ProcessInfo, Request, Response, RpcError, codec,
    decode_frame, encode_frame,
};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::Framed;

/// What one generation of [`fake_daemon_across_handovers`] does with the
/// `Hello` a client sends it: the three shapes a reconnecting client has
/// to survive, and no others.
#[derive(Debug, Clone)]
pub enum Handshake {
    /// Answer the `Hello` with this ack, then serve requests until the
    /// connection is cut.
    Accept(HelloAck),
    /// Refuse the `Hello` with this error and close, as a successor
    /// compiled against an older protocol would.
    Refuse(RpcError),
    /// Close without answering the `Hello`, as a successor still coming up
    /// would.
    Drop,
}

/// A fake shepherd that survives a handover: one listener, one accepted
/// connection at a time, and a [`Self::cut`] that drops the accepted
/// connection out from under the client while the listener stays bound,
/// matching a real daemon's `execve`, which carries the listening socket
/// across but cannot carry an accepted connection.
///
/// Each connection is served by the next [`Handshake`] in the list; once
/// they run out, the last one repeats.
///
/// Panics if `path` cannot be bound, or on any accept, decode or encode
/// failure.
#[derive(Debug)]
pub struct Handovers {
    cut: mpsc::Sender<()>,
    cut_on_next_request: Arc<AtomicBool>,
    accepted: Arc<AtomicU32>,
    hellos: Arc<Mutex<Vec<Hello>>>,
    envelopes: Arc<Mutex<Vec<(u32, Envelope)>>>,
    armed_list: Arc<Mutex<Vec<ProcessInfo>>>,
    armed_dog_section: Arc<Mutex<String>>,
    armed_subscribe_err: Arc<Mutex<Option<RpcError>>>,
    task: JoinHandle<()>,
}

impl Drop for Handovers {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Binds `path` and serves one connection per entry in `handshakes`, in
/// order. See [`Handovers`] for what this fixture models.
///
/// Panics if `path` cannot be bound.
#[must_use]
pub fn fake_daemon_across_handovers(path: &Path, handshakes: Vec<Handshake>) -> Handovers {
    assert!(
        !handshakes.is_empty(),
        "a handover fixture needs at least one generation"
    );
    let mut listener = bind(path);
    let (cut_tx, mut cut_rx) = mpsc::channel(SCRIPT_CHANNEL_CAPACITY);
    let cut_on_next_request = Arc::new(AtomicBool::new(false));
    let accepted = Arc::new(AtomicU32::new(0));
    let hellos: Arc<Mutex<Vec<Hello>>> = Arc::new(Mutex::new(Vec::new()));
    let envelopes: Arc<Mutex<Vec<(u32, Envelope)>>> = Arc::new(Mutex::new(Vec::new()));
    let armed_list: Arc<Mutex<Vec<ProcessInfo>>> = Arc::new(Mutex::new(Vec::new()));
    let armed_dog_section: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let armed_subscribe_err: Arc<Mutex<Option<RpcError>>> = Arc::new(Mutex::new(None));

    let task = tokio::spawn({
        let cut_on_next_request = Arc::clone(&cut_on_next_request);
        let accepted = Arc::clone(&accepted);
        let hellos = Arc::clone(&hellos);
        let envelopes = Arc::clone(&envelopes);
        let armed_list = Arc::clone(&armed_list);
        let armed_dog_section = Arc::clone(&armed_dog_section);
        let armed_subscribe_err = Arc::clone(&armed_subscribe_err);
        async move {
            let mut generation: u32 = 0;
            while let Ok(stream) = listener.accept().await {
                generation += 1;
                accepted.fetch_add(1, Ordering::SeqCst);
                let index = usize::try_from(generation - 1).unwrap_or(usize::MAX);
                let script = handshakes
                    .get(index)
                    .unwrap_or_else(|| handshakes.last().expect("non-empty, asserted above"))
                    .clone();
                let mut frames = Framed::new(stream, codec());
                match script {
                    Handshake::Drop => continue,
                    Handshake::Refuse(err) => {
                        // Recorded before the refusal: the only path where
                        // a real daemon learns which dog it just refused.
                        if let Some(Ok(first)) = frames.next().await {
                            hellos.lock().unwrap().push(decode_frame(&first).unwrap());
                        }
                        let reply: HelloReply = Err(err);
                        let _ = frames.send(encode_frame(&reply).unwrap()).await;
                        continue;
                    }
                    Handshake::Accept(ack) => {
                        let hello = handshake(&mut frames, ack).await;
                        hellos.lock().unwrap().push(hello);
                    }
                }
                loop {
                    tokio::select! {
                        frame = frames.next() => {
                            let Some(Ok(bytes)) = frame else { break };
                            let envelope: Envelope = decode_frame(&bytes).unwrap();
                            let id = envelope.id;
                            let body = envelope.body.clone();
                            envelopes.lock().unwrap().push((generation, envelope));
                            if cut_on_next_request.swap(false, Ordering::SeqCst) {
                                break;
                            }
                            // Armed refusal, taken so it answers one
                            // `Subscribe` and the next is served normally.
                            // Taken into a local first: the guard must not
                            // be alive across the write below, or this
                            // future stops being `Send`.
                            let refusal = if matches!(body, Request::Subscribe { .. }) {
                                armed_subscribe_err.lock().unwrap().take()
                            } else {
                                None
                            };
                            if let Some(refusal) = refusal {
                                write_err(&mut frames, id, refusal.code, refusal.message).await;
                                continue;
                            }
                            let response = match body {
                                Request::ListFlock => {
                                    Response::Flock(armed_list.lock().unwrap().clone())
                                }
                                Request::Subscribe { .. } => Response::Subscribed,
                                // A dog refuses to run on a reply it can't
                                // parse, and this is its first request.
                                Request::DogConfig { .. } => Response::DogSection {
                                    toml: armed_dog_section.lock().unwrap().clone().into(),
                                },
                                _ => Response::Pong,
                            };
                            write_reply(&mut frames, id, response).await;
                        }
                        _ = cut_rx.recv() => break,
                    }
                }
            }
        }
    });

    Handovers {
        cut: cut_tx,
        cut_on_next_request,
        accepted,
        hellos,
        envelopes,
        armed_list,
        armed_dog_section,
        armed_subscribe_err,
        task,
    }
}

impl Handovers {
    /// Drops the currently accepted connection, leaving the listener bound,
    /// matching what a real `execve` produces for the client.
    pub async fn cut(&self) {
        let _ = self.cut.send(()).await;
    }

    /// Arms the current connection to read its next request envelope, then
    /// die without answering it.
    ///
    /// Synchronous, not `async`: an `await` here would give the serving
    /// task a chance to run before the test arms it.
    pub fn cut_on_next_request(&self) {
        self.cut_on_next_request.store(true, Ordering::SeqCst);
    }

    /// How many connections this fake has accepted so far, across every
    /// generation.
    #[must_use]
    pub fn accepted(&self) -> u32 {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Every `Hello` this fake has read, in order, including ones it went
    /// on to refuse: the only path where a real daemon learns which dog it
    /// just turned away.
    #[must_use]
    pub fn hellos(&self) -> Vec<Hello> {
        self.hellos.lock().unwrap().clone()
    }

    /// Every request envelope received so far, paired with the 1-based
    /// generation that received it.
    #[must_use]
    pub fn envelopes(&self) -> Vec<(u32, Envelope)> {
        self.envelopes.lock().unwrap().clone()
    }

    /// Arms the answer every generation gives to `Request::ListFlock`.
    /// Unlike [`FakeDaemon::reply_to_list`], not consumed: a reload test
    /// asks both generations the same question.
    pub fn reply_to_list(&self, flock: Vec<ProcessInfo>) {
        *self.armed_list.lock().unwrap() = flock;
    }

    /// Arms the `[<name>]` section every generation answers
    /// `Request::DogConfig` with. Empty until set, matching a home with no
    /// dog configured.
    ///
    /// Not consumed, like [`Self::reply_to_list`]: a dog asks again after a
    /// handover.
    pub fn reply_to_dog_config(&self, section: &str) {
        *self.armed_dog_section.lock().unwrap() = section.to_owned();
    }

    /// Arms the next `Request::Subscribe` to be answered with `refusal`
    /// instead of `Response::Subscribed`.
    ///
    /// Taken rather than held, so exactly one `Subscribe` is refused and a
    /// later one is served. That is the shape a caller needs to tell a
    /// shepherd refusing the request from a connection that died: the
    /// second answers, and only a caller that stopped on the first will
    /// not see it.
    pub fn refuse_next_subscribe(&self, refusal: RpcError) {
        *self.armed_subscribe_err.lock().unwrap() = Some(refusal);
    }
}
