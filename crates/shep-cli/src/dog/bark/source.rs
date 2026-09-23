//! Bark's shepherd seams implemented over a real [`ReconnectingClient`].

use std::sync::Arc;

use shep_client::{EventStream, LinkLost, RECONNECT_MIN_DELAY, ReconnectingClient, RequestError};
use shep_core::protocol::{BusEvent, ProcessInfo, Request, Response, RpcError, RpcErrorCode};

use super::{ConfigSource, EventSource, FlockSource, Resubscribe};
use crate::dog::SHEPHERD_RETURN_BUDGET;

/// Bark's subscription, and what arming a fresh one after a handover
/// takes: the client to ask, and the topics the first one named.
///
/// A subscription belongs to one connection generation, so the stream ends
/// every time the shepherd execs a successor. Carrying the topics here is
/// what keeps the second subscription asking for the same thing as the
/// first.
pub(super) struct ClientEvents {
    /// Reached through the same [`Arc`] the flock and config sources use,
    /// so every role speaks to one client rather than to clients that
    /// would reconnect independently.
    pub(super) shepherd: Arc<ClientShepherd>,
    pub(super) topics: Vec<String>,
    pub(super) stream: EventStream,
}

/// `self.stream.next()` resolves to [`EventStream`]'s own inherent method,
/// not a recursive call into this trait impl.
impl EventSource for ClientEvents {
    async fn next(&mut self) -> Option<Result<BusEvent, u64>> {
        self.stream
            .next()
            .await
            .map(|item| item.map_err(|lagged| lagged.count))
    }

    async fn resubscribe(&mut self) -> Result<(), Resubscribe> {
        let started = tokio::time::Instant::now();
        // Every wait below is taken from this rather than from a value
        // computed earlier in the pass: `connected_within` and `subscribe`
        // each consume time, so a `left` read before them is spent by the
        // time the next one starts.
        let remaining = |elapsed| SHEPHERD_RETURN_BUDGET.saturating_sub(elapsed);
        loop {
            let left = remaining(started.elapsed());
            // Checked here rather than left to `connected_within`, which
            // returns `Ok` on a live link without consulting the budget. A
            // shepherd that answers the handshake and then fails every
            // `Subscribe` would otherwise keep this loop going for as long
            // as it stayed up.
            if left.is_zero() {
                return Err(Resubscribe::Lost(LinkLost::Budget {
                    waited: started.elapsed(),
                }));
            }
            self.shepherd
                .client
                .connected_within(left)
                .await
                .map_err(Resubscribe::Lost)?;

            // Bounded by what is left rather than by the request's own
            // deadline. `Client::subscribe` carries `DEFAULT_DEADLINE` plus
            // `DEADLINE_GRACE`, seven seconds, which on its own outlasts
            // the budget this whole function is meant to keep. Dropping the
            // future is safe: the client actor expects a reply receiver to
            // go away.
            let left = remaining(started.elapsed());
            if left.is_zero() {
                return Err(Resubscribe::Lost(LinkLost::Budget {
                    waited: started.elapsed(),
                }));
            }
            let asked =
                tokio::time::timeout(left, self.shepherd.client.subscribe(self.topics.clone()));
            match asked.await {
                Ok(Ok(stream)) => {
                    self.stream = stream;
                    return Ok(());
                }
                // The generation it was issued on had already gone. The
                // supervisor is about to say so, and the budget decides
                // whether to keep asking.
                Ok(Err(RequestError::Closed)) => {}
                // The request reached a shepherd and did not succeed.
                // Waiting cannot change that, and the error already decides
                // the exit code the opening `Subscribe` would have used.
                Ok(Err(other)) => return Err(Resubscribe::Request(other)),
                Err(_elapsed) => {
                    return Err(Resubscribe::Lost(LinkLost::Budget {
                        waited: started.elapsed(),
                    }));
                }
            }
            // The supervisor reports a connection's death a moment after
            // the socket does, so a bare retry would spin against a link
            // still reading as connected. One rung of the supervisor's own
            // ladder outlasts that and is short against the handover.
            tokio::time::sleep(RECONNECT_MIN_DELAY.min(remaining(started.elapsed()))).await;
        }
    }
}

/// The error for a reply that is not the variant the request names.
///
/// Never returned by a daemon on the same protocol version; kept
/// reportable rather than `unreachable!()`. One function rather than the
/// literal at each impl, so the two reports keep saying the same thing
/// in the same shape.
fn unexpected_reply(request: &str, expected: &str) -> RequestError {
    RequestError::Rpc(RpcError {
        code: RpcErrorCode::Internal,
        message: format!("the shepherd answered {request} with something other than {expected}"),
        daemon_version: None,
    })
}

/// Wraps [`ReconnectingClient`] as both [`FlockSource`] and
/// [`ConfigSource`]. [`ReconnectingClient`] is not `Clone`, so the
/// two roles reach it through one [`Arc`] rather than through two clients
/// that would reconnect independently.
pub(super) struct ClientShepherd {
    pub(super) client: ReconnectingClient,
    /// The dog whose section [`ConfigSource`] re-asks for.
    pub(super) dog: String,
}

impl FlockSource for ClientShepherd {
    async fn flock(&self) -> Result<Vec<ProcessInfo>, RequestError> {
        match self.client.request(Request::ListFlock).await? {
            Response::Flock(flock) => Ok(flock),
            _ => Err(unexpected_reply("ListFlock", "Response::Flock")),
        }
    }
}

impl ConfigSource for ClientShepherd {
    async fn section(&self) -> Result<String, RequestError> {
        let response = self
            .client
            .request(Request::DogConfig {
                name: self.dog.clone(),
            })
            .await?;
        match response {
            Response::DogSection { toml } => Ok(toml.as_str().to_string()),
            _ => Err(unexpected_reply("DogConfig", "Response::DogSection")),
        }
    }
}

/// Forwarding impls, so nothing in `bark` has to know the production
/// shepherd is shared through an [`Arc`].
impl FlockSource for Arc<ClientShepherd> {
    async fn flock(&self) -> Result<Vec<ProcessInfo>, RequestError> {
        FlockSource::flock(&**self).await
    }
}

impl ConfigSource for Arc<ClientShepherd> {
    async fn section(&self) -> Result<String, RequestError> {
        ConfigSource::section(&**self).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_client::testing::{Handshake, fake_daemon_across_handovers, sample_ack};

    use super::*;

    /// fails if the production adapter cannot arm a second subscription
    /// after a handover.
    ///
    /// `run_loop` is driven by a fake in bark's own tests, so this is
    /// the only thing that exercises `ClientEvents` itself: the client and
    /// topics held beside the stream, the wait for the link, and the
    /// re-subscribe. Ten real reloads showed it working, which is evidence
    /// rather than a guard.
    #[tokio::test]
    async fn the_bark_adapter_arms_a_second_subscription_after_a_handover() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &socket,
            vec![
                Handshake::Accept(sample_ack()),
                Handshake::Accept(sample_ack()),
            ],
        );
        let client = ReconnectingClient::connect_as_dog(&socket, "bark")
            .await
            .unwrap();
        let topics = vec!["process.*".to_owned(), "config.dog.bark".to_owned()];
        let stream = client.subscribe(topics.clone()).await.unwrap();
        let shepherd = Arc::new(ClientShepherd {
            client,
            dog: "bark".to_owned(),
        });
        let mut events = ClientEvents {
            shepherd: Arc::clone(&shepherd),
            topics,
            stream,
        };

        // The handover, exactly: the accepted connection dies while the
        // listener stays bound.
        shepherds.cut().await;
        let armed = tokio::time::timeout(Duration::from_secs(10), events.resubscribe())
            .await
            .expect("a re-subscribe must not outlive its own budget");
        assert!(
            armed.is_ok(),
            "a successor was there to subscribe to: {armed:?}"
        );

        // `Ok` alone does not prove the adapter kept what it was handed. An
        // adapter that answered `Ok` and left the dead stream in place
        // satisfies every other assertion here, and a dead stream ends at
        // once where a live one has nothing to say yet.
        let ended = tokio::time::timeout(Duration::from_millis(250), events.next()).await;
        assert!(
            ended.is_err(),
            "the armed stream ended straight away, so it is the dead one: {ended:?}"
        );

        assert_eq!(
            shepherds.accepted(),
            2,
            "one connection before the handover and one after"
        );
        let asked: Vec<_> = shepherds
            .hellos()
            .iter()
            .map(|hello| hello.dog_name.clone())
            .collect();
        assert_eq!(
            asked,
            vec![Some("bark".to_owned()), Some("bark".to_owned())],
            "the second handshake must name the dog too, or a refusal is unactionable"
        );
    }

    /// fails if the adapter treats a shepherd that answers and refuses as
    /// one that never answered.
    ///
    /// The bark loop's own test drives a fake that hands it a
    /// `Resubscribe::Request` ready-made. This is the other half: the
    /// adapter producing one from a real shepherd that accepts the
    /// handshake and then rejects the `Subscribe`. Conflating it with
    /// `Closed` would retry until the budget was gone and then report an
    /// unreachable shepherd for one that answered.
    #[tokio::test]
    async fn the_bark_adapter_keeps_a_refusal_rather_than_retrying_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let shepherds = fake_daemon_across_handovers(
            &socket,
            vec![
                Handshake::Accept(sample_ack()),
                Handshake::Accept(sample_ack()),
            ],
        );
        let client = ReconnectingClient::connect_as_dog(&socket, "bark")
            .await
            .unwrap();
        let topics = vec!["process.*".to_owned()];
        let stream = client.subscribe(topics.clone()).await.unwrap();
        let shepherd = Arc::new(ClientShepherd {
            client,
            dog: "bark".to_owned(),
        });
        let mut events = ClientEvents {
            shepherd: Arc::clone(&shepherd),
            topics,
            stream,
        };

        // The successor accepts the handshake and refuses the one
        // subscription that follows it.
        shepherds.refuse_next_subscribe(RpcError {
            code: RpcErrorCode::Unsupported,
            message: "this shepherd does not serve that topic".into(),
            daemon_version: None,
        });
        shepherds.cut().await;

        let started = tokio::time::Instant::now();
        let refused = tokio::time::timeout(SHEPHERD_RETURN_BUDGET * 2, events.resubscribe())
            .await
            .expect("a refusal must end the wait, not hang it");
        let waited = started.elapsed();

        let Err(Resubscribe::Request(err)) = refused else {
            panic!("expected a kept refusal, got {refused:?}");
        };
        assert!(
            matches!(&err, RequestError::Rpc(rpc) if rpc.code == RpcErrorCode::Unsupported),
            "the shepherd's own error must survive: {err:?}"
        );
        assert!(
            waited < SHEPHERD_RETURN_BUDGET,
            "spent {waited:?} of the {SHEPHERD_RETURN_BUDGET:?} budget, so it retried a \
             refusal instead of keeping it"
        );
    }

    /// Tests that wait out a real [`SHEPHERD_RETURN_BUDGET`]. Five seconds
    /// of elapsed time is the point, so a paused clock would test nothing.
    mod slow {
        use super::*;

        /// fails if the bark adapter waits for a shepherd that is never
        /// coming back, or gives up before the budget it was given.
        ///
        /// The success path has its own test above. This is the other half:
        /// the `?` that carries a spent budget out of `resubscribe` and
        /// ends the dog, which is the whole point of the wait being bounded.
        #[tokio::test]
        async fn the_bark_adapter_gives_up_once_its_budget_is_spent() {
            let dir = tempfile::tempdir().unwrap();
            let socket = shep_client::testing::control_address(dir.path());
            let shepherds =
                fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
            let client = ReconnectingClient::connect_as_dog(&socket, "bark")
                .await
                .unwrap();
            let topics = vec!["process.*".to_owned()];
            let stream = client.subscribe(topics.clone()).await.unwrap();
            let shepherd = Arc::new(ClientShepherd {
                client,
                dog: "bark".to_owned(),
            });
            let mut events = ClientEvents {
                shepherd: Arc::clone(&shepherd),
                topics,
                stream,
            };

            // Gone for good, listener and all, which is what a stopped
            // shepherd leaves behind. A handover leaves the listener bound.
            drop(shepherds);
            let started = tokio::time::Instant::now();

            let gave_up = tokio::time::timeout(SHEPHERD_RETURN_BUDGET * 3, events.resubscribe())
                .await
                .expect("a spent budget must end the wait, not hang it");
            let waited = started.elapsed();

            assert!(
                matches!(gave_up, Err(Resubscribe::Lost(LinkLost::Budget { .. }))),
                "expected a spent budget, got {gave_up:?}"
            );
            assert!(
                waited >= SHEPHERD_RETURN_BUDGET,
                "gave up after {waited:?}, inside the {SHEPHERD_RETURN_BUDGET:?} a handover \
                 is allowed to take, which is the restart-per-reload this rule exists to avoid"
            );
        }
    }
}
