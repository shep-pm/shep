//! Watching the bus for a dog's own lifecycle events: recording an exhausted
//! restart budget as a local [`Bark`], and narrating a dog's spawn and exit
//! into its own log.

use std::path::{Path, PathBuf};

use shep_core::barks::{self, Bark};
use shep_core::config::AppConfig;
use shep_core::protocol::{BusEvent, ProcessEventKind};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::bus::{Bus, SharedEvent};

use super::narrate;

/// Watches the bus and records, locally, every enabled dog that exhausts its
/// restart budget, and writes each dog's spawn and exit into its own log.
///
/// The shepherd cannot deliver an alert about a dead bark dog: it has no sinks
/// and no webhook code, so what it guarantees is a local trail in `shep barks`.
/// Read from the bus rather than from the call sites: a `Start` on the bus is a
/// spawn that really happened, while `start_dog` answering `Ok` covers its
/// idempotent no-op too.
///
/// Its `JoinHandle` is held by the caller and aborted at teardown: the task
/// parks on a broadcast receiver.
pub fn spawn_dog_watch(
    mut events: broadcast::Receiver<SharedEvent>,
    publish: Bus,
    barks: PathBuf,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                // Only a dog's `Errored` earns a bark record: bark writes the
                // sheep ones itself, and one event with two authors in one file
                // is a history nobody can trust. `Exit` fires on every restart
                // a dog survives, so it stays out of the barks file.
                Ok(event) => {
                    let BusEvent::Process {
                        event: kind, info, ..
                    } = &*event
                    else {
                        continue;
                    };
                    if info.dog.is_none() {
                        continue;
                    }
                    match kind {
                        ProcessEventKind::Errored => {
                            record_dog_errored(&barks, &info.name, info.restarts);
                        }
                        ProcessEventKind::Start => {
                            let pid = info
                                .pid
                                .map_or_else(|| "unknown".to_string(), |pid| pid.to_string());
                            narrate(
                                &publish,
                                info,
                                &format!("shep started this dog; its process is pid {pid}"),
                            )
                            .await;
                        }
                        ProcessEventKind::Exit => {
                            narrate(&publish, info, &narrate::exit_words(info)).await;
                        }
                        _ => {}
                    }
                }
                // The bus drops events for a lagging subscriber, so a dog's
                // death notice may be among what this receiver just lost.
                // Metrics' `shep_dog_up` is the intended answer.
                Err(RecvError::Lagged(count)) => {
                    tracing::warn!(
                        count,
                        "the shepherd's dog watch dropped bus events; a dog's exhausted restart budget may have gone unrecorded"
                    );
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

/// Records `name`'s exhausted restart budget as a [`Bark`] the shepherd
/// wrote itself, and logs the same facts at `tracing::error!`.
///
/// `sinks` is left empty, which is how a [`Bark`] says the shepherd has no
/// webhook code of its own. [`super::dog_app`] never overrides `max_restarts`,
/// so `AppConfig::default().max_restarts` is the exhausted budget for every
/// dog.
fn record_dog_errored(barks_path: &Path, name: &str, restarts: u32) {
    let budget = AppConfig::default().max_restarts;
    tracing::error!(dog = %name, restarts, budget, "a dog exhausted its restart budget");
    let bark = Bark {
        at_ms: crate::now_ms(),
        rule: "daemon".to_string(),
        subject: name.to_string(),
        message: format!(
            "dog {name} exhausted its restart budget: {restarts} restarts against a budget of {budget}"
        ),
        sinks: Vec::new(),
    };
    if let Err(err) = barks::append(barks_path, &bark, barks::DEFAULT_MAX_BYTES) {
        tracing::warn!(%err, dog = %name, "failed to record a dog's exhausted restart budget");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shep_core::protocol::{DogSource, ProcessInfo};
    use shep_core::status::ProcStatus;

    /// A minimal `Process` bus event, `name` carrying either a sheep's or a
    /// dog's entry depending on `dog`.
    fn process_event(name: &str, kind: ProcessEventKind, dog: Option<DogSource>) -> SharedEvent {
        SharedEvent::new(BusEvent::Process {
            event: kind,
            info: ProcessInfo::builder(1, name, ProcStatus::Errored)
                .restarts(16)
                .dog(dog)
                .build(),
            manually: false,
            at_ms: 1_700_000_000_000,
        })
    }

    fn errored_event(name: &str, dog: Option<DogSource>) -> SharedEvent {
        process_event(name, ProcessEventKind::Errored, dog)
    }

    /// Polls `path` under a real timeout until it holds at least `n` barks:
    /// the watcher writing to it runs as a separate task, so a bare read races
    /// it.
    async fn await_barks(path: &std::path::Path, n: usize) -> Vec<Bark> {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let found = barks::read(path).unwrap_or_default();
                if found.len() >= n {
                    return found;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("barks.jsonl never reached the expected record count")
    }

    /// Both halves are needed: without the negative assertion, a watcher that
    /// recorded every `Errored` passes.
    #[tokio::test]
    async fn the_shepherd_records_a_dog_that_gave_up_and_leaves_the_sheep_to_bark() {
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");
        let (events, rx) = crate::bus::test_bus(16);
        let watch = spawn_dog_watch(rx, events.clone(), barks_path.clone());

        events.send(errored_event("web", None)).unwrap();
        events
            .send(errored_event("bark", Some(DogSource::BuiltIn)))
            .unwrap();

        let recorded = await_barks(&barks_path, 1).await;
        assert_eq!(recorded.len(), 1, "one record, and it is the dog's");
        assert_eq!(recorded[0].subject, "bark");
        assert_eq!(recorded[0].rule, "daemon");
        assert!(
            recorded[0].sinks.is_empty(),
            "the shepherd has no sinks and says so by carrying none"
        );

        watch.abort();
    }

    #[tokio::test]
    async fn a_dog_that_merely_exited_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let barks_path = dir.path().join("barks.jsonl");
        let (events, rx) = crate::bus::test_bus(16);
        let watch = spawn_dog_watch(rx, events.clone(), barks_path.clone());

        events
            .send(process_event(
                "bark",
                ProcessEventKind::Exit,
                Some(DogSource::BuiltIn),
            ))
            .unwrap();
        // A real `Errored` after it proves the watcher was listening at all:
        // without it, a watcher that recorded nothing would pass.
        events
            .send(errored_event("bark", Some(DogSource::BuiltIn)))
            .unwrap();

        let recorded = await_barks(&barks_path, 1).await;
        assert_eq!(
            recorded.len(),
            1,
            "the Exit left no record; only the Errored that followed it did"
        );

        watch.abort();
    }
}
