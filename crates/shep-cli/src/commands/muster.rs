//! `save`/`muster`: write the muster roll now, and assemble the flock from
//! it on demand.
//!
//! Neither `Request::SaveRoll` nor `Request::Muster` carries fields: the roll
//! always covers the whole flock, so there is no selector to parse. Two call
//! sites, each inlining its own match rather than sharing
//! `commands::query`'s `request_and_render`.

use shep_client::{Client, RELOAD_DEADLINE};
use shep_core::protocol::{Request, Response};
use shep_core::status::ProcStatus;

use crate::cli::Format;
use crate::exit::ExitCode;
use crate::flourish;
use crate::output::{FlockRows, SavedRollRow, Streams, emit, write_outcome};

/// Asks the daemon to write the muster roll now, and reports where it landed
/// and how many apps it recorded. A failed save is loud, never a no-op.
pub async fn save(client: &Client, streams: &mut Streams<'_>) -> ExitCode {
    match client.request(Request::SaveRoll).await {
        Ok(Response::RollSaved { path, apps }) => write_outcome(emit(
            &mut *streams.out,
            streams.fmt,
            "save",
            SavedRollRow { file: path, apps },
            streams.style,
        )),
        Ok(_unrecognised) => {
            let message = "the daemon answered with a response this client does not understand";
            streams.fail(ExitCode::Internal, message)
        }
        Err(err) => {
            let code = ExitCode::from(&err);
            streams.fail(code, &err.to_string())
        }
    }
}

/// Asks the daemon to assemble the flock from the muster roll `save` wrote,
/// and reports who came up. Autostart reaches this through
/// `connect_or_spawn_client`; boot already restores the roll by then, so
/// this spawns nothing new.
///
/// Sent with [`RELOAD_DEADLINE`], which is 60s, the daemon's own
/// `MAX_DEADLINE_MS` and the most it will honour. A cold restore already
/// outran the client's 5s default, and `Request::Muster` now runs the staged
/// walk inside the request handler: it holds each stage until its members
/// settle, so the reply costs the SUM of the stages rather than the longest
/// spawn in the roll. `START_DEADLINE`'s 30s is the budget `shep reload` was
/// moved off for the same reason, and this walk is the largest of them, since
/// a muster spans the whole roll rather than a selector. `with_deadline`
/// DROPS the work future when the budget expires, so a short one abandons a
/// restore mid-walk with its later stages never issued and reports failure
/// for a flock that was coming up.
///
/// `Response::Mustered` names every sheep the roll restored, not only the
/// ones this call spawned, so the verb is safe to run twice. An empty
/// `Mustered` gets a stderr notice beside the empty table.
pub async fn muster(client: &Client, streams: &mut Streams<'_>) -> ExitCode {
    match client
        .request_with_deadline(Request::Muster, Some(RELOAD_DEADLINE))
        .await
    {
        Ok(Response::Mustered(procs)) => {
            if procs.is_empty() {
                streams.aside(
                    "muster_restored_nothing",
                    "the muster roll restored nothing",
                );
            }
            // Read before `procs` moves into `FlockRows`. Built from real
            // statuses, so it cannot disagree with the table above it.
            let statuses: Vec<ProcStatus> = procs.iter().map(|p| p.status).collect();
            let outcome = write_outcome(emit(
                &mut *streams.out,
                streams.fmt,
                "muster",
                FlockRows(procs),
                streams.style,
            ));
            if streams.fmt == Format::Table && !statuses.is_empty() && streams.style.level.sheep() {
                let _ = write!(streams.out, "{}", flourish::mustered(&statuses));
            }
            outcome
        }
        Ok(_unrecognised) => {
            let message = "the daemon answered with a response this client does not understand";
            streams.fail(ExitCode::Internal, message)
        }
        Err(err) => {
            let code = ExitCode::from(&err);
            streams.fail(code, &err.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_client::testing::{
        fake_client_capturing_envelopes, fake_client_on, fake_client_replying_err, sample_info,
    };
    use shep_core::protocol::{BusEvent, ProcessEventKind, RpcErrorCode};

    use super::*;

    /// Bounds every `envelopes.recv()` here: a mutation that skips the
    /// request must fail with a named assertion rather than hang.
    const FAKE_REPLY_WAIT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn save_sends_save_roll_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let _ = save(&client, &mut streams).await;

        let envelope = tokio::time::timeout(FAKE_REPLY_WAIT, envelopes.recv())
            .await
            .expect("the fake daemon must answer inside the bound")
            .unwrap();
        assert_eq!(envelope.body, Request::SaveRoll);
    }

    #[tokio::test]
    async fn a_failed_save_exits_non_zero_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _served) = fake_client_replying_err(
            &path,
            RpcErrorCode::Internal,
            "the supervisor engine has stopped; no roll was written",
        )
        .await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            save(&client, &mut streams).await
        };
        assert_eq!(code, ExitCode::Internal);
        assert!(out.is_empty(), "a failed save prints no success table");
        assert!(String::from_utf8(err).unwrap().contains("engine"));
    }

    /// fails if the verb goes back to a budget that predates the staged
    /// walk. `Request::Muster` runs the walk inside the request handler and
    /// `with_deadline` drops the work future, so a short budget abandons a
    /// restore that is proceeding and reports it as a failure.
    #[tokio::test]
    async fn muster_asks_for_the_daemons_whole_budget_because_it_walks_stages() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let _ = muster(&client, &mut streams).await;

        let envelope = tokio::time::timeout(FAKE_REPLY_WAIT, envelopes.recv())
            .await
            .expect("the fake daemon must answer inside the bound")
            .unwrap();
        assert_eq!(envelope.body, Request::Muster);
        assert_eq!(
            envelope.deadline_ms,
            Some(u64::try_from(RELOAD_DEADLINE.as_millis()).unwrap())
        );
    }

    #[tokio::test]
    async fn a_muster_that_restored_nothing_says_so_on_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_on(&path).await;
        // The one `shep_client::testing` helper that answers an arbitrary
        // `Response`. Its trailing event lands unread on a wire this call
        // never subscribes to.
        daemon.queue_reply_then_event(
            Response::Mustered(Vec::new()),
            BusEvent::Process {
                event: ProcessEventKind::Online,
                info: sample_info(),
                manually: true,
                at_ms: 0,
            },
        );
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            muster(&client, &mut streams).await
        };
        assert_eq!(code, ExitCode::Success, "an empty roll is not a failure");
        assert!(
            !err.is_empty(),
            "an empty muster must not be a silent success"
        );
    }
}
