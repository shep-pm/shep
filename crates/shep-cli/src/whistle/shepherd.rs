//! One connection per tool call.
//!
//! lookout holds a long-lived connection with a reconnect ladder, because a
//! dashboard must keep showing what it last knew. whistle has no screen: it
//! connects, sends, and drops. One handshake per call is cheap.
//!
//! A read tool that names a host makes the same call over ssh instead:
//! `ssh -T -- <dest> shep dial-stdio`, through
//! [`Client::connect_ssh_with`](shep_client::Client::connect_ssh_with), with
//! no prompts and a bound. The two targets are one [`Shepherd`].

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use rmcp::model::CallToolResult;
use shep_client::{Client, ConnectError, RequestError, SshOptions};
use shep_core::protocol::{HelloAck, HostUsage, ProcessInfo, Request, Response};

use crate::commands::rpc::deadline_for;
use crate::exit::ExitCode;
use crate::whistle::read::unexpected_response;

/// How long a host call's ssh dial may take before the tool call gives up.
///
/// Twice `crate::commands::hosts::HOST_CONNECT_TIMEOUT`: a fleet listing
/// abandons a slow host so the others print, while a whistle call dials one
/// host for a model waiting on one result.
const HOST_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The ssh settings every host call runs under: no prompts, and bounded.
///
/// A tool call has no terminal behind it and nobody watching it. ssh allowed
/// to ask for a passphrase or a host-key answer sits until the timeout, and
/// the model gets a timeout in place of the refusal ssh could have given in
/// one line. The same settings `shep flock` over a `hosts` list dials under.
#[must_use]
pub fn host_ssh() -> SshOptions {
    crate::commands::hosts::unattended(HOST_CONNECT_TIMEOUT)
}

/// Where this call's shepherd is: this machine's socket, or a host named out
/// of `shep.toml`'s `hosts` list.
#[derive(Debug, Clone)]
enum Target {
    /// The shepherd at this home's control socket.
    Local(PathBuf),
    /// The shepherd on `dest`, reached through `options`.
    Host {
        /// The destination, as the operator listed it.
        dest: String,
        /// How ssh is run.
        options: SshOptions,
    },
}

impl Target {
    /// The destination a host call dials, and `None` for this home's socket.
    ///
    /// A refusal needs it. `ConnectError` names the destination in its `Ssh`
    /// arm only, so every other host failure arrives without the one word the
    /// caller asked for.
    fn host(&self) -> Option<&str> {
        match self {
            Self::Local(_) => None,
            Self::Host { dest, .. } => Some(dest),
        }
    }
}

/// The socket, or the host, and the one operation anything in `whistle`
/// performs on it.
///
/// `super::read`'s five tools and `super::control`'s four both call
/// [`Self::call`]/[`Self::call_with_ack`] through `Whistle::shepherd`, one
/// held per `Whistle`. A read tool that names a host holds one of these per
/// call instead, built by [`Whistle::shepherd_for`](super::Whistle::shepherd_for).
#[derive(Debug, Clone)]
pub struct Shepherd {
    target: Target,
}

impl Shepherd {
    /// Wraps a socket path. Connects to nothing until [`Self::call`].
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self {
            target: Target::Local(socket),
        }
    }

    /// The shepherd on `dest`, dialled through `options`.
    ///
    /// `options` is [`host_ssh`] for every call this process makes. The seam a
    /// test uses is `Whistle::for_test_with_ssh_program`, which keeps those
    /// settings and swaps only the program.
    #[must_use]
    pub fn for_host(dest: &str, options: SshOptions) -> Self {
        Self {
            target: Target::Host {
                dest: dest.to_owned(),
                options,
            },
        }
    }

    /// Connects, sends one request, and drops the connection.
    ///
    /// Never `connect_or_spawn`: a model calling `list_flock` against a
    /// machine with no daemon running must be told so, not handed one it
    /// did not ask for.
    ///
    /// # Errors
    ///
    /// A [`CallToolResult`] with `is_error: true`, never an
    /// [`rmcp::ErrorData`], carrying the shepherd's own message.
    pub async fn call(&self, request: Request) -> Result<Response, CallToolResult> {
        self.call_with_ack(request)
            .await
            .map(|(_ack, response)| response)
    }

    /// [`Self::call`], plus the handshake the connection was opened with.
    ///
    /// `get_metrics` needs `daemon_version` and `daemon_pid`, which live on
    /// the [`Client`] that [`Self::call`] drops before returning; `call`
    /// delegates here and throws the ack away.
    ///
    /// # Errors
    ///
    /// As [`Self::call`].
    pub async fn call_with_ack(
        &self,
        request: Request,
    ) -> Result<(HelloAck, Response), CallToolResult> {
        let (client, ack) = self.connect().await?;
        let deadline = deadline_for(&request);
        let response = client
            .request_with_deadline(request, deadline)
            .await
            .map_err(|err| refusal(&err));
        // Dropping the client ends its actor task and closes the socket. Done
        // explicitly rather than by scope end so the ordering is visible: the
        // reply is already in hand.
        let _ = client.close().await;
        response.map(|response| (ack, response))
    }

    /// [`Client::list_flock`] on a connection of its own, plus the handshake
    /// it was opened with.
    ///
    /// # Errors
    ///
    /// As [`Self::call`]. A reply that is not a listing is the same refusal
    /// `read`'s other tools give for a reply they did not ask for.
    pub async fn list_flock_with_ack(
        &self,
    ) -> Result<(HelloAck, Vec<ProcessInfo>), CallToolResult> {
        let (client, ack) = self.connect().await?;
        let flock = client.list_flock().await.map_err(|err| match err {
            RequestError::UnexpectedReply { .. } => unexpected_response(),
            other => refusal(&other),
        });
        let _ = client.close().await;
        flock.map(|flock| (ack, flock))
    }

    /// [`Self::list_flock_with_ack`], plus the shepherd's own [`HostUsage`] over
    /// the same connection.
    ///
    /// `None` is a shepherd that has not measured itself yet. A shepherd that
    /// predates `Request::HostUsage` answers `Unsupported`, which is a refusal.
    ///
    /// # Errors
    ///
    /// As [`Self::list_flock_with_ack`], for either request.
    pub async fn flock_and_host_usage_with_ack(
        &self,
    ) -> Result<(HelloAck, Vec<ProcessInfo>, Option<HostUsage>), CallToolResult> {
        let (client, ack) = self.connect().await?;
        let read = async {
            let flock = client.list_flock().await?;
            let usage = client.host_usage().await?;
            Ok((flock, usage))
        }
        .await
        .map_err(|err| match err {
            RequestError::UnexpectedReply { .. } => unexpected_response(),
            other => refusal(&other),
        });
        let _ = client.close().await;
        read.map(|(flock, usage)| (ack, flock, usage))
    }

    /// Opens the connection one call uses and refuses a skewed shepherd.
    ///
    /// The local socket and a host differ here and only here: one is a
    /// `Client::connect`, the other `Client::connect_ssh_with`, and every
    /// step past the handshake is shared.
    async fn connect(&self) -> Result<(Client, HelloAck), CallToolResult> {
        let dialled = match &self.target {
            Target::Local(socket) => Client::connect(socket).await,
            Target::Host { dest, options } => Client::connect_ssh_with(dest, options).await,
        };
        let client = dialled.map_err(|err| connect_refusal(&err, self.target.host()))?;
        // `whistle` drives the daemon on every tool call, so it can never be
        // one of `RECOVERY_VERBS`.
        refuse_if_skewed(&client)?;
        let ack = client.daemon().clone();
        Ok((client, ack))
    }
}

/// Refuses `client` if its shepherd disagrees with this binary's crate
/// version, reusing [`crate::version_guard::refuse_version_skew`].
///
/// Never writes to stdout, which is whistle's MCP transport: the model sees
/// an in-band [`CallToolResult`]. Over ssh only a wire-protocol difference
/// refuses; `crate::version_guard::skew_of` states why.
///
/// # Errors
///
/// A [`CallToolResult`] with `is_error: true` when the shepherd's crate
/// version differs from this binary's, or when a host speaks another protocol.
fn refuse_if_skewed(client: &Client) -> Result<(), CallToolResult> {
    if client.host().is_some() {
        return match crate::version_guard::host_skew(
            client,
            crate::version_guard::VersionGuard::Enforce,
        ) {
            Ok(None) => Ok(()),
            Ok(Some(notice)) => {
                // stderr only, as every other word this module writes is:
                // stdout is the transport. The model gets the flock either
                // way; the operator is the one who can install one version on
                // both hosts.
                let _ = writeln!(std::io::stderr(), "whistle: {notice}");
                Ok(())
            }
            Err(message) => Err(own_refusal(ExitCode::VersionSkew.code_str(), message)),
        };
    }
    let mut sink = std::io::sink();
    let mut err = std::io::stderr();
    let mut streams = crate::output::Streams {
        out: &mut sink,
        err: &mut err,
        style: crate::style::Presentation::BARE,
        fmt: crate::cli::Format::Table,
    };
    crate::version_guard::refuse_version_skew(
        &mut streams,
        client,
        crate::version_guard::VersionGuard::Enforce,
    )
    .map_err(|_code| {
        CallToolResult::structured_error(serde_json::json!({
            "code": crate::exit::ExitCode::VersionSkew.code_str(),
            "message": format!(
                "this shep is {}, the running shepherd is {}; \
                 `cargo install shep` replaced the binary without \
                 restarting it. Run `shep daemon reload`",
                env!("CARGO_PKG_VERSION"),
                client.daemon().daemon_version,
            ),
        }))
    })
}

/// A connect failure, as an in-band tool error that says which half failed.
///
/// `host` is the destination this call dialled, `None` for this home's
/// socket. The two targets fail in different words, and a model that
/// branches on the code must not read one as the other.
///
/// # Errors
///
/// A [`CallToolResult`] with `is_error: true`, never an [`rmcp::ErrorData`]:
/// `no_shepherd` for this machine's socket and for a host whose shepherd is
/// not running, `version_skew` for a protocol difference, `host_unreachable`
/// for a host whose session never completed the handshake, and
/// `invalid_config` for a listed entry `Destination::parse` turns down.
fn connect_refusal(err: &ConnectError, host: Option<&str>) -> CallToolResult {
    // A refusal is not an absence. The far side answered the handshake and
    // said the two builds do not speak, so a live host reaches this arm rather
    // than the ssh one. Its `Display` already names which side of the gap to
    // replace.
    if matches!(err, ConnectError::ProtocolMismatch { .. }) {
        return own_refusal(ExitCode::VersionSkew.code_str(), err.to_string());
    }
    let (code, message) = match (err, host) {
        (ConnectError::Ssh { .. }, _) => ("no_shepherd", err.to_string()),
        // An entry an operator wrote into `shep.toml` by hand that
        // `Destination::parse` turns down: nothing was spawned, and the file
        // is where the fix is.
        (ConnectError::Destination(_), Some(dest)) => (
            ExitCode::InvalidConfig.code_str(),
            format!("`{dest}` is a listed host ssh cannot take: {err}"),
        ),
        // A host whose ssh session opened and never finished the handshake.
        // `ConnectError`'s words for that name no destination, so this adds
        // the one the caller asked for.
        (other, Some(dest)) => ("host_unreachable", format!("`{dest}`: {other}")),
        (other, None) => ("no_shepherd", format!("no shepherd is running: {other}")),
    };
    own_refusal(code, message)
}

/// A request failure, as an in-band tool error carrying the shepherd's words.
///
/// `structured_error`, not `Err(ErrorData)`: MCP reserves protocol errors for
/// unknown tools and malformed params. The message passes through unaltered,
/// even when the code is imprecise: the wire has no conflict code yet.
fn refusal(err: &RequestError) -> CallToolResult {
    let (code, message) = match err {
        // `exit.rs` is the one place this binary spells a daemon error
        // code; a second `match` here would drift. The message is
        // untouched: it routinely carries an app's own name.
        RequestError::Rpc(rpc) => (
            ExitCode::from(rpc.code).code_str().to_string(),
            rpc.message.clone(),
        ),
        // `Timeout`, `Closed` and `Wire` each have a `Display` that already
        // says what happened in one clause; there is nothing to add.
        other => ("transport".to_string(), other.to_string()),
    };
    CallToolResult::structured_error(serde_json::json!({
        "code": code,
        "message": message,
    }))
}

/// whistle's own refusal, before anything reaches the wire.
///
/// One shape for every caller: an unreadable log file (`tail_bleats`), an
/// unreadable `barks.jsonl` (`list_barks`), and `start_sheep`'s
/// already-running refusal all go through this.
pub fn own_refusal(code: &str, message: String) -> CallToolResult {
    CallToolResult::structured_error(serde_json::json!({
        "code": code,
        "message": message,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whistle::matching_ack;
    use shep_core::protocol::{RpcError, RpcErrorCode};

    /// shep does not paraphrase the shepherd: `is_error: true` keeps the
    /// message in front of the model, where an `Err(ErrorData)` would
    /// become a host-routed protocol error instead.
    #[test]
    fn a_daemon_refusal_is_an_in_band_error_keeping_its_own_message() {
        let result = refusal(&RequestError::Rpc(RpcError {
            code: RpcErrorCode::Internal,
            message: "api is already being reloaded".to_string(),
            daemon_version: None,
        }));
        assert_eq!(result.is_error, Some(true));
        let structured = result
            .structured_content
            .expect("a refusal carries structured content a model can branch on");
        assert_eq!(structured["message"], "api is already being reloaded");
        assert_eq!(
            structured["code"], "internal",
            "and the code, so a model can tell a conflict from a not-found: {structured}"
        );
    }

    /// "connection refused" alone tells a model nothing it can act on; the
    /// path is what an operator greps for.
    ///
    /// `ConnectError::Connect` is a struct variant, not a tuple one.
    #[test]
    fn an_unreachable_shepherd_names_the_socket_once() {
        let socket = std::path::Path::new("/nonexistent/shep/run/shep.sock");
        let result = connect_refusal(
            &ConnectError::Connect {
                path: socket.to_path_buf(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            },
            None,
        );
        assert_eq!(result.is_error, Some(true));
        let message = result.structured_content.expect("structured")["message"]
            .as_str()
            .expect("a string")
            .to_string();
        assert!(message.contains("/nonexistent/shep/run/shep.sock"));
        assert!(
            message.contains("no shepherd"),
            "and says what is missing, not just what failed: {message}"
        );
        // Once, not twice: `ConnectError`'s own `Display` already prints
        // the path, so a wrapper that prepends it too reads as two sockets.
        assert_eq!(
            message.matches("/nonexistent/shep/run/shep.sock").count(),
            1,
            "the socket path appears once, not once per layer: {message}"
        );
    }

    /// A host that answers the handshake only to turn it down is a version
    /// gap, not an absent shepherd. `shep_client::ssh::explain` returns a
    /// refusal as it is, so a live host reaches this arm rather than the ssh
    /// one, and a model branching on the code would report a host with a
    /// running shepherd as down.
    #[test]
    fn a_host_that_turns_down_the_handshake_is_refused_as_a_version_gap() {
        let result = connect_refusal(
            &ConnectError::ProtocolMismatch {
                client: shep_core::protocol::PROTOCOL_VERSION,
                daemon_version: Some("0.1.8".to_string()),
                message: "this daemon speaks protocol 2".to_string(),
            },
            Some("amp2"),
        );
        assert_eq!(result.is_error, Some(true));
        let structured = result
            .structured_content
            .expect("a refusal carries structured content a model can branch on");
        assert_eq!(
            structured["code"],
            ExitCode::VersionSkew.code_str(),
            "the same code this module gives a host that answers and agrees to disagree: {structured}"
        );
        let message = structured["message"]
            .as_str()
            .expect("a string")
            .to_string();
        assert!(message.contains("protocol mismatch"), "{message}");
        assert!(
            message.contains("older of the two"),
            "and names which side of the gap to replace: {message}"
        );
        assert!(
            !message.contains("no shepherd is running"),
            "amp2 has a shepherd, which refused: {message}"
        );
    }

    /// A host that opens an ssh session and never completes the handshake is
    /// a slow host, and `ConnectError`'s words for it name no destination at
    /// all, so the refusal has to add the one the caller asked for.
    #[test]
    fn a_host_that_never_answers_the_handshake_names_the_host() {
        let result = connect_refusal(
            &ConnectError::HandshakeTimeout {
                after: HOST_CONNECT_TIMEOUT,
            },
            Some("amp2"),
        );
        assert_eq!(result.is_error, Some(true));
        let structured = result
            .structured_content
            .expect("a refusal carries structured content a model can branch on");
        assert_eq!(
            structured["code"], "host_unreachable",
            "a code that does not claim the shepherd is missing: {structured}"
        );
        let message = structured["message"]
            .as_str()
            .expect("a string")
            .to_string();
        assert!(
            message.contains("amp2"),
            "the refusal names the host it dialled: {message}"
        );
        assert!(message.contains("handshake"), "{message}");
        assert!(
            !message.contains("no shepherd is running"),
            "the link was the slow part: {message}"
        );
    }

    /// A `hosts` entry an operator edited in by hand that
    /// `Destination::parse` turns down: nothing was spawned, and the file the
    /// entry sits in is where the fix is.
    #[test]
    fn a_listed_host_ssh_cannot_take_is_refused_as_a_config_problem() {
        let listed = "amp 2";
        let destination = shep_core::destination::Destination::parse(listed)
            .expect_err("a space reads to ssh as a second argument");
        let result = connect_refusal(&ConnectError::Destination(destination), Some(listed));
        assert_eq!(result.is_error, Some(true));
        let structured = result
            .structured_content
            .expect("a refusal carries structured content a model can branch on");
        assert_eq!(
            structured["code"],
            ExitCode::InvalidConfig.code_str(),
            "the entry is the problem, not the host: {structured}"
        );
        let message = structured["message"]
            .as_str()
            .expect("a string")
            .to_string();
        assert!(message.contains(listed), "{message}");
        assert!(message.contains("destination"), "{message}");
    }

    /// Bounded: a `call` that hung on a dead handle would otherwise hang
    /// the suite rather than fail it.
    #[tokio::test]
    async fn two_calls_survive_a_shepherd_that_restarted_in_between() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());

        let (first, first_served) = shep_client::testing::fake_daemon_accepting_repeatedly_with_ack(
            &socket,
            matching_ack(),
            Response::Pong,
        );
        let shepherd = Shepherd::new(socket.clone());
        let one = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            shepherd.call(Request::Ping),
        )
        .await
        .expect("the first call finished within ten seconds");
        assert!(one.is_ok());
        assert_eq!(
            first_served.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one call is one connection — not zero, and not a retry"
        );

        // The shepherd goes away entirely: task aborted, socket file removed.
        // A `Shepherd` holding a connection would be holding a dead one.
        first.abort();
        // Awaited, not just aborted: `abort` only requests cancellation,
        // and the bound listener is released only once it is reclaimed.
        // On Windows an unreclaimed pipe fails the rebind below with
        // `ERROR_ACCESS_DENIED`.
        let _ = first.await;
        // Unix only: the listener leaves a socket file behind that outlives
        // the aborted task, so it must be unlinked to simulate a departed
        // shepherd. A named pipe has no directory entry to remove.
        #[cfg(unix)]
        std::fs::remove_file(&socket).unwrap();

        let (second, _second_served) =
            shep_client::testing::fake_daemon_accepting_repeatedly_with_ack(
                &socket,
                matching_ack(),
                Response::Pong,
            );
        let two = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            shepherd.call(Request::Ping),
        )
        .await
        .expect("the second call finished within ten seconds");
        assert!(two.is_ok(), "a fresh connection per call needs no ladder");
        second.abort();
    }

    /// Must refuse without writing to stdout, the one byte this module's
    /// doc says must never happen.
    #[tokio::test]
    async fn a_version_skewed_shepherd_is_an_in_band_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let addr = shep_client::testing::control_address(dir.path());
        let ack = HelloAck {
            daemon_version: "0.1.8".to_string(),
            protocol: shep_core::protocol::PROTOCOL_VERSION,
            pid: 4242,
            min_supported: None,
        };
        let (client, _fake) = shep_client::testing::fake_client_with_ack(&addr, ack).await;

        let result = super::refuse_if_skewed(&client).expect_err("a skew must be refused");
        assert_eq!(result.is_error, Some(true));
        let structured = result
            .structured_content
            .expect("a refusal carries structured content a model can branch on");
        assert_eq!(structured["code"], "version_skew");
        let message = structured["message"].as_str().expect("a string message");
        assert!(message.contains(env!("CARGO_PKG_VERSION")), "{message}");
        assert!(message.contains("0.1.8"), "{message}");
    }

    /// A shepherd of this binary's own version is not a skew.
    #[tokio::test]
    async fn a_matching_version_proceeds() {
        let dir = tempfile::tempdir().unwrap();
        let addr = shep_client::testing::control_address(dir.path());
        let ack = HelloAck {
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: shep_core::protocol::PROTOCOL_VERSION,
            pid: 4242,
            min_supported: None,
        };
        let (client, _fake) = shep_client::testing::fake_client_with_ack(&addr, ack).await;

        super::refuse_if_skewed(&client).expect("a matching version is not a skew");
    }
}
