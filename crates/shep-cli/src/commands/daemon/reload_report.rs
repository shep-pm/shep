//! Post-reload reporting: muster or flock status, the dog-settle wait, and
//! the sentences that tell an operator which dogs did not come back.

use shep_client::Client;

use crate::commands::muster;
use crate::exit::ExitCode;
use crate::output::Streams;

/// Reports the shepherd now serving, then what happened to each sheep.
///
/// The handover arm does not stop the flock, so nothing here may announce that
/// it did or assume a sheep has a new pid.
///
/// `restored` says which arm ran, and decides how the flock is asked for.
/// Under the stop arm the successor has already restored the roll, so
/// `Request::Muster` spawns nothing new. Under the handover arm it must not
/// muster: a successor answers its socket as soon as the listener is carried,
/// before its rehydrate has finished, so `ListFlock` asks for nothing.
pub(super) async fn report_reload(
    client: &Client,
    streams: &mut Streams<'_>,
    restored: bool,
) -> ExitCode {
    report_reload_waiting(client, streams, restored, DOG_SETTLE_WAIT).await
}

/// As [`report_reload`], but with a caller-chosen dog wait.
async fn report_reload_waiting(
    client: &Client,
    streams: &mut Streams<'_>,
    restored: bool,
    dog_wait: std::time::Duration,
) -> ExitCode {
    let shepherd = client.daemon();
    let message = format!(
        "the shepherd is now {} (pid {})",
        shepherd.daemon_version, shepherd.pid
    );
    streams.aside("reload", &message);
    report_dog_staleness(client, streams, &shepherd.daemon_version, dog_wait).await;
    if restored {
        return muster::muster(client, streams).await;
    }
    crate::commands::query::flock(client, streams).await
}

/// How long a reload waits for the flock's dogs to finish reconnecting.
///
/// Three seconds, sized against the round trip it has to outlast: a dog
/// refused on the handshake is restarted once from the binary on disk, and
/// only the second refusal, after a full kill ladder and a fresh spawn, makes
/// it stale. Paid only when a dog has not answered.
const DOG_SETTLE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Gap between [`report_dog_staleness`]'s asks. Coarser than
/// `reload::SUCCESSOR_POLL_INTERVAL`: this waits on a process being killed
/// and respawned rather than an `execve`.
const DOG_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Reports the dogs that could not come back, once the shepherd has heard
/// from all of them.
///
/// The waiting is the point: a dog's recorded crate version describes the
/// process that was running when it connected, so a reading taken before the
/// reload answers for the wrong daemon.
///
/// Silent unless something is wrong. A shepherd that will not answer is left
/// alone: the reload has already succeeded by the time this runs.
async fn report_dog_staleness(
    client: &Client,
    streams: &mut Streams<'_>,
    daemon_version: &str,
    wait: std::time::Duration,
) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let Ok(shep_core::protocol::Response::DogStaleness { stale, pending }) = client
            .request(shep_core::protocol::Request::DogStaleness)
            .await
        else {
            return;
        };
        let out_of_time = tokio::time::Instant::now() >= deadline;
        if pending.is_empty() || out_of_time {
            if !stale.is_empty() {
                streams.aside("reload", &stale_dog_report(&stale, daemon_version));
            }
            if out_of_time && !pending.is_empty() {
                streams.aside("reload", &unsettled_dog_report(&pending, wait));
            }
            return;
        }
        tokio::time::sleep(DOG_POLL_INTERVAL).await;
    }
}

/// The sentence naming the dogs this shepherd has given up on.
///
/// Two whole sentences rather than one with the number interpolated: the
/// singular and the plural differ in four places. It prescribes no remedy,
/// because `shep_daemon::dogs::DogRefusals::stale` holds both a dog refused
/// twice, where a rebuild is the fix, and one that never spoke, where it may
/// not be. Only the dog's own log tells them apart.
fn stale_dog_report(stale: &[String], daemon_version: &str) -> String {
    match stale {
        [only] => format!(
            "the `{only}` dog cannot talk to this shepherd; restarting it from the binary on \
             disk did not help, so shep has given up and will not restart it again. \
             `shep bleats {only}` holds what shep saw when it gave up, and the fix follows from \
             that -- reinstalling the same build is not always it. This shepherd is shep \
             {daemon_version}, if a rebuild is what that log calls for"
        ),
        many => format!(
            "these dogs cannot talk to this shepherd: {}; restarting them from the binaries on \
             disk did not help, so shep has given up and will not restart them again. \
             `shep bleats <dog>` holds what shep saw when it gave up on each, and the fix \
             follows from that -- reinstalling the same build is not always it. This shepherd \
             is shep {daemon_version}, if a rebuild is what those logs call for",
            quoted_names(many)
        ),
    }
}

/// The sentence for dogs that had not answered within the reload's settle
/// wait.
///
/// Silence would read like a clean reload: the reading was taken before these
/// dogs answered, so it speaks for the rest of the flock and not for them.
///
/// The ladder restarts a silent dog at
/// [`shep_daemon::dogs::DOG_SILENCE_BUDGET`] and marks it stale five seconds
/// later, both after this reload's own wait, so a dog stuck on a protocol it
/// cannot speak lands here rather than in [`stale_dog_report`].
fn unsettled_dog_report(pending: &[String], wait: std::time::Duration) -> String {
    let budget = shep_daemon::dogs::DOG_SILENCE_BUDGET;
    match pending {
        [only] => format!(
            "the `{only}` dog has not answered this shepherd after {wait:?}; a dog silent \
             past {budget:?} is restarted once from the binary on disk and then reported \
             stale, and `shep bleats {only}` shows why"
        ),
        many => format!(
            "these dogs have not answered this shepherd after {wait:?}: {}; a dog silent past \
             {budget:?} is restarted once from the binary on disk and then reported stale, and \
             `shep bleats <dog>` shows why for each",
            quoted_names(many)
        ),
    }
}

/// `` `a`, `b`, `c` ``: the list shape both reports end on.
fn quoted_names(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Format;
    use shep_core::protocol::{Request, Response};

    /// The shepherd here reports `metrics` as unsettled twice and stale on the
    /// third ask, so a reload that asked once would pass the output check.
    #[tokio::test]
    async fn a_reload_waits_for_a_pending_dog_before_it_reports_staleness() {
        let dir = tempfile::tempdir().unwrap();
        let addr = shep_client::testing::control_address(dir.path());
        let asks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&asks);
        let (client, _envelopes) = shep_client::testing::fake_client_answering(&addr, move |req| {
            if !matches!(req, Request::DogStaleness) {
                return Response::Mustered(vec![]);
            }
            let seen = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if seen < 2 {
                Response::DogStaleness {
                    stale: vec![],
                    pending: vec!["metrics".to_string()],
                }
            } else {
                Response::DogStaleness {
                    stale: vec!["metrics".to_string()],
                    pending: vec![],
                }
            }
        })
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            report_reload_waiting(
                &client,
                &mut streams,
                true,
                std::time::Duration::from_secs(3),
            )
            .await;
        }

        let text = String::from_utf8(err).unwrap();
        assert!(
            text.contains("metrics") && text.contains("shep has given up"),
            "the dog that could not come back must be named: {text}"
        );
        assert!(
            asks.load(std::sync::atomic::Ordering::SeqCst) >= 3,
            "an answer taken on the first ask is a claim about a dog that had not spoken"
        );
    }

    #[tokio::test]
    async fn a_reload_whose_dogs_all_answered_says_nothing_about_them() {
        let dir = tempfile::tempdir().unwrap();
        let addr = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = shep_client::testing::fake_client_answering(&addr, |req| {
            if matches!(req, Request::DogStaleness) {
                Response::DogStaleness {
                    stale: vec![],
                    pending: vec![],
                }
            } else {
                Response::Mustered(vec![shep_client::testing::sample_info()])
            }
        })
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            report_reload_waiting(
                &client,
                &mut streams,
                true,
                std::time::Duration::from_secs(3),
            )
            .await;
        }

        let text = format!(
            "{}{}",
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap()
        );
        assert!(
            !text.contains("dog"),
            "a flock whose dogs all came back has nothing to say about them: {text}"
        );
    }

    #[tokio::test]
    async fn a_reload_stops_waiting_for_a_dog_that_never_answers() {
        let dir = tempfile::tempdir().unwrap();
        let addr = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = shep_client::testing::fake_client_answering(&addr, |req| {
            if matches!(req, Request::DogStaleness) {
                Response::DogStaleness {
                    stale: vec![],
                    pending: vec!["metrics".to_string()],
                }
            } else {
                Response::Mustered(vec![])
            }
        })
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            // The forcing mechanism as well as the assertion: a budget that
            // is never consulted would hang the suite instead of failing.
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                report_reload_waiting(
                    &client,
                    &mut streams,
                    true,
                    std::time::Duration::from_millis(150),
                ),
            )
            .await
            .expect("a dog that never answers must not hold the verb open");
        }

        let text = String::from_utf8(err).unwrap();
        assert!(
            text.contains("metrics") && text.contains("shep bleats metrics"),
            "an unanswered dog is reported as unanswered, not as healthy: {text}"
        );
    }

    /// Pinned as an exact string in both shapes, since the singular and the
    /// plural are written out separately.
    #[test]
    fn the_stale_report_says_what_happened_and_never_reads_the_disk() {
        let one = stale_dog_report(&["metrics".to_string()], "0.1.22");
        assert_eq!(
            one,
            "the `metrics` dog cannot talk to this shepherd; restarting it from the binary on \
             disk did not help, so shep has given up and will not restart it again. \
             `shep bleats metrics` holds what shep saw when it gave up, and the fix follows \
             from that -- reinstalling the same build is not always it. This shepherd is shep \
             0.1.22, if a rebuild is what that log calls for"
        );

        let two = stale_dog_report(&["bark".to_string(), "metrics".to_string()], "0.1.22");
        assert_eq!(
            two,
            "these dogs cannot talk to this shepherd: `bark`, `metrics`; restarting them from \
             the binaries on disk did not help, so shep has given up and will not restart them \
             again. `shep bleats <dog>` holds what shep saw when it gave up on each, and the \
             fix follows from that -- reinstalling the same build is not always it. This \
             shepherd is shep 0.1.22, if a rebuild is what those logs call for"
        );
    }

    /// Pinned as an exact string in both shapes.
    #[test]
    fn the_unsettled_report_says_what_to_check_and_never_claims_a_verdict() {
        let one = unsettled_dog_report(&["metrics".to_string()], std::time::Duration::from_secs(3));
        assert_eq!(
            one,
            "the `metrics` dog has not answered this shepherd after 3s; a dog silent past 5s \
             is restarted once from the binary on disk and then reported stale, and `shep \
             bleats metrics` shows why"
        );

        let two = unsettled_dog_report(
            &["bark".to_string(), "metrics".to_string()],
            std::time::Duration::from_secs(3),
        );
        assert_eq!(
            two,
            "these dogs have not answered this shepherd after 3s: `bark`, `metrics`; a dog \
             silent past 5s is restarted once from the binary on disk and then reported \
             stale, and `shep bleats <dog>` shows why for each"
        );
    }
}
