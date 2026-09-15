//! What this daemon has observed arriving on its own socket, keyed by the
//! connecting process's pid: the raw material `dogs::silent` reads to tell a
//! dog that never reached the socket apart from one that reached it and
//! never named itself.

use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::time::Instant;

use super::DOG_SILENCE_BUDGET;

/// How many distinct peer pids [`PeerContacts`] remembers at once.
///
/// What has to survive eviction is a handful of long-lived dog processes,
/// which reconnect and so refresh their own entries, against whatever else
/// dialled the socket recently. A thousand distinct pids inside one
/// [`DOG_SILENCE_BUDGET`] would take about two hundred `shep` invocations a
/// second, and the degradation is `record_silent_dog`'s unattributed arm.
const PEER_CONTACT_CAPACITY: usize = 1024;

/// How long this map must have been watching before a pid's absence from it
/// means anything.
///
/// A successor built by [`crate::boot`] starts empty at every `execve`, so
/// without this every dog carried across a `shep daemon reload` would look,
/// for its first seconds, like a dog that never called. The stale rung is
/// spent once, so a verdict against a cold map would be the last one.
///
/// While the map warms, `from_pid` answers [`Contact::Unknown`], which routes
/// to `Silence::Unattributed`.
///
/// `pub(super)`: `dogs::silent`'s own tests drive a map across this whole
/// warm-up window.
pub(super) const PEER_CONTACT_WARMUP: Duration = DOG_SILENCE_BUDGET;

/// What this daemon has observed arriving on its socket, keyed by the
/// connecting process's pid.
///
/// One question, asked by `record_silent_dog`: when a dog has been running
/// without ever handshaking, is it failing to reach this daemon, or reaching
/// it and not saying who it is? Those have opposite fixes. A pid is the
/// identifier both sides already have, so nothing is added to a protocol the
/// dogs being diagnosed are too old to speak.
///
/// Unix only in practice: Windows has no post-accept peer check, so this map
/// stays empty there and every lookup answers [`Contact::Unknown`].
#[derive(Debug, Clone, Default)]
pub struct PeerContacts {
    seen: Arc<Mutex<Contacts>>,
}

/// What [`PeerContacts`] holds, under its one lock.
#[derive(Debug)]
struct Contacts {
    /// When this map started watching, which is this daemon's own boot.
    ///
    /// [`tokio::time::Instant`], so a paused test moves the clock instead of
    /// sleeping out a budget. Under the lock so a test that drives a real
    /// socket can back-date it through `&self`.
    watching_since: Instant,
    /// One entry per remembered peer pid, at most
    /// [`PEER_CONTACT_CAPACITY`] of them.
    by_pid: BTreeMap<u32, Seen>,
    /// Ticks once per recorded connection, and is what
    /// [`Contacts::evict_oldest`] compares.
    ///
    /// A counter rather than an `Instant`: the only question asked of it is
    /// which of two entries was touched later.
    clock: u64,
}

/// What has been seen from one peer pid.
#[derive(Debug)]
struct Seen {
    /// Whether any connection from this pid carried a `Hello.dog_name`.
    ///
    /// Recorded whatever the handshake's verdict was: a dog refused on
    /// protocol skew still named itself.
    named_a_dog: bool,
    /// [`Contacts::clock`] as of the most recent connection from this pid.
    touched: u64,
}

/// What [`PeerContacts`] has seen from one pid.
///
/// `#[non_exhaustive]`: a fourth answer would otherwise be a breaking change
/// for an out-of-tree matcher.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contact {
    /// Nothing has ever connected from this pid.
    ///
    /// A dog running as this pid is not reaching the socket at all, the one
    /// case where reinstalling the binary is the right advice.
    None,
    /// Connections have arrived from this pid, and not one of them named a
    /// dog in its `Hello`.
    ///
    /// The dog is reaching this daemon and may be serving every request it is
    /// asked. It is built against shep-client older than 0.1.23, or it connects
    /// with `Client::connect` rather than
    /// `ReconnectingClient::connect_as_dog`.
    Anonymous,
    /// A connection from this pid named a dog in its `Hello`.
    Named,
    /// There is nothing recorded either way: no pid was available, or this
    /// pid's entry has been evicted.
    ///
    /// Distinct from [`Self::None`]: "nothing has connected" is a finding, and
    /// "I could not look" is not.
    Unknown,
}

impl PeerContacts {
    /// Builds an empty record: a daemon nothing has connected to yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one connection arriving from `pid`.
    ///
    /// Called before a byte is read, so a peer that connects and says nothing
    /// still counts as having reached this daemon.
    pub fn connected(&self, pid: u32) {
        let mut seen = self.lock();
        seen.clock = seen.clock.saturating_add(1);
        let clock = seen.clock;
        match seen.by_pid.get_mut(&pid) {
            Some(entry) => entry.touched = clock,
            None => {
                seen.by_pid.insert(
                    pid,
                    Seen {
                        named_a_dog: false,
                        touched: clock,
                    },
                );
                seen.evict_oldest();
            }
        }
    }

    /// Records that a connection from `pid` named a dog in its `Hello`.
    ///
    /// Sticky: the question is whether this process has ever named itself, so
    /// a later anonymous connection from the same pid does not unsay it.
    pub fn named_a_dog(&self, pid: u32) {
        let mut seen = self.lock();
        seen.clock = seen.clock.saturating_add(1);
        let clock = seen.clock;
        let entry = seen.by_pid.entry(pid).or_insert(Seen {
            named_a_dog: false,
            touched: clock,
        });
        entry.named_a_dog = true;
        entry.touched = clock;
        seen.evict_oldest();
    }

    /// Whether this map is still too new for an absence to mean anything.
    ///
    /// Read by [`spawn_silent_dog_watch`](super::spawn_silent_dog_watch),
    /// which judges no dog while it is true.
    #[must_use]
    pub fn is_warming(&self) -> bool {
        self.lock().watching_since.elapsed() < PEER_CONTACT_WARMUP
    }

    /// Back-dates the watching clock so this map reads as warm.
    ///
    /// For the cases that drive a real socket and so cannot pause their
    /// clock.
    #[cfg(test)]
    pub(crate) fn force_warm(&self) {
        let mut seen = self.lock();
        seen.watching_since = Instant::now() - PEER_CONTACT_WARMUP * 2;
    }

    /// What has been seen from `pid`, or [`Contact::Unknown`] when there is
    /// no pid to ask about.
    #[must_use]
    pub fn from_pid(&self, pid: Option<u32>) -> Contact {
        let Some(pid) = pid else {
            return Contact::Unknown;
        };
        let seen = self.lock();
        match seen.by_pid.get(&pid) {
            // Absence is a finding only once this map has been watching long
            // enough for it to be one.
            None if seen.watching_since.elapsed() < PEER_CONTACT_WARMUP => Contact::Unknown,
            None => Contact::None,
            Some(seen) if seen.named_a_dog => Contact::Named,
            Some(_) => Contact::Anonymous,
        }
    }

    /// Takes the lock, treating a poisoned one as ordinary data: every
    /// critical section here is a lookup or an increment on a plain
    /// `BTreeMap`, so a panic elsewhere cannot leave a torn value.
    fn lock(&self) -> std::sync::MutexGuard<'_, Contacts> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for Contacts {
    fn default() -> Self {
        Self {
            watching_since: Instant::now(),
            by_pid: BTreeMap::new(),
            clock: 0,
        }
    }
}

impl Contacts {
    /// Drops the least recently touched entry, if the map has outgrown
    /// [`PEER_CONTACT_CAPACITY`].
    ///
    /// A scan rather than a second index: it runs only on the insert that
    /// overflows a full map.
    fn evict_oldest(&mut self) {
        if self.by_pid.len() <= PEER_CONTACT_CAPACITY {
            return;
        }
        let oldest = self
            .by_pid
            .iter()
            .min_by_key(|(_, seen)| seen.touched)
            .map(|(pid, _)| *pid);
        if let Some(pid) = oldest {
            self.by_pid.remove(&pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogs::silent::{Silence, stale_verdict};

    /// The whole diagnosis rests on that difference: one means the dog is not
    /// reaching the socket, the other that it is reaching it and not naming
    /// itself, and they have opposite fixes.
    #[tokio::test(start_paused = true)]
    async fn a_pid_that_never_called_is_told_apart_from_one_that_called_anonymously() {
        let contacts = PeerContacts::new();

        // Past the warm-up: on a map this new, absence is not yet a finding.
        // The subject here is the None/Anonymous/Named distinction.
        tokio::time::advance(PEER_CONTACT_WARMUP * 2).await;

        assert_eq!(
            contacts.from_pid(Some(4242)),
            Contact::None,
            "nothing has connected from this pid, and that is a finding"
        );
        assert_eq!(
            contacts.from_pid(None),
            Contact::Unknown,
            "no pid to ask about is not the same as a pid nothing came from"
        );

        contacts.connected(4242);
        assert_eq!(
            contacts.from_pid(Some(4242)),
            Contact::Anonymous,
            "a connection that named no dog is exactly the case the operator lost two days to"
        );

        contacts.named_a_dog(4242);
        assert_eq!(contacts.from_pid(Some(4242)), Contact::Named);
    }

    /// A successor's map starts empty at every `execve`, so for its first
    /// seconds every dog carried across the handover is absent from it. Reading
    /// that absence as "this dog never called" puts the reinstall verdict on a
    /// dog that is fine.
    #[tokio::test(start_paused = true)]
    async fn a_cold_map_does_not_claim_a_pid_never_called() {
        let contacts = PeerContacts::new();

        assert_eq!(
            contacts.from_pid(Some(4242)),
            Contact::Unknown,
            "a map this new was not listening long enough for an absence to mean anything"
        );
        assert_eq!(
            stale_verdict("metrics", Silence::of(Some(4242), &contacts)),
            stale_verdict("metrics", Silence::Unattributed),
            "an unwarmed map must reach the arm that names both candidates"
        );

        // One tick short of the warm-up is still too new.
        tokio::time::advance(PEER_CONTACT_WARMUP - Duration::from_millis(1)).await;
        assert_eq!(contacts.from_pid(Some(4242)), Contact::Unknown);

        // And past it the absence is earned, so the reinstall advice comes
        // back.
        tokio::time::advance(Duration::from_millis(2)).await;
        assert_eq!(
            contacts.from_pid(Some(4242)),
            Contact::None,
            "shep was listening for a whole budget past the dog's silence"
        );
        assert!(
            stale_verdict("metrics", Silence::of(Some(4242), &contacts))
                .contains("cannot reach this shep"),
            "the earned reinstall advice must survive"
        );
    }

    /// The question is whether this process has ever named itself, so a
    /// reconnect read before its `Hello` must not move it back into the pile.
    #[test]
    fn a_pid_that_has_named_a_dog_goes_on_having_named_one() {
        let contacts = PeerContacts::new();
        contacts.named_a_dog(7);
        contacts.connected(7);
        assert_eq!(contacts.from_pid(Some(7)), Contact::Named);
    }

    /// The bound stops this state growing without limit, and the eviction rule
    /// stops the bound costing the answer: a dog reconnects, so it is touched,
    /// so it survives any amount of churn from short-lived `shep` invocations.
    #[tokio::test(start_paused = true)]
    async fn a_full_map_forgets_the_pid_that_stopped_calling() {
        let contacts = PeerContacts::new();
        // An evicted entry reads as `None` only once the map is old enough for
        // an absence to be a finding. The subject here is eviction.
        tokio::time::advance(PEER_CONTACT_WARMUP * 2).await;
        let dog = 1;
        contacts.named_a_dog(dog);

        // Every stranger arrives after the dog's first call, and the dog calls
        // again partway through, which is what a live dog does.
        for pid in 2..=u32::try_from(PEER_CONTACT_CAPACITY).unwrap() {
            contacts.connected(pid);
            if pid % 8 == 0 {
                contacts.connected(dog);
            }
        }
        let stranger = 2;
        for pid in 1_000_000..1_000_100 {
            contacts.connected(pid);
        }

        assert_eq!(
            contacts.from_pid(Some(dog)),
            Contact::Named,
            "a peer that keeps calling must outlive a hundred that called once"
        );
        assert_eq!(
            contacts.from_pid(Some(stranger)),
            Contact::None,
            "the oldest untouched entry is the one the bound spends"
        );
        assert!(
            contacts.lock().by_pid.len() <= PEER_CONTACT_CAPACITY,
            "the map must not grow past its bound"
        );
    }
}
