//! The two threads behind a live channel: the writer drains the outbox
//! onto the transport, and the reader answers what the shepherd sends.
//!
//! Free functions over `BufRead` and `Write`, so a test drives either one
//! over a buffer with no thread or socket.

use std::io::{BufRead, Write};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;

use crate::dispatch::{Dispatch, Outcome, Resolved, run};
use crate::outbox::Outbox;
use crate::{ChannelError, ShepherdMessage, session};

/// What to tell an author whose app was asked to stop and registered nothing.
const UNHANDLED_SHUTDOWN_ADVICE: &str = "the shepherd sent shutdown and no on_shutdown handler is registered. This \
     process will be killed when kill_timeout expires. Register one to stop gracefully.";

/// How many action handlers the reader may have running at once.
///
/// A starting guess, like the outbox's capacity: wide enough that a flock
/// of triggers landing on one app is not serialized, narrow enough that an
/// app whose handlers all block costs it 32 threads rather than one per
/// message. Past the cap the reader answers actions itself rather than
/// dropping them.
const MAX_CONCURRENT_HANDLERS: usize = 32;

/// The name every handler thread carries, which is what a stack dump or a
/// debugger shows for a stuck one.
const HANDLER_THREAD: &str = "shep-channel-handler";

/// Drives the writer side: drains `outbox` and writes what it took, a
/// burst at a time. Stops once the transport fails or the outbox closes.
pub(crate) fn writer_loop<W: Write>(writer: &mut W, outbox: &Outbox) {
    let mut batch = Vec::new();
    let mut line = Vec::new();
    while outbox.take_batch(&mut batch) {
        if session::write_messages(writer, &batch, &mut line).is_err() {
            break;
        }
        outbox.wrote(batch.len());
    }
    // `stop`, not `close`: a failed write leaves its message unwritten,
    // and a `flush` waiting on it has to hear that rather than time out.
    outbox.stop();
}

// Running handlers are joined before a shutdown is looked up, so no reply
// is lost with the reader. `warn` is injected so a test can assert what
// was said and how often. That pins the once-per-loop malformed-frame
// latch.
/// Drives the reader side: reads one message, resolves it against
/// `dispatch`, and runs the result. The registry's lock is dropped first.
///
/// An action's handler runs on a thread of its own, so a slow one does not
/// hold up the messages behind it. A shutdown is handled on the reader. So
/// is an action when [`MAX_CONCURRENT_HANDLERS`] are running or a spawn
/// fails.
pub(crate) fn reader_loop<R: BufRead>(
    reader: &mut R,
    outbox: &Arc<Outbox>,
    dispatch: &RwLock<Dispatch>,
    warn: &dyn Fn(&str),
) {
    let mut warned_malformed = false;
    let mut line = Vec::new();
    let mut handlers: Vec<JoinHandle<()>> = Vec::new();
    loop {
        match session::read_message(reader, &mut line) {
            Ok(Some(message)) => {
                handlers.retain(|handler| !handler.is_finished());
                if matches!(message, ShepherdMessage::Shutdown) {
                    // An `on_shutdown` handler that flushes and exits must not
                    // race a reply still being computed. Looking the shutdown
                    // up afterwards also keeps the loop's order for an action
                    // that registers `on_shutdown` itself.
                    join_handlers(&mut handlers, warn);
                }
                let resolved = dispatch
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .resolve(message);
                if runs_off_reader(&resolved, handlers.len())
                    && let Some(handle) = spawn_handler(&resolved, outbox)
                {
                    handlers.push(handle);
                    continue;
                }
                if !answer(resolved, outbox, warn) {
                    break;
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
    join_handlers(&mut handlers, warn);
    outbox.close();
}

/// Waits for every handler thread still running, and clears `handlers`.
fn join_handlers(handlers: &mut Vec<JoinHandle<()>>, warn: &dyn Fn(&str)) {
    for handler in handlers.drain(..) {
        if handler.join().is_err() {
            warn("a channel action handler thread panicked, and its reply was lost");
        }
    }
}

/// Whether `resolved` can be given a thread of its own. A shutdown is not:
/// it is the reader's own cue to stop reading, and an unknown action has
/// no handler to wait on.
fn runs_off_reader(resolved: &Resolved, running: usize) -> bool {
    matches!(resolved, Resolved::Action { .. }) && running < MAX_CONCURRENT_HANDLERS
}

/// Runs a copy of `resolved` on a thread of its own, pushing its reply
/// through `outbox`. `None` when the spawn failed, leaving the caller to
/// run it itself.
fn spawn_handler(resolved: &Resolved, outbox: &Arc<Outbox>) -> Option<JoinHandle<()>> {
    let writing = Arc::clone(outbox);
    let running = resolved.clone();
    std::thread::Builder::new()
        .name(HANDLER_THREAD.to_string())
        .spawn(move || {
            if let Outcome::Reply(reply) = run(running) {
                // The shepherd is gone, so this reply has nowhere to go.
                let _ = writing.push_blocking(reply);
            }
        })
        .ok()
}

/// Runs `resolved` on the calling thread and queues what it produced.
/// `false` when the outbox refused the reply, which is the reader's own
/// signal to stop.
fn answer(resolved: Resolved, outbox: &Arc<Outbox>, warn: &dyn Fn(&str)) -> bool {
    match run(resolved) {
        Outcome::Reply(reply) => outbox.push_blocking(reply).is_ok(),
        Outcome::Handled => true,
        Outcome::UnhandledShutdown => {
            warn(UNHANDLED_SHUTDOWN_ADVICE);
            true
        }
        Outcome::ShutdownFailed(message) => {
            warn(&format!("shutdown handler panicked: {message}"));
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::{ActionHandler, ChildMessage};

    /// Bounds every wait in this module's tests. A working reader answers
    /// in microseconds; this is slack for a loaded runner.
    const DEADLINE: Duration = Duration::from_secs(5);

    /// How long a wait that must never be satisfied gets before the test
    /// calls it a failure.
    const NOT_YET: Duration = Duration::from_millis(200);

    /// The action replies in `messages`, sorted by the id each carries.
    /// Order is what the outbox decides, so a test asserts ids, not
    /// positions.
    fn replies_sorted(messages: Vec<ChildMessage>) -> Vec<(Option<u64>, String, String)> {
        let mut replies: Vec<(Option<u64>, String, String)> = messages
            .into_iter()
            .map(|message| match message {
                ChildMessage::ActionReply { action, body, id } => (id, action, body),
                other => panic!("expected an action reply, got {other:?}"),
            })
            .collect();
        replies.sort_by_key(|(id, ..)| *id);
        replies
    }

    /// A transport that hands every batch it is given to the test, one
    /// line-set per write. What the test waits on is bytes the writer
    /// actually reached, not a count it guessed at.
    #[derive(Debug)]
    struct ReportingSink {
        wire: mpsc::Sender<String>,
    }

    impl Write for ReportingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let text = String::from_utf8(buf.to_vec()).expect("a utf8 frame");
            self.wire.send(text).expect("the test is gone");
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Registers `handler` under `name` in `dispatch`.
    #[track_caller]
    fn register(
        dispatch: &RwLock<Dispatch>,
        name: &str,
        handler: impl Fn(Option<&str>, &str) -> String + Send + Sync + 'static,
    ) {
        dispatch
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register_action(name.to_string(), Box::new(handler));
    }

    /// Runs `reader_loop` over `lines` on its own thread, and reports when
    /// it returns.
    fn spawn_reader(
        lines: Vec<u8>,
        outbox: &Arc<Outbox>,
        dispatch: &Arc<RwLock<Dispatch>>,
    ) -> mpsc::Receiver<()> {
        let (done, done_rx) = mpsc::channel();
        let writing = Arc::clone(outbox);
        let registry = Arc::clone(dispatch);
        std::thread::spawn(move || {
            let mut reader = Cursor::new(lines);
            reader_loop(&mut reader, &writing, &registry, &|_: &str| {});
            let _ = done.send(());
        });
        done_rx
    }

    /// Starts a writer over `ReportingSink`, handing the test every write.
    fn spawn_writer(outbox: &Arc<Outbox>) -> (mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
        let (wire, wire_rx) = mpsc::channel();
        let mut sink = ReportingSink { wire };
        let writing = Arc::clone(outbox);
        let handle = std::thread::spawn(move || writer_loop(&mut sink, &writing));
        (wire_rx, handle)
    }

    /// The next thing the writer reached, or a failure naming what it had
    /// not reached.
    #[track_caller]
    fn next_write(wire: &mpsc::Receiver<String>, what: &str) -> String {
        wire.recv_timeout(DEADLINE).expect(what)
    }

    /// A handler that reports it has started, then waits on the returned
    /// sender before it answers. `body` is what it answers with.
    fn held_handler(
        started: mpsc::Sender<()>,
        body: &'static str,
    ) -> (ActionHandler, mpsc::Sender<()>) {
        let (release, waiting) = mpsc::channel();
        let waiting = Arc::new(std::sync::Mutex::new(waiting));
        let handler: ActionHandler = Box::new(move |_, _| {
            started.send(()).expect("started");
            waiting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv()
                .expect("the test never released the handler");
            body.to_string()
        });
        (handler, release)
    }

    /// The action replies in `lines`, decoded from the wire bytes the
    /// writer reached.
    #[track_caller]
    fn replies_on_the_wire(lines: &[String]) -> Vec<(Option<u64>, String, String)> {
        let mut replies = Vec::new();
        for line in lines {
            for frame in line.lines() {
                let decoded: ChildMessage = serde_json::from_str(frame).expect("a wire frame");
                match decoded {
                    ChildMessage::ActionReply { action, body, id } => {
                        replies.push((id, action, body))
                    }
                    other => panic!("expected an action reply on the wire, got {other:?}"),
                }
            }
        }
        replies.sort_by_key(|(id, ..)| *id);
        replies
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
        let outbox = Arc::new(Outbox::new(4));
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
    /// thread in `take_batch` forever.
    #[test]
    fn end_of_stream_breaks_the_loop_and_closes_the_outbox() {
        let mut reader = Cursor::new(Vec::new());
        let outbox = Arc::new(Outbox::new(4));
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
            outbox.take(),
            vec![],
            "outbox should read as closed-and-empty after EOF, not park a waiter"
        );
    }

    /// fails if a reply never reaches the outbox, or drops its id. The
    /// daemon needs that id to match a reply to its trigger.
    #[test]
    fn an_actions_reply_reaches_the_outbox_carrying_its_id() {
        let mut reader = Cursor::new(b"{\"kind\":\"action\",\"name\":\"gc\",\"id\":7}\n".to_vec());
        let outbox = Arc::new(Outbox::new(4));
        let dispatch = RwLock::new(Dispatch::default());
        dispatch
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register_action("gc".to_string(), Box::new(|_, _| "ok".to_string()));
        let warn = |_: &str| {};

        reader_loop(&mut reader, &outbox, &dispatch, &warn);

        match outbox.take().as_slice() {
            [ChildMessage::ActionReply { action, body, id }] => {
                assert_eq!(action, "gc");
                assert_eq!(body, "ok");
                assert_eq!(*id, Some(7));
            }
            other => panic!("expected an action reply, got {other:?}"),
        }
    }

    /// The whole point of running a handler off the reader: an action held
    /// by the shepherd's lock used to park every action queued behind it.
    #[test]
    fn a_fast_action_is_answered_while_a_slow_one_is_still_running() {
        let outbox = Arc::new(Outbox::new(8));
        let dispatch = Arc::new(RwLock::new(Dispatch::default()));

        let (started, started_rx) = mpsc::channel();
        let (slow, release) = held_handler(started, "slow ok");
        register(&dispatch, "slow", slow);
        register(&dispatch, "fast", |_, _| "fast ok".to_string());

        let (wire, writer) = spawn_writer(&outbox);
        let done = spawn_reader(
            b"{\"kind\":\"action\",\"name\":\"slow\",\"id\":1}\n\
              {\"kind\":\"action\",\"name\":\"fast\",\"id\":2}\n"
                .to_vec(),
            &outbox,
            &dispatch,
        );

        started_rx
            .recv_timeout(DEADLINE)
            .expect("the slow handler never started");
        let line = next_write(
            &wire,
            "nothing reached the wire while the slow handler was held",
        );
        assert_eq!(
            replies_on_the_wire(&[line]),
            vec![(Some(2), "fast".to_string(), "fast ok".to_string())],
            "the fast action was not answered while a slower one was still running"
        );

        release.send(()).expect("release the slow handler");
        done.recv_timeout(DEADLINE)
            .expect("reader_loop never returned");
        writer.join().expect("writer panicked");
    }

    /// The case the id exists for: two replies that reach the outbox in
    /// the opposite order to the actions that produced them.
    #[test]
    fn two_actions_reply_with_their_own_ids_whatever_order_they_finish_in() {
        let outbox = Arc::new(Outbox::new(8));
        let dispatch = Arc::new(RwLock::new(Dispatch::default()));

        let (started, started_rx) = mpsc::channel();
        let (slow, release) = held_handler(started, "slow ok");
        register(&dispatch, "slow", slow);
        register(&dispatch, "quick", |_, _| "quick ok".to_string());

        let (wire, writer) = spawn_writer(&outbox);
        let done = spawn_reader(
            b"{\"kind\":\"action\",\"name\":\"slow\",\"id\":1}\n\
              {\"kind\":\"action\",\"name\":\"quick\",\"id\":2}\n"
                .to_vec(),
            &outbox,
            &dispatch,
        );

        started_rx
            .recv_timeout(DEADLINE)
            .expect("the slow handler never started");
        assert_eq!(
            replies_on_the_wire(&[next_write(&wire, "the quick action was never answered")]),
            vec![(Some(2), "quick".to_string(), "quick ok".to_string())],
            "the action read second did not answer first"
        );

        release.send(()).expect("release the slow handler");
        assert_eq!(
            replies_on_the_wire(&[next_write(
                &wire,
                "the held action never answered after its release",
            )]),
            vec![(Some(1), "slow".to_string(), "slow ok".to_string())],
            "a reply reached the wire carrying the wrong id"
        );

        done.recv_timeout(DEADLINE)
            .expect("reader_loop never returned");
        writer.join().expect("writer panicked");
    }

    /// The reader's end-of-stream join is what stops a reply still being
    /// computed from dying with the loop.
    #[test]
    fn end_of_stream_waits_for_a_running_handler_and_its_reply_is_written() {
        let outbox = Arc::new(Outbox::new(4));
        let dispatch = Arc::new(RwLock::new(Dispatch::default()));
        let (started, started_rx) = mpsc::channel();
        let (slow, release) = held_handler(started, "slow ok");
        register(&dispatch, "slow", slow);

        let done = spawn_reader(
            b"{\"kind\":\"action\",\"name\":\"slow\",\"id\":9}\n".to_vec(),
            &outbox,
            &dispatch,
        );

        started_rx
            .recv_timeout(DEADLINE)
            .expect("the handler never started");
        assert!(
            done.recv_timeout(NOT_YET).is_err(),
            "the reader closed the outbox while a handler was still running"
        );

        release.send(()).expect("release the handler");
        done.recv_timeout(DEADLINE)
            .expect("reader_loop never returned");

        assert_eq!(
            replies_sorted(outbox.take()),
            vec![(Some(9), "slow".to_string(), "slow ok".to_string())],
            "a reply a handler was still computing did not reach the outbox"
        );
    }

    /// A handler that unwinds is caught on its own thread too, and the
    /// action behind it is still answered.
    #[test]
    fn a_panicking_handler_replies_with_the_error_and_does_not_end_the_reader() {
        let outbox = Arc::new(Outbox::new(4));
        let dispatch = RwLock::new(Dispatch::default());
        register(&dispatch, "boom", |_, _| panic!("no such state"));
        register(&dispatch, "next", |_, _| "next ok".to_string());

        let mut reader = Cursor::new(
            b"{\"kind\":\"action\",\"name\":\"boom\",\"id\":1}\n\
              {\"kind\":\"action\",\"name\":\"next\",\"id\":2}\n"
                .to_vec(),
        );
        reader_loop(&mut reader, &outbox, &dispatch, &|_: &str| {});

        assert_eq!(
            replies_sorted(outbox.take()),
            vec![
                (
                    Some(1),
                    "boom".to_string(),
                    "action handler failed: no such state".to_string(),
                ),
                (Some(2), "next".to_string(), "next ok".to_string()),
            ],
            "a panicking handler stopped the reader answering the action behind it"
        );
    }

    /// Past the cap the reader answers actions itself: the serialized
    /// behavior, not a dropped action.
    #[test]
    fn at_the_cap_a_handler_runs_inline_on_the_reader() {
        let outbox = Arc::new(Outbox::new(MAX_CONCURRENT_HANDLERS + 4));
        let dispatch = Arc::new(RwLock::new(Dispatch::default()));

        let (started, started_rx) = mpsc::channel();
        let mut releases = Vec::new();
        for index in 0..MAX_CONCURRENT_HANDLERS {
            let (held, release) = held_handler(started.clone(), "held ok");
            register(&dispatch, &format!("held{index}"), held);
            releases.push(release);
        }

        let (last_started, last_rx) = mpsc::channel();
        let (last, last_release) = held_handler(last_started, "last ok");
        register(&dispatch, "last", last);

        let (shutdown_ran, shutdown_rx) = mpsc::channel();
        dispatch
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register_shutdown(Box::new(move || {
                shutdown_ran.send(()).expect("shutdown handler ran");
            }));

        let last_id = MAX_CONCURRENT_HANDLERS + 1;
        let mut lines = String::new();
        for index in 0..MAX_CONCURRENT_HANDLERS {
            lines.push_str(&format!(
                "{{\"kind\":\"action\",\"name\":\"held{index}\",\"id\":{}}}\n",
                index + 1
            ));
        }
        lines.push_str(&format!(
            "{{\"kind\":\"action\",\"name\":\"last\",\"id\":{last_id}}}\n{{\"kind\":\"shutdown\"}}\n"
        ));

        let done = spawn_reader(lines.into_bytes(), &outbox, &dispatch);

        for _ in 0..MAX_CONCURRENT_HANDLERS {
            started_rx
                .recv_timeout(DEADLINE)
                .expect("a handler the reader should have spawned never started");
        }
        last_rx
            .recv_timeout(DEADLINE)
            .expect("the action past the cap was never answered");
        assert!(
            shutdown_rx.recv_timeout(NOT_YET).is_err(),
            "shutdown was handled while the reader was still inside the action past the cap"
        );

        for release in releases {
            release.send(()).expect("release a held handler");
        }
        last_release.send(()).expect("release the last handler");
        done.recv_timeout(DEADLINE)
            .expect("reader_loop never returned");
        shutdown_rx
            .recv_timeout(DEADLINE)
            .expect("the shutdown handler never ran");

        let replies = replies_sorted(outbox.take());
        assert_eq!(
            replies.last(),
            Some(&(
                Some(last_id as u64),
                "last".to_string(),
                "last ok".to_string()
            )),
            "the action past the cap lost its reply"
        );
        assert_eq!(
            replies.len(),
            MAX_CONCURRENT_HANDLERS + 1,
            "an action at the cap was dropped rather than answered on the reader"
        );
    }

    /// Shutdown is the reader's own cue, so it cannot overtake an action
    /// the reader is still waiting on.
    #[test]
    fn a_shutdown_is_handled_on_the_reader_after_the_earlier_actions_replied_or_were_joined() {
        let outbox = Arc::new(Outbox::new(4));
        let dispatch = Arc::new(RwLock::new(Dispatch::default()));
        let (started, started_rx) = mpsc::channel();
        let (slow, release) = held_handler(started, "slow ok");
        register(&dispatch, "slow", slow);

        let (shutdown_ran, shutdown_rx) = mpsc::channel();
        dispatch
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register_shutdown(Box::new(move || {
                shutdown_ran.send(()).expect("shutdown handler ran");
            }));

        let done = spawn_reader(
            b"{\"kind\":\"action\",\"name\":\"slow\",\"id\":1}\n{\"kind\":\"shutdown\"}\n".to_vec(),
            &outbox,
            &dispatch,
        );

        started_rx
            .recv_timeout(DEADLINE)
            .expect("the action handler never started");
        assert!(
            shutdown_rx.recv_timeout(NOT_YET).is_err(),
            "shutdown was handled while an action sent before it was still running"
        );

        release.send(()).expect("release the action handler");
        done.recv_timeout(DEADLINE)
            .expect("reader_loop never returned");
        shutdown_rx
            .recv_timeout(DEADLINE)
            .expect("the shutdown handler never ran");

        assert_eq!(
            replies_sorted(outbox.take()),
            vec![(Some(1), "slow".to_string(), "slow ok".to_string())],
            "the action sent before the shutdown lost its reply"
        );
    }

    /// The other half of the barrier the serial loop gave: joining an
    /// earlier action is worth nothing unless the shutdown is looked up
    /// afterwards. An action handler that registers `on_shutdown` has to
    /// have run by then, or the reader answers the shutdown with the
    /// registry as it was before the app armed it.
    #[test]
    fn a_shutdown_is_resolved_after_a_handler_that_registers_the_shutdown_handler() {
        let outbox = Arc::new(Outbox::new(4));
        let dispatch = Arc::new(RwLock::new(Dispatch::default()));

        let (started, started_rx) = mpsc::channel();
        let (shutdown_ran, shutdown_rx) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let waiting = Arc::new(std::sync::Mutex::new(waiting));
        let arming = Arc::clone(&dispatch);
        let armed = shutdown_ran;
        let handler: ActionHandler = Box::new(move |_, _| {
            started.send(()).expect("started");
            waiting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv()
                .expect("the test never released the handler");
            let armed = armed.clone();
            arming
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .register_shutdown(Box::new(move || {
                    armed.send(()).expect("the registered shutdown handler ran");
                }));
            "armed".to_string()
        });
        dispatch
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register_action("arm".to_string(), handler);

        let warnings: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let (done, done_rx) = mpsc::channel();
        let reading = Arc::clone(&outbox);
        let registry = Arc::clone(&dispatch);
        let noted = Arc::clone(&warnings);
        std::thread::spawn(move || {
            let mut reader = Cursor::new(
                b"{\"kind\":\"action\",\"name\":\"arm\",\"id\":1}\n{\"kind\":\"shutdown\"}\n"
                    .to_vec(),
            );
            reader_loop(&mut reader, &reading, &registry, &|message: &str| {
                noted
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(message.to_string());
            });
            let _ = done.send(());
        });

        started_rx
            .recv_timeout(DEADLINE)
            .expect("the action handler never started");
        assert!(
            done_rx.recv_timeout(NOT_YET).is_err(),
            "the reader finished while the action that registers on_shutdown was still held"
        );

        release.send(()).expect("release the action handler");
        done_rx
            .recv_timeout(DEADLINE)
            .expect("reader_loop never returned");
        shutdown_rx
            .recv_timeout(DEADLINE)
            .expect("the shutdown handler registered by the held action never ran");

        let collected = warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let unhandled: Vec<&String> = collected
            .iter()
            .filter(|warning| warning.contains("no on_shutdown handler"))
            .collect();
        assert!(
            unhandled.is_empty(),
            "the reader answered the shutdown off a registry it resolved before the action armed it: {collected:?}"
        );
        assert_eq!(
            replies_sorted(outbox.take()),
            vec![(Some(1), "arm".to_string(), "armed".to_string())],
            "the action that registered the shutdown handler lost its reply"
        );
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
                metric_type: None,
                labels: std::collections::BTreeMap::new(),
                help: None,
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

    /// A burst already queued costs the transport one write, however many
    /// messages it holds, and every one of them still reaches the wire.
    #[test]
    fn the_writer_writes_a_queued_burst_in_one_write() {
        struct CountingSink {
            wire: Vec<u8>,
            writes: usize,
        }

        impl Write for CountingSink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.writes += 1;
                self.wire.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let outbox = Outbox::new(8);
        for value in [1.0, 2.0, 3.0] {
            outbox.push_lossy(ChildMessage::Metric {
                name: "rps".to_string(),
                value,
                metric_type: None,
                labels: std::collections::BTreeMap::new(),
                help: None,
            });
        }
        outbox.close();

        let mut sink = CountingSink {
            wire: Vec::new(),
            writes: 0,
        };
        writer_loop(&mut sink, &outbox);

        assert_eq!(sink.writes, 1, "the burst was written message by message");
        assert_eq!(
            String::from_utf8(sink.wire)
                .expect("valid utf8")
                .lines()
                .count(),
            3
        );
        assert_eq!(outbox.pending(), 0, "a written burst is still pending");
    }
}
