use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use crate::dispatch::Dispatch;
use crate::loops::{reader_loop, writer_loop};
use crate::metric::MetricOptions;
use crate::outbox::{DEFAULT_CAPACITY, Drain, Outbox};
use crate::{CHANNEL_VERSION, Channel, ChannelError, ChildMessage, LambLabel, VERSION_VAR};

/// What to tell an author running under shep with no channel.
const NO_CHANNEL_ADVICE: &str = "no channel on this process. Set `channel = true` \
     (or `wait_ready` / `shutdown_with_message`) on this app in the Flockfile to open one.";

/// Writes one line of advice to stderr, prefixed so it is attributable.
///
/// This crate has no logging dependency. shep already collects an app's
/// stderr as bleats. The author reads this where they already look.
fn warn(message: &str) {
    eprintln!("shep-channel: {message}");
}

/// A handle on this process's shepherd channel.
///
/// Cheap to clone and safe to share across threads: every method takes
/// `&self`. With no channel, every method is a no-op. Callers need no
/// branch on whether the operator opted in.
#[derive(Clone)]
pub struct Shepherd(Arc<Inner>);

// Hand-written: the derive reaches the outbox's queued messages.
// It would print reply bodies and metric names verbatim (IR-41).
impl core::fmt::Debug for Shepherd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Shepherd")
            .field("active", &self.is_active())
            .field("dropped_metrics", &self.dropped_metrics())
            // Whether the shepherd stamped a version, not which one: the
            // value comes straight from the environment.
            .field("stamped", &self.0.version.is_some())
            .finish()
    }
}

// Pins the "safe to share... from any thread" claim at compile time.
// A field addition that breaks it fails the build, not a reader's assumption.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Shepherd>();
};

#[derive(Debug)]
struct Inner {
    /// `None` when this process has no channel.
    outbox: Option<Arc<Outbox>>,
    dispatch: Arc<RwLock<Dispatch>>,
    version: Option<String>,
}

impl Shepherd {
    fn inert(version: Option<String>) -> Self {
        Self(Arc::new(Inner {
            outbox: None,
            dispatch: Arc::new(RwLock::new(Dispatch::default())),
            version,
        }))
    }

    /// Whether this process's channel is live right now.
    ///
    /// False before the operator opens one, and false again once the shepherd
    /// goes away. [`Shepherd::dropped_metrics`] never freezes silently as a
    /// result.
    ///
    /// Checking this is optional: without a channel nothing is sent.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.0
            .outbox
            .as_ref()
            .is_some_and(|outbox| !outbox.is_closed())
    }

    /// The `SHEP_CHANNEL_VERSION` stamp, when the shepherd set one.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.0.version.as_deref()
    }

    /// How many metric samples and notes have been dropped because the
    /// shepherd was not keeping up. Always 0 without a channel.
    #[must_use]
    pub fn dropped_metrics(&self) -> u64 {
        self.0.outbox.as_ref().map_or(0, |outbox| outbox.dropped())
    }

    /// Registers a handler for one action name, replacing any prior one.
    ///
    /// The handler takes the action's params and its name, in that order.
    /// Params are `None` when the trigger carried none, and the return
    /// value becomes the reply body. Two handlers may run at once, hence
    /// the `Send + Sync` bound. Safe to call from inside a handler.
    pub fn on_action<H>(&self, name: impl AsRef<str>, handler: H) -> &Self
    where
        H: Fn(Option<&str>, &str) -> String + Send + Sync + 'static,
    {
        self.0
            .dispatch
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register_action(name.as_ref().to_string(), Box::new(handler));
        self
    }

    /// Registers the handler run when the shepherd asks this app to stop.
    ///
    /// Without one, a shutdown message warns and nothing else happens. This
    /// crate never ends a process on its own judgement.
    ///
    /// A shutdown is looked up only after the running action handlers
    /// finish. An action handler can therefore register this one.
    pub fn on_shutdown<H>(&self, handler: H) -> &Self
    where
        H: Fn() + Send + Sync + 'static,
    {
        self.0
            .dispatch
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register_shutdown(Box::new(handler));
        self
    }

    /// Says this app is up. Blocks only until the message is queued.
    ///
    /// # Errors
    ///
    /// [`ChannelError::Closed`] when the shepherd has gone away. Without a
    /// channel this always returns `Ok(())`: nothing to report, no
    /// failure to handle.
    pub fn ready(&self) -> Result<(), ChannelError> {
        match &self.0.outbox {
            Some(outbox) => outbox.push_blocking(ChildMessage::Ready),
            None => Ok(()),
        }
    }

    /// Waits for everything already queued to reach the shepherd.
    ///
    /// `ready`, `metric` and an action reply hand their message to a writer
    /// thread and return. A process that ends first loses them. Call this
    /// before exiting from `on_shutdown`. Safe to call from a handler. It
    /// returns as soon as the outbox is empty, so another thread can hold
    /// it open. Keep `timeout` inside `kill_timeout`.
    ///
    /// # Errors
    ///
    /// - [`ChannelError::TimedOut`] when `timeout` ran out; [`Shepherd::pending`] counts what is left.
    /// - [`ChannelError::Closed`] when the writer stopped first: the shepherd
    ///   has gone, and those messages are lost.
    pub fn flush(&self, timeout: Duration) -> Result<(), ChannelError> {
        let Some(outbox) = &self.0.outbox else {
            return Ok(());
        };
        match outbox.drain(timeout) {
            Drain::Empty => Ok(()),
            Drain::Stopped => Err(ChannelError::Closed),
            Drain::TimedOut => Err(ChannelError::TimedOut),
        }
    }

    /// How many messages are still waiting to reach the shepherd.
    ///
    /// A snapshot, since the writer thread is draining while you read
    /// it. After [`Shepherd::flush`] returns [`ChannelError::Closed`] it
    /// is exact and is what the app lost. Always 0 without a channel.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.0.outbox.as_ref().map_or(0, |outbox| outbox.pending())
    }

    /// Records one metric sample. Never blocks and never fails.
    ///
    /// A sample may be dropped if the shepherd stops reading, which keeps a
    /// hot path from parking on a full socket. See
    /// [`Shepherd::dropped_metrics`].
    ///
    /// Unlike `on_action`, this takes `impl Into<String>`: an owned `String` moves in for free.
    pub fn metric(&self, name: impl Into<String>, value: f64) {
        if let Some(outbox) = &self.0.outbox {
            outbox.push_lossy(ChildMessage::Metric {
                name: name.into(),
                value,
                metric_type: None,
                labels: std::collections::BTreeMap::new(),
                help: None,
            });
        }
    }

    /// Records one metric sample with a kind, labels or help text.
    ///
    /// Never blocks and never fails, like [`Shepherd::metric`]: a sample
    /// may be dropped under backpressure; see [`Shepherd::dropped_metrics`].
    ///
    /// ```
    /// use shep_channel::{MetricOptions, MetricType};
    ///
    /// let shepherd = shep_channel::serve();
    /// shepherd.metric_with(
    ///     "queue_depth",
    ///     3.0,
    ///     MetricOptions::new()
    ///         .kind(MetricType::Counter)
    ///         .label("model", "qwen")
    ///         .help("Requests waiting for the lease."),
    /// );
    /// ```
    pub fn metric_with(&self, name: impl Into<String>, value: f64, options: MetricOptions) {
        if let Some(outbox) = &self.0.outbox {
            outbox.push_lossy(ChildMessage::Metric {
                name: name.into(),
                value,
                metric_type: options.kind,
                labels: options.labels,
                help: options.help,
            });
        }
    }

    /// Sends a dog a name and a number that is not a metric sample.
    ///
    /// The shepherd publishes it as `channel.note`, which the metrics dog
    /// does not subscribe to, so `name` needs no metric grammar. Never
    /// blocks and never fails, like [`Shepherd::metric`]: a note may be
    /// dropped under backpressure and counts in [`Shepherd::dropped_metrics`].
    pub fn note(&self, name: impl Into<String>, value: f64) {
        if let Some(outbox) = &self.0.outbox {
            outbox.push_lossy(ChildMessage::Note {
                name: name.into(),
                value,
            });
        }
    }

    /// Names one of this app's child processes in `shep describe`.
    ///
    /// Blocks only until the message is queued, like [`Shepherd::ready`]:
    /// a dropped label would leave the lamb unnamed with nothing to say
    /// why. An empty label clears the one `pid` had.
    ///
    /// ```
    /// use shep_channel::LambLabel;
    ///
    /// let shepherd = shep_channel::serve();
    /// // Usually `Child::id()` of a process this app just spawned.
    /// let worker_pid = 4312;
    /// shepherd
    ///     .label_lamb(worker_pid, LambLabel::new("worker 1").unwrap())
    ///     .unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// [`ChannelError::Closed`] when the shepherd has gone away. Without a
    /// channel this always returns `Ok(())`.
    pub fn label_lamb(&self, pid: u32, label: LambLabel) -> Result<(), ChannelError> {
        match &self.0.outbox {
            Some(outbox) => outbox.push_blocking(ChildMessage::LambLabel { pid, label }),
            None => Ok(()),
        }
    }
}

/// Opens this process's channel and starts serving it.
///
/// Always returns a usable handle. With no channel, every call on it is a
/// no-op. An app needs no branch at its emit sites. One line goes to stderr
/// when `SHEP_NAME` says this process runs under shep.
///
/// A process singleton: the channel is one descriptor, owned once.
#[must_use]
pub fn serve() -> Shepherd {
    static SHEPHERD: OnceLock<Shepherd> = OnceLock::new();
    static CALLS: AtomicU32 = AtomicU32::new(0);

    let shepherd = SHEPHERD.get_or_init(start);
    if CALLS.fetch_add(1, Ordering::Relaxed) == 1 {
        warn(
            "serve() called more than once; returning the first handle. \
              The channel is one descriptor and cannot be opened twice.",
        );
    }
    shepherd.clone()
}

fn start() -> Shepherd {
    let channel = match Channel::open() {
        Ok(Some(channel)) => channel,
        Ok(None) => {
            if std::env::var_os("SHEP_NAME").is_some() {
                warn(NO_CHANNEL_ADVICE);
            }
            return Shepherd::inert(None);
        }
        Err(error) => {
            warn(&format!("{error}; continuing without a channel"));
            return Shepherd::inert(None);
        }
    };

    let (reader, mut writer, version) = channel.into_halves();
    if let Some(stamp) = &version
        && stamp != CHANNEL_VERSION
    {
        warn(&format!(
            "the shepherd stamps {VERSION_VAR}={stamp} and this crate implements \
             {CHANNEL_VERSION}; continuing, since a newer wire has so far only added \
             fields an older reader ignores"
        ));
    }

    let outbox = Arc::new(Outbox::new(DEFAULT_CAPACITY));
    let dispatch = Arc::new(RwLock::new(Dispatch::default()));

    let writing = Arc::clone(&outbox);
    let writer_spawn = std::thread::Builder::new()
        .name("shep-channel-writer".to_string())
        .spawn(move || writer_loop(&mut writer, &writing));
    if let Err(error) = writer_spawn {
        // Without this thread nothing drains the outbox, so `ready()` would
        // queue silently and `wait_ready` would hang. The handle below is
        // inert instead: it holds no outbox. Its stopped one drops on the
        // next line.
        warn(&format!(
            "failed to spawn the shep-channel writer thread: {error}; continuing without a channel"
        ));
        outbox.stop();
        return Shepherd::inert(version);
    }

    let reading = Arc::clone(&outbox);
    let handlers = Arc::clone(&dispatch);
    let reader_spawn = std::thread::Builder::new()
        .name("shep-channel-reader".to_string())
        .spawn(move || {
            let mut reader = reader;
            reader_loop(&mut reader, &reading, &handlers, &warn);
        });
    if let Err(error) = reader_spawn {
        // The writer still works without this thread: `ready()` and
        // `metric()` reach the shepherd. Only actions go unanswered, since
        // nothing reads the action message off the wire. Warn and keep
        // the handle partly working.
        warn(&format!(
            "failed to spawn the shep-channel reader thread: {error}; readiness and metrics still work, but no action sent to this process will ever be answered"
        ));
    }

    Shepherd(Arc::new(Inner {
        outbox: Some(outbox),
        dispatch,
        version,
    }))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor, Write, pipe};
    use std::sync::{PoisonError, mpsc};
    use std::time::Duration;

    use super::*;
    use crate::MetricType;

    /// Bounds the one test below that could hang forever on a real
    /// regression. A working reader answers in microseconds. This is slack
    /// for a loaded runner, not an expected duration.
    const DEADLINE: Duration = Duration::from_secs(5);

    /// A handle over `outbox`, the shape `serve()` builds once a channel
    /// opens. `Shepherd::inert` is the other one.
    fn shepherd_over(outbox: &Arc<Outbox>) -> Shepherd {
        Shepherd(Arc::new(Inner {
            outbox: Some(Arc::clone(outbox)),
            dispatch: Arc::new(RwLock::new(Dispatch::default())),
            version: None,
        }))
    }

    /// Runs `flush(timeout)` on its own thread, bounded from outside the
    /// call.
    ///
    /// `timeout` is the input to the call under test, so it cannot also
    /// be this test's forcing mechanism (IR-46). `DEADLINE` is, and it
    /// is held on this side, so a flush that never returns fails here
    /// rather than parking the binary.
    #[track_caller]
    fn flush_bounded(shepherd: &Shepherd, timeout: Duration) -> Result<(), ChannelError> {
        let (tx, rx) = mpsc::channel();
        let flushing = shepherd.clone();
        // Never joined, for the reason `outbox::tests::drain_bounded`
        // gives: joining a parked flush puts the hang back.
        std::thread::spawn(move || {
            let _ = tx.send(flushing.flush(timeout));
        });
        rx.recv_timeout(DEADLINE).expect("flush never returned")
    }

    #[test]
    fn an_inert_handle_accepts_everything_and_does_nothing() {
        let shepherd = Shepherd::inert(None);
        assert!(!shepherd.is_active());
        shepherd.on_action("gc", |_, _| "ok".to_string());
        shepherd.on_shutdown(|| {});
        shepherd.metric("rps", 42.0);
        shepherd.metric_with("rps", 42.0, MetricOptions::new());
        shepherd.note("lease.web.want.1", 1.0);
        shepherd.ready().expect("an inert ready is not an error");
        shepherd
            .flush(Duration::ZERO)
            .expect("an inert flush is not an error");
        assert_eq!(shepherd.pending(), 0);
        assert_eq!(shepherd.dropped_metrics(), 0);
        assert_eq!(shepherd.version(), None);
    }

    #[test]
    fn a_handle_stops_being_active_once_the_channel_closes() {
        let outbox = Arc::new(Outbox::new(4));
        let shepherd = shepherd_over(&outbox);
        assert!(shepherd.is_active(), "a fresh channel should read as live");

        outbox.close();

        assert!(
            !shepherd.is_active(),
            "a handle whose shepherd went away still reads as live"
        );
    }

    /// Reads what `metric_with` left in the outbox, serialized: the
    /// exact bytes the writer will hand the transport.
    #[test]
    fn metric_with_puts_labels_type_and_help_on_the_wire() {
        let outbox = Arc::new(Outbox::new(4));
        let shepherd = shepherd_over(&outbox);
        shepherd.metric_with(
            "queue_depth",
            3.0,
            MetricOptions::new()
                .kind(MetricType::Counter)
                .label("model", "qwen")
                .help("Requests waiting for the lease."),
        );

        let taken = outbox.take();
        let [ChildMessage::Metric { .. }] = taken.as_slice() else {
            panic!("expected one metric sample in the outbox, got {taken:?}");
        };
        assert_eq!(
            serde_json::to_string(&taken[0]).expect("encode"),
            concat!(
                r#"{"kind":"metric","name":"queue_depth","value":3.0,"type":"counter","#,
                r#""labels":{"model":"qwen"},"help":"Requests waiting for the lease."}"#,
            )
        );
    }

    /// The lossy push, like a sample: a full queue drops and counts.
    #[test]
    fn a_note_goes_on_the_wire_and_drops_like_a_sample() {
        let outbox = Arc::new(Outbox::new(1));
        let shepherd = shepherd_over(&outbox);
        shepherd.note("lease.web.want.1", 1.0);
        shepherd.note("lease.web.want.2", 2.0);

        assert_eq!(shepherd.dropped_metrics(), 1);
        let taken = outbox.take();
        assert_eq!(
            serde_json::to_string(&taken).expect("encode"),
            r#"[{"kind":"note","name":"lease.web.want.2","value":2.0}]"#
        );
    }

    /// IR-41: pins the exact string, and separately that no queued payload
    /// appears. Either check alone would catch a regression.
    #[test]
    fn a_shepherds_debug_names_state_and_never_a_queued_payload() {
        let outbox = Arc::new(Outbox::new(4));
        outbox.push_lossy(ChildMessage::ActionReply {
            action: "gc".into(),
            body: "SECRET-REPLY-BODY".into(),
            id: Some(7),
        });
        outbox.push_lossy(ChildMessage::Metric {
            name: "SECRET-METRIC-NAME".into(),
            value: 1.0,
            metric_type: None,
            labels: std::collections::BTreeMap::new(),
            help: None,
        });
        let shepherd = Shepherd(Arc::new(Inner {
            outbox: Some(Arc::clone(&outbox)),
            dispatch: Arc::new(RwLock::new(Dispatch::default())),
            version: Some("1".into()),
        }));

        let rendered = format!("{shepherd:?}");
        assert_eq!(
            rendered,
            "Shepherd { active: true, dropped_metrics: 0, stamped: true }"
        );
        assert!(
            !rendered.contains("SECRET-REPLY-BODY") && !rendered.contains("SECRET-METRIC-NAME"),
            "a queued payload reached the Debug output: {rendered}"
        );
    }

    /// Checks the exact `channel = true` clause, not the bare substring
    /// "channel". That substring is already in the advice's own preamble.
    #[test]
    fn the_no_channel_advice_names_every_field_that_opens_one() {
        for field in ["channel = true", "wait_ready", "shutdown_with_message"] {
            assert!(
                NO_CHANNEL_ADVICE.contains(field),
                "advice does not mention {field}"
            );
        }
    }

    /// Only the blocking push reports a refusal. A lossy one would count a
    /// dropped message and return nothing.
    #[test]
    fn a_lamb_label_the_shepherd_cannot_take_is_an_error_not_a_drop() {
        let outbox = Arc::new(Outbox::new(1));
        let shepherd = shepherd_over(&outbox);
        let label = LambLabel::new("worker 1").expect("a valid label");

        shepherd
            .label_lamb(4312, label.clone())
            .expect("room for the label");
        assert_eq!(
            outbox.take(),
            vec![ChildMessage::LambLabel {
                pid: 4312,
                label: label.clone()
            }]
        );
        outbox.close();
        assert!(matches!(
            shepherd.label_lamb(4312, label),
            Err(ChannelError::Closed)
        ));
    }

    /// The issue this API exists for. The sink is held shut until the
    /// test opens it, so a handler that did not wait would read an empty
    /// wire every run rather than losing the race sometimes.
    ///
    /// Also the deadlock check: the shutdown handler runs on the reader
    /// thread, and a flush that waited on anything the reader still owes
    /// would never return. `DEADLINE` bounds that.
    #[test]
    fn a_shutdown_handler_can_flush_a_reply_that_was_still_queued() {
        let outbox = Arc::new(Outbox::new(4));
        let shepherd = shepherd_over(&outbox);
        outbox
            .push_blocking(ChildMessage::ActionReply {
                action: "gc".to_string(),
                body: "collected".to_string(),
                id: Some(7),
            })
            .expect("room for the reply");

        let wire = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (open, gate) = mpsc::channel();
        let mut sink = GatedSink {
            wire: Arc::clone(&wire),
            gate: Some(gate),
        };
        let writing = Arc::clone(&outbox);
        let writer = std::thread::spawn(move || writer_loop(&mut sink, &writing));

        // What the handler saw at the moment its flush returned.
        let (report, handled) = mpsc::channel();
        let flushing = shepherd.clone();
        let seen = Arc::clone(&wire);
        shepherd.on_shutdown(move || {
            let flushed = flushing.flush(DEADLINE).is_ok();
            let bytes = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
            report
                .send((flushed, String::from_utf8(bytes).expect("valid utf8")))
                .expect("report");
        });

        let dispatch = Arc::clone(&shepherd.0.dispatch);
        let reader = std::thread::spawn(move || {
            let mut input = Cursor::new(b"{\"kind\":\"shutdown\"}\n".to_vec());
            reader_loop(&mut input, &outbox, &dispatch, &|_: &str| {});
        });

        assert!(
            handled.recv_timeout(Duration::from_millis(200)).is_err(),
            "the shutdown handler returned before the queued reply was written"
        );
        open.send(()).expect("open the sink");

        let (flushed, written) = handled
            .recv_timeout(DEADLINE)
            .expect("the shutdown handler never returned");
        assert!(flushed, "flush did not report the reply delivered");
        assert!(
            written.contains("\"body\":\"collected\""),
            "the handler resumed before its queued reply reached the wire: {written:?}"
        );

        reader.join().expect("reader panicked");
        writer.join().expect("writer panicked");
    }

    /// A failed write loses the message it was carrying. Reporting that
    /// as a clean drain is the one answer `flush` must never give.
    #[test]
    fn a_flush_reports_closed_when_the_writer_failed_mid_message() {
        let outbox = Arc::new(Outbox::new(4));
        let shepherd = shepherd_over(&outbox);
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for readiness");

        writer_loop(&mut FailingSink, &outbox);

        assert!(matches!(
            flush_bounded(&shepherd, DEADLINE),
            Err(ChannelError::Closed)
        ));
        assert_eq!(shepherd.pending(), 1, "the lost message is still counted");
    }

    /// A slow shepherd is not a gone one, and an app that gives up early
    /// should be told which it hit. The 50ms is this test's own bound:
    /// nothing drains this outbox, so a regression fails rather than
    /// parks.
    #[test]
    fn a_flush_reports_a_timeout_rather_than_a_closed_channel() {
        let outbox = Arc::new(Outbox::new(4));
        let shepherd = shepherd_over(&outbox);
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for readiness");

        assert!(matches!(
            flush_bounded(&shepherd, Duration::from_millis(50)),
            Err(ChannelError::TimedOut)
        ));
        assert_eq!(shepherd.pending(), 1);
    }

    /// A `Write` the test holds shut. The writer thread parks on its
    /// first write until [`GatedSink::gate`] is sent, which is what puts
    /// the flush under test in a window it cannot win by luck.
    #[derive(Debug)]
    struct GatedSink {
        wire: Arc<std::sync::Mutex<Vec<u8>>>,
        gate: Option<mpsc::Receiver<()>>,
    }

    impl Write for GatedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Some(gate) = self.gate.take() {
                gate.recv().expect("the test never opened the sink");
            }
            self.wire
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A transport that has gone away under the writer.
    #[derive(Debug)]
    struct FailingSink;

    impl Write for FailingSink {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("the shepherd went away"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs on its own thread with a deadline. A real deadlock would hang
    /// the whole suite otherwise.
    ///
    /// The `late` line is written only after the reload handler says it
    /// registered, so the reader cannot have resolved that action as an
    /// unknown one. What the two replies then have to carry is their own
    /// id, which is the only thing matching them once handlers overlap.
    #[test]
    fn a_handler_that_registers_another_handler_is_matched_by_id() {
        let shepherd = Shepherd::inert(None);
        let inner_shepherd = shepherd.clone();
        let (registered, registered_rx) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let waiting = Arc::new(std::sync::Mutex::new(waiting));
        shepherd.on_action("reload", move |_, _| {
            inner_shepherd.on_action("late", |_, _| "late ok".to_string());
            registered.send(()).expect("registered");
            waiting
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv()
                .expect("the test never released the reload handler");
            "reloaded".to_string()
        });

        let outbox = Arc::new(Outbox::new(4));
        let dispatch = Arc::clone(&shepherd.0.dispatch);
        let (stream, mut shepherd_end) = pipe().expect("pipe");
        let (done, done_rx) = mpsc::channel();
        let reading = Arc::clone(&outbox);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            reader_loop(&mut reader, &reading, &dispatch, &|_: &str| {});
            let _ = done.send(());
        });

        shepherd_end
            .write_all(b"{\"kind\":\"action\",\"name\":\"reload\",\"id\":1}\n")
            .expect("write the reload");
        registered_rx
            .recv_timeout(DEADLINE)
            .expect("the reload handler never registered the late one");
        shepherd_end
            .write_all(b"{\"kind\":\"action\",\"name\":\"late\",\"id\":2}\n")
            .expect("write the late action");
        drop(shepherd_end);
        release.send(()).expect("release the reload handler");

        done_rx
            .recv_timeout(DEADLINE)
            .expect("reader_loop deadlocked: a handler that registers a handler hung the reader");

        let mut replies: Vec<(Option<u64>, String, String)> = outbox
            .take()
            .into_iter()
            .map(|message| match message {
                ChildMessage::ActionReply { action, body, id } => (id, action, body),
                other => panic!("expected an action reply, got {other:?}"),
            })
            .collect();
        replies.sort_by_key(|(id, ..)| *id);
        assert_eq!(
            replies,
            vec![
                (Some(1), "reload".to_string(), "reloaded".to_string()),
                (Some(2), "late".to_string(), "late ok".to_string()),
            ],
            "a handler registered mid-flight was not answered, or a reply lost the id it answers"
        );
    }
}
