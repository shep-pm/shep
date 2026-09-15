use super::*;
use std::time::Duration;
// tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
// budget below is measured against a `tokio::time::sleep` that does too.
use crate::client::Client;
use crate::connection::{ConnectError, HANDSHAKE_TIMEOUT};
use tokio::time::Instant;

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
    /// [`RECONNECT_MAX_DELAY`]; a refusal is not. An [`EventStream`](crate::events::EventStream) taken
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

#[cfg(test)]
mod tests {

    use std::time::Duration;

    // tokio's Instant, not std's: it moves with `tokio::time::pause`, and the
    // budget below is measured against a `tokio::time::sleep` that does too.
    use crate::client::Client;
    use crate::connection::{ConnectError, HANDSHAKE_TIMEOUT};
    use shep_core::protocol::{HelloAck, Request};

    use super::super::testing::*;
    use super::*;
    use crate::testing::{Handshake, control_address, fake_daemon_across_handovers};
    use shep_core::protocol::{PROTOCOL_VERSION, RpcError, RpcErrorCode};

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
}
