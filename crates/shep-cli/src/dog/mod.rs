//! `shep dog <name>`: the hidden re-exec target a built-in dog runs as, and
//! [`DogRuntime`], the connection and configuration every dog needs.
//!
//! A dog inherits `$SHEP_HOME` and nothing else: no `[dog.<name>]` value
//! rides in the environment, since that is readable from the process
//! table and captured into crash dumps. [`DogRuntime::start`] instead
//! connects to the socket and asks for the section over
//! `Request::DogConfig`.
//!
//! [`run_dog`] validates the name against [`BUILT_IN_DOGS`], connects, and
//! dispatches: `"metrics"` to [`metrics::run`], `"bark"` to [`run_bark`].

pub mod bark;
pub mod metrics;
mod runtime;

pub use runtime::DogRuntime;
use runtime::exit_code_for;

use std::sync::Arc;
use std::time::Duration;

use shep_client::{EventStream, LinkLost, RECONNECT_MIN_DELAY, ReconnectingClient, RequestError};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{BusEvent, ProcessInfo, Request, Response, RpcError, RpcErrorCode};

use crate::exit::ExitCode;

/// The dog names this binary can run built-in.
///
/// `enabled_dogs` accepts any name at all, an adopted dog's own choice, but
/// a re-exec through `shep dog <name>` only ever reaches one of these two.
/// [`run_dog`] refuses anything else before touching the socket.
pub(crate) const BUILT_IN_DOGS: [&str; 2] = ["metrics", "bark"];

/// How long a dog waits for a shepherd to answer again before it gives up
/// and exits.
///
/// A shepherd that execs a successor has not gone away, and every dog is
/// meant to cross that without restarting. A shepherd that stopped has
/// gone away, and a dog that waits for it indefinitely is still running
/// when an unrelated shepherd binds that socket later, at which point it
/// attaches itself to that one beside that shepherd's own dog of the same
/// kind and doubles its alerts quietly.
///
/// Measured on this machine over ten `shep daemon reload` runs against a
/// three-sheep flock: the control socket turned away a full connect,
/// handshake and request for 38ms at the shortest, 254ms at the longest,
/// 80ms on average. A larger flock and a busier host both push that up.
///
/// [`DOG_SILENCE_BUDGET`](shep_daemon::dogs::DOG_SILENCE_BUDGET) is the
/// number to reuse rather than a second one to invent: it is how long the
/// shepherd lets a dog go quiet before acting on it, so a dog that waits
/// exactly that long cannot outlive the budget it is judged by. It is also
/// around twenty times the longest handover measured, which leaves room
/// for a far slower one. Five seconds of waiting is not the lingering this
/// guards against; that one is measured in however long it takes another
/// shepherd to come along.
const SHEPHERD_RETURN_BUDGET: Duration = shep_daemon::dogs::DOG_SILENCE_BUDGET;

/// The exit code a dog reports when it gave up on its shepherd.
///
/// Not `Success`, because a dog that stopped because nothing answered has
/// not finished its work, and an operator told a running shepherd was
/// unreachable goes looking for the wrong thing. The wildcard is what
/// [`LinkLost`]'s `non_exhaustive` asks for: until something says
/// otherwise, a variant added later is one more way of not reaching a
/// shepherd.
fn exit_for(lost: &LinkLost) -> ExitCode {
    match lost {
        LinkLost::Refused { .. } => ExitCode::ProtocolMismatch,
        _ => ExitCode::DaemonUnreachable,
    }
}

/// The schema a built-in dog would print for the schema flag, without
/// spawning anything: a built-in dog is this binary, so the answer is one
/// call away rather than a subprocess and a timeout away.
///
/// [`None`] for a name that is not a built-in, which is how a caller tells
/// an adopted dog (spawn its recorded path and ask) from a built-in
/// (this).
pub(crate) fn builtin_schema(name: &str) -> Option<serde_json::Value> {
    use shep_client::dogs::config_schema;

    let schema = match name {
        "metrics" => config_schema::<metrics::MetricsConfig>(),
        "bark" => config_schema::<bark::BarkConfig>(),
        _ => return None,
    };
    serde_json::to_value(schema).ok()
}

/// Runs the named dog until it is signalled. `main`'s `Commands::Dog` arm.
///
/// An unknown name is refused before the socket is touched
/// ([`ExitCode::Usage`]), naming the two built-ins in the refusal.
///
/// A dog's own diagnostics go to stderr, plain text: the daemon's log pump
/// captures it into `$SHEP_HOME/logs/<name>-0-err.log` like any sheep's,
/// read with `shep bleats <name>`.
pub async fn run_dog(name: &str, paths: ShepPaths) -> ExitCode {
    if !BUILT_IN_DOGS.contains(&name) {
        eprintln!("shep dog: unknown dog {name:?}; the built-in dogs are \"metrics\" and \"bark\"");
        return ExitCode::Usage;
    }
    let runtime = match DogRuntime::start(name, paths).await {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("shep dog {name}: {err}");
            return exit_code_for(&err);
        }
    };
    match name {
        "metrics" => metrics::run(runtime).await,
        "bark" => run_bark(runtime).await,
        _ => unreachable!("checked against BUILT_IN_DOGS above"),
    }
}

/// Runs the bark dog until it is signalled.
///
/// Parses `[dog.bark]`, builds [`bark::rules::Rules`] (or
/// [`bark::rules::Rules::default_rules`] when the operator configured
/// none), subscribes to the shepherd's bus on `process.*`, and hands both
/// to [`bark::run_loop`] alongside a [`ClientShepherd`] wrapping this same
/// connection.
///
/// A refused config or a rule set `Rules::new` rejects are both
/// [`ExitCode::InvalidConfig`].
async fn run_bark(runtime: DogRuntime) -> ExitCode {
    let config = match runtime.config::<bark::BarkConfig>() {
        Ok(config) => config,
        Err(_err) => {
            // The fact, not the value: a `[bark]` section can carry a
            // webhook URL with a bearer token in its path.
            eprintln!("shep dog bark: [bark] in dogs.toml does not parse; see `shep dogs`");
            return ExitCode::InvalidConfig;
        }
    };
    let rules = match bark::rules_for(&config) {
        Ok(rules) => rules,
        Err(err) => {
            eprintln!("shep dog bark: {err}");
            return ExitCode::InvalidConfig;
        }
    };
    // Subscribes to this dog's own `config.dog.<name>` topic, not
    // `config.*`, which would hand it every other dog's prompts too. `dog`
    // is reused below for `ClientShepherd`'s re-read request, so the two
    // cannot drift apart.
    let dog = runtime.name.clone();
    // Named once, because `ClientEvents` asks for the same list again on
    // every handover and a second literal could drift from this one.
    let topics = vec!["process.*".to_owned(), format!("config.dog.{dog}")];
    let stream = match runtime.client.subscribe(topics.clone()).await {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("shep dog bark: could not subscribe to the shepherd's bus: {err}");
            return ExitCode::from(&err);
        }
    };
    let barks_path = runtime.paths.barks.clone();
    let shepherd = Arc::new(ClientShepherd {
        client: runtime.client,
        dog,
    });
    let events = ClientEvents {
        shepherd: Arc::clone(&shepherd),
        topics,
        stream,
    };
    bark::run_loop(
        events,
        Arc::clone(&shepherd),
        rules,
        &config,
        &barks_path,
        shepherd,
    )
    .await
}

/// Bark's subscription, and what arming a fresh one after a handover
/// takes: the client to ask, and the topics the first one named.
///
/// A subscription belongs to one connection generation, so the stream ends
/// every time the shepherd execs a successor. Carrying the topics here is
/// what keeps the second subscription asking for the same thing as the
/// first.
struct ClientEvents {
    /// Reached through the same [`Arc`] the flock and config sources use,
    /// so every role speaks to one client rather than to clients that
    /// would reconnect independently.
    shepherd: Arc<ClientShepherd>,
    topics: Vec<String>,
    stream: EventStream,
}

/// `self.stream.next()` resolves to [`EventStream`]'s own inherent method,
/// not a recursive call into this trait impl.
impl bark::EventSource for ClientEvents {
    async fn next(&mut self) -> Option<Result<BusEvent, u64>> {
        self.stream
            .next()
            .await
            .map(|item| item.map_err(|lagged| lagged.count))
    }

    async fn resubscribe(&mut self) -> Result<(), bark::Resubscribe> {
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
                return Err(bark::Resubscribe::Lost(LinkLost::Budget {
                    waited: started.elapsed(),
                }));
            }
            self.shepherd
                .client
                .connected_within(left)
                .await
                .map_err(bark::Resubscribe::Lost)?;

            // Bounded by what is left rather than by the request's own
            // deadline. `Client::subscribe` carries `DEFAULT_DEADLINE` plus
            // `DEADLINE_GRACE`, seven seconds, which on its own outlasts
            // the budget this whole function is meant to keep. Dropping the
            // future is safe: the client actor expects a reply receiver to
            // go away.
            let left = remaining(started.elapsed());
            if left.is_zero() {
                return Err(bark::Resubscribe::Lost(LinkLost::Budget {
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
                Ok(Err(other)) => return Err(bark::Resubscribe::Request(other)),
                Err(_elapsed) => {
                    return Err(bark::Resubscribe::Lost(LinkLost::Budget {
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

/// Wraps [`ReconnectingClient`] as both [`bark::FlockSource`] and
/// [`bark::ConfigSource`]. [`ReconnectingClient`] is not `Clone`, so the
/// two roles reach it through one [`Arc`] rather than through two clients
/// that would reconnect independently.
struct ClientShepherd {
    client: ReconnectingClient,
    /// The dog whose section [`bark::ConfigSource`] re-asks for.
    dog: String,
}

impl bark::FlockSource for ClientShepherd {
    async fn flock(&self) -> Result<Vec<ProcessInfo>, RequestError> {
        match self.client.request(Request::ListFlock).await? {
            Response::Flock(flock) => Ok(flock),
            _ => Err(unexpected_reply("ListFlock", "Response::Flock")),
        }
    }
}

impl bark::ConfigSource for ClientShepherd {
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
impl bark::FlockSource for Arc<ClientShepherd> {
    async fn flock(&self) -> Result<Vec<ProcessInfo>, RequestError> {
        bark::FlockSource::flock(&**self).await
    }
}

impl bark::ConfigSource for Arc<ClientShepherd> {
    async fn section(&self) -> Result<String, RequestError> {
        bark::ConfigSource::section(&**self).await
    }
}

#[cfg(test)]
mod builtin_schema_tests {
    use super::*;

    /// The secret marker reaching the schema a pane reads is the one
    /// thing standing between a webhook bearer token and the screen.
    #[test]
    fn both_built_ins_answer_and_a_stranger_does_not() {
        assert!(builtin_schema("metrics").is_some());
        let bark = builtin_schema("bark").expect("bark is a built-in");
        assert_eq!(
            bark["properties"]["sinks"][shep_core::dogs::SECRET_KEY],
            true
        );
        assert!(builtin_schema("otel").is_none());
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use shep_client::testing::{
        Handshake, fake_daemon_across_handovers, sample_ack, serve_one_request,
    };

    use super::*;

    /// fails if the production adapter cannot arm a second subscription
    /// after a handover.
    ///
    /// `bark::run_loop` is driven by a fake in bark's own tests, so this is
    /// the only thing that exercises `ClientEvents` itself: the client and
    /// topics held beside the stream, the wait for the link, and the
    /// re-subscribe. Ten real reloads showed it working, which is evidence
    /// rather than a guard.
    #[tokio::test]
    async fn the_bark_adapter_arms_a_second_subscription_after_a_handover() {
        use bark::EventSource as _;

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
        use bark::EventSource as _;
        use shep_core::protocol::{RpcError, RpcErrorCode};

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

        let Err(bark::Resubscribe::Request(err)) = refused else {
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
            use bark::EventSource as _;

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
                matches!(
                    gave_up,
                    Err(bark::Resubscribe::Lost(LinkLost::Budget { .. }))
                ),
                "expected a spent budget, got {gave_up:?}"
            );
            assert!(
                waited >= SHEPHERD_RETURN_BUDGET,
                "gave up after {waited:?}, inside the {SHEPHERD_RETURN_BUDGET:?} a handover \
                 is allowed to take, which is the restart-per-reload this rule exists to avoid"
            );
        }
    }

    /// fails if [`SHEPHERD_RETURN_BUDGET`] moves without the prose that
    /// names its value moving too.
    ///
    /// Three doc comments say "five seconds" in words: the constant's own,
    /// and the two `mod slow` headers that explain why those tests take
    /// that long. None of them can be checked by a reader, because the
    /// value arrives from `shep_daemon` rather than from the line above
    /// them, so a change there leaves all three quietly wrong while every
    /// doc-link still resolves.
    ///
    /// Pinning it here rather than deleting the numbers: a budget a reader
    /// has to go and look up is worse documentation, and this makes the
    /// concrete version safe to keep.
    #[test]
    fn the_budget_is_the_five_seconds_the_docs_promise() {
        assert_eq!(
            SHEPHERD_RETURN_BUDGET,
            Duration::from_secs(5),
            "docs/dogs.md, the web dogs page and three doc comments all say five \
             seconds; change them together or not at all"
        );
    }

    /// A [`ShepPaths`] rooted at `dir`, with `socket` pointed wherever the
    /// caller's fake daemon actually bound. Flat, not nested under `run/`,
    /// so a test never has to create that directory.
    pub(in crate::dog) fn test_paths(dir: &Path, socket: PathBuf) -> ShepPaths {
        let home = dir.to_path_buf();
        ShepPaths {
            daemon_config: home.join("shep.toml"),
            dogs_config: home.join("dogs.toml"),
            snapshot: home.join("flock.json"),
            logs: home.join("logs"),
            pids: home.join("pids"),
            run: home.join("run"),
            socket,
            barks: home.join("barks.jsonl"),
            kv: home.join("kv.json"),
            overrides: home.join("overrides.json"),
            secrets: home.join("secrets.json"),
            secrets_cache: home.join("secrets-cache.json"),
            home,
        }
    }

    /// No listener is bound at this path: a connection attempt would
    /// report `DaemonUnreachable`, not `Usage`, proving the name check
    /// runs first.
    #[tokio::test]
    async fn an_unknown_dog_name_is_usage_without_touching_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(
            dir.path(),
            shep_client::testing::control_address(dir.path()),
        );
        let code = run_dog("otel", paths).await;
        assert_eq!(code, ExitCode::Usage);
    }

    /// Proves dispatch reaches [`DogRuntime::start`], nothing about what
    /// `run_bark` does next: `serve_one_request`'s fake daemon closes the
    /// connection right after this one `DogConfig` reply.
    #[tokio::test]
    async fn run_dog_reaches_bark() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let response = Response::DogSection {
            toml: String::new().into(),
        };
        let handle = serve_one_request(&socket, sample_ack(), response).await;
        let paths = test_paths(dir.path(), socket);

        let task = tokio::spawn(run_dog("bark", paths));

        let envelope = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run_dog must reach the wire")
            .unwrap();
        assert_eq!(
            envelope.body,
            Request::DogConfig {
                name: "bark".to_string()
            }
        );

        task.abort();
    }

    /// [`metrics::run`] blocks on a shutdown signal once it is up, so this
    /// spawns it, waits for the `DogConfig` request, then aborts rather
    /// than awaiting a return that never comes. The section answers
    /// `bind = "127.0.0.1:0"`, an OS-assigned port, never
    /// [`metrics::MetricsConfig::default`]'s fixed `9615`.
    #[tokio::test]
    async fn run_dog_reaches_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let response = Response::DogSection {
            toml: "bind = \"127.0.0.1:0\"\n".to_string().into(),
        };
        let handle = serve_one_request(&socket, sample_ack(), response).await;
        let paths = test_paths(dir.path(), socket);

        let task = tokio::spawn(run_dog("metrics", paths));

        let envelope = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run_dog must reach the wire")
            .unwrap();
        assert_eq!(
            envelope.body,
            Request::DogConfig {
                name: "metrics".to_string()
            }
        );

        task.abort();
    }

    #[tokio::test]
    async fn run_dog_reports_daemon_unreachable_with_no_shepherd_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(
            dir.path(),
            shep_client::testing::control_address(dir.path()),
        );
        let code = run_dog("metrics", paths).await;
        assert_eq!(code, ExitCode::DaemonUnreachable);
    }

    /// A handover fixture rather than `serve_one_request`: that one closes
    /// after its single reply, so the `Subscribe` that follows a dog's
    /// `DogConfig` is never read off the wire. This one keeps the
    /// connection open and records every envelope.
    #[tokio::test]
    async fn bark_subscribes_to_its_own_config_topic() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let daemon = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        // A real sink: bark refuses to run without one. Port 1 is never
        // dialled; no bark fires in this test.
        daemon.reply_to_dog_config(
            "[sinks.ops]\nkind = \"json\"\nurl = \"http://127.0.0.1:1/hook\"\n",
        );
        let paths = test_paths(dir.path(), socket);

        let task = tokio::spawn(run_dog("bark", paths));

        // Polled rather than slept on, and bounded: the fixture records
        // envelopes as it reads them, so the test yields until the
        // subscribe arrives and fails with its own message if it never
        // does.
        let topics =
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let subscribed = daemon.envelopes().into_iter().find_map(|(_, envelope)| {
                        match envelope.body {
                            Request::Subscribe { topics } => Some(topics),
                            _ => None,
                        }
                    });
                    if let Some(topics) = subscribed {
                        break topics;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("bark must subscribe once its config parses");

        assert!(
            topics.iter().any(|topic| topic == "config.dog.bark"),
            "bark must ask for its own config topic: {topics:?}"
        );
        assert!(
            topics.iter().any(|topic| topic == "process.*"),
            "the lifecycle topics every rule reads must survive: {topics:?}"
        );

        task.abort();
    }
}
