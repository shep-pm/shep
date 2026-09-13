//! The connected [`Client`] handle: [`Client::connect`], [`Client::request`],
//! [`RequestError`].
//!
//! A `Client` is a thin, actor-backed handle: the socket itself is owned by
//! the task [`crate::actor::spawn`] starts, and every method here sends a
//! command to that task and awaits the answer. `&self` is enough for every
//! method, so concurrent callers share one `Client` (behind an `Arc`, or
//! just a shared reference) instead of cloning a handle per caller.

use core::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use shep_core::protocol::{HelloAck, Request, Response, RpcError, WireError};

use crate::actor::{self, Command};
use crate::connection::{ConnectError, Connection, HANDSHAKE_TIMEOUT};
use crate::events::EventStream;

/// Daemon-side budget applied when a caller names none. Mirrors the daemon's
/// own `DEFAULT_DEADLINE_MS = 5_000` (`shep-daemon/src/rpc.rs`).
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);

/// Budget for `Request::Start`. A cold spawn plus a readiness probe routinely
/// outruns the 5s default, and the daemon clamps anything over
/// `MAX_DEADLINE_MS = 60_000` (`shep-daemon/src/rpc.rs:38`), so this is well
/// inside what the daemon will honour.
pub const START_DEADLINE: Duration = Duration::from_secs(30);

/// Budget for `Request::Reload`.
///
/// A reload matching several sheep is walked in dependency order, and the
/// daemon holds every stage for the drains and readiness waits of the apps a
/// later stage needs, so two stages at the default timeouts already clear
/// [`START_DEADLINE`]. A client that gave up there would drop the walk with
/// its later stages never issued and hand the operator a timeout over a
/// half-reloaded fold. This asks for the daemon's whole `MAX_DEADLINE_MS`
/// (60s), which is the most it will honour, so the client is no longer the
/// one that gives up first.
///
/// It does not make the budget big enough, and no client-side value can. A
/// reload stage is bounded by `max(listen_timeout + graceful_timeout) *
/// swaps + STAGE_SLACK`, 16s per single-instance stage at the defaults: four
/// such stages is 64s, and two stages of a three-instance app is 76s. Both
/// are past the daemon's own 60s clamp, where the shepherd drops the walk
/// exactly as the client used to.
pub const RELOAD_DEADLINE: Duration = Duration::from_secs(60);

/// Budget for the log-plane verbs that walk the flock file by file:
/// `Request::Reopen` and `Request::Flush`.
///
/// The daemon visits matched sheep one after another with no per-sheep
/// bound, so a wedged or NFS-backed log directory can make the whole walk
/// take as long as the kernel does. Same 30s as [`START_DEADLINE`],
/// comfortably inside the daemon's own clamp.
pub const LOG_PLANE_DEADLINE: Duration = Duration::from_secs(30);

/// Budget for `Request::Trigger`.
///
/// An app's own `AppConfig::action_timeout` can reach `MAX_ACTION_TIMEOUT`
/// (58s), 2s under the daemon's own 60s clamp, so this asks for the full
/// 60s rather than abandon a reply the daemon is still building.
pub const TRIGGER_DEADLINE: Duration = Duration::from_secs(60);

/// How much longer the client waits than the deadline it asked the daemon
/// to honour, so it doesn't report a timeout for work that succeeded.
pub const DEADLINE_GRACE: Duration = Duration::from_secs(2);

/// Why a [`Client::request`] (or [`Client::request_with_deadline`]) call failed.
///
/// Non-exhaustive: expect more variants.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequestError {
    /// The daemon accepted the request and answered it with a structured error.
    Rpc(RpcError),
    /// No reply arrived within the request's own deadline plus [`DEADLINE_GRACE`].
    Timeout {
        /// The client-side budget that was exceeded.
        after: Duration,
    },
    /// The connection closed (daemon exit, crash, or a prior [`Client::close`])
    /// before this request's reply arrived.
    Closed,
    /// `body` failed to encode onto the wire.
    Wire(WireError),
    /// The daemon's reply arrived but this build could not decode it.
    ///
    /// Distinct from [`Self::Rpc`]: the daemon did not report a structured
    /// error, it answered with something (a `Reply` or an unrecognized
    /// `ServerFrame` variant) that does not match the shapes this build
    /// knows. The likely cause is a daemon newer than this client, not a
    /// daemon-side failure, so the message says which side could not read.
    Undecodable(WireError),
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rpc(err) => write!(f, "the daemon reported {:?}: {}", err.code, err.message),
            Self::Timeout { after } => write!(f, "no reply within {after:?}"),
            Self::Closed => f.write_str("the connection closed before a reply arrived"),
            Self::Wire(err) => write!(f, "request frame error: {err}"),
            Self::Undecodable(err) => write!(
                f,
                "this client could not decode the daemon's reply ({err}); the daemon is likely newer than this build"
            ),
        }
    }
}

impl core::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Wire(err) | Self::Undecodable(err) => Some(err),
            Self::Rpc(_) | Self::Timeout { .. } | Self::Closed => None,
        }
    }
}

/// Which daemon answered a [`Client::reconnect`].
///
/// Identity is the daemon pid in [`HelloAck`], the one per-process fact the
/// handshake carries. A handover `execve`s in place, so the pid and the
/// instance-id counter cross it together and the two questions have one
/// answer. A successor that reused its predecessor's pid would read as
/// [`Self::SameDaemon`], and nothing in the handshake can rule that out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a reconnect is worth making only if the caller reads which daemon answered"]
pub enum Reconnected {
    /// The same daemon process as before. Every id, name and fold means
    /// what it meant: the instance-id counter crossed the handover.
    SameDaemon,
    /// A different daemon process. Ids were minted afresh, so one held from
    /// before this call names a different sheep now, or none.
    NewDaemon,
}

/// A live connection to the daemon.
///
/// Backed by one actor task (see the crate's `actor` module) that owns the
/// socket; `request`/`request_with_deadline`/`close` all take `&self`, so
/// callers share one `Client` behind an `Arc` or a reference rather than
/// cloning a handle per caller.
pub struct Client {
    commands: mpsc::Sender<Command>,
    ack: HelloAck,
    socket: PathBuf,
    /// The name this client announced itself as a dog under, re-sent on
    /// every [`Self::reconnect`]. See [`Self::connect_as`].
    dog_name: Option<String>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("socket", &self.socket)
            .field("dog_name", &self.dog_name)
            .field("ack", &self.ack)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Connects to `socket` and performs the version handshake, bounded by
    /// [`HANDSHAKE_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// See [`Self::connect_with_timeout`].
    pub async fn connect(socket: &Path) -> Result<Self, ConnectError> {
        Self::connect_with_timeout(socket, HANDSHAKE_TIMEOUT).await
    }

    /// As [`Self::connect`], but with a caller-supplied handshake timeout.
    ///
    /// # Errors
    ///
    /// - [`ConnectError::Connect`]: the initial `connect(2)` call failed.
    /// - [`ConnectError::Wire`]: `Hello` failed to encode, or the reply failed to decode.
    /// - [`ConnectError::Io`]: a framed read or write failed after connect.
    /// - [`ConnectError::HandshakeClosed`]: the peer closed before a `HelloReply`.
    /// - [`ConnectError::HandshakeTimeout`]: no `HelloReply` arrived within `timeout`.
    /// - [`ConnectError::ProtocolMismatch`]: the daemon refused on protocol-version skew.
    pub async fn connect_with_timeout(
        socket: &Path,
        timeout: Duration,
    ) -> Result<Self, ConnectError> {
        Self::connect_as(socket, timeout, None).await
    }

    /// As [`Self::connect_with_timeout`], but announcing this client as the
    /// dog registered under `dog_name`.
    ///
    /// Crate-private: the public constructors above pass `None`, and only
    /// [`ReconnectingClient`](crate::ReconnectingClient) passes a name. A
    /// client that could claim an arbitrary dog name could get that dog
    /// restarted on its own say-so.
    ///
    /// # Errors
    ///
    /// See [`Self::connect_with_timeout`].
    pub(crate) async fn connect_as(
        socket: &Path,
        timeout: Duration,
        dog_name: Option<&str>,
    ) -> Result<Self, ConnectError> {
        let connection = Connection::open(socket, timeout, dog_name).await?;
        let (frames, ack) = connection.into_parts();
        let commands = actor::spawn(frames);
        Ok(Self {
            commands,
            ack,
            socket: socket.to_path_buf(),
            dog_name: dog_name.map(str::to_owned),
        })
    }

    /// The daemon's handshake acknowledgement.
    #[must_use]
    pub fn daemon(&self) -> &HelloAck {
        &self.ack
    }

    /// Resolves once this connection has ended: daemon exit, crash,
    /// `execve`, a write failure, or a prior [`Self::close`]. Resolves
    /// immediately if already gone, and may be awaited more than once.
    ///
    /// This does not reconnect, and [`Self::request`] does not retry. See
    /// [`ReconnectingClient`](crate::ReconnectingClient) for the supervised
    /// wrapper.
    pub async fn closed(&self) {
        self.commands.closed().await;
    }

    /// The path this client is connected through.
    ///
    /// `HelloAck` doesn't carry the socket path, so the `Client` keeps the
    /// `PathBuf` it connected with, for a caller that needs it after
    /// teardown.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Re-dials [`Self::socket`] and handshakes again, bounded by
    /// [`HANDSHAKE_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// See [`Self::reconnect_within`].
    pub async fn reconnect(&mut self) -> Result<Reconnected, ConnectError> {
        self.reconnect_within(HANDSHAKE_TIMEOUT).await
    }

    /// As [`Self::reconnect`], with a caller-supplied handshake timeout.
    ///
    /// Nothing calls this for you: [`Self::request`] reports a dead
    /// connection and never re-dials. An id one daemon minted names a
    /// different sheep under the next, so a silent retry could land a
    /// `Stop` on the wrong one. `&mut self` keeps the choice the caller's,
    /// since a shared `Arc<Client>` cannot reconnect. A supervised dog
    /// exits on [`RequestError::Closed`] instead of calling this, rather
    /// than race the shepherd's own restart of it.
    ///
    /// Any [`EventStream`] taken before this call is dead; subscribe again.
    ///
    /// # Errors
    ///
    /// The set [`Self::connect_with_timeout`] raises. On any of them this
    /// client is unchanged, still holding the connection it had.
    pub async fn reconnect_within(
        &mut self,
        timeout: Duration,
    ) -> Result<Reconnected, ConnectError> {
        let fresh = Self::connect_as(&self.socket, timeout, self.dog_name.as_deref()).await?;
        let same_daemon = fresh.ack.pid == self.ack.pid;
        // Dropping the predecessor ends its actor task and its socket with
        // it, exactly as `close` does. Installed only once the successor
        // has handshaken, so a failure above leaves this client as it was.
        *self = fresh;
        Ok(if same_daemon {
            Reconnected::SameDaemon
        } else {
            Reconnected::NewDaemon
        })
    }

    /// Sends `body` with [`DEFAULT_DEADLINE`].
    ///
    /// Shorthand for [`Self::request_with_deadline`]`(body, None)`.
    ///
    /// # Errors
    ///
    /// See [`Self::request_with_deadline`].
    pub async fn request(&self, body: Request) -> Result<Response, RequestError> {
        self.request_with_deadline(body, None).await
    }

    /// Sends `body` with `deadline`, or [`DEFAULT_DEADLINE`] if `None`. The
    /// client waits `deadline` plus [`DEADLINE_GRACE`] for a reply before
    /// giving up locally.
    ///
    /// # Errors
    ///
    /// - [`RequestError::Rpc`]: the daemon answered with a structured error.
    /// - [`RequestError::Timeout`]: no reply within `deadline + DEADLINE_GRACE`.
    /// - [`RequestError::Closed`]: the connection closed before a reply arrived.
    /// - [`RequestError::Wire`]: `body` failed to encode.
    pub async fn request_with_deadline(
        &self,
        body: Request,
        deadline: Option<Duration>,
    ) -> Result<Response, RequestError> {
        let deadline = deadline.unwrap_or(DEFAULT_DEADLINE);
        let (reply_to, reply_rx) = oneshot::channel();
        self.commands
            .send(Command::Request {
                body,
                deadline_ms: Some(millis(deadline)),
                reply_to,
            })
            .await
            .map_err(|_send_error| RequestError::Closed)?;

        // Saturating: `deadline` is the caller's, and `Duration`'s `Add`
        // panics rather than saturating on overflow.
        let budget = deadline.saturating_add(DEADLINE_GRACE);
        match tokio::time::timeout(budget, reply_rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_recv_error)) => Err(RequestError::Closed),
            Err(_elapsed) => Err(RequestError::Timeout { after: budget }),
        }
    }

    /// Subscribes this connection to `topics`: dotted glob patterns matched
    /// against [`shep_core::protocol::BusEvent::topic`] (`process.*`,
    /// `log.*`, `daemon.*`, ...).
    ///
    /// A second call on the same `Client` replaces the daemon-side filter
    /// rather than adding to it; a caller wanting two topic sets needs two
    /// `Client`s. The returned [`EventStream`]'s receiver is installed
    /// before the `Subscribe` request is sent, so no pushed event is missed.
    ///
    /// # Errors
    ///
    /// Same as [`Self::request`].
    pub async fn subscribe(&self, topics: Vec<String>) -> Result<EventStream, RequestError> {
        let (reply_to, reply_rx) = oneshot::channel();
        self.commands
            .send(Command::Subscribe { reply_to })
            .await
            .map_err(|_send_error| RequestError::Closed)?;
        let receiver = reply_rx.await.map_err(|_recv_error| RequestError::Closed)?;

        // reply is `Response::Subscribed`; unchecked, since interpreting
        // `Response` variants is shep-cli's job, not this crate's
        self.request(Request::Subscribe { topics }).await?;
        Ok(EventStream::new(receiver))
    }

    /// Closes the connection.
    ///
    /// Drops the command channel to the actor task, which ends the actor's
    /// loop and drops the underlying socket.
    ///
    /// # Errors
    ///
    /// Never fails today. `Result` leaves room for a later, more graceful
    /// teardown (draining in-flight requests before dropping, say) to start
    /// returning one without an API break.
    pub async fn close(self) -> Result<(), RequestError> {
        drop(self.commands);
        Ok(())
    }
}

/// Saturating `Duration` to wire milliseconds. A caller-supplied `Duration`
/// above the wire range saturates at `u64::MAX` ms rather than overflowing.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use shep_core::protocol::PROTOCOL_VERSION;

    use super::*;
    use crate::testing::{Handovers, Handshake, control_address, fake_daemon_across_handovers};

    /// Every bounded wait here uses one budget: generous against a loaded
    /// CI runner, small enough that a stuck test fails rather than hangs.
    const BOUND: Duration = Duration::from_secs(5);

    /// An ack distinguishable per generation, so a test can tell which
    /// daemon answered rather than only that one did.
    fn ack_from(pid: u32) -> HelloAck {
        HelloAck {
            daemon_version: format!("0.0.{pid}"),
            protocol: PROTOCOL_VERSION,
            pid,
            min_supported: None,
        }
    }

    /// Cuts the accepted connection and waits for the client to notice, so
    /// a reconnect dials into a listener with nobody on the other end.
    ///
    /// The wait is the forcing mechanism: `cut` only sends, and a reconnect
    /// issued before the predecessor's socket dies would queue behind it in
    /// the fake's single-connection accept loop.
    async fn cut_and_settle(client: &Client, shepherds: &Handovers) {
        shepherds.cut().await;
        tokio::time::timeout(BOUND, client.closed())
            .await
            .expect("the cut connection must be reported closed");
    }

    /// fails if a reconnect across a handover reports a new daemon. A
    /// handover `execve`s in place, so the ids the caller holds still name
    /// the sheep they named, and a caller told otherwise would throw them
    /// away and re-resolve every one.
    #[tokio::test]
    async fn a_reconnect_onto_the_same_pid_reports_the_same_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Accept(ack_from(11)),
            ],
        );
        let mut client = Client::connect(&path).await.unwrap();

        cut_and_settle(&client, &shepherds).await;
        let outcome = tokio::time::timeout(BOUND, client.reconnect())
            .await
            .expect("the reconnect must not hang")
            .expect("the successor accepts");

        assert_eq!(outcome, Reconnected::SameDaemon);
        let served = tokio::time::timeout(BOUND, client.request(Request::Ping))
            .await
            .expect("the request after a reconnect must not hang");
        assert!(served.is_ok(), "after the reconnect: {served:?}");
    }

    /// fails if a reconnect onto a restarted daemon is reported as the same
    /// one. Ids are minted per daemon lifetime and never persisted, so a
    /// caller that kept using them would address a different sheep, or none.
    #[tokio::test]
    async fn a_reconnect_onto_a_fresh_pid_reports_a_new_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Accept(ack_from(22)),
            ],
        );
        let mut client = Client::connect(&path).await.unwrap();
        assert_eq!(client.daemon().pid, 11);

        cut_and_settle(&client, &shepherds).await;
        let outcome = tokio::time::timeout(BOUND, client.reconnect())
            .await
            .expect("the reconnect must not hang")
            .expect("the successor accepts");

        assert_eq!(outcome, Reconnected::NewDaemon);
        assert_eq!(
            client.daemon().daemon_version,
            "0.0.22",
            "the ack must come from the daemon now answering"
        );
    }

    /// fails if a failed reconnect tears down the client it was called on.
    /// The caller's next move is to try again, and a client left holding
    /// neither connection could not say which socket or which dog it was.
    #[tokio::test]
    async fn a_failed_reconnect_leaves_the_client_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![Handshake::Accept(ack_from(11)), Handshake::Drop],
        );
        let mut client = Client::connect(&path).await.unwrap();

        cut_and_settle(&client, &shepherds).await;
        let refused = tokio::time::timeout(BOUND, client.reconnect())
            .await
            .expect("a refused reconnect must fail, not hang");

        // Either shape: a peer that closes with the handshake unread
        // sends RST on Linux, which arrives as `Io(ConnectionReset)`, and
        // EOF on macOS. `connection`'s own close test accepts the same
        // pair. Which one is not this test's question.
        assert!(
            matches!(
                refused,
                Err(ConnectError::HandshakeClosed | ConnectError::Io(_))
            ),
            "a successor that closes mid-handshake: {refused:?}"
        );
        assert_eq!(client.daemon().pid, 11, "the ack must be the one it had");
        assert_eq!(client.socket(), path, "the socket must be the one it had");
    }

    /// fails if a dog's name reaches the first daemon and not the one it
    /// reconnects to. The name is how a shepherd says which dog it refused,
    /// and a predecessor is not around to be asked.
    #[tokio::test]
    async fn a_dogs_name_rides_its_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Accept(ack_from(22)),
            ],
        );
        let mut client = Client::connect_as(&path, HANDSHAKE_TIMEOUT, Some("metrics"))
            .await
            .unwrap();

        cut_and_settle(&client, &shepherds).await;
        let _ = tokio::time::timeout(BOUND, client.reconnect())
            .await
            .expect("the reconnect must not hang")
            .expect("the successor accepts");

        let named: Vec<Option<String>> = shepherds
            .hellos()
            .into_iter()
            .map(|hello| hello.dog_name)
            .collect();
        assert_eq!(
            named,
            vec![Some("metrics".to_string()), Some("metrics".to_string())],
            "every generation must be told which dog is talking to it"
        );
    }

    /// fails if a client that is not a dog invents a name on reconnect.
    #[tokio::test]
    async fn a_client_that_is_not_a_dog_stays_anonymous_across_a_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Accept(ack_from(22)),
            ],
        );
        let mut client = Client::connect(&path).await.unwrap();

        cut_and_settle(&client, &shepherds).await;
        let _ = tokio::time::timeout(BOUND, client.reconnect())
            .await
            .expect("the reconnect must not hang")
            .expect("the successor accepts");

        assert!(
            shepherds
                .hellos()
                .iter()
                .all(|hello| hello.dog_name.is_none()),
            "an unnamed client must stay unnamed: {:?}",
            shepherds.hellos()
        );
    }
}
