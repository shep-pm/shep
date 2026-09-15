//! Shep's own narration into a dog's log: what shep itself has to say about a
//! dog, written as a line in that dog's own log and published to whoever is
//! following it live.

use std::path::Path;

use shep_core::protocol::{BusEvent, ProcessInfo};

use crate::bus::Bus;
use crate::supervisor::SupervisorHandle;

/// The marker every line shep writes into a dog's own log begins with, once
/// the timestamp is past.
///
/// A dog's log is the dog's voice and shep writes into that file too, so the
/// file has to say which lines are whose. Short and bracketed because it sits
/// behind a 30-character timestamp.
const SHEP_VOICE: &str = "[shep]";

/// Writes one line of shep's own narration into `info`'s log, and publishes
/// it to whoever is following that log live.
///
/// The file is written directly rather than through the pump, which ends when
/// its sheep's streams reach EOF, before there is anything to say about how the
/// dog exited. Safe because [`open_append`] opens with `O_APPEND`: every write
/// seeks to end atomically, so the whole line is assembled and written in one
/// call. The cost is ordering, since a narration line can land ahead of dog
/// output still in the pump's buffer, bounded by `IDLE_FLUSH`.
///
/// A dog with no `err_file` still reaches a live follower.
pub(crate) async fn narrate(events: &Bus, info: &ProcessInfo, message: &str) {
    let line = format!("{SHEP_VOICE} {message}");
    if let Some(path) = &info.err_file {
        let mut written = String::with_capacity(line.len() + 32);
        shep_core::logstamp::stamp_into(&mut written);
        written.push_str(&line);
        written.push('\n');
        // A failed open is already logged by `open_append`; a failed write is
        // not. Neither is propagated: a log shep cannot write to must not
        // change what shep does about the dog.
        if let Ok(mut file) = crate::tokio_runner::open_append(Path::new(path)).await {
            use tokio::io::AsyncWriteExt as _;
            // Taken after the open: the pump waits on this lock for every line
            // it writes, so holding it across a filesystem open would stall a
            // sheep's output. Held across the write and the flush together.
            let _record = crate::tokio_runner::record_lock(Path::new(path))
                .lock_owned()
                .await;
            // `tokio::fs::File` hands the real `write(2)` to the blocking pool
            // and does not flush on drop, so `write_all` returning means the
            // bytes were accepted rather than written.
            let written = async {
                file.write_all(written.as_bytes()).await?;
                file.flush().await
            }
            .await;
            if let Err(error) = written {
                tracing::warn!(
                    dog = %info.name,
                    %error,
                    "shep's own narration did not reach this dog's log"
                );
            }
        }
    }
    events.publish_log(BusEvent::LogErr { id: info.id, line });
}

/// `narrate`, for a caller that knows a dog's name and not its listing.
///
/// Spawned rather than awaited: both callers are connection handlers
/// mid-handshake, and neither may be held up by a listing round trip and a
/// file open. A name that does not resolve to a dog is silently nothing.
pub(crate) fn narrate_by_name(
    supervisor: &SupervisorHandle,
    events: &Bus,
    name: &str,
    message: String,
) {
    let supervisor = supervisor.clone();
    let events = events.clone();
    let name = name.to_string();
    tokio::spawn(async move {
        let Ok(infos) = supervisor.list_checked().await else {
            return;
        };
        if let Some(info) = infos
            .iter()
            .find(|info| info.name == name && info.dog.is_some())
        {
            narrate(&events, info, &message).await;
        }
    });
}

/// How a dog's process stopped existing, in the plainest words there are.
///
/// A signal number rather than a name, the rule
/// [`ExitInfo::signal`](shep_core::protocol::ExitInfo::signal) states for
/// itself: a dog's log is read next to `journalctl`.
pub(super) fn exit_words(info: &ProcessInfo) -> String {
    match info.last_exit {
        Some(exit) => match (exit.code, exit.signal) {
            (Some(code), _) => format!("this dog's process exited with code {code}"),
            (None, Some(signal)) => {
                format!("this dog's process was killed by signal {signal}")
            }
            (None, None) => {
                "this dog's process stopped, and the OS reported neither an exit code nor a signal"
                    .to_string()
            }
        },
        // Reachable rather than defensive: `last_exit` is `None` when the peer
        // that built this listing predates the field.
        None => "this dog's process stopped, and this shepherd has no record of how".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::*;
    use crate::fake::ProcScript;

    use super::super::test_support::start_test_dog;

    /// Fails if shep's own account of a dog stays in `shepd.err.log`, where the
    /// dog's operator was never told to look.
    ///
    /// Both halves are asserted: the file is what survives to be read
    /// afterwards, and the bus is what a `shep bleats --follow` sees live.
    #[tokio::test]
    async fn shep_s_own_account_of_a_dog_reaches_that_dog_s_log() {
        let h = crate::testing::harness(vec![ProcScript::never_exits()]);
        start_test_dog(&h.ctx, "log-rotate").await;
        let info = h
            .ctx
            .supervisor
            .list()
            .await
            .into_iter()
            .find(|info| info.name == "log-rotate")
            .expect("the dog fixture must be listed");
        let err_log = info
            .err_file
            .clone()
            .expect("a dog's log paths are resolved");

        // A real `log.*` forwarder: `Bus::publish_log` skips the whole publish
        // while nothing has registered an interest in log topics, so a plain
        // `subscribe()` would assert the gate is shut. Registered first,
        // because a broadcast receiver starts at the channel's current tail.
        let (out_tx, mut following) = tokio::sync::mpsc::channel(16);
        let forwarder = crate::bus::spawn_forwarder(
            &h.ctx.events,
            crate::bus::TopicFilter::new(&["log.*".to_string()]).unwrap(),
            out_tx,
        );

        narrate(&h.ctx.events, &info, "shep did a thing worth saying").await;

        let written = std::fs::read_to_string(&err_log).expect("the narration must reach the log");
        let line = written
            .strip_suffix('\n')
            .expect("one whole line, newline included");
        assert!(
            line.ends_with("[shep] shep did a thing worth saying"),
            "the line must be marked as shep's voice, not the dog's: {line:?}"
        );
        let (stamp, rest) = line.split_at(shep_core::logstamp::LOG_STAMP_BYTES);
        assert_eq!(
            rest, "[shep] shep did a thing worth saying",
            "the stamp is the same fixed-width prefix every other line carries: {line:?}"
        );
        chrono::DateTime::parse_from_rfc3339(stamp.trim_end())
            .unwrap_or_else(|err| panic!("{stamp:?} must parse as RFC 3339: {err}"));

        let frame = tokio::time::timeout(Duration::from_secs(5), following.recv())
            .await
            .expect("a follower must be told inside the budget")
            .expect("the forwarder must deliver rather than end");
        match shep_core::protocol::decode_frame::<BusEvent>(&frame).unwrap() {
            BusEvent::LogErr { id, line } => {
                assert_eq!(id, info.id, "the line belongs to the dog it is about");
                assert_eq!(
                    line, "[shep] shep did a thing worth saying",
                    "a follower sees the marker and not the file's stamp"
                );
            }
            other => panic!("narration must reach a follower as a log line, got {other:?}"),
        }
        forwarder.abort();
    }
}
