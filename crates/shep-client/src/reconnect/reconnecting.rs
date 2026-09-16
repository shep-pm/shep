use super::*;
use core::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
// tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
// budget below is measured against a `tokio::time::sleep` that does too.
use crate::client::{Client, RequestError};
use crate::connection::{ConnectError, HANDSHAKE_TIMEOUT};
use crate::events::EventStream;
use shep_core::protocol::{HelloAck, Request, Response};
use tokio::time::Instant;

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
pub(super) struct Shared {
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
pub(super) struct State {
    client: Arc<Client>,
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
pub(super) async fn supervise(shared: Arc<Shared>) {
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
    /// One case does not get that fresh budget, and it is the watch's
    /// nature rather than a gap to close. A [`watch`] receiver keeps only
    /// the latest value, so a successor that connects and dies again before
    /// this task next runs is never observed as `Connected`, and its
    /// outage and the one before it are spent as a single budget. The
    /// answer is the same either way: a shepherd flapping that fast is a
    /// shepherd this dog cannot work with, and exiting is what it should
    /// do.
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

#[cfg(test)]
mod tests {

    use std::time::Duration;

    // tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
    // budget below is measured against a `tokio::time::sleep` that does too.
    use crate::client::{Client, RequestError};
    use crate::connection::HANDSHAKE_TIMEOUT;
    use shep_core::protocol::Request;

    use super::super::testing::*;
    use super::*;
    use crate::testing::{Handshake, control_address, fake_daemon_across_handovers};
    use shep_core::protocol::{RpcError, RpcErrorCode};

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
