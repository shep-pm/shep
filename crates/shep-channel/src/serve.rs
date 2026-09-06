use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use crate::dispatch::{Dispatch, Outcome, run};
use crate::outbox::{DEFAULT_CAPACITY, Outbox};
use crate::{CHANNEL_VERSION, Channel, ChannelError, ChildMessage, VERSION_VAR, session};

/// What to tell an author running under shep with no channel.
const NO_CHANNEL_ADVICE: &str = "no channel on this process. Set `channel = true` \
     (or `wait_ready` / `shutdown_with_message`) on this app in the Flockfile to open one.";

/// What to tell an author whose app was asked to stop and registered nothing.
const UNHANDLED_SHUTDOWN_ADVICE: &str = "the shepherd sent shutdown and no on_shutdown handler is registered. This \
     process will be killed when kill_timeout expires. Register one to stop gracefully.";

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
    /// False before the operator opens one, and false again once the
    /// shepherd goes away. [`Shepherd::dropped_metrics`] never freezes
    /// silently as a result.
    ///
    /// Checking this is optional: every method already does nothing without
    /// a channel.
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

    /// How many metrics have been dropped because the shepherd was not
    /// keeping up. Always 0 without a channel.
    #[must_use]
    pub fn dropped_metrics(&self) -> u64 {
        self.0.outbox.as_ref().map_or(0, |outbox| outbox.dropped())
    }

    /// Registers a handler for one action name, replacing any prior one.
    ///
    /// Called with the action's params (`None` if triggered with none) and
    /// its name, in that order. The return value becomes the reply body.
    ///
    /// Safe to call from another thread, or from inside a handler.
    /// A `reload` action can swap its own handlers this way. The
    /// registry's lock is never held while a handler runs.
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
    /// Without one, a shutdown message warns and nothing else happens.
    /// This crate never ends a process on its own judgement.
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

    /// Records one metric sample. Never blocks and never fails.
    ///
    /// A sample may be dropped if the shepherd stops reading; see
    /// [`Shepherd::dropped_metrics`]. That trade avoids parking a hot path
    /// on a full socket.
    ///
    /// Takes `impl Into<String>`, unlike `on_action`'s `impl AsRef<str>`.
    /// This runs per sample on a hot path. An owned `String` the caller
    /// already has moves in for free.
    pub fn metric(&self, name: impl Into<String>, value: f64) {
        if let Some(outbox) = &self.0.outbox {
            outbox.push_lossy(ChildMessage::Metric {
                name: name.into(),
                value,
            });
        }
    }
}

/// Opens this process's channel and starts serving it.
///
/// Always returns a usable handle. With no channel, every call on it
/// is a no-op. So an app needs no branch at its emit sites. One
/// line goes to stderr when `SHEP_NAME` says this process runs under shep.
///
/// A process singleton: the channel is one descriptor and can be owned
/// once. A second call returns the same handle.
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

/// Drives the writer side: drains `outbox` and writes each message.
/// Stops once the transport fails or the outbox closes.
///
/// A free function over `BufRead`/`Write`, not an inline closure. A test
/// can drive it over a `Vec<u8>`, with no thread or socket.
pub(crate) fn writer_loop<W: Write>(writer: &mut W, outbox: &Outbox) {
    while let Some(message) = outbox.pop() {
        if session::write_message(writer, &message).is_err() {
            break;
        }
    }
    outbox.close();
}

/// Drives the reader side: reads one message, resolves it against
/// `dispatch`, and runs the result. The registry's lock is dropped before
/// that run, per `dispatch`'s own module doc.
///
/// `warn` is injected so a test can assert what was said and how
/// often. This pins the once-per-loop malformed-frame latch.
pub(crate) fn reader_loop<R: BufRead>(
    reader: &mut R,
    outbox: &Outbox,
    dispatch: &RwLock<Dispatch>,
    warn: &dyn Fn(&str),
) {
    let mut warned_malformed = false;
    loop {
        match session::read_message(reader) {
            Ok(Some(message)) => {
                let resolved = dispatch
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .resolve(message);
                let outcome = run(resolved);
                match outcome {
                    Outcome::Reply(reply) => {
                        if outbox.push_blocking(reply).is_err() {
                            break;
                        }
                    }
                    Outcome::Handled => {}
                    Outcome::UnhandledShutdown => warn(UNHANDLED_SHUTDOWN_ADVICE),
                    Outcome::ShutdownFailed(message) => {
                        warn(&format!("shutdown handler panicked: {message}"));
                    }
                }
            }
            Err(ChannelError::Malformed(message)) => {
                if !warned_malformed {
                    warned_malformed = true;
                    warn(&format!("malformed frame from the shepherd: {message}"));
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    outbox.close();
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
        // Without this thread, nothing drains the outbox.
        // `ready()` would queue silently and `wait_ready` would hang.
        // Close the outbox first so `ready()`/`metric()` fail honestly.
        // The handle still carries the version stamp, since the channel opened.
        warn(&format!(
            "failed to spawn the shep-channel writer thread: {error}; continuing without a channel"
        ));
        outbox.close();
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
    use std::io::Cursor;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Bounds the one test below that could hang forever on a real
    /// regression. A working reader answers in microseconds. This is slack
    /// for a loaded runner, not an expected duration.
    const DEADLINE: Duration = Duration::from_secs(5);

    #[test]
    fn an_inert_handle_accepts_everything_and_does_nothing() {
        let shepherd = Shepherd::inert(None);
        assert!(!shepherd.is_active());
        shepherd.on_action("gc", |_, _| "ok".to_string());
        shepherd.on_shutdown(|| {});
        shepherd.metric("rps", 42.0);
        shepherd.ready().expect("an inert ready is not an error");
        assert_eq!(shepherd.dropped_metrics(), 0);
        assert_eq!(shepherd.version(), None);
    }

    #[test]
    fn a_handle_stops_being_active_once_the_channel_closes() {
        let outbox = Arc::new(Outbox::new(4));
        let shepherd = Shepherd(Arc::new(Inner {
            outbox: Some(Arc::clone(&outbox)),
            dispatch: Arc::new(RwLock::new(Dispatch::default())),
            version: None,
        }));
        assert!(shepherd.is_active(), "a fresh channel should read as live");

        outbox.close();

        assert!(
            !shepherd.is_active(),
            "a handle whose shepherd went away still reads as live"
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

    /// This warning is the only thing standing between a missing handler
    /// and a `kill_timeout` kill.
    #[test]
    fn the_unhandled_shutdown_warning_names_the_method_to_call() {
        assert!(UNHANDLED_SHUTDOWN_ADVICE.contains("on_shutdown"));
        assert!(UNHANDLED_SHUTDOWN_ADVICE.contains("kill_timeout"));
    }

    #[test]
    fn two_malformed_lines_warn_once_and_the_loop_keeps_going() {
        let mut reader =
            Cursor::new(b"not json\nalso not json\n{\"kind\":\"shutdown\"}\n".to_vec());
        let outbox = Outbox::new(4);
        let dispatch = RwLock::new(Dispatch::default());
        let shutdown_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&shutdown_hits);
        dispatch
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register_shutdown(Box::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));

        let warnings: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let warn = |message: &str| warnings.lock().unwrap().push(message.to_string());

        reader_loop(&mut reader, &outbox, &dispatch, &warn);

        let collected = warnings.into_inner().unwrap();
        assert_eq!(
            collected.len(),
            1,
            "expected exactly one warning for two malformed lines, got {collected:?}"
        );
        assert_eq!(
            shutdown_hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the well-formed shutdown after the two bad lines was never reached"
        );
    }

    /// fails if end of stream leaves the outbox open, parking the writer
    /// thread in `pop()` forever.
    #[test]
    fn end_of_stream_breaks_the_loop_and_closes_the_outbox() {
        let mut reader = Cursor::new(Vec::new());
        let outbox = Outbox::new(4);
        let dispatch = RwLock::new(Dispatch::default());
        let warn = |_: &str| {};

        reader_loop(&mut reader, &outbox, &dispatch, &warn);

        // Non-blocking probe: `push_blocking` on a closed outbox returns
        // `Err(Closed)` at once rather than waiting.
        // A dropped `close()` in `reader_loop` fails here in microseconds.
        // Otherwise this test hangs on an open, unbounded queue.
        assert!(
            outbox.push_blocking(ChildMessage::Ready).is_err(),
            "reader_loop returned without closing the outbox"
        );
        assert_eq!(
            outbox.pop(),
            None,
            "outbox should read as closed-and-empty after EOF, not park a waiter"
        );
    }

    /// fails if a reply never reaches the outbox, or drops its id. The
    /// daemon needs that id to match a reply to its trigger.
    #[test]
    fn an_actions_reply_reaches_the_outbox_carrying_its_id() {
        let mut reader = Cursor::new(b"{\"kind\":\"action\",\"name\":\"gc\",\"id\":7}\n".to_vec());
        let outbox = Outbox::new(4);
        let dispatch = RwLock::new(Dispatch::default());
        dispatch
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register_action("gc".to_string(), Box::new(|_, _| "ok".to_string()));
        let warn = |_: &str| {};

        reader_loop(&mut reader, &outbox, &dispatch, &warn);

        match outbox.pop() {
            Some(ChildMessage::ActionReply { action, body, id }) => {
                assert_eq!(action, "gc");
                assert_eq!(body, "ok");
                assert_eq!(id, Some(7));
            }
            other => panic!("expected an action reply, got {other:?}"),
        }
    }

    /// A shutdown reply queued right before the shepherd leaves must still
    /// reach the wire.
    #[test]
    fn the_writer_drains_what_is_already_queued_after_close() {
        let outbox = Outbox::new(4);
        outbox
            .push_blocking(ChildMessage::Ready)
            .expect("room for the first message");
        outbox
            .push_blocking(ChildMessage::Metric {
                name: "rps".to_string(),
                value: 1.0,
            })
            .expect("room for the second message");
        outbox.close();

        let mut written = Vec::new();
        writer_loop(&mut written, &outbox);

        let text = String::from_utf8(written).expect("valid utf8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "expected both already-queued messages to be written, got {lines:?}"
        );
        assert!(lines[0].contains("\"kind\":\"ready\""));
        assert!(lines[1].contains("\"kind\":\"metric\""));
    }

    /// Runs on its own thread with a deadline. A real deadlock would hang
    /// the whole suite otherwise.
    #[test]
    fn a_handler_that_registers_another_handler_does_not_deadlock_the_reader() {
        let shepherd = Shepherd::inert(None);
        let inner_shepherd = shepherd.clone();
        shepherd.on_action("reload", move |_, _| {
            inner_shepherd.on_action("late", |_, _| "late ok".to_string());
            "reloaded".to_string()
        });

        let outbox = Arc::new(Outbox::new(4));
        let dispatch = Arc::clone(&shepherd.0.dispatch);
        let outbox_thread = Arc::clone(&outbox);

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = Cursor::new(
                b"{\"kind\":\"action\",\"name\":\"reload\",\"id\":1}\n\
                  {\"kind\":\"action\",\"name\":\"late\",\"id\":2}\n"
                    .to_vec(),
            );
            let warn = |_: &str| {};
            reader_loop(&mut reader, &outbox_thread, &dispatch, &warn);
            let _ = tx.send(());
        });

        rx.recv_timeout(DEADLINE)
            .expect("reader_loop deadlocked: a handler that registers a handler hung the reader");

        let first = outbox.pop().expect("the reload reply");
        let second = outbox
            .pop()
            .expect("the late reply, registered inside the reload handler");
        match (first, second) {
            (
                ChildMessage::ActionReply {
                    action: action1,
                    body: body1,
                    id: id1,
                },
                ChildMessage::ActionReply {
                    action: action2,
                    body: body2,
                    id: id2,
                },
            ) => {
                assert_eq!(action1, "reload");
                assert_eq!(body1, "reloaded");
                assert_eq!(id1, Some(1));
                assert_eq!(action2, "late");
                assert_eq!(body2, "late ok");
                assert_eq!(id2, Some(2));
            }
            other => panic!("expected two action replies, got {other:?}"),
        }
    }
}
