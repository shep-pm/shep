//! `signal`: send a unix signal to matched sheep's own processes, never
//! their process group, or with `--lamb` to one lamb of one sheep.
//!
//! The signal name is validated locally so a malformed one costs no round
//! trip. The daemon re-validates it anyway: peer input is untrusted. A
//! shepherd below [`LAMB_SIGNAL_SINCE`] would drop `--lamb` and signal the
//! sheep itself, so that is refused here before anything is sent.
//!
//! A row's own outcome is never a request failure. `shep signal` exits
//! non-`Success` only when the RPC itself failed.

use shep_client::Client;
use shep_core::protocol::{LAMB_SIGNAL_SINCE, Request, Response};
use shep_core::signals::OperatorSignal;

use crate::cli::SignalArgs;
use crate::commands::rpc::request_and_render;
use crate::commands::selector::parse_selector_spec;
use crate::exit::ExitCode;
use crate::output::{SignalledRows, Streams};

/// Sends `args.signal` to the sheep matching `args.selector`, and renders one
/// row per match.
pub async fn signal(client: &Client, streams: &mut Streams<'_>, args: &SignalArgs) -> ExitCode {
    let selector = match parse_selector_spec(streams, &args.selector) {
        Ok(selector) => selector,
        Err(code) => return code,
    };

    let Some(sig) = OperatorSignal::parse(&args.signal) else {
        let message = format!(
            "`{}` is not a signal shep will send; accepted: {}",
            args.signal,
            OperatorSignal::ACCEPTED.join(", ")
        );
        return streams.fail(ExitCode::Usage, &message);
    };

    if args.lamb.is_some() && client.daemon().protocol < LAMB_SIGNAL_SINCE {
        let message = format!(
            "this shepherd speaks protocol {} and signals a lamb from protocol \
             {LAMB_SIGNAL_SINCE}, so it would signal the sheep itself. Restart the shepherd \
             with `shep daemon reload` and run this again; nothing was sent",
            client.daemon().protocol
        );
        return streams.fail(ExitCode::Unsupported, &message);
    }

    let body = Request::Signal {
        selector,
        // Canonical spelling: the wire carries `SIGHUP` whether the operator
        // typed `hup`, `Hup` or `SIGHUP`.
        signal: sig.as_str().to_string(),
        lamb: args.lamb,
    };

    request_and_render(
        client,
        streams,
        "signal",
        body,
        None,
        |response| match response {
            Response::Signalled(replies) => Some(SignalledRows(replies)),
            _ => None,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use shep_client::testing::{
        fake_client_capturing_envelopes_with_ack, fake_client_replying_err, sample_ack,
    };
    use shep_core::protocol::SelectorSpec;
    use shep_core::protocol::{HelloAck, RpcErrorCode};

    use super::*;
    use crate::cli::Format;

    fn args(selector: &str, signal: &str) -> SignalArgs {
        SignalArgs {
            selector: selector.to_string(),
            signal: signal.to_string(),
            lamb: None,
        }
    }

    fn args_for_lamb(selector: &str, signal: &str, lamb: u32) -> SignalArgs {
        SignalArgs {
            lamb: Some(lamb),
            ..args(selector, signal)
        }
    }

    /// Runs the verb against a fake daemon that captures envelopes.
    ///
    /// The same shape as `commands::trigger`'s helper, so the two verbs'
    /// tests read alike.
    async fn run(
        args: &SignalArgs,
    ) -> (
        ExitCode,
        Vec<u8>,
        Vec<u8>,
        tokio::sync::mpsc::Receiver<shep_core::protocol::Envelope>,
    ) {
        run_against(args, sample_ack()).await
    }

    /// [`run`], against a shepherd that handshakes with `ack`.
    async fn run_against(
        args: &SignalArgs,
        ack: HelloAck,
    ) -> (
        ExitCode,
        Vec<u8>,
        Vec<u8>,
        tokio::sync::mpsc::Receiver<shep_core::protocol::Envelope>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, envelopes) = fake_client_capturing_envelopes_with_ack(&path, ack).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            signal(&client, &mut streams, args).await
        };
        (code, out, err, envelopes)
    }

    /// Runs the verb against a fake daemon scripted to answer with an RPC
    /// error. A second helper rather than a flag on [`run`]: the two fake
    /// daemons come from different factories.
    async fn run_replying_err(
        args: &SignalArgs,
        code: RpcErrorCode,
        message: &str,
    ) -> (ExitCode, Vec<u8>, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _served) = fake_client_replying_err(&path, code, message).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            signal(&client, &mut streams, args).await
        };
        (exit, out, err)
    }

    /// `"/[/"` is one of the only three inputs the selector grammar rejects.
    #[tokio::test]
    async fn a_malformed_selector_is_a_local_usage_error() {
        let (code, _out, _err, mut envelopes) = run(&args("/[/", "hup")).await;
        assert_eq!(code, ExitCode::Usage);
        assert!(
            envelopes.try_recv().is_err(),
            "a malformed selector must fail locally, never reach the wire"
        );
    }

    #[tokio::test]
    async fn a_bad_signal_name_never_reaches_the_wire() {
        let (code, out, err, mut envelopes) = run(&args("web", "SIGHUPP")).await;
        assert_eq!(code, ExitCode::Usage);
        assert!(
            envelopes.try_recv().is_err(),
            "a malformed signal name must fail locally, never reach the wire"
        );
        assert!(out.is_empty());
        assert!(!err.is_empty());
    }

    #[tokio::test]
    async fn a_not_found_reply_exits_not_found_rather_than_being_swallowed() {
        let (code, out, _err) = run_replying_err(
            &args("nonexistent", "hup"),
            RpcErrorCode::NotFound,
            "no sheep matched",
        )
        .await;
        assert_eq!(code, ExitCode::NotFound);
        assert!(out.is_empty());
    }

    /// Lowercase input on purpose: the canonical-spelling assertion is
    /// meaningless against an already-canonical name.
    #[tokio::test]
    async fn the_request_carries_the_selector_and_the_canonical_signal_name() {
        let (_code, _out, _err, mut envelopes) = run(&args("web", "hup")).await;

        let envelope = envelopes.recv().await.unwrap();
        assert_eq!(
            envelope.body,
            Request::Signal {
                selector: SelectorSpec::Name("web".to_string()),
                signal: "SIGHUP".to_string(),
                lamb: None,
            }
        );
    }

    #[tokio::test]
    async fn a_lamb_rides_the_request_to_the_shepherd() {
        let (_code, _out, _err, mut envelopes) = run(&args_for_lamb("web", "hup", 123)).await;

        let envelope = envelopes.recv().await.unwrap();
        assert_eq!(
            envelope.body,
            Request::Signal {
                selector: SelectorSpec::Name("web".to_string()),
                signal: "SIGHUP".to_string(),
                lamb: Some(123),
            }
        );
    }

    /// The bytes an operator who never types `--lamb` has always sent: the
    /// key is not there at all, not `null`.
    #[tokio::test]
    async fn without_a_lamb_the_request_is_the_bytes_it_always_was() {
        let (_code, _out, _err, mut envelopes) = run(&args("web", "hup")).await;

        let envelope = envelopes.recv().await.unwrap();
        assert_eq!(
            serde_json::to_string(&envelope.body).unwrap(),
            r#"{"kind":"signal","selector":{"kind":"name","value":"web"},"signal":"SIGHUP"}"#
        );
    }

    /// Protocol 12 reads `Signal` and drops `lamb`, so it would signal the
    /// sheep. Nothing may reach it.
    #[tokio::test]
    async fn a_shepherd_below_protocol_13_is_sent_nothing_and_told_to_restart() {
        let ack = HelloAck {
            protocol: 12,
            ..sample_ack()
        };
        let (code, out, err, mut envelopes) =
            run_against(&args_for_lamb("web", "hup", 123), ack).await;

        assert_eq!(code, ExitCode::Unsupported);
        assert!(out.is_empty());
        assert!(
            envelopes.try_recv().is_err(),
            "a request this shepherd would misread must not be sent"
        );
        let err = String::from_utf8(err).unwrap();
        assert!(err.contains("protocol 12"), "{err}");
        assert!(err.contains("Restart the shepherd"), "{err}");
    }

    /// The gate is for `--lamb` alone: an old shepherd still takes a plain
    /// signal.
    #[tokio::test]
    async fn a_shepherd_below_protocol_13_still_takes_a_signal_without_a_lamb() {
        let ack = HelloAck {
            protocol: 12,
            ..sample_ack()
        };
        let (_code, _out, _err, mut envelopes) = run_against(&args("web", "hup"), ack).await;

        assert!(envelopes.recv().await.is_some());
    }

    /// The fake daemon's `Pong` stands in for a `Response` variant this
    /// `match` has no arm for.
    #[tokio::test]
    async fn an_unrecognised_response_exits_internal() {
        let (code, out, err, _envelopes) = run(&args("web", "hup")).await;
        assert_eq!(code, ExitCode::Internal);
        assert!(out.is_empty());
        assert!(!err.is_empty());
    }
}
