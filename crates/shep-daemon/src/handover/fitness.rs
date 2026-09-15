//! The carryability gate: whether a flock can be replaced in place.
//!
//! Whole-flock, and it refuses whole. The blob describes one process image,
//! so one sheep that cannot be carried sends the flock down the
//! stop-and-start arm instead. One refusal lives here: a live sheep whose log
//! pump did not report its descriptors before the snapshot's deadline.

use crate::entry::ProcessEntry;

/// Whether a flock can be handed over in place, or must fall back to a
/// stop-and-start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fitness {
    /// Every sheep in the flock is carryable.
    Carryable,
    /// At least one sheep is not carryable, and why.
    Refused(RefusedReason),
}

/// Why a flock cannot be handed over in place. The caller falls back to the
/// stop arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefusedReason {
    /// The sheep's log pump did not report its descriptors before the
    /// snapshot's deadline, so nothing knows which descriptors it holds.
    PumpUnresponsive {
        /// The sheep's name.
        sheep: String,
    },
}

impl core::fmt::Display for RefusedReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Load-bearing text: `cli_e2e` probes for "falls back to a
        // stop-and-start" to tell a carried flock from a stopped one.
        match self {
            Self::PumpUnresponsive { sheep } => write!(
                f,
                "sheep '{sheep}' has a log pump that did not report its descriptors in \
                 time; reload falls back to a stop-and-start instead"
            ),
        }
    }
}

/// One sheep's carryability-relevant facts: a [`ProcessEntry`] plus the one
/// fact that does not live on it.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    /// The sheep's lifecycle entry.
    pub entry: &'a ProcessEntry,
    /// Whether this sheep's log pump was asked for its descriptors and did
    /// not answer in time.
    ///
    /// Distinct from [`CarriedFds::none`](super::CarriedFds::none), which is
    /// what a stopped sheep reports: a wedged live pump collapsed into it is
    /// carried with its descriptors silently dropped.
    pub pump_unresponsive: bool,
}

/// A [`Candidate`] that owns its entry.
///
/// Snapshot assembly awaits every log pump, which may not happen on the actor
/// loop, so it runs on a task of its own and the entries travel there.
#[derive(Debug, Clone)]
pub struct OwnedCandidate {
    /// The sheep's lifecycle entry, cloned off the supervisor's slot.
    pub entry: ProcessEntry,
    /// Whether this sheep's log pump missed the snapshot's deadline; see
    /// [`Candidate::pump_unresponsive`].
    pub pump_unresponsive: bool,
}

impl OwnedCandidate {
    /// Borrow this as the [`Candidate`] [`fitness`] takes.
    #[must_use]
    pub fn as_candidate(&self) -> Candidate<'_> {
        Candidate {
            entry: &self.entry,
            pump_unresponsive: self.pump_unresponsive,
        }
    }
}

/// Decide whether a flock can be handed over in place.
///
/// Whole-flock: the blob describes one process image, so a flock is carried
/// whole or refused whole. An empty flock is carryable.
#[must_use]
pub fn fitness(sheep: &[Candidate<'_>]) -> Fitness {
    for candidate in sheep {
        if let Some(reason) = refusal(candidate) {
            return Fitness::Refused(reason);
        }
    }
    Fitness::Carryable
}

/// Why `candidate` alone refuses the flock, if it does.
fn refusal(candidate: &Candidate<'_>) -> Option<RefusedReason> {
    let entry = candidate.entry;
    let config = entry.spec.config();
    let name = || config.name.clone();

    if candidate.pump_unresponsive {
        return Some(RefusedReason::PumpUnresponsive { sheep: name() });
    }
    None
}

#[cfg(test)]
mod tests {
    use shep_core::protocol::DogSource;

    use super::{Candidate, Fitness, fitness};
    use crate::handover::fixtures::{entry_fixture, plain, wedged};

    #[test]
    fn a_plain_sheep_is_carryable() {
        let e = entry_fixture(|_| {});
        assert_eq!(fitness(&[plain(&e)]), Fitness::Carryable);
    }

    #[test]
    fn one_unsupported_sheep_refuses_the_whole_flock() {
        let carryable = entry_fixture(|_| {});
        let unsupported = entry_fixture(|_| {});
        assert!(matches!(
            fitness(&[plain(&carryable), wedged(&unsupported)]),
            Fitness::Refused(_)
        ));
    }

    #[test]
    fn the_refusal_names_which_sheep_and_why() {
        let unsupported = entry_fixture(|_| {});
        let Fitness::Refused(r) = fitness(&[wedged(&unsupported)]) else {
            panic!("expected a refusal")
        };
        let text = r.to_string();
        assert!(text.contains("did not report its descriptors"), "{text}");
        assert!(text.contains("web"), "{text}");
        assert!(
            text.contains("falls back to a stop-and-start"),
            "the refusal must say what happens instead, not only that it declined: {text}"
        );
    }

    #[test]
    fn a_pump_that_did_not_report_in_time_refuses_as_a_fault_not_a_feature() {
        let e = entry_fixture(|_| {});
        let candidate = Candidate {
            entry: &e,
            pump_unresponsive: true,
        };
        let Fitness::Refused(r) = fitness(&[candidate]) else {
            panic!("a sheep whose descriptors are unknown cannot be carried")
        };
        let text = r.to_string();
        assert!(
            text.contains("did not report its descriptors in time"),
            "{text}"
        );
        assert!(
            !text.contains("cannot yet"),
            "a wedged pump is not a feature a later phase ships: {text}"
        );
    }

    #[test]
    fn an_empty_flock_is_carryable() {
        assert_eq!(fitness(&[]), Fitness::Carryable);
    }

    #[test]
    fn a_sheep_with_a_channel_is_carried() {
        let e = entry_fixture(|app| app.channel = true);
        assert_eq!(fitness(&[plain(&e)]), Fitness::Carryable);
    }

    #[test]
    fn wait_ready_alone_is_carried() {
        let e = entry_fixture(|app| app.wait_ready = true);
        assert_eq!(fitness(&[plain(&e)]), Fitness::Carryable);
    }

    /// `shutdown_with_message` is the one of the three whose channel traffic
    /// runs from the shepherd to the child, so the writer half has to work.
    #[test]
    fn shutdown_with_message_alone_is_carried() {
        let e = entry_fixture(|app| app.shutdown_with_message = true);
        assert_eq!(fitness(&[plain(&e)]), Fitness::Carryable);
    }

    #[test]
    fn a_sheep_with_stdin_is_carried() {
        let e = entry_fixture(|app| app.stdin = true);
        assert_eq!(fitness(&[plain(&e)]), Fitness::Carryable);
    }

    /// Both sources, since a gate reading only `DogSource::BuiltIn` would pass
    /// on one of them.
    #[test]
    fn a_dog_is_carried_rather_than_refused() {
        let mut built_in = entry_fixture(|_| {});
        built_in.dog = Some(DogSource::BuiltIn);
        assert_eq!(fitness(&[plain(&built_in)]), Fitness::Carryable);

        let mut adopted = entry_fixture(|_| {});
        adopted.dog = Some(DogSource::Adopted {
            path: "/opt/bin/shep-log-rotate".to_string(),
        });
        assert_eq!(fitness(&[plain(&adopted)]), Fitness::Carryable);
    }

    /// Both slots, since a version that stopped reading `instances` on slot 0
    /// alone would pass a one-candidate case.
    #[test]
    fn an_app_with_more_than_one_instance_is_carried() {
        let mut zero = entry_fixture(|app| app.instances = 2);
        let mut one = entry_fixture(|app| app.instances = 2);
        one.id = 2;
        one.instance = 1;
        one.pid = Some(101);
        assert_eq!(fitness(&[plain(&zero), plain(&one)]), Fitness::Carryable);
        // A gate that read only the first candidate would pass the assertion
        // above too.
        zero.instance = 1;
        one.instance = 0;
        assert_eq!(fitness(&[plain(&one), plain(&zero)]), Fitness::Carryable);
    }
}
