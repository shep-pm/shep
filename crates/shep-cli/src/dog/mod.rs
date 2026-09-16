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

use core::fmt;
use std::sync::Arc;
use std::time::Duration;

use shep_client::{
    ConnectError, EventStream, LinkLost, RECONNECT_MIN_DELAY, ReconnectingClient, RequestError,
};
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
/// (this). Also [`None`] when the schema could not be built at all:
/// `config_schema` refusing a `#[shep(secret)]` mark that landed on no
/// property is a bug in this binary, not a fact about the dog, and the
/// caller has one way of saying "no schema" either way.
pub(crate) fn builtin_schema(name: &str) -> Option<serde_json::Value> {
    use shep_client::dogs::config_schema;

    let schema = match name {
        "metrics" => config_schema::<metrics::MetricsConfig>().ok()?,
        "bark" => config_schema::<bark::BarkConfig>().ok()?,
        _ => return None,
    };
    serde_json::to_value(schema).ok()
}

/// A dog's connection to the shepherd, and its own configuration.
///
/// Locate the socket from `$SHEP_HOME`, connect, handshake, ask for
/// `[dog.<name>]`, parse it.
pub struct DogRuntime {
    /// The connected client. A dog IS a client; there is no second protocol.
    ///
    /// A [`ReconnectingClient`] rather than a bare
    /// [`Client`](shep_client::Client): a dog's process survives the
    /// shepherd's `execve` for free, but only the listening socket crosses
    /// that exec, so the accepted connection underneath this field dies on
    /// every reload.
    pub client: ReconnectingClient,
    /// This dog's `[dog.<name>]` section, exactly as the shepherd rendered
    /// it, for the dog to parse into its own shape. Empty when the file has
    /// no such section.
    pub section: String,
    /// `$SHEP_HOME` as this dog resolved it.
    pub paths: ShepPaths,
    /// The dog's own name, kept so [`Self::config`] can name it in a
    /// [`DogRunError::Section`] without every caller threading it through
    /// again.
    name: String,
}

/// Manual: [`Self::section`] is a dog's raw `[dog.<name>]` config text,
/// which routinely carries a webhook URL with a bearer token in its query
/// string. A derived `Debug` would print it in full. `client` and `paths`
/// carry nothing sensitive and print unchanged.
impl fmt::Debug for DogRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DogRuntime")
            .field("client", &self.client)
            .field("section", &format!("<{} bytes>", self.section.len()))
            .field("paths", &self.paths)
            .field("name", &self.name)
            .finish()
    }
}

/// Why [`DogRuntime::start`] or [`DogRuntime::config`] failed.
pub enum DogRunError {
    /// No shepherd answered at the socket.
    Connect(ConnectError),
    /// The shepherd refused the config request.
    Request(RequestError),
    /// The shepherd answered `Request::DogConfig` with something other
    /// than `Response::DogSection`. Never returned by a daemon on the same
    /// protocol version; kept reportable rather than `unreachable!()`, so
    /// a dog exits cleanly instead of panicking.
    UnexpectedReply,
    /// The section does not fit the shape [`DogRuntime::config`] was asked
    /// to parse it as.
    Section {
        /// The dog's own name.
        name: String,
        /// The parser's full complaint, which can quote the offending
        /// line.
        message: String,
    },
}

/// Manual: [`DogRunError::Section`]'s `message` is the TOML parser's own
/// complaint, which can quote a `[dog.<name>]` webhook URL verbatim.
/// Redacted to the dog's name and a fixed description. `Connect`/`Request`
/// wrap types with their own non-leaking `Debug` already.
impl fmt::Debug for DogRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => f.debug_tuple("Connect").field(err).finish(),
            Self::Request(err) => f.debug_tuple("Request").field(err).finish(),
            Self::UnexpectedReply => f.write_str("UnexpectedReply"),
            Self::Section { name, .. } => f
                .debug_struct("Section")
                .field("name", name)
                .field("message", &"<redacted: may quote the section>")
                .finish(),
        }
    }
}

impl fmt::Display for DogRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "no shepherd answered at the socket: {err}"),
            Self::Request(err) => write!(f, "the shepherd refused the config request: {err}"),
            Self::UnexpectedReply => {
                f.write_str("the shepherd answered with a response this client does not understand")
            }
            Self::Section { name, message } => {
                write!(f, "dog {name}'s own configuration does not fit: {message}")
            }
        }
    }
}

impl core::error::Error for DogRunError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Connect(err) => Some(err),
            Self::Request(err) => Some(err),
            Self::UnexpectedReply | Self::Section { .. } => None,
        }
    }
}

impl From<ConnectError> for DogRunError {
    fn from(source: ConnectError) -> Self {
        Self::Connect(source)
    }
}

impl From<RequestError> for DogRunError {
    fn from(source: RequestError) -> Self {
        Self::Request(source)
    }
}

impl DogRuntime {
    /// Connects and fetches `name`'s section.
    ///
    /// Announces itself as the dog registered under `name`, so a daemon
    /// that refuses this handshake on protocol skew knows which dog it
    /// just refused and can restart it from disk.
    ///
    /// # Errors
    /// - [`DogRunError::Connect`]: no shepherd answered at the socket.
    /// - [`DogRunError::Request`]: the shepherd refused the config request.
    /// - [`DogRunError::UnexpectedReply`]: the shepherd answered
    ///   `Request::DogConfig` with something other than
    ///   `Response::DogSection`.
    pub async fn start(name: &str, paths: ShepPaths) -> Result<Self, DogRunError> {
        let client = ReconnectingClient::connect_as_dog(&paths.socket, name).await?;
        let response = client
            .request(Request::DogConfig {
                name: name.to_string(),
            })
            .await?;
        let Response::DogSection { toml } = response else {
            return Err(DogRunError::UnexpectedReply);
        };
        Ok(Self {
            section: toml.as_str().to_string(),
            client,
            paths,
            name: name.to_string(),
        })
    }

    /// This dog's section parsed into `T`, or `T::default()` when the
    /// shepherd had no section for it.
    ///
    /// # Errors
    /// - [`DogRunError::Section`]: the section does not fit `T`, naming
    ///   the dog and the parser's own message.
    pub fn config<T>(&self) -> Result<T, DogRunError>
    where
        T: serde::de::DeserializeOwned + Default,
    {
        if self.section.is_empty() {
            return Ok(T::default());
        }
        toml::from_str(&self.section).map_err(|err| DogRunError::Section {
            name: self.name.clone(),
            message: err.to_string(),
        })
    }
}

/// Maps a failed [`DogRuntime::start`] to the exit code that reports it.
///
/// `Connect`/`Request` defer to the same `ExitCode` conversions every
/// other verb's client-connect/request failure goes through. `Section` is
/// [`ExitCode::InvalidConfig`]; `UnexpectedReply` is [`ExitCode::Internal`].
fn exit_code_for(err: &DogRunError) -> ExitCode {
    match err {
        DogRunError::Connect(inner) => ExitCode::from(inner),
        DogRunError::Request(inner) => ExitCode::from(inner),
        DogRunError::Section { .. } => ExitCode::InvalidConfig,
        DogRunError::UnexpectedReply => ExitCode::Internal,
    }
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
        Handshake, fake_daemon_across_handovers, fake_reconnecting_client_on, sample_ack,
        serve_one_request,
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

    /// Builds a [`DogRuntime`] carrying `section`, backed by a real (if
    /// otherwise unused) connection: the field has to hold one, even
    /// though [`DogRuntime::config`] never touches it. Bridges into its
    /// own fresh Tokio runtime, so call sites stay plain `#[test]`s.
    fn runtime_with_section(section: &str) -> DogRuntime {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (client, _daemon) = fake_reconnecting_client_on(&socket).await;
            DogRuntime {
                client,
                section: section.to_string(),
                paths: test_paths(dir.path(), socket),
                name: "testdog".to_string(),
            }
        })
    }

    #[test]
    fn a_section_that_does_not_fit_is_refused_rather_than_defaulted() {
        #[derive(Debug, Default, serde::Deserialize, PartialEq)]
        #[serde(deny_unknown_fields, default)]
        struct Cfg {
            port: u16,
        }
        let runtime = runtime_with_section("port = \"nine thousand\"\n");
        let err = runtime.config::<Cfg>().unwrap_err();
        assert!(matches!(err, DogRunError::Section { .. }));
        assert!(err.to_string().contains("port"));

        let empty = runtime_with_section("");
        assert_eq!(empty.config::<Cfg>().unwrap(), Cfg::default());
    }

    #[tokio::test]
    async fn a_dog_asks_for_its_own_section_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let response = Response::DogSection {
            toml: "webhook = \"https://example.invalid/hook\"\n"
                .to_string()
                .into(),
        };
        let handle = serve_one_request(&socket, sample_ack(), response).await;
        let paths = test_paths(dir.path(), socket);

        let runtime = DogRuntime::start("bark", paths).await.unwrap();

        let envelope = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("DogRuntime::start must reach the wire; it hung instead of connecting")
            .unwrap();
        assert_eq!(
            envelope.body,
            Request::DogConfig {
                name: "bark".to_string()
            }
        );
        assert_eq!(
            runtime.section,
            "webhook = \"https://example.invalid/hook\"\n"
        );
    }

    /// The fake closes right after acking, so the `DogConfig` request that
    /// follows fails and `start` returns an error; the handshake has
    /// already happened by then, and it is the frame under test.
    #[tokio::test]
    async fn a_dog_announces_its_own_name_at_the_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let served = shep_client::testing::fake_daemon(&socket, Ok(sample_ack())).await;
        let paths = test_paths(dir.path(), socket);

        let _started = DogRuntime::start("bark", paths).await;

        let hello = tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("DogRuntime::start must reach the wire; it hung instead of connecting")
            .unwrap();
        assert_eq!(
            hello.dog_name.as_deref(),
            Some("bark"),
            "a dog must announce the name it was registered under"
        );
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

    /// `Debug` on a section mismatch carries the dog's name and a fixed
    /// description, never the parser's message, which can quote a webhook
    /// URL.
    #[test]
    fn dog_run_error_section_debug_never_prints_the_message() {
        let secret = "https://hooks.example.com/services/T00/B00/super-secret-token";
        let err = DogRunError::Section {
            name: "bark".to_string(),
            message: format!("invalid type: string \"{secret}\", expected u16\nin `webhook`"),
        };
        let debug = format!("{err:?}");
        assert!(!debug.contains(secret), "{debug}");
        assert!(!debug.contains("webhook"), "{debug}");
        assert_eq!(
            debug,
            "Section { name: \"bark\", message: \"<redacted: may quote the section>\" }"
        );
    }

    /// `client`'s own `Debug` embeds this test's tempdir socket path, so
    /// the whole struct cannot be one hardcoded exact string; the redacted
    /// `section` field alone gets that pin.
    #[test]
    fn dog_runtime_debug_never_prints_the_section() {
        let secret = "https://hooks.example.com/services/T00/B00/super-secret-token";
        let section = format!("webhook = \"{secret}\"\n");
        let byte_len = section.len();
        let runtime = runtime_with_section(&section);
        let debug = format!("{runtime:?}");
        assert!(!debug.contains(secret), "{debug}");
        assert!(!debug.contains("webhook"), "{debug}");
        assert!(
            debug.contains(&format!("section: \"<{byte_len} bytes>\"")),
            "{debug}"
        );
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
