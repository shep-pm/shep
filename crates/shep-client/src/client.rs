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

use shep_core::destination::Destination;
use shep_core::protocol::{
    HelloAck, Request, Response, RpcError, RpcErrorCode, SANDBOX_SINCE, WireError,
};

use crate::actor::{self, Command};
use crate::connection::{ConnectError, Connection, HANDSHAKE_TIMEOUT};
use crate::events::EventStream;
use crate::ssh::{self, SshOptions};

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
/// A reload walks several sheep in dependency order. It holds every stage
/// for the drains and readiness waits a later stage needs. Two stages at
/// the default timeouts already pass [`START_DEADLINE`]. This asks for the
/// daemon's whole `MAX_DEADLINE_MS` (60s), the most it honours. Four
/// default single-instance stages need 64s, past even that.
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

/// Budget for `Request::Stop` and `Request::Delete`.
///
/// A stop runs the kill ladder, then the lamb sweep. Each is bounded by a
/// sheep's `kill_timeout`, and a selector can match many sheep. The client
/// cannot size that: asking the daemon for each `kill_timeout` costs a
/// round trip and still races the actor. This asks for the daemon's whole
/// `MAX_DEADLINE_MS` (60s) instead.
pub const STOP_DEADLINE: Duration = Duration::from_secs(60);

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
    /// The daemon answered with a [`Response`] other than the one a typed
    /// request expects, such as a `Flock` for a `DogConfig`.
    ///
    /// Names both by variant alone: a listing or a dog's section never
    /// reaches the message.
    UnexpectedReply {
        /// The request sent, by its `Request` variant name.
        asked: &'static str,
        /// The answer received, by [`Response::name`].
        answered: &'static str,
    },
}

impl RequestError {
    /// The [`shep_core::exit`] code a process stopping on this error uses.
    ///
    /// A refusal takes its own code's number, and a closed connection is
    /// nothing answering. A reply this build cannot decode is a plain
    /// failure, since neither side is known to be at fault.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        use shep_core::exit;
        match self {
            Self::Rpc(rpc) => rpc.code.exit_code(),
            Self::Timeout { .. } => exit::DEADLINE_EXCEEDED,
            Self::Closed => exit::DAEMON_UNREACHABLE,
            Self::Wire(_) | Self::UnexpectedReply { .. } => exit::INTERNAL,
            Self::Undecodable(_) => exit::FAILURE,
        }
    }
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
            Self::UnexpectedReply { asked, answered } => {
                write!(f, "the daemon answered {asked} with {answered}")
            }
        }
    }
}

impl core::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Wire(err) | Self::Undecodable(err) => Some(err),
            Self::Rpc(_) | Self::Timeout { .. } | Self::Closed | Self::UnexpectedReply { .. } => {
                None
            }
        }
    }
}

/// How a [`Client`] was dialled, kept so a reconnect dials the same way.
#[derive(Debug, Clone)]
pub(crate) enum Dial {
    /// A shepherd's socket on this host.
    Socket(PathBuf),
    /// A shepherd on another host, through the system ssh.
    Ssh {
        destination: Destination,
        options: SshOptions,
    },
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
    dial: Dial,
    /// The name this client announces itself as a dog under, re-sent on
    /// every [`Client::reconnect`]. See [`Self::connect_as`].
    dog_name: Option<String>,
}

// Manual, not derived: a path, a host, an ack and a dog's name carry no
// secret, and none of them is what a derived impl would lead with. The command
// channel says nothing a reader can act on.
impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("socket", &self.socket())
            .field("host", &self.host())
            .field("ack", &self.ack)
            .field("dog_name", &self.dog_name)
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
        Ok(Self::from_connection(
            connection,
            Dial::Socket(socket.to_path_buf()),
            dog_name,
        ))
    }

    /// As [`Self::connect_as`], over `stream`, a connection the shepherd
    /// already opened. Dialled again through `socket` on a reconnect.
    ///
    /// # Errors
    ///
    /// See [`Self::connect_with_timeout`], bar [`ConnectError::Connect`]:
    /// there is no `connect(2)` to fail.
    pub(crate) async fn connect_over(
        stream: shep_core::transport::ClientStream,
        socket: &Path,
        timeout: Duration,
        dog_name: Option<&str>,
    ) -> Result<Self, ConnectError> {
        let connection = tokio::time::timeout(timeout, Connection::open_over(stream, dog_name))
            .await
            .map_err(|_elapsed| ConnectError::HandshakeTimeout { after: timeout })??;
        Ok(Self::from_connection(
            connection,
            Dial::Socket(socket.to_path_buf()),
            dog_name,
        ))
    }

    fn from_connection(connection: Connection, dial: Dial, dog_name: Option<&str>) -> Self {
        let (frames, ack) = connection.into_parts();
        let commands = actor::spawn(frames);
        Self {
            commands,
            ack,
            dial,
            dog_name: dog_name.map(str::to_owned),
        }
    }

    /// Connects to the shepherd on another host through the system `ssh`,
    /// with [`SshOptions::default`].
    ///
    /// Shorthand for [`Self::connect_ssh_with`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// use shep_client::Client;
    /// use shep_client::shep_core::protocol::Request;
    ///
    /// # async fn list() -> Result<(), Box<dyn core::error::Error>> {
    /// let client = Client::connect_ssh("deploy@build-01").await?;
    /// let flock = client.request(Request::ListFlock).await?;
    /// # let _ = flock;
    /// # Ok(())
    /// # }
    /// # let _ = list;
    /// ```
    ///
    /// # Errors
    ///
    /// See [`Self::connect_ssh_with`].
    pub async fn connect_ssh(dest: &str) -> Result<Self, ConnectError> {
        Self::connect_ssh_with(dest, &SshOptions::default()).await
    }

    /// Connects to the shepherd on another host by running
    /// `ssh -T [-o BatchMode=yes] -- <dest> shep dial-stdio` and handshaking
    /// over the child's pipes.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// use shep_client::{Client, SshOptions};
    ///
    /// # async fn dial() -> Result<(), Box<dyn core::error::Error>> {
    /// let options = SshOptions::default()
    ///     .batch_mode(true)
    ///     .timeout(Some(Duration::from_secs(10)));
    /// let client = Client::connect_ssh_with("build-01", &options).await?;
    /// assert_eq!(client.host(), Some("build-01"));
    /// # Ok(())
    /// # }
    /// # let _ = dial;
    /// ```
    /// # Errors
    ///
    /// - [`ConnectError::Destination`]: `dest` is not a destination ssh can be handed
    ///   safely. ssh would read `-oProxyCommand=..` as an option and run it.
    /// - [`ConnectError::Ssh`]: ssh could not run, reach the host, or be served by the far side.
    /// - [`ConnectError::Wire`], [`ConnectError::Io`]: a frame failed to encode or decode.
    /// - [`ConnectError::HandshakeClosed`]: the stream closed before a `HelloReply`.
    /// - [`ConnectError::HandshakeTimeout`]: the phase ran past [`SshOptions::timeout`].
    /// - [`ConnectError::ProtocolMismatch`]: the shepherd refused on version skew.
    pub async fn connect_ssh_with(dest: &str, options: &SshOptions) -> Result<Self, ConnectError> {
        let destination = Destination::parse(dest).map_err(ConnectError::Destination)?;
        let dial = Dial::Ssh {
            destination,
            options: options.clone(),
        };
        Self::connect_dial(&dial, options.timeout_or(None), None).await
    }

    /// Dials `dial` again, as a reconnect does, or for the first time.
    ///
    /// `budget` bounds the attempt: a socket dial takes it as is, and an ssh
    /// dial takes the smaller of it and [`SshOptions::timeout`]. `None` leaves
    /// an ssh dial to its options alone.
    ///
    /// # Errors
    ///
    /// See [`Self::connect_with_timeout`] and [`Self::connect_ssh_with`].
    pub(crate) async fn connect_dial(
        dial: &Dial,
        budget: Option<Duration>,
        dog_name: Option<&str>,
    ) -> Result<Self, ConnectError> {
        match dial {
            Dial::Socket(socket) => {
                Self::connect_as(socket, budget.unwrap_or(HANDSHAKE_TIMEOUT), dog_name).await
            }
            Dial::Ssh {
                destination,
                options,
            } => {
                let connection =
                    ssh::open(destination, options, options.timeout_or(budget), dog_name).await?;
                Ok(Self::from_connection(connection, dial.clone(), dog_name))
            }
        }
    }

    /// How this client was dialled.
    pub(crate) fn dial(&self) -> &Dial {
        &self.dial
    }

    /// The name this client announces itself as a dog under, or `None` for
    /// a caller that is not one.
    ///
    /// Crate-private for the reason [`Self::connect_as`] is: only this
    /// crate may set a name, so only this crate may report one.
    pub(crate) fn dog_name(&self) -> Option<&str> {
        self.dog_name.as_deref()
    }

    /// Takes over `fresh`'s connection, dropping this handle's own.
    ///
    /// Dropping the old command channel signals its actor rather than
    /// stopping it where it stands. An actor already awaiting a write
    /// finishes that write, sees the closed channel when it next reaches
    /// its select, and only then drops the old socket.
    pub(crate) fn replace_connection(&mut self, fresh: Self) {
        *self = fresh;
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
    /// teardown. Empty for a client from [`Self::connect_ssh`], which has no
    /// socket on this host: see [`Self::host`].
    #[must_use]
    pub fn socket(&self) -> &Path {
        match &self.dial {
            Dial::Socket(socket) => socket,
            Dial::Ssh { .. } => Path::new(""),
        }
    }

    /// The destination this client reached over ssh, or `None` for a client
    /// connected to a socket on this host.
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        match &self.dial {
            Dial::Socket(_) => None,
            Dial::Ssh { destination, .. } => Some(destination.as_str()),
        }
    }

    /// Hands `command` to the actor task behind this handle.
    async fn send_command(&self, command: Command) -> Result<(), RequestError> {
        self.commands
            .send(command)
            .await
            .map_err(|_send_error| RequestError::Closed)
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
    /// - [`RequestError::Rpc`]: the daemon answered with a structured error,
    ///   or `body` asks for a `sandbox` from a shepherd below
    ///   [`SANDBOX_SINCE`], which is refused as `Unsupported` before anything
    ///   is sent.
    /// - [`RequestError::Timeout`]: no reply within `deadline + DEADLINE_GRACE`.
    /// - [`RequestError::Closed`]: the connection closed before a reply arrived.
    /// - [`RequestError::Wire`]: `body` failed to encode.
    pub async fn request_with_deadline(
        &self,
        body: Request,
        deadline: Option<Duration>,
    ) -> Result<Response, RequestError> {
        if body.asks_for_sandbox() && self.ack.protocol < SANDBOX_SINCE {
            return Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::Unsupported,
                message: format!(
                    "this shepherd speaks protocol {} and runs a sandbox from protocol \
                     {SANDBOX_SINCE}, so it would start the sheep unsandboxed. Restart the \
                     shepherd and run this again; nothing was sent",
                    self.ack.protocol
                ),
                daemon_version: None,
            }));
        }
        let deadline = deadline.unwrap_or(DEFAULT_DEADLINE);
        let (reply_to, reply_rx) = oneshot::channel();
        self.send_command(Command::Request {
            body,
            deadline_ms: Some(millis(deadline)),
            reply_to,
        })
        .await?;

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
        self.send_command(Command::Subscribe { reply_to }).await?;
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
    use shep_core::exit;
    use shep_core::protocol::RpcErrorCode;

    use super::*;

    #[test]
    fn a_request_error_exits_on_its_causes_code() {
        let unsupported = RequestError::Rpc(RpcError {
            code: RpcErrorCode::Unsupported,
            message: "no such verb".to_owned(),
            daemon_version: None,
        });
        let cases = [
            (unsupported, exit::UNSUPPORTED),
            (RequestError::Closed, exit::DAEMON_UNREACHABLE),
            (
                RequestError::Timeout {
                    after: Duration::from_secs(7),
                },
                exit::DEADLINE_EXCEEDED,
            ),
            (
                RequestError::UnexpectedReply {
                    asked: "ListFlock",
                    answered: "Pong",
                },
                exit::INTERNAL,
            ),
        ];
        for (err, code) in cases {
            assert_eq!(err.exit_code(), code, "{err}");
        }
    }

    fn sandboxed_start() -> Request {
        let app = shep_core::config::AppConfig {
            sandbox: Some("sandbox.json".to_owned()),
            ..shep_core::config::AppConfig::minimal("web", "./srv")
        };
        Request::Start { apps: vec![app] }
    }

    #[tokio::test]
    async fn a_sandbox_is_refused_before_it_reaches_an_older_shepherd() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::testing::control_address(dir.path());
        let ack = HelloAck {
            protocol: SANDBOX_SINCE - 1,
            ..crate::testing::sample_ack()
        };
        let (client, mut sent) =
            crate::testing::fake_client_capturing_envelopes_with_ack(&path, ack).await;

        let refused = client.request(sandboxed_start()).await.unwrap_err();

        let RequestError::Rpc(rpc) = &refused else {
            panic!("expected a refusal, got {refused:?}");
        };
        assert_eq!(rpc.code, RpcErrorCode::Unsupported);
        assert!(rpc.message.contains("nothing was sent"), "{}", rpc.message);
        assert_eq!(refused.exit_code(), exit::UNSUPPORTED);
        assert!(sent.try_recv().is_err(), "the request reached the wire");
    }

    #[tokio::test]
    async fn a_sandbox_is_sent_to_a_shepherd_that_runs_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::testing::control_address(dir.path());
        let ack = HelloAck {
            protocol: SANDBOX_SINCE,
            ..crate::testing::sample_ack()
        };
        let (client, mut sent) =
            crate::testing::fake_client_capturing_envelopes_with_ack(&path, ack).await;

        let _ = client.request(sandboxed_start()).await;

        assert!(
            sent.try_recv().is_ok(),
            "the request never reached the wire"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_ssh_client_has_a_host_and_no_socket() {
        let dir = tempfile::tempdir().unwrap();
        crate::ssh::stand_in::write_reply(dir.path());
        let program = crate::ssh::stand_in::stand_in(
            dir.path(),
            "cat \"$(dirname \"$0\")/reply.bin\"\ncat >/dev/null",
        );

        let client =
            Client::connect_ssh_with("deploy@build-01", &SshOptions::default().program(program))
                .await
                .unwrap();

        assert_eq!(client.host(), Some("deploy@build-01"));
        assert_eq!(client.socket(), Path::new(""));
        let shown = format!("{client:?}");
        assert!(shown.contains("host: Some(\"deploy@build-01\")"), "{shown}");
    }

    #[tokio::test]
    async fn a_socket_client_has_a_socket_and_no_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::testing::control_address(dir.path());
        let (client, _daemon) = crate::testing::fake_client_on(&path).await;

        assert_eq!(client.host(), None);
        assert_eq!(client.socket(), path);
    }
}
