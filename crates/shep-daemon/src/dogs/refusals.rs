//! The handshake-refusal ladder: which dogs this daemon has refused, how
//! often since each last got in, and what to do about it (one restart from
//! disk, then a stale report, then silence).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use shep_core::selector::ProcessSelector;

use crate::supervisor::SupervisorHandle;

/// What a refused handshake costs the dog that sent it.
///
/// Derived from how many times that dog has been refused since it last
/// handshook, by [`DogRefusals::refused`] and nothing else.
///
/// `#[non_exhaustive]`: a fourth verdict would otherwise be a breaking change
/// for an out-of-tree matcher.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The first refusal since this dog last handshook. Restart it once from
    /// disk: an upgrade that replaced the file usually leaves the disk binary
    /// already correct.
    Restart,
    /// The second: the restarted dog speaks the same protocol, which proves
    /// the binary on disk cannot satisfy this daemon either. Report it stale
    /// and stop.
    Stale,
    /// Already stale, already reported. Say nothing further: a stale dog its
    /// own `autorestart` keeps respawning would write one line per respawn.
    AlreadyStale,
}

/// Which dogs this daemon has refused at the handshake, and how often since
/// each last got in.
///
/// Cheap to clone (one `Arc`), and shared by every connection through
/// [`RpcContext`](crate::rpc::RpcContext).
///
/// A count is cleared by a successful handshake and by nothing else, which
/// bounds the ladder: a dog that keeps being refused never clears, so it never
/// earns a second restart. Nothing survives a handover, and a dog a successor
/// can talk to is not stale.
#[derive(Debug, Clone, Default)]
pub struct DogRefusals {
    /// Both halves under one lock: a dog seen as refused and handshook at once
    /// is a state no reader should be able to observe.
    seen: Arc<Mutex<Links>>,
}

/// What [`DogRefusals`] holds: how often each dog has been refused, and
/// which dogs have ever got in.
///
/// Only ever reached under [`DogRefusals`]'s one lock.
#[derive(Debug, Default)]
struct Links {
    /// Refusals per dog name since that dog last handshook. A name absent
    /// from the map has not been refused since it last got in.
    refusals: BTreeMap<String, u32>,
    /// Dogs whose handshake this daemon has accepted and not refused since.
    ///
    /// Not derivable from the absence of a refusal: a dog that has never
    /// connected and one that is talking happily both have no entry in
    /// [`Self::refusals`].
    handshook: BTreeSet<String>,
}

impl DogRefusals {
    /// Builds an empty record: a daemon that has refused nobody.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one refused handshake from the dog named `name`, and says
    /// what the daemon should do about it.
    ///
    /// The first refusal earns [`Refusal::Restart`], the second
    /// [`Refusal::Stale`], every one after that [`Refusal::AlreadyStale`].
    pub fn refused(&self, name: &str) -> Refusal {
        let mut seen = self.lock();
        // The connection that earned the mark is gone, and the process behind
        // the name may not be the one that made it.
        seen.handshook.remove(name);
        let count = seen.refusals.entry(name.to_string()).or_insert(0);
        *count = count.saturating_add(1);
        match *count {
            1 => Refusal::Restart,
            2 => Refusal::Stale,
            _ => Refusal::AlreadyStale,
        }
    }

    /// Records that `name` handshook successfully, clearing whatever this
    /// daemon held against it.
    ///
    /// Answers whether that changed anything, so the caller can write into the
    /// dog's own log the first time this shepherd hears from it and not once
    /// per reconnect.
    pub fn handshook(&self, name: &str) -> bool {
        let mut seen = self.lock();
        seen.refusals.remove(name);
        seen.handshook.insert(name.to_string())
    }

    /// Whether `name` has handshook with this daemon and not been refused
    /// since.
    #[must_use]
    pub fn has_handshook(&self, name: &str) -> bool {
        self.lock().handshook.contains(name)
    }

    /// Every dog whose one restart from disk is in flight, sorted.
    ///
    /// Exactly the dogs refused once: the restart they are owed has been asked
    /// for and its outcome has not arrived.
    #[must_use]
    pub fn restarting(&self) -> Vec<String> {
        self.lock()
            .refusals
            .iter()
            .filter(|(_, count)| **count == 1)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Every dog this daemon has given up on, sorted.
    ///
    /// A dog is stale once it has been refused twice, which means its one
    /// restart from disk did not help.
    #[must_use]
    pub fn stale(&self) -> Vec<String> {
        self.lock()
            .refusals
            .iter()
            .filter(|(_, count)| **count >= 2)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// The record, treating a poisoned lock as ordinary data: every critical
    /// section here is a lookup or an increment, so a panic elsewhere cannot
    /// leave a torn value.
    fn lock(&self) -> std::sync::MutexGuard<'_, Links> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Records a dog's refused handshake and acts on it.
///
/// The first refusal earns one restart from the binary on disk, enough where a
/// package replaced the file and the running process is merely old. A second
/// proves the disk binary cannot satisfy this daemon either, so the dog is
/// reported stale and left alone. A dog that cannot get in never clears its
/// count, so it cannot earn a second restart.
///
/// The restart runs on its own task: it is a full kill ladder, and the caller
/// is a connection handler holding a socket this daemon has already refused.
pub fn record_refused_dog(
    name: &str,
    client_version: &str,
    refusals: &DogRefusals,
    supervisor: &SupervisorHandle,
) -> Refusal {
    let verdict = refusals.refused(name);
    match verdict {
        Refusal::Restart => {
            tracing::warn!(
                dog = %name,
                dog_version = %client_version,
                "refused a dog on protocol skew; restarting it once from the binary on disk"
            );
            let supervisor = supervisor.clone();
            let name = name.to_string();
            tokio::spawn(async move { restart_refused_dog(&supervisor, &name).await });
        }
        Refusal::Stale => tracing::error!(
            dog = %name,
            dog_version = %client_version,
            "refused a dog on protocol skew again after restarting it: the binary on disk speaks the same protocol the running one did, so this dog is stale and will not be restarted again. Rebuild or reinstall it against this shep"
        ),
        Refusal::AlreadyStale => tracing::debug!(
            dog = %name,
            dog_version = %client_version,
            "refused a dog already reported stale"
        ),
    }
    verdict
}

/// Restarts the dog named `name`, logging either outcome.
///
/// [`SupervisorHandle::restart_automatic`] rather than the operator door:
/// nobody typed this, so an operator's own `stop` or `delete` landing
/// mid-ladder takes the dog off the ladder. An exact-name selector is the only
/// kind that reaches a dog: the supervisor keeps dogs out of `all` and out of
/// pattern matches.
///
/// `pub(super)`: `dogs::silent`'s inferred-silence ladder shares this
/// restart with the named-refusal one above, and calls it directly.
pub(super) async fn restart_refused_dog(supervisor: &SupervisorHandle, name: &str) {
    match supervisor
        .restart_automatic(ProcessSelector::Name(name.to_string()))
        .await
    {
        Ok(_) => tracing::info!(dog = %name, "restarted a refused dog from the binary on disk"),
        // Not an error the daemon can act on: the dog may have been disabled
        // between the refusal and this restart, or the engine may be shutting
        // down.
        Err(err) => tracing::warn!(dog = %name, %err, "a refused dog could not be restarted"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_dog_earns_one_restart_and_is_then_stale_forever() {
        let refusals = DogRefusals::new();
        assert!(refusals.stale().is_empty());

        assert_eq!(refusals.refused("metrics"), Refusal::Restart);
        assert!(
            refusals.stale().is_empty(),
            "one refusal is a dog to restart, not a dog to give up on"
        );

        assert_eq!(refusals.refused("metrics"), Refusal::Stale);
        assert_eq!(refusals.stale(), vec!["metrics".to_string()]);

        // The refusals a stale dog's own autorestart goes on producing must
        // not each buy another restart.
        for _ in 0..5 {
            assert_eq!(refusals.refused("metrics"), Refusal::AlreadyStale);
        }
        assert_eq!(refusals.stale(), vec!["metrics".to_string()]);
    }

    /// The count is cleared by a successful handshake and by nothing else, so
    /// "one restart" means one per episode rather than one per daemon.
    #[test]
    fn a_dog_that_gets_in_is_owed_a_fresh_restart_if_it_is_ever_refused_again() {
        let refusals = DogRefusals::new();
        assert_eq!(refusals.refused("metrics"), Refusal::Restart);
        refusals.handshook("metrics");
        assert!(refusals.stale().is_empty());
        assert_eq!(
            refusals.refused("metrics"),
            Refusal::Restart,
            "the restart that fixed it must not be charged against the next episode"
        );
    }

    #[test]
    fn each_dog_carries_its_own_count() {
        let refusals = DogRefusals::new();
        assert_eq!(refusals.refused("bark"), Refusal::Restart);
        assert_eq!(refusals.refused("bark"), Refusal::Stale);

        assert_eq!(
            refusals.refused("metrics"),
            Refusal::Restart,
            "bark's two refusals are bark's"
        );
        refusals.handshook("metrics");
        assert_eq!(
            refusals.stale(),
            vec!["bark".to_string()],
            "one dog getting in says nothing about another"
        );
    }

    /// The unsettled-dog report is taken once every dog has settled, and a dog
    /// refused once has not: the restart it is owed has been asked for and its
    /// verdict has not come back.
    #[test]
    fn a_dog_mid_restart_is_neither_stale_nor_settled() {
        let refusals = DogRefusals::new();
        assert!(refusals.restarting().is_empty());

        refusals.refused("metrics");
        assert_eq!(refusals.restarting(), vec!["metrics".to_string()]);
        assert!(refusals.stale().is_empty());

        refusals.refused("metrics");
        assert!(
            refusals.restarting().is_empty(),
            "a dog that has been given up on is settled, not still being restarted"
        );
        assert_eq!(refusals.stale(), vec!["metrics".to_string()]);
    }

    /// A dog that has never connected and one talking happily both have no
    /// refusal recorded, and telling them apart is what the unsettled-dog
    /// report waits on.
    #[test]
    fn only_an_accepted_handshake_says_a_dog_has_answered() {
        let refusals = DogRefusals::new();
        assert!(
            !refusals.has_handshook("metrics"),
            "a dog nobody has heard from has not answered"
        );

        refusals.handshook("metrics");
        assert!(refusals.has_handshook("metrics"));
        assert!(!refusals.has_handshook("bark"), "one dog answers for one");

        refusals.refused("metrics");
        assert!(
            !refusals.has_handshook("metrics"),
            "the handshake that earned the mark is the one that just died"
        );
    }
}
