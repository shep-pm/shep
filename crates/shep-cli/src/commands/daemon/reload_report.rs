//! Post-reload reporting: muster or flock status, the dog-settle wait, and
//! the sentences that tell an operator which dogs did not come back.

use shep_client::Client;
use shep_core::paths::ShepPaths;

use crate::commands::{dogs, muster};
use crate::exit::ExitCode;
use crate::output::Streams;

/// Reports the shepherd now serving, then what happened to each sheep.
///
/// The handover arm does not stop the flock, so nothing here may announce that
/// it did or assume a sheep has a new pid.
///
/// `restored` says which arm ran. Under the handover arm `ListFlock` asks for
/// nothing: a successor answers its socket before its rehydrate has finished.
///
/// `paths` is where `shep.toml` is read from, to tell an adopted dog its entry
/// changed. See [`report_kept_dog_entries`].
pub(super) async fn report_reload(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    restored: bool,
) -> ExitCode {
    report_reload_waiting(client, streams, paths, restored, DOG_SETTLE_WAIT).await
}

/// As [`report_reload`], but with a caller-chosen dog wait.
async fn report_reload_waiting(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
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
    // After the settle wait, so the dogs are registered to be asked about. Not
    // under the stop arm: every dog it runs was spawned a moment ago from the
    // entry `shep.toml` holds now.
    if !restored {
        report_kept_dog_entries(client, streams, paths).await;
    }
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
/// process running when it connected. Silent unless something is wrong.
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

/// Names each running adopted dog that was carried across the handover with an
/// entry other than the one `shep.toml` now holds.
///
/// The handover restarts no dog, so a changed `path` or `env` stays unapplied
/// until the dog is disabled and enabled. Silent unless something differs.
async fn report_kept_dog_entries(client: &Client, streams: &mut Streams<'_>, paths: &ShepPaths) {
    for kept in dogs::kept_entries(client, paths, None).await {
        streams.aside("reload", &dogs::kept_entry_report(&kept));
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

/// The sentence for dogs that had not answered within the settle wait.
///
/// Silence would read like a clean reload: the reading was taken before
/// these dogs answered, so it speaks for the rest of the flock.
///
/// A dog stuck on a protocol it cannot speak lands here rather than in
/// [`stale_dog_report`].
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
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            report_reload_waiting(
                &client,
                &mut streams,
                &ShepPaths::resolve(&|_| None, dir.path()),
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
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            report_reload_waiting(
                &client,
                &mut streams,
                &ShepPaths::resolve(&|_| None, dir.path()),
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
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            // The forcing mechanism as well as the assertion: a budget that
            // is never consulted would hang the suite instead of failing.
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                report_reload_waiting(
                    &client,
                    &mut streams,
                    &ShepPaths::resolve(&|_| None, dir.path()),
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

    /// Runs the handover arm's report against a shepherd that carried `kelpie`
    /// across, spawned with `spawned_env` while `shep.toml` now holds
    /// `entry_env`, and hands back stderr.
    async fn handover_report(entry_env: &str, spawned_env: &[(&str, &str)]) -> String {
        use crate::commands::dogs::entry_drift_support::{home_and_spawned, shepherd_running};

        let dir = tempfile::tempdir().unwrap();
        let (paths, spawned) = home_and_spawned(dir.path(), entry_env, spawned_env);
        let addr = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = shep_client::testing::fake_client_answering(
            &addr,
            shepherd_running(spawned, shep_core::status::ProcStatus::Online),
        )
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            report_reload_waiting(
                &client,
                &mut streams,
                &paths,
                false,
                std::time::Duration::from_millis(50),
            )
            .await;
        }
        String::from_utf8(err).unwrap()
    }

    #[tokio::test]
    async fn a_reload_names_a_running_adopted_dog_whose_env_gained_a_key() {
        let text = handover_report("PADDOCK_KEY = \"{{secret:paddock-key}}\"", &[]).await;

        assert!(
            text.contains("`kelpie`")
                && text.contains("`env`")
                && text.contains("shep disable kelpie")
                && text.contains("shep enable kelpie"),
            "the dog, the field and the way to apply it must be named: {text}"
        );
        assert!(
            !text.contains("paddock-key"),
            "a value, or a reference standing for one, must never be printed: {text}"
        );
    }

    #[tokio::test]
    async fn a_reload_with_an_adopted_dogs_entry_unchanged_says_nothing_about_it() {
        let text = handover_report(
            "PADDOCK_KEY = \"{{secret:paddock-key}}\"",
            &[("PADDOCK_KEY", "{{secret:paddock-key}}")],
        )
        .await;

        assert!(
            !text.contains("kelpie"),
            "a dog already running its entry has nothing to be told: {text}"
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
