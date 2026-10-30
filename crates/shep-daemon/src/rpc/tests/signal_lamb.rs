//! `Signal` with a `lamb`: only a pid in the sheep's fresh lamb walk is sent
//! to, the selector must name one sheep, and the sheep is told on its channel.

use super::*;
use crate::channel::ShepherdMessage;
use shep_core::protocol::{SignalOutcome, SignalReply};
use shep_core::signals::OperatorSignal;

const ROOT: u32 = FIRST_SCRIPTED_PID;
const LAMB: u32 = FIRST_SCRIPTED_PID + 1;
/// In the process table, but under nobody's sheep.
const STRANGER: u32 = FIRST_SCRIPTED_PID + 90;

fn process_table() -> Vec<crate::limits::sample::ProcessIdentity> {
    vec![
        identity(ROOT, None, "srv"),
        identity(LAMB, Some(ROOT), "node"),
        identity(STRANGER, None, "sshd"),
    ]
}

async fn start(h: &Harness, names: &[&str]) {
    let apps = names
        .iter()
        .map(|name| {
            let mut app = AppConfig::minimal(name, "./srv");
            app.channel = true;
            app
        })
        .collect();
    let started = reply_of(dispatch(envelope(1, Request::Start { apps }), &h.ctx).await);
    assert!(started.result.is_ok(), "{:?}", started.result);
}

async fn signal_lamb(
    h: &Harness,
    selector: SelectorSpec,
    lamb: u32,
) -> Result<Vec<SignalReply>, shep_core::protocol::RpcError> {
    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::Signal {
                    selector,
                    signal: "hup".to_string(),
                    lamb: Some(lamb),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    reply.result.map(|response| {
        let Response::Signalled(rows) = response else {
            panic!("expected a signalled reply, got {response:?}");
        };
        rows
    })
}

fn web() -> SelectorSpec {
    SelectorSpec::Name("web".to_string())
}

#[tokio::test(start_paused = true)]
async fn a_pid_in_the_lamb_walk_is_sent_the_signal_and_reported_delivered() {
    let h = harness_identifying(vec![ProcScript::never_exits()], process_table());
    start(&h, &["web"]).await;

    let rows = signal_lamb(&h, web(), LAMB).await.unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "web");
    assert_eq!(rows[0].outcome, SignalOutcome::Delivered);
    assert_eq!(h.pid_signals.sent(), vec![(LAMB, OperatorSignal::Hup)]);
}

/// The refusal that keeps `shep signal` from being a general `kill`: the
/// stranger is a real process in the table, and the sheep's own pid is not
/// its own lamb.
#[tokio::test(start_paused = true)]
async fn a_pid_outside_the_lamb_walk_is_refused_and_nothing_is_sent() {
    let h = harness_identifying(vec![ProcScript::never_exits()], process_table());
    start(&h, &["web"]).await;

    for pid in [STRANGER, ROOT, 31_337] {
        let rows = signal_lamb(&h, web(), pid).await.unwrap();
        assert_eq!(rows[0].outcome, SignalOutcome::NotALamb, "pid {pid}");
    }
    assert_eq!(h.pid_signals.sent(), Vec::new());
}

#[tokio::test(start_paused = true)]
async fn a_selector_matching_two_sheep_is_refused_and_nothing_is_sent() {
    let h = harness_identifying(
        vec![ProcScript::never_exits(), ProcScript::never_exits()],
        process_table(),
    );
    start(&h, &["web", "worker"]).await;

    let err = signal_lamb(&h, SelectorSpec::All, LAMB).await.unwrap_err();

    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(err.message.contains("matched 2"), "{}", err.message);
    assert_eq!(h.pid_signals.sent(), Vec::new());
}

#[tokio::test(start_paused = true)]
async fn a_selector_matching_no_sheep_is_not_found() {
    let h = harness_identifying(vec![], process_table());

    let err = signal_lamb(&h, web(), LAMB).await.unwrap_err();

    assert_eq!(err.code, RpcErrorCode::NotFound);
    assert_eq!(h.pid_signals.sent(), Vec::new());
}

#[tokio::test(start_paused = true)]
async fn a_sheep_that_is_not_running_has_no_lambs_to_signal() {
    let h = harness_identifying(vec![ProcScript::never_exits()], process_table());
    start(&h, &["web"]).await;
    reply_of(dispatch(envelope(3, Request::Stop { selector: web() }), &h.ctx).await);

    let rows = signal_lamb(&h, web(), LAMB).await.unwrap();

    assert_eq!(rows[0].outcome, SignalOutcome::NotRunning);
    assert_eq!(h.pid_signals.sent(), Vec::new());
}

#[tokio::test(start_paused = true)]
async fn the_sheep_hears_on_its_channel_which_lamb_was_signalled() {
    let h = harness_identifying(vec![ProcScript::never_exits()], process_table());
    start(&h, &["web"]).await;
    let mut io = h.runner.io_handles(0);

    signal_lamb(&h, web(), LAMB).await.unwrap();

    assert_eq!(
        io.to_child_rx.recv().await,
        Some(ShepherdMessage::LambSignalled {
            pid: LAMB,
            signal: "SIGHUP".to_string(),
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_refused_lamb_is_not_announced_on_the_channel() {
    let h = harness_identifying(vec![ProcScript::never_exits()], process_table());
    start(&h, &["web"]).await;
    let mut io = h.runner.io_handles(0);

    signal_lamb(&h, web(), STRANGER).await.unwrap();

    assert!(io.to_child_rx.try_recv().is_err());
}
