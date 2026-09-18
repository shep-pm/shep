//! The booted daemon and the connection that drives it, with the pid
//! bookkeeping that reaps real children on the panic path.

use super::*;

/// A booted daemon on its own `$SHEP_HOME`, with its run loop spawned.
///
/// `run`/`dir` are `Option`-wrapped so this type can carry a [`Drop`] impl:
/// a field cannot be moved out of a value whose type implements `Drop`.
pub(crate) struct Fixture {
    pub(crate) dir: Option<tempfile::TempDir>,
    pub(crate) paths: ShepPaths,
    pub(crate) ctx: RpcContext,
    pub(crate) run: Option<tokio::task::JoinHandle<Result<(), BootError>>>,
    // Real OS pids this fixture must reap on the panic path.
    pub(crate) spawned: std::sync::Arc<std::sync::Mutex<Vec<i32>>>,
}

impl Fixture {
    pub(crate) async fn boot(dir: tempfile::TempDir, restore: bool) -> Self {
        // $SHEP_HOME is the tempdir root itself: `sun_path` caps the socket
        // path at 104 bytes on macOS, and macOS temp paths are already long.
        let home = dir.path().to_path_buf();
        let paths = ShepPaths::resolve(
            &|key| (key == "SHEP_HOME").then(|| home.display().to_string()),
            std::path::Path::new("/nonexistent"),
        );
        // Bounded: `boot` binds the control address, which on Windows is a
        // machine-global pipe name rather than a path under `dir`.
        let daemon = tokio::time::timeout(
            RECV_TIMEOUT,
            boot(
                TokioRunner::new(),
                paths.clone(),
                BootOptions {
                    restore,
                    ..BootOptions::default()
                },
            ),
        )
        .await
        .expect("boot must not hang: the control address is already held")
        .expect("the daemon must boot on a fresh home");
        let ctx = daemon.context();
        let run = tokio::spawn(daemon.run());
        Self {
            dir: Some(dir),
            paths,
            ctx,
            run: Some(run),
            spawned: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    pub(crate) async fn connect(&self) -> Client {
        // `transport::connect` retries ERROR_PIPE_BUSY forever, and is safe
        // only because every caller bounds it.
        let stream = tokio::time::timeout(RECV_TIMEOUT, transport::connect(&self.paths.socket))
            .await
            .expect("connect must not hang: the pipe stayed busy")
            .unwrap();
        let mut client = Client {
            frames: Framed::new(stream, codec()),
            next_id: 1,
            hello_ack: None,
            pending: std::collections::VecDeque::new(),
            spawned: self.spawned.clone(),
        };
        client
            .send(&Hello {
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: PROTOCOL_VERSION,
                dog_name: None,
            })
            .await;
        let ack: HelloReply = client.recv_as().await;
        client.hello_ack = Some(ack.expect("the daemon must ack our protocol"));
        client
    }

    /// Shuts the daemon down and waits for its ordered teardown.
    pub(crate) async fn shutdown(mut self) -> tempfile::TempDir {
        self.ctx.shutdown();
        let run = self.run.take().expect("shutdown is only ever called once");
        tokio::time::timeout(RECV_TIMEOUT, run)
            .await
            .expect("teardown must not hang")
            .unwrap()
            .unwrap();
        self.dir.take().expect("dir is only ever taken once")
    }
}

/// Last-resort net for a test that panics before [`Fixture::shutdown`].
impl Drop for Fixture {
    /// Stops the daemon a panicking test never shut down, then reaps what a
    /// unix sheep leaves behind.
    ///
    /// `shutdown()` on the context alone, not a join: this runs during an
    /// unwind and must not block. A `current_thread` runtime unwinding stops
    /// polling `run`, so its kill ladder never finishes and the pids are
    /// signalled here, by process group to reach a `sleep 1` grandchild.
    ///
    /// Unix only: a Windows sheep cannot leave its job object.
    fn drop(&mut self) {
        // `None` only once `shutdown()` has taken it: the panic path.
        if self.run.is_some() {
            self.ctx.shutdown();
        }
        #[cfg(unix)]
        {
            let pids = self
                .spawned
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for &pid in pids.iter() {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(-pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
    }
}

/// A handshaken connection to a booted [`Fixture`], speaking shep-core's own
/// length-delimited/JSON codec directly rather than through a client crate.
pub(crate) struct Client {
    pub(crate) frames: Framed<ClientStream, LengthDelimitedCodec>,
    pub(crate) next_id: u64,
    pub(crate) hello_ack: Option<HelloAck>,
    // Frames the current call was not looking for, in arrival order. A bus
    // event is emitted before the reply to the command that caused it, so
    // discarding one would hang a later `await_process_event`.
    pub(crate) pending: std::collections::VecDeque<ServerFrame>,
    // Shared with the owning `Fixture`.
    pub(crate) spawned: std::sync::Arc<std::sync::Mutex<Vec<i32>>>,
}

impl Client {
    /// The daemon's handshake answer, set by [`Fixture::connect`].
    pub(crate) fn hello_ack(&self) -> &HelloAck {
        self.hello_ack
            .as_ref()
            .expect("hello_ack is only set after a successful handshake")
    }

    pub(crate) async fn send<T: Serialize>(&mut self, value: &T) {
        self.frames
            .send(encode_frame(value).unwrap())
            .await
            .unwrap();
    }

    /// Reads and decodes the next frame as `T`, timing out rather than
    /// hanging forever.
    pub(crate) async fn recv_as<T: DeserializeOwned>(&mut self) -> T {
        let frame = tokio::time::timeout(RECV_TIMEOUT, self.frames.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("connection closed early")
            .unwrap();
        decode_frame(&frame).unwrap()
    }

    /// The next frame of any kind: whatever an earlier call read but didn't
    /// consume, oldest first, else the next one off the wire.
    ///
    /// Records every process event's pid: a reload's replacement can be
    /// orphaned by a panic before any reply carries it.
    pub(crate) async fn next_frame(&mut self) -> ServerFrame {
        let frame = match self.pending.pop_front() {
            Some(frame) => frame,
            None => self.recv_as().await,
        };
        if let ServerFrame::Event(BusEvent::Process { info, .. }) = &frame {
            track_pid(&self.spawned, info);
        }
        frame
    }

    /// Sends one request, then reads frames until its `Reply` arrives,
    /// re-queueing any bus events that arrive in between for a later call.
    pub(crate) async fn request(&mut self, body: Request) -> Reply {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Envelope {
            id,
            deadline_ms: None,
            body,
        })
        .await;
        // Bounded: a daemon that never answers must fail by name, not hang.
        let mut skipped = Vec::new();
        let reply = tokio::time::timeout(RECV_TIMEOUT, async {
            loop {
                match self.next_frame().await {
                    ServerFrame::Reply(reply) if reply.id == id => break reply,
                    other => skipped.push(other),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for a reply to request {id}"));
        requeue(&mut self.pending, skipped);
        track_spawned(&self.spawned, &reply);
        reply
    }

    /// Sends a hand-built request body, past every validating newtype on
    /// [`Request`]. `None` when the daemon ended the connection instead,
    /// which is what it does with a body it cannot decode.
    ///
    /// Reads the wire directly: [`Self::next_frame`] panics on a closed
    /// connection, one of the two answers wanted here.
    pub(crate) async fn request_raw(&mut self, body: serde_json::Value) -> Option<Reply> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&serde_json::json!({
            "id": id,
            "deadline_ms": serde_json::Value::Null,
            "body": body,
        }))
        .await;
        let mut skipped = Vec::new();
        let reply = loop {
            let frame = tokio::time::timeout(RECV_TIMEOUT, self.frames.next())
                .await
                .expect("timed out waiting for a frame");
            let Some(frame) = frame else {
                break None; // the daemon closed on us: a refusal, not a hang
            };
            match decode_frame(&frame.unwrap()).unwrap() {
                ServerFrame::Reply(reply) if reply.id == id => break Some(reply),
                other => skipped.push(other),
            }
        };
        requeue(&mut self.pending, skipped);
        reply
    }

    /// Reads frames until a `Process` event of `kind` for `id` arrives,
    /// re-queueing everything else.
    pub(crate) async fn await_process_event(
        &mut self,
        id: u32,
        kind: ProcessEventKind,
    ) -> ProcessInfo {
        let mut skipped = Vec::new();
        let info = loop {
            let frame = self.next_frame().await;
            if let ServerFrame::Event(BusEvent::Process { event, info, .. }) = &frame
                && *event == kind
                && info.id == id
            {
                break info.clone();
            }
            skipped.push(frame);
        };
        requeue(&mut self.pending, skipped);
        info
    }

    /// Reads frames until a `Process` event of one of `kinds` arrives, and
    /// answers with which one.
    ///
    /// Stops at the first match, for a case whose subject is the order.
    pub(crate) async fn next_process_event_of(
        &mut self,
        kinds: &[ProcessEventKind],
    ) -> (ProcessEventKind, ProcessInfo) {
        let mut skipped = Vec::new();
        let found = loop {
            let frame = self.next_frame().await;
            if let ServerFrame::Event(BusEvent::Process { event, info, .. }) = &frame
                && kinds.contains(event)
            {
                break (*event, info.clone());
            }
            skipped.push(frame);
        };
        requeue(&mut self.pending, skipped);
        found
    }

    /// Reads frames until a `Process` event of `kind` arrives for any sheep,
    /// re-queueing everything else.
    ///
    /// For a reload's replacement, whose fresh id first reaches the client on
    /// the event itself.
    pub(crate) async fn await_any_process_event(&mut self, kind: ProcessEventKind) -> ProcessInfo {
        let mut skipped = Vec::new();
        let info = loop {
            let frame = self.next_frame().await;
            if let ServerFrame::Event(BusEvent::Process { event, info, .. }) = &frame
                && *event == kind
            {
                break info.clone();
            }
            skipped.push(frame);
        };
        requeue(&mut self.pending, skipped);
        info
    }

    /// Reads frames until a `LogOut` event for `id` arrives, re-queueing
    /// everything else.
    ///
    /// One overall [`RECV_TIMEOUT`], not `recv_as`'s per-frame one: a daemon
    /// emitting other frames forever must not spin this loop.
    pub(crate) async fn await_log_line(&mut self, id: u32) -> String {
        tokio::time::timeout(RECV_TIMEOUT, async {
            let mut skipped = Vec::new();
            let line = loop {
                let frame = self.next_frame().await;
                if let ServerFrame::Event(BusEvent::LogOut { id: event_id, line }) = &frame
                    && *event_id == id
                {
                    break line.clone();
                }
                skipped.push(frame);
            };
            requeue(&mut self.pending, skipped);
            line
        })
        .await
        .expect("timed out waiting for a log.* event")
    }
}

/// Records every live pid a reply's `ProcessInfo`s carry, for `Fixture`'s
/// panic-path cleanup.
///
/// Every variant that can carry a pid, not just `Started`: a muster restore's
/// fresh pids are only ever seen via a post-reboot `ListFlock`.
pub(crate) fn track_spawned(spawned: &std::sync::Arc<std::sync::Mutex<Vec<i32>>>, reply: &Reply) {
    let Ok(response) = &reply.result else {
        return;
    };
    let infos: &[ProcessInfo] = match response {
        Response::Flock(infos)
        | Response::Described(infos)
        | Response::Started(infos)
        | Response::Stopped(infos)
        | Response::Reopened(infos)
        | Response::Flushed(infos) => infos,
        // Struct-shaped, so neither can join the or-pattern above; the rows
        // a restart or a reload accepted are the half that carries pids.
        Response::Restarted { accepted, .. } | Response::Reloading { accepted, .. } => accepted,
        _ => return,
    };
    for info in infos {
        track_pid(spawned, info);
    }
}

/// Records one `ProcessInfo`'s pid, if it has one.
pub(crate) fn track_pid(spawned: &std::sync::Arc<std::sync::Mutex<Vec<i32>>>, info: &ProcessInfo) {
    if let Some(pid) = info.pid
        && let Ok(pid) = i32::try_from(pid)
    {
        spawned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(pid);
    }
}

/// Restores frames a call read but didn't want to the front of `pending`, in
/// arrival order, so [`Client::next_frame`] sees them before the wire.
pub(crate) fn requeue(
    pending: &mut std::collections::VecDeque<ServerFrame>,
    skipped: Vec<ServerFrame>,
) {
    for frame in skipped.into_iter().rev() {
        pending.push_front(frame);
    }
}

#[cfg(unix)]
/// Polls `kill(pid, None)` for ESRCH, no such process, rather than sleeping a
/// fixed guess.
pub(crate) async fn assert_reaped(pid: i32) {
    let reaped = tokio::time::timeout(RECV_TIMEOUT, async {
        loop {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                Err(nix::errno::Errno::ESRCH) => break,
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await;
    assert!(
        reaped.is_ok(),
        "pid {pid} must be reaped by teardown's kill ladder"
    );
}
