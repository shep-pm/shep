//! [`ReconnectingClient`]: a connection that re-establishes itself when the daemon on the other end is replaced.
//!
//! A handover carries the listening socket across `execve` but not an accepted
//! one, so a dog's own process survives holding a dead socket. [`Client`] has
//! no such mode: the CLI's one-shot verbs must never see a request silently
//! retried, so in-flight requests here fail too, and only the connection
//! re-establishes. A background task reconnects as soon as the connection
//! dies rather than on the next request, so an idle dog still re-handshakes
//! before `shep daemon reload` polls dog staleness, and a refusing successor
//! stops the supervisor ([`LinkState::Refused`]) rather than retrying.
//! [`ReconnectingClient::connect_as_dog`] names the dog on every handshake so
//! a refusal is actionable.
//!
//! The two differ in more than retry policy. This type's swap happens in its
//! supervisor task, concurrently with `&self` requests, so a request really
//! can be in flight when the daemon is replaced and really does fail.
//! [`Client::reconnect`] takes `&mut self`, which excludes that case instead
//! of handling it.
//!
//! That signature also decides who can call it. A dog holds a
//! [`ReconnectingClient`], which is not [`Clone`] and keeps its [`Client`]
//! behind an [`Arc`], so `&mut Client` is out of reach: a dog waits on this
//! type's own link state instead. The callers [`Client::reconnect`] is for
//! are the ones that own their [`Client`] outright.
//!
//! # Which daemon answered
//!
//! [`Client::reconnect`] reports [`Reconnected::SameDaemon`] when the daemon
//! now answering carries the [`HelloAck`] pid the predecessor did. A handover
//! is an `execve`, which keeps the pid, and its blob carries the flock's id
//! counter across with it, so the two facts move together: a matching pid
//! means an id minted before the drop still names the same sheep. A daemon
//! stopped and started again gets a fresh pid and a fresh id space. The gap
//! is pid reuse inside one reconnect, which nothing on the wire today could
//! tell apart.

use core::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
// tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
// budget below is measured against a `tokio::time::sleep` that does too.
use tokio::time::Instant;

use shep_core::protocol::{HelloAck, Request, Response};

use crate::client::{Client, RequestError};
use crate::connection::{ConnectError, HANDSHAKE_TIMEOUT};
use crate::events::EventStream;

/// How long the supervisor waits after its FIRST failed reconnect attempt
/// before trying again.
///
/// The first attempt carries no delay: across a handover the listening
/// socket stays bound, so `connect(2)` succeeds into the backlog and only
/// the handshake waits for the successor to start accepting. 50ms is the
/// pause before a second attempt, short enough that a successor a few
/// milliseconds late costs one of these rather than a visible outage.
pub const RECONNECT_MIN_DELAY: Duration = Duration::from_millis(50);

/// The ceiling [`RECONNECT_MIN_DELAY`] doubles up to.
///
/// Same order as [`HANDSHAKE_TIMEOUT`]: past this point one further attempt
/// costs about as much as the wait between attempts, so the loop is
/// bounded noise rather than a spin. A dog whose daemon is genuinely gone
/// sits here indefinitely: the daemon that would have reaped it is the one
/// that vanished.
pub const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(5);

/// The next delay in the reconnect ladder, doubling up to
/// [`RECONNECT_MAX_DELAY`].
///
/// Both reconnect paths walk this one ladder: the supervisor behind
/// [`ReconnectingClient`], and [`Client::reconnect_within`]. Shared so a
/// change to the schedule cannot reach one of them and miss the other.
fn next_delay(current: Duration) -> Duration {
    // Saturating: `Duration`'s `Mul` panics on overflow, and a ladder that
    // cannot overflow is one fewer precondition on a caller's own value.
    current.saturating_mul(2).min(RECONNECT_MAX_DELAY)
}

/// What a [`ReconnectingClient`]'s supervisor is currently doing.
///
/// Non-exhaustive: expect more variants.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkState {
    /// Connected. Requests go out on this generation of the connection.
    Connected,
    /// The connection dropped and the supervisor is re-establishing it.
    /// Requests issued now fail with [`RequestError::Closed`]; they are not
    /// queued and not retried.
    Reconnecting,
    /// A successor refused the handshake on protocol-version skew. The
    /// supervisor has stopped, and every later request fails with
    /// [`RequestError::Closed`]: the daemon that refused is the party
    /// that can fix it, not a retry.
    Refused {
        /// The daemon's own crate version, when it named one. `None` from a
        /// daemon built before the refusal carried it.
        daemon_version: Option<String>,
        /// The daemon's refusal message, verbatim.
        message: String,
    },
}

/// Why a bounded wait on the link ended with the link still down.
///
/// Non-exhaustive: expect more variants.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkLost {
    /// The wait ran out with the supervisor still dialling.
    Budget {
        /// How long the caller waited before giving up.
        waited: Duration,
    },
    /// A successor refused the handshake on protocol-version skew, so the
    /// supervisor has stopped and no further wait could succeed.
    Refused {
        /// The daemon's own crate version, when it named one. `None` from a
        /// daemon built before the refusal carried it.
        daemon_version: Option<String>,
        /// The daemon's refusal message, verbatim.
        message: String,
    },
}

impl fmt::Display for LinkLost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Budget { waited } => write!(f, "no shepherd answered within {waited:?}"),
            Self::Refused { message, .. } => {
                write!(f, "the shepherd refused this connection: {message}")
            }
        }
    }
}

impl core::error::Error for LinkLost {}

// Exhaustive on purpose, unlike `LinkState`: the question is binary, and a
// caller branching on it is better served by a match a third variant would
// break than by a wildcard arm that goes on compiling.
/// Whether a [`Client::reconnect`] reached the daemon it was talking to
/// before, or a different one.
///
/// See this module's own docs for how the two are told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "the verdict says whether ids held from before the reconnect still mean anything"]
pub enum Reconnected {
    /// The daemon now answering is the one that answered before, so an id
    /// minted before the connection dropped still names the same sheep.
    ///
    /// It says nothing about the connection. The old generation and the
    /// subscription on it died under either verdict, so a caller wanting
    /// events subscribes again whichever one it gets.
    SameDaemon,
    /// A different daemon is answering, minting ids from its own fresh
    /// space. Every id the caller still holds names nothing here.
    NewDaemon,
}

impl Client {
    /// Re-establishes this connection, reporting which daemon answered.
    ///
    /// Shorthand for [`Self::reconnect_within`] with [`HANDSHAKE_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// See [`Self::reconnect_within`].
    pub async fn reconnect(&mut self) -> Result<Reconnected, ConnectError> {
        self.reconnect_within(HANDSHAKE_TIMEOUT).await
    }

    /// Re-establishes this connection, retrying until `budget` is spent.
    ///
    /// Meant for a connection that has already ended, as [`Self::closed`]
    /// reports; a live one is replaced only once the new handshake finishes.
    /// `&mut self` holds the handle exclusively, so none of its own requests
    /// can be in flight while this runs. A successor still coming up is
    /// retried, from [`RECONNECT_MIN_DELAY`] doubling to
    /// [`RECONNECT_MAX_DELAY`]; a refusal is not. An [`EventStream`] taken
    /// before this call belongs to the old connection, so a caller wanting
    /// events past it subscribes again.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use shep_client::{Client, Reconnected};
    ///
    /// # async fn watch(client: &mut Client, held: &mut Option<u32>)
    /// # -> Result<(), Box<dyn core::error::Error>> {
    /// client.closed().await;
    /// if client.reconnect().await? == Reconnected::NewDaemon {
    ///     // Ids are minted per daemon and never persisted, so this one now
    ///     // names a different sheep, or none at all.
    ///     *held = None;
    /// }
    /// # Ok(())
    /// # }
    /// # let _ = watch;
    /// ```
    ///
    /// # Errors
    ///
    /// The failure of the last attempt `budget` allowed:
    ///
    /// - [`ConnectError::ProtocolMismatch`]: a daemon refused on
    ///   protocol-version skew. Returned without a retry, since asking the
    ///   same daemon again cannot change its answer.
    /// - Anything else [`Self::connect_with_timeout`] reports, once `budget`
    ///   leaves no room for a further attempt.
    ///
    /// # Cancellation safety
    ///
    /// Safe to cancel. The old connection is held until a new one has
    /// handshaken, so dropping this future leaves the handle on the
    /// connection it already had rather than on neither.
    pub async fn reconnect_within(
        &mut self,
        budget: Duration,
    ) -> Result<Reconnected, ConnectError> {
        let started = Instant::now();
        let socket = self.socket().to_path_buf();
        let dog_name = self.dog_name().map(str::to_owned);
        let predecessor = self.daemon().pid;
        let mut delay = RECONNECT_MIN_DELAY;

        loop {
            let left = budget.saturating_sub(started.elapsed());
            let attempt =
                Client::connect_as(&socket, left.min(HANDSHAKE_TIMEOUT), dog_name.as_deref()).await;

            let failed = match attempt {
                Ok(fresh) => {
                    let verdict = if fresh.daemon().pid == predecessor {
                        Reconnected::SameDaemon
                    } else {
                        Reconnected::NewDaemon
                    };
                    self.replace_connection(fresh);
                    return Ok(verdict);
                }
                Err(refusal @ ConnectError::ProtocolMismatch { .. }) => return Err(refusal),
                // Everything else is a daemon that is not ready yet, which
                // resolves on its own if the budget outlasts it.
                Err(transient) => transient,
            };

            if budget.saturating_sub(started.elapsed()) <= delay {
                return Err(failed);
            }
            tokio::time::sleep(delay).await;
            delay = next_delay(delay);
        }
    }
}

/// A [`Client`] that re-establishes its own connection when the daemon it
/// was talking to is replaced.
///
/// Built for dogs, which outlive the shepherd they connected to: a dog's
/// process crosses a daemon handover as an ordinary child, so it is still
/// running when its socket dies. The CLI uses a bare [`Client`] instead;
/// see this module's own docs for why.
///
/// # Example
///
/// ```no_run
/// use shep_client::{ReconnectingClient, shep_core::protocol::Request};
///
/// # async fn dog(socket: &std::path::Path) -> Result<(), Box<dyn core::error::Error>> {
/// // `name` is what the daemon put in `$SHEP_DOG_NAME` when it spawned
/// // this process; it lets the daemon act on a refusal (G8).
/// let client = ReconnectingClient::connect_as_dog(socket, "metrics").await?;
/// // Survives the daemon being replaced underneath it; a request that was
/// // in flight at the moment it happened still fails rather than retrying.
/// let _flock = client.request(Request::ListFlock).await?;
/// # Ok(())
/// # }
/// # let _ = dog;
/// ```
pub struct ReconnectingClient {
    shared: Arc<Shared>,
    supervisor: JoinHandle<()>,
}

/// Manual, not derived: the socket path and the [`HelloAck`] carry no
/// secret, and neither does [`LinkState`], whose one payload is the
/// daemon's own refusal sentence. Manual because [`RwLock`] and
/// [`JoinHandle`] derive into noise, and a derived impl would print the
/// inner [`Client`] where the link state is the useful fact.
impl fmt::Debug for ReconnectingClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReconnectingClient")
            .field("socket", &self.shared.socket)
            .field("dog_name", &self.shared.dog_name)
            .field("link", &self.link())
            .field("ack", &self.daemon())
            .finish_non_exhaustive()
    }
}

/// Everything the handle and its supervisor both reach: the address to
/// reconnect to, the budget to do it under, and the current generation.
struct Shared {
    socket: PathBuf,
    handshake_timeout: Duration,
    /// The name this client announces itself as a dog under, re-sent on
    /// every reconnect. See [`ReconnectingClient::connect_as_dog`].
    dog_name: Option<String>,
    state: RwLock<State>,
    /// What the supervisor last reported, and how a waiter hears it
    /// change. The only home of the link state: a second copy beside the
    /// generation would be one more thing to keep in step, and the two
    /// have no reader that needs them to move together.
    link: watch::Sender<LinkState>,
}

/// The generation of the connection in force right now.
struct State {
    client: Arc<Client>,
}

impl Shared {
    /// A read guard, treating a poisoned lock as ordinary data.
    ///
    /// Nothing inside a critical section here can panic: every one is a
    /// clone or an assignment of a plain struct, so poisoning cannot
    /// signal a torn value.
    fn read(&self) -> RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// A write guard. See [`Self::read`] for the poisoning argument.
    fn write(&self) -> RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// The current generation, cloned out so the guard is dropped before
    /// any caller awaits on it: no lock is ever held across an `await`.
    fn client(&self) -> Arc<Client> {
        Arc::clone(&self.read().client)
    }

    fn set_link(&self, link: LinkState) {
        self.link.send_replace(link);
    }

    /// Swaps in a freshly handshaken generation, dropping the dead one.
    ///
    /// The generation lands before the link is announced, so a waiter woken
    /// by [`LinkState::Connected`] finds the connection it was told about.
    fn install(&self, client: Client) {
        self.write().client = Arc::new(client);
        self.set_link(LinkState::Connected);
    }
}

impl ReconnectingClient {
    /// Connects to `socket`, performs the version handshake bounded by
    /// [`HANDSHAKE_TIMEOUT`], and starts the supervisor that will
    /// re-establish this connection whenever it dies.
    ///
    /// The FIRST connection is not supervised: a socket nobody is listening
    /// on is a caller's error, not a handover, so this reports it rather
    /// than retrying behind the caller's back.
    ///
    /// # Errors
    ///
    /// See [`Self::connect_with_timeout`].
    pub async fn connect(socket: &Path) -> Result<Self, ConnectError> {
        Self::connect_with_timeout(socket, HANDSHAKE_TIMEOUT).await
    }

    /// As [`Self::connect`], but with a caller-supplied handshake timeout,
    /// used for the first connection and for every reconnect after it.
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
        Self::connect_inner(socket, timeout, None).await
    }

    /// As [`Self::connect`], but announcing this client as the dog
    /// registered under `name`, the value the daemon put in
    /// `$SHEP_DOG_NAME` when it spawned this process.
    ///
    /// A dog should use this rather than [`Self::connect`]: the name makes
    /// a refused handshake actionable, since a refusal never reaches a
    /// request. Without it, the daemon still stops rather than spins on a
    /// refusal, but cannot say which dog went stale or restart it.
    ///
    /// # Errors
    ///
    /// See [`Self::connect_with_timeout`].
    pub async fn connect_as_dog(socket: &Path, name: &str) -> Result<Self, ConnectError> {
        Self::connect_as_dog_with_timeout(socket, HANDSHAKE_TIMEOUT, name).await
    }

    /// As [`Self::connect_as_dog`], but with a caller-supplied handshake
    /// timeout, used for the first connection and for every reconnect after
    /// it.
    ///
    /// # Errors
    ///
    /// See [`Self::connect_with_timeout`].
    pub async fn connect_as_dog_with_timeout(
        socket: &Path,
        timeout: Duration,
        name: &str,
    ) -> Result<Self, ConnectError> {
        Self::connect_inner(socket, timeout, Some(name)).await
    }

    async fn connect_inner(
        socket: &Path,
        timeout: Duration,
        dog_name: Option<&str>,
    ) -> Result<Self, ConnectError> {
        let client = Client::connect_as(socket, timeout, dog_name).await?;
        let shared = Arc::new(Shared {
            socket: socket.to_path_buf(),
            handshake_timeout: timeout,
            dog_name: dog_name.map(str::to_owned),
            link: watch::Sender::new(LinkState::Connected),
            state: RwLock::new(State {
                client: Arc::new(client),
            }),
        });
        let supervisor = tokio::spawn(supervise(Arc::clone(&shared)));
        Ok(Self { shared, supervisor })
    }

    /// The name this client announces itself as a dog under, or `None` for
    /// a caller that is not one.
    #[must_use]
    pub fn dog_name(&self) -> Option<&str> {
        self.shared.dog_name.as_deref()
    }

    /// The handshake acknowledgement of the daemon this client is talking
    /// to right now.
    ///
    /// Owned rather than borrowed, unlike [`Client::daemon`]: the ack
    /// belongs to a generation a reconnect can replace at any moment, so a
    /// reference would either pin a stale one or need a guard in the
    /// caller's hands. A cached ack would describe the predecessor, which
    /// a dog publishing `daemon_version` must not do.
    #[must_use]
    pub fn daemon(&self) -> HelloAck {
        self.shared.read().client.daemon().clone()
    }

    /// The path this client connects (and reconnects) through.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.shared.socket
    }

    /// What the supervisor is doing right now.
    #[must_use]
    pub fn link(&self) -> LinkState {
        self.shared.link.borrow().clone()
    }

    /// Sends `body` with [`DEFAULT_DEADLINE`](crate::DEFAULT_DEADLINE) on
    /// the current generation of the connection.
    ///
    /// # Errors
    ///
    /// See [`Self::request_with_deadline`].
    pub async fn request(&self, body: Request) -> Result<Response, RequestError> {
        self.request_with_deadline(body, None).await
    }

    /// Sends `body` with `deadline` on the current generation.
    ///
    /// Never retried: a request on the wire when the daemon was replaced
    /// fails, and so does one issued while reconnecting. Only the
    /// connection re-establishes; the caller decides whether resending is safe.
    ///
    /// # Errors
    ///
    /// - [`RequestError::Rpc`]: the daemon answered with a structured error.
    /// - [`RequestError::Timeout`]: no reply within the request's budget.
    /// - [`RequestError::Closed`]: the connection closed before a reply arrived, or already had.
    /// - [`RequestError::Wire`]: `body` failed to encode.
    pub async fn request_with_deadline(
        &self,
        body: Request,
        deadline: Option<Duration>,
    ) -> Result<Response, RequestError> {
        self.shared
            .client()
            .request_with_deadline(body, deadline)
            .await
    }

    /// Subscribes the current generation of the connection to `topics`.
    ///
    /// The returned stream belongs to one generation and is not re-armed
    /// across a reconnect: it ends when that connection dies, like
    /// [`Client::subscribe`]'s would. A consumer that wants events past a
    /// handover subscribes again; re-arming here would silently swallow
    /// the gap between the old connection dying and a new `Subscribe`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::request`].
    pub async fn subscribe(&self, topics: Vec<String>) -> Result<EventStream, RequestError> {
        self.shared.client().subscribe(topics).await
    }

    /// Waits for the supervisor to be on a connection again, for at most
    /// `budget`, and returns at once when it already is.
    ///
    /// What a caller needing a fresh [`EventStream`] after a handover waits
    /// on: a `Subscribe` issued against a dead generation fails at once
    /// with [`RequestError::Closed`], which says nothing about whether a
    /// successor is on its way.
    ///
    /// A returned `Ok` is where to try, never a promise that it will work.
    /// The supervisor parks on the current connection dying, so its report
    /// can still say [`LinkState::Connected`] for the moment between the
    /// socket going and the supervisor waking, and a live connection can
    /// die immediately afterwards anyway. A caller that needs a stream
    /// asks for one and comes back here while its budget lasts.
    ///
    /// # Errors
    ///
    /// - [`LinkLost::Refused`]: a successor refused on protocol-version
    ///   skew, so the supervisor has stopped and no later wait can succeed.
    /// - [`LinkLost::Budget`]: `budget` ran out with the supervisor still
    ///   dialling.
    pub async fn connected_within(&self, budget: Duration) -> Result<(), LinkLost> {
        let started = Instant::now();
        let mut link = self.shared.link.subscribe();
        loop {
            // Cloned out of the guard, which must not be held across an
            // await.
            match link.borrow_and_update().clone() {
                LinkState::Connected => return Ok(()),
                LinkState::Refused {
                    daemon_version,
                    message,
                } => {
                    return Err(LinkLost::Refused {
                        daemon_version,
                        message,
                    });
                }
                _ => {}
            }
            let left = budget.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(LinkLost::Budget {
                    waited: started.elapsed(),
                });
            }
            match tokio::time::timeout(left, link.changed()).await {
                Ok(Ok(())) => {}
                // Out of budget, or a sender that is gone so the state can
                // never move again. The second is unreachable while `&self`
                // holds that sender.
                Ok(Err(_)) | Err(_) => {
                    return Err(LinkLost::Budget {
                        waited: started.elapsed(),
                    });
                }
            }
        }
    }

    /// Waits until the link has been down for a whole `budget` without
    /// coming back, or has been refused.
    ///
    /// The clock runs only while the link is down, so a connection that
    /// drops and returns inside `budget` leaves the next one a full budget
    /// of its own. Never resolves while the link is up, which is what lets
    /// it sit in a `select!` arm beside the work a caller does when it is.
    ///
    /// A supervised dog is the caller this exists for: one whose shepherd
    /// is genuinely gone should exit rather than wait for a shepherd that
    /// is not coming, since a dog still running when an unrelated shepherd
    /// later binds that socket would attach itself to that one.
    pub async fn link_lost(&self, budget: Duration) -> LinkLost {
        let mut link = self.shared.link.subscribe();
        loop {
            // Parking here is the only await on the path a connected client
            // takes, so it is what keeps this future yielding as well as
            // what stops a live link spending the budget: `connected_within`
            // returns at once while the link is up, and the outer loop would
            // spin on it.
            loop {
                let up = matches!(*link.borrow_and_update(), LinkState::Connected);
                if !up {
                    break;
                }
                if link.changed().await.is_err() {
                    // The state can never move again, so the link can never
                    // be lost. Unreachable while `&self` holds the sender.
                    core::future::pending::<()>().await;
                }
            }
            match self.connected_within(budget).await {
                // Back inside the budget: this was a handover, not a loss.
                Ok(()) => {}
                Err(lost) => return lost,
            }
        }
    }
}

/// Stops the supervisor when the handle goes away.
///
/// Without this, a dropped `ReconnectingClient` whose daemon is gone leaves
/// a task reconnecting forever against an address nobody will answer,
/// holding the last generation's socket open with it. `abort` rather than a
/// cooperative signal because the supervisor spends its life parked on
/// either a connection's death or a connect attempt, neither of which would
/// notice a flag until it woke.
impl Drop for ReconnectingClient {
    fn drop(&mut self) {
        self.supervisor.abort();
    }
}

/// The supervisor loop: wait for the current generation to die, then
/// re-establish it, forever.
///
/// Ends only on a protocol refusal (see [`LinkState::Refused`]) or when the
/// handle is dropped and this task is aborted.
async fn supervise(shared: Arc<Shared>) {
    loop {
        // held only for the death await, dropped before reconnecting so the
        // dead socket goes away as soon as `install` replaces it
        let generation = shared.client();
        generation.closed().await;
        drop(generation);
        shared.set_link(LinkState::Reconnecting);

        let mut delay = RECONNECT_MIN_DELAY;
        loop {
            match Client::connect_as(
                &shared.socket,
                shared.handshake_timeout,
                shared.dog_name.as_deref(),
            )
            .await
            {
                Ok(fresh) => {
                    shared.install(fresh);
                    break;
                }
                Err(ConnectError::ProtocolMismatch {
                    daemon_version,
                    message,
                    ..
                }) => {
                    shared.set_link(LinkState::Refused {
                        daemon_version,
                        message,
                    });
                    return;
                }
                // Everything else is a daemon that is not ready yet: the
                // successor has not started accepting, or the socket is
                // momentarily unbound. Those resolve on their own, so the
                // only question is how often to ask.
                Err(_transient) => {
                    tokio::time::sleep(delay).await;
                    delay = next_delay(delay);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_core::protocol::{PROTOCOL_VERSION, RpcError, RpcErrorCode};

    use super::*;
    use crate::testing::{Handovers, Handshake, control_address, fake_daemon_across_handovers};

    /// Every bounded wait in this module uses one budget: generous against
    /// a loaded CI runner, small enough that a genuinely stuck test fails
    /// rather than hanging the suite.
    const BOUND: Duration = Duration::from_secs(5);

    /// The window a test watches for something that must not happen.
    ///
    /// Eight times [`RECONNECT_MIN_DELAY`], so a supervisor still looping
    /// would have made several further attempts inside it, the first after
    /// 50ms. Stated against the delay it has to outrun, not a round number.
    const NEGATIVE_WINDOW: Duration = Duration::from_millis(400);

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

    /// Waits until the fake has accepted `accepts` connections and the
    /// client has installed the newest of them, or fails inside [`BOUND`].
    ///
    /// Both halves are needed: the accept count alone rises before the
    /// handshake completes, and the link alone still reads `Connected` in
    /// the instant after a cut. The supervisor sets `Reconnecting` before
    /// it dials, so together they are unambiguous.
    ///
    /// Does not observe [`ReconnectingClient::daemon`]: some tests assert
    /// on the ack, and a helper that waited on it would make those fail
    /// for an unrelated reason.
    async fn await_reconnect(client: &ReconnectingClient, shepherds: &Handovers, accepts: u32) {
        let seen = tokio::time::timeout(BOUND, async {
            loop {
                if shepherds.accepted() >= accepts && client.link() == LinkState::Connected {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            seen.is_ok(),
            "no reconnect within {BOUND:?}: {} accepts, link {:?}",
            shepherds.accepted(),
            client.link()
        );
    }

    /// Subscribes, cuts the connection under the subscription, and drains
    /// the stream to its end.
    ///
    /// The sequence a dog actually meets: it learns its connection died by
    /// its own event stream ending, never by reading a link state. A test
    /// that cuts and asks straight away would be asking before the client
    /// itself has noticed.
    async fn subscribe_then_lose_it(client: &ReconnectingClient, shepherds: &Handovers) {
        let mut events = client
            .subscribe(vec!["process.*".to_owned()])
            .await
            .expect("the first subscription must be answered");
        shepherds.cut().await;
        let ended =
            tokio::time::timeout(BOUND, async { while events.next().await.is_some() {} }).await;
        assert!(ended.is_ok(), "the stream must end within {BOUND:?}");
    }

    /// Waits until `client`'s link reaches a refusal, or fails inside
    /// [`BOUND`].
    async fn await_refusal(client: &ReconnectingClient) -> LinkState {
        let seen = tokio::time::timeout(BOUND, async {
            loop {
                let link = client.link();
                if matches!(link, LinkState::Refused { .. }) {
                    return link;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        seen.unwrap_or_else(|_| panic!("the link never reached a refusal within {BOUND:?}"))
    }

    /// Reconnects `client` inside [`BOUND`], failing the test rather than
    /// hanging, and hands back the verdict.
    ///
    /// Most cases want exactly this and differ only in what they assert
    /// afterwards. The refusal and spent-budget cases spell it out instead,
    /// because they assert on the error this unwraps.
    async fn reconnect_ok(client: &mut Client) -> Reconnected {
        tokio::time::timeout(BOUND, client.reconnect())
            .await
            .expect("the reconnect must not hang")
            .expect("the successor accepted this handshake")
    }

    /// fails if a dog is left holding a dead socket after its daemon is
    /// replaced: only the listening socket crosses the exec, and an
    /// accepted one dies with the image.
    #[tokio::test]
    async fn a_dropped_connection_is_re_established_and_the_next_request_is_served() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Accept(ack_from(22)),
            ],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();

        let before = tokio::time::timeout(BOUND, client.request(Request::Ping))
            .await
            .expect("the first request must not hang");
        assert!(before.is_ok(), "before the handover: {before:?}");

        // The handover, exactly: the accepted connection dies under the
        // client while the listener stays bound.
        shepherds.cut().await;
        await_reconnect(&client, &shepherds, 2).await;

        let after = tokio::time::timeout(BOUND, client.request(Request::Ping))
            .await
            .expect("the request after a handover must not hang");
        assert!(
            after.is_ok(),
            "a request after the handover must be served, got {after:?}"
        );
    }

    /// fails if the ack keeps describing the predecessor after a
    /// reconnect, which would make a dog publish `daemon_version` for a
    /// daemon no longer running.
    #[tokio::test]
    async fn the_ack_follows_the_successor_rather_than_the_predecessor() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Accept(ack_from(22)),
            ],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();
        assert_eq!(client.daemon().pid, 11);
        assert_eq!(client.daemon().daemon_version, "0.0.11");

        shepherds.cut().await;
        await_reconnect(&client, &shepherds, 2).await;

        assert_eq!(
            client.daemon().daemon_version,
            "0.0.22",
            "the version must come from the daemon now answering"
        );
    }

    /// fails if a dog's name reaches the FIRST daemon and not its
    /// successor. The refusal is the one that matters and it is the second
    /// one here: a dog that named itself at boot and then reconnected
    /// anonymously would leave the successor unable to say which dog it
    /// just refused, which is precisely the case G8 exists for. A daemon's
    /// predecessor is not around to be asked.
    #[tokio::test]
    async fn a_dogs_name_rides_every_handshake_including_the_refused_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Refuse(RpcError {
                    code: RpcErrorCode::ProtocolMismatch,
                    message: "daemon speaks protocol 3, client sent 2".to_string(),
                    daemon_version: Some("0.2.0".to_string()),
                }),
            ],
        );
        let client = ReconnectingClient::connect_as_dog(&path, "metrics")
            .await
            .unwrap();
        assert_eq!(client.dog_name(), Some("metrics"));

        shepherds.cut().await;
        await_refusal(&client).await;

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

    /// fails if a client that is not a dog claims to be one. The name is
    /// what lets a daemon restart a dog on a refused handshake, so a
    /// `ReconnectingClient` built without one must stay anonymous rather
    /// than inventing a name from its environment or its path.
    #[tokio::test]
    async fn a_client_that_is_not_a_dog_names_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(&path, vec![Handshake::Accept(ack_from(11))]);
        let client = ReconnectingClient::connect(&path).await.unwrap();
        assert_eq!(client.dog_name(), None);

        shepherds.cut().await;
        await_reconnect(&client, &shepherds, 2).await;

        assert!(
            shepherds
                .hellos()
                .iter()
                .all(|hello| hello.dog_name.is_none()),
            "an unnamed client must stay unnamed across a reconnect: {:?}",
            shepherds.hellos()
        );
    }

    /// fails if a request that was in flight when the daemon was replaced
    /// is re-sent to the successor: a re-issued `Stop` could stop a sheep
    /// twice, and the client cannot tell a request the daemon never saw
    /// from one it already acted on.
    ///
    /// The proof is positional: the successor's first envelope must be the
    /// request issued after the reconnect, not the abandoned `Ping`.
    #[tokio::test]
    async fn an_in_flight_request_fails_and_is_never_re_sent_to_the_successor() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Accept(ack_from(22)),
            ],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();

        // the predecessor reads this envelope and dies without answering it
        shepherds.cut_on_next_request();
        let lost = tokio::time::timeout(BOUND, client.request(Request::Ping))
            .await
            .expect("an abandoned request must fail, not hang");
        assert_eq!(
            lost,
            Err(RequestError::Closed),
            "an in-flight request must fail when the daemon is replaced"
        );

        await_reconnect(&client, &shepherds, 2).await;
        let served = tokio::time::timeout(BOUND, client.request(Request::ListFlock))
            .await
            .expect("the request after a handover must not hang");
        assert!(served.is_ok(), "after the handover: {served:?}");

        let successor: Vec<Request> = shepherds
            .envelopes()
            .into_iter()
            .filter(|(generation, _)| *generation == 2)
            .map(|(_, envelope)| envelope.body)
            .collect();
        assert_eq!(
            successor,
            vec![Request::ListFlock],
            "the successor must see only the request issued after the reconnect"
        );
    }

    /// fails if a refused reconnect is retried. A successor that refuses on
    /// protocol skew has said something no retry can change; the design's
    /// G8 puts the fix on the daemon (restart that dog once, from disk) and
    /// forbids the client spinning against it in the meantime.
    #[tokio::test]
    async fn a_refused_reconnect_stops_rather_than_spinning() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Refuse(RpcError {
                    code: RpcErrorCode::ProtocolMismatch,
                    message: "daemon speaks protocol 3, client speaks 2".into(),
                    daemon_version: Some("0.9.9".into()),
                }),
            ],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();

        shepherds.cut().await;
        let link = await_refusal(&client).await;

        let LinkState::Refused {
            daemon_version,
            message,
        } = link
        else {
            unreachable!("await_refusal only returns a refusal");
        };
        assert_eq!(daemon_version.as_deref(), Some("0.9.9"));
        assert!(message.contains("protocol 3"), "{message}");
        assert_eq!(
            shepherds.accepted(),
            2,
            "one initial connection plus exactly one refused reconnect"
        );
        // And it must STAY at two. Reaching `Refused` only proves the
        // supervisor got there; a supervisor that recorded the refusal and
        // then went round again would satisfy every assertion above and
        // still be the spin G8 forbids, so the window is the assertion.
        let spun = tokio::time::timeout(NEGATIVE_WINDOW, async {
            while shepherds.accepted() < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            spun.is_err(),
            "the supervisor retried a refusal: {} accepts",
            shepherds.accepted()
        );
    }

    /// fails if a reconnect gives up on a daemon that is merely not ready
    /// yet. Across a real handover the listening socket stays bound while
    /// the successor replays its blob, so a connect that completes and a
    /// handshake that is not yet answered is the ordinary case, not a
    /// failure.
    #[tokio::test]
    async fn a_reconnect_retries_past_a_successor_that_is_not_accepting_yet() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Drop,
                Handshake::Drop,
                Handshake::Accept(ack_from(44)),
            ],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();

        shepherds.cut().await;
        await_reconnect(&client, &shepherds, 4).await;

        assert_eq!(
            shepherds.accepted(),
            4,
            "two unanswered handshakes must be retried past, not given up on"
        );
        let served = tokio::time::timeout(BOUND, client.request(Request::Ping))
            .await
            .expect("the request after the retries must not hang");
        assert!(served.is_ok(), "after the retries: {served:?}");
    }

    /// fails if dropping the handle leaves the supervisor reconnecting
    /// forever against an address nobody will answer.
    ///
    /// Asserts a negative: the bound is the assertion, and the accept
    /// count must stay flat within it.
    #[tokio::test]
    async fn dropping_the_handle_stops_the_supervisor() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![Handshake::Accept(ack_from(11)), Handshake::Drop],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();

        shepherds.cut().await;
        // Let the supervisor get as far as its first (dropped) reconnect,
        // so it is provably still running at the moment the handle goes.
        let reached = tokio::time::timeout(BOUND, async {
            while shepherds.accepted() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(reached.is_ok(), "the supervisor never retried at all");

        drop(client);

        let grew = tokio::time::timeout(NEGATIVE_WINDOW, async {
            while shepherds.accepted() < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            grew.is_err(),
            "the supervisor kept reconnecting after its handle was dropped: {} accepts",
            shepherds.accepted()
        );
    }

    /// fails if the ladder changes shape while its constants stay put.
    ///
    /// Pins the exact sequence both reconnect paths walk, without sleeping
    /// through any of it. The last two entries are the assertion that
    /// matters: doubling stops at the ceiling rather than running past it.
    #[test]
    fn the_reconnect_ladder_doubles_then_holds_at_the_ceiling() {
        let mut delay = RECONNECT_MIN_DELAY;
        let mut walked = vec![delay];
        for _ in 0..8 {
            delay = next_delay(delay);
            walked.push(delay);
        }
        assert_eq!(
            walked,
            [50, 100, 200, 400, 800, 1600, 3200, 5000, 5000].map(Duration::from_millis)
        );

        // A duration no ladder can reach today, so this pins the function
        // as total rather than the loop as correct.
        assert_eq!(next_delay(Duration::MAX), RECONNECT_MAX_DELAY);
    }

    /// fails if a reconnect that reached the very daemon it was talking to
    /// before reports a different one, which would have a caller throw away
    /// ids that are still valid.
    #[tokio::test]
    async fn a_reconnect_to_the_same_daemon_reports_same_daemon() {
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

        shepherds.cut().await;
        let verdict = reconnect_ok(&mut client).await;

        assert_eq!(verdict, Reconnected::SameDaemon);
    }

    /// fails if a reconnect that landed on a daemon started after the
    /// predecessor died reports the same one. That daemon mints its ids
    /// from a fresh space, so a caller acting on a held id would act on
    /// whatever now happens to wear the number.
    #[tokio::test]
    async fn a_reconnect_to_a_daemon_with_another_pid_reports_new_daemon() {
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

        shepherds.cut().await;
        let verdict = reconnect_ok(&mut client).await;

        assert_eq!(verdict, Reconnected::NewDaemon);
    }

    /// fails if a reconnect reports a verdict without actually swapping the
    /// connection under the handle, which would leave every later request
    /// going to a socket nobody is serving.
    ///
    /// The proof is positional: the successor's first envelope must be the
    /// request issued after the reconnect.
    #[tokio::test]
    async fn a_reconnected_client_sends_its_next_request_to_the_successor() {
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

        shepherds.cut().await;
        // the verdict is asserted by its own cases above; this one is about
        // what the reconnect did, not what it reported
        let _ = reconnect_ok(&mut client).await;

        let served = tokio::time::timeout(BOUND, client.request(Request::ListFlock))
            .await
            .expect("the request after a reconnect must not hang");
        assert!(served.is_ok(), "after the reconnect: {served:?}");

        let successor: Vec<Request> = shepherds
            .envelopes()
            .into_iter()
            .filter(|(generation, _)| *generation == 2)
            .map(|(_, envelope)| envelope.body)
            .collect();
        assert_eq!(successor, vec![Request::ListFlock]);
    }

    /// fails if the ack keeps describing the predecessor after a reconnect,
    /// so a caller reading `daemon_version` would report a build that is no
    /// longer running.
    #[tokio::test]
    async fn a_reconnect_updates_the_ack_to_the_daemon_now_answering() {
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
        assert_eq!(client.daemon().daemon_version, "0.0.11");

        shepherds.cut().await;
        // the verdict is asserted by its own cases above; this one is about
        // what the reconnect did, not what it reported
        let _ = reconnect_ok(&mut client).await;

        assert_eq!(client.daemon().pid, 22);
        assert_eq!(client.daemon().daemon_version, "0.0.22");
    }

    /// fails if a reconnect gives up on a daemon that is merely not ready
    /// yet. Across a handover the listening socket stays bound while the
    /// successor replays its blob, so a connect that completes and a
    /// handshake that is not yet answered is the ordinary case.
    #[tokio::test]
    async fn a_reconnect_retries_past_a_successor_still_coming_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Drop,
                Handshake::Drop,
                Handshake::Accept(ack_from(44)),
            ],
        );
        let mut client = Client::connect(&path).await.unwrap();

        shepherds.cut().await;
        let verdict = reconnect_ok(&mut client).await;

        assert_eq!(verdict, Reconnected::NewDaemon);
        assert_eq!(
            shepherds.accepted(),
            4,
            "two unanswered handshakes must be retried past, not given up on"
        );
    }

    /// fails if a refused reconnect is retried. A successor that refuses on
    /// protocol skew has said something no retry can change, so the caller
    /// gets the refusal rather than the budget being spent against it.
    #[tokio::test]
    async fn a_reconnect_returns_a_refusal_rather_than_retrying_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Refuse(RpcError {
                    code: RpcErrorCode::ProtocolMismatch,
                    message: "daemon speaks protocol 3, client speaks 2".into(),
                    daemon_version: Some("0.9.9".into()),
                }),
            ],
        );
        let mut client = Client::connect(&path).await.unwrap();

        shepherds.cut().await;
        let refused = tokio::time::timeout(BOUND, client.reconnect_within(BOUND))
            .await
            .expect("a refusal must be reported, not waited out")
            .expect_err("the successor refused this handshake");

        let ConnectError::ProtocolMismatch {
            daemon_version,
            message,
            ..
        } = refused
        else {
            panic!("a refusal must arrive as a mismatch, got {refused:?}");
        };
        assert_eq!(daemon_version.as_deref(), Some("0.9.9"));
        assert!(message.contains("protocol 3"), "{message}");
        assert_eq!(
            shepherds.accepted(),
            2,
            "one initial connection plus exactly one refused reconnect"
        );
    }

    /// fails if a reconnect against a daemon that is genuinely gone waits
    /// forever, or gives up on its first attempt without retrying at all.
    ///
    /// The budget is the forcing mechanism, and the elapsed time is the
    /// assertion at both ends: at least one delay means it retried, and
    /// returning at all means it stopped.
    #[tokio::test]
    async fn a_reconnect_gives_up_when_its_budget_is_spent() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(&path, vec![Handshake::Accept(ack_from(11))]);
        let mut client = Client::connect(&path).await.unwrap();

        // A daemon that is gone for good, listener and all, rather than one
        // replaced: nothing will answer this address again.
        drop(shepherds);

        let budget = Duration::from_millis(200);
        let started = tokio::time::Instant::now();
        let gave_up = tokio::time::timeout(BOUND, client.reconnect_within(budget))
            .await
            .expect("a spent budget must return, not hang");
        let elapsed = started.elapsed();

        assert!(
            gave_up.is_err(),
            "a daemon that is gone must not report a reconnect: {gave_up:?}"
        );
        assert!(
            elapsed >= RECONNECT_MIN_DELAY,
            "gave up without retrying once, after {elapsed:?}"
        );
    }

    /// fails if a dog's name reaches the daemon it booted against and not
    /// the one it reconnects to. The refusal that matters is the second
    /// one, and a daemon cannot ask its predecessor which dog was talking.
    #[tokio::test]
    async fn a_dogs_name_rides_a_reconnect() {
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

        shepherds.cut().await;
        // the verdict is asserted by its own cases above; this one is about
        // what the reconnect did, not what it reported
        let _ = reconnect_ok(&mut client).await;

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

    /// fails if a client that is not a dog claims to be one after a
    /// reconnect, which would have a daemon restart a dog nobody adopted.
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

        shepherds.cut().await;
        // the verdict is asserted by its own cases above; this one is about
        // what the reconnect did, not what it reported
        let _ = reconnect_ok(&mut client).await;

        assert!(
            shepherds
                .hellos()
                .iter()
                .all(|hello| hello.dog_name.is_none()),
            "an unnamed client must stay unnamed: {:?}",
            shepherds.hellos()
        );
    }

    /// fails if the verdict follows `daemon_version` rather than the pid.
    ///
    /// `shep daemon reload` onto a NEW build is still an execve, so it keeps
    /// the pid and carries the id counter across while the version changes.
    /// A version comparison would call that a new daemon and have every
    /// caller discard ids that are still perfectly good.
    ///
    /// Every other case here uses `ack_from`, which derives the version from
    /// the pid, so the two fields always move together and neither could
    /// tell these two rules apart.
    #[tokio::test]
    async fn an_upgrade_handover_keeps_its_pid_and_reports_same_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let upgraded = |version: &str| HelloAck {
            daemon_version: version.to_string(),
            protocol: PROTOCOL_VERSION,
            pid: 11,
            min_supported: None,
        };
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(upgraded("0.8.0")),
                Handshake::Accept(upgraded("0.9.0")),
            ],
        );
        let mut client = Client::connect(&path).await.unwrap();

        shepherds.cut().await;
        let verdict = reconnect_ok(&mut client).await;

        assert_eq!(verdict, Reconnected::SameDaemon);
        assert_eq!(
            client.daemon().daemon_version,
            "0.9.0",
            "the ack must still follow the build now answering"
        );
    }

    /// fails if the verdict follows `daemon_version` rather than the pid, in
    /// the other direction: a daemon stopped and started again on the SAME
    /// build mints its ids from a fresh space, so a version comparison would
    /// wave a caller through to reuse ids that now mean nothing.
    #[tokio::test]
    async fn a_restart_onto_the_same_build_reports_new_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let same_build = |pid: u32| HelloAck {
            daemon_version: "0.9.0".to_string(),
            protocol: PROTOCOL_VERSION,
            pid,
            min_supported: None,
        };
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(same_build(11)),
                Handshake::Accept(same_build(22)),
            ],
        );
        let mut client = Client::connect(&path).await.unwrap();

        shepherds.cut().await;
        let verdict = reconnect_ok(&mut client).await;

        assert_eq!(verdict, Reconnected::NewDaemon);
    }

    /// fails if a cancelled reconnect leaves the handle on neither
    /// connection. The old one is held until a new one has handshaken, so a
    /// caller whose future loses a `select!` race still has the connection
    /// it started with.
    ///
    /// The fake serves one connection at a time, so the second handshake
    /// cannot finish while the first is still open. That is what holds the
    /// reconnect open long enough for the 1ms budget to cancel it, and the
    /// elapsed timeout is the assertion that it really was still in flight.
    #[tokio::test]
    async fn a_cancelled_reconnect_leaves_the_handle_on_its_old_connection() {
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

        let cancelled =
            tokio::time::timeout(Duration::from_millis(1), client.reconnect_within(BOUND)).await;
        assert!(
            cancelled.is_err(),
            "the reconnect must still have been in flight when it was cancelled"
        );

        assert_eq!(
            client.daemon().pid,
            11,
            "a cancelled reconnect must not disturb the connection in hand"
        );
        let served = tokio::time::timeout(BOUND, client.request(Request::Ping))
            .await
            .expect("the old connection must still answer");
        assert!(served.is_ok(), "after the cancelled reconnect: {served:?}");
        assert_eq!(
            shepherds.accepted(),
            1,
            "the fake serves one connection at a time, so the cancelled dial \
             never got past the backlog"
        );
    }

    /// fails if a caller waiting out a handover is told the link came back
    /// when it did not, or is left waiting after it did.
    #[tokio::test]
    async fn a_wait_for_the_link_returns_once_the_successor_is_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Accept(ack_from(11)),
            ],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();

        subscribe_then_lose_it(&client, &shepherds).await;
        let waited = tokio::time::timeout(BOUND, client.connected_within(BOUND))
            .await
            .expect("the wait must not outlive the bound");

        assert_eq!(waited, Ok(()), "a successor did come up");
        assert_eq!(client.link(), LinkState::Connected);
    }

    /// fails if a spent budget starts refusing a link that is already up.
    ///
    /// Pins the trap rather than the convenience. A caller looping on this
    /// cannot use it as the loop's bound, because a live link answers `Ok`
    /// without ever consulting the budget, and a caller whose own work
    /// keeps failing against that live link would never leave the loop.
    /// `ClientEvents::resubscribe` in the CLI checks the budget itself for
    /// exactly this reason.
    #[tokio::test]
    async fn a_wait_on_a_live_link_answers_at_once_even_with_no_budget_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let _shepherds = fake_daemon_across_handovers(&path, vec![Handshake::Accept(ack_from(11))]);
        let client = ReconnectingClient::connect(&path).await.unwrap();

        let answered = tokio::time::timeout(BOUND, client.connected_within(Duration::ZERO))
            .await
            .expect("a live link must answer without waiting");

        assert_eq!(answered, Ok(()));
    }

    /// fails if a dog whose shepherd is gone for good waits forever, which
    /// is the lingering that lets it attach to an unrelated shepherd later.
    #[tokio::test]
    async fn a_wait_for_a_shepherd_that_never_comes_spends_its_budget_and_gives_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(&path, vec![Handshake::Accept(ack_from(11))]);
        let client = ReconnectingClient::connect(&path).await.unwrap();

        subscribe_then_lose_it(&client, &shepherds).await;
        // Gone for good, listener and all: nothing answers this address
        // again, which is what a stopped shepherd leaves behind.
        drop(shepherds);

        let budget = Duration::from_millis(200);
        let started = tokio::time::Instant::now();
        let lost = tokio::time::timeout(BOUND, client.connected_within(budget))
            .await
            .expect("a spent budget must return, not hang");
        let elapsed = started.elapsed();

        assert!(
            matches!(lost, Err(LinkLost::Budget { .. })),
            "expected a spent budget, got {lost:?}"
        );
        assert!(
            elapsed >= budget,
            "gave up after {elapsed:?}, short of the {budget:?} it was given"
        );
    }

    /// fails if a bark dog that cannot speak the protocol waits out its
    /// whole budget before exiting: the daemon that refused is the party
    /// that can fix it, so there is nothing to wait for.
    #[tokio::test]
    async fn a_wait_ends_on_a_refusal_rather_than_serving_out_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Refuse(RpcError {
                    code: RpcErrorCode::ProtocolMismatch,
                    message: "daemon speaks protocol 3, client speaks 2".into(),
                    daemon_version: Some("0.9.9".into()),
                }),
            ],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();

        subscribe_then_lose_it(&client, &shepherds).await;
        // An hour, so serving the budget out could never look like passing.
        let lost = tokio::time::timeout(BOUND, client.connected_within(Duration::from_secs(3600)))
            .await
            .expect("a refusal must end the wait long before its budget");

        let Err(LinkLost::Refused {
            daemon_version,
            message,
        }) = lost
        else {
            panic!("expected a refusal, got {lost:?}");
        };
        assert_eq!(daemon_version.as_deref(), Some("0.9.9"));
        assert!(message.contains("protocol 3"), "{message}");
    }

    /// fails if a dog watching for a lost shepherd fires while its shepherd
    /// is right there, which would exit every dog on a healthy flock.
    #[tokio::test]
    async fn a_watch_for_a_lost_shepherd_stays_quiet_while_the_link_is_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let _shepherds = fake_daemon_across_handovers(&path, vec![Handshake::Accept(ack_from(11))]);
        let client = ReconnectingClient::connect(&path).await.unwrap();

        let fired =
            tokio::time::timeout(NEGATIVE_WINDOW, client.link_lost(Duration::from_millis(1))).await;

        assert!(
            fired.is_err(),
            "a connected client reported its shepherd lost: {fired:?}"
        );
    }

    /// fails if a handover looks like a lost shepherd, which is the whole
    /// distinction: the shepherd execs a successor on purpose, and every
    /// dog is meant to cross that without exiting.
    #[tokio::test]
    async fn a_watch_for_a_lost_shepherd_rides_out_a_handover() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Accept(ack_from(11)),
            ],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();

        shepherds.cut().await;
        // Far longer than the reconnect ladder's first rung, so a successor
        // that is coming has room to arrive.
        let fired = tokio::time::timeout(NEGATIVE_WINDOW, client.link_lost(BOUND)).await;

        assert!(
            fired.is_err(),
            "a handover was reported as a lost shepherd: {fired:?}"
        );
        assert_eq!(client.link(), LinkState::Connected);
    }

    /// fails if a dog a shepherd refuses sits out its whole budget before
    /// noticing. `link_lost` reaches a refusal only by delegating to
    /// `connected_within`, so the early exit is one call away from being
    /// lost in a refactor, and nothing else here would catch it.
    #[tokio::test]
    async fn a_watch_for_a_lost_shepherd_ends_on_a_refusal_rather_than_a_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &path,
            vec![
                Handshake::Accept(ack_from(11)),
                Handshake::Refuse(RpcError {
                    code: RpcErrorCode::ProtocolMismatch,
                    message: "daemon speaks protocol 3, client speaks 2".into(),
                    daemon_version: Some("0.9.9".into()),
                }),
            ],
        );
        let client = ReconnectingClient::connect(&path).await.unwrap();

        subscribe_then_lose_it(&client, &shepherds).await;
        // An hour, so serving the budget out could never look like passing.
        let lost = tokio::time::timeout(BOUND, client.link_lost(Duration::from_secs(3600)))
            .await
            .expect("a refusal must end the watch long before its budget");

        let LinkLost::Refused {
            daemon_version,
            message,
        } = lost
        else {
            panic!("expected a refusal, got {lost:?}");
        };
        assert_eq!(daemon_version.as_deref(), Some("0.9.9"));
        assert!(message.contains("protocol 3"), "{message}");
    }

    /// fails if a dog whose shepherd stopped keeps waiting: the metrics dog
    /// has no stream to end, so this watch is the only thing that tells it.
    #[tokio::test]
    async fn a_watch_for_a_lost_shepherd_fires_once_the_budget_is_spent() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(&path, vec![Handshake::Accept(ack_from(11))]);
        let client = ReconnectingClient::connect(&path).await.unwrap();

        drop(shepherds);

        let budget = Duration::from_millis(200);
        let lost = tokio::time::timeout(BOUND, client.link_lost(budget))
            .await
            .expect("a spent budget must fire, not hang");

        assert!(
            matches!(lost, LinkLost::Budget { .. }),
            "expected a spent budget, got {lost:?}"
        );
    }

    /// fails if a refusal reaches an operator as a timeout, which would
    /// send them looking for a shepherd that is running and answering.
    #[test]
    fn a_lost_link_says_which_of_the_two_it_was() {
        let budget = LinkLost::Budget {
            waited: Duration::from_secs(5),
        };
        assert_eq!(budget.to_string(), "no shepherd answered within 5s");

        let refused = LinkLost::Refused {
            daemon_version: Some("0.9.9".into()),
            message: "daemon speaks protocol 3, client speaks 2".into(),
        };
        assert_eq!(
            refused.to_string(),
            "the shepherd refused this connection: daemon speaks protocol 3, client speaks 2"
        );
    }

    /// fails if `Reconnected` starts printing anything but its verdict. It
    /// carries no payload, and a caller logging one must never begin
    /// emitting a daemon's own details alongside it.
    #[test]
    fn reconnected_debug_is_the_verdict_and_nothing_else() {
        assert_eq!(format!("{:?}", Reconnected::SameDaemon), "SameDaemon");
        assert_eq!(format!("{:?}", Reconnected::NewDaemon), "NewDaemon");
    }

    /// fails if `Client`'s `Debug` starts printing its command channel, or
    /// stops naming the dog it announces itself as. A derived impl would do
    /// both, and the channel says nothing a reader can act on.
    #[tokio::test]
    async fn client_debug_names_the_socket_the_ack_and_the_dog() {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let ack = ack_from(11);
        let _shepherds = fake_daemon_across_handovers(&path, vec![Handshake::Accept(ack.clone())]);
        let client = Client::connect_as(&path, HANDSHAKE_TIMEOUT, Some("metrics"))
            .await
            .unwrap();

        assert_eq!(
            format!("{client:?}"),
            format!("Client {{ socket: {path:?}, ack: {ack:?}, dog_name: Some(\"metrics\"), .. }}")
        );
    }
}
