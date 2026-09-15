//! What this shepherd tells an operator about a silent dog: evidence read off
//! [`PeerContacts`] and turned into the two lines `dogs::silent` logs and
//! narrates, one per rung of the ladder.
//!
//! Pure with respect to the refusal ladder: everything here reads a
//! [`PeerContacts`] snapshot and produces a `String`, and nothing here
//! mutates a [`DogRefusals`](super::refusals::DogRefusals).

use super::{Contact, PeerContacts};

/// What this shepherd observed about a silent dog's connections: the
/// difference between two silences that look identical in a listing and have
/// opposite fixes.
///
/// Built from two facts and no inference: the pid the supervisor spawned the
/// dog as, and what [`PeerContacts`] has seen arrive from that pid.
///
/// `pub(super)`: `dogs::contacts`'s own test drives `stale_verdict` off this
/// type to check the warm-up gate from the contacts side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Silence {
    /// Nothing has ever connected from the dog's pid. The dog is not
    /// reaching this shepherd's socket at all.
    Unreachable {
        /// The pid nothing has arrived from.
        pid: u32,
    },
    /// Connections have arrived from the dog's pid, and not one of them named
    /// a dog. The dog reaches this shepherd and may be serving every request it
    /// is asked; what it does not do is say who it is.
    Anonymous {
        /// The pid those connections came from.
        pid: u32,
    },
    /// There is no pid to attribute by, so neither of the above can be ruled
    /// in or out: Windows, an OS that declines to name a peer's pid, a process
    /// already gone, or an entry aged out of a full [`PeerContacts`].
    Unattributed,
}

impl Silence {
    /// What `pid`'s connection history says, if anything.
    ///
    /// [`Contact::Named`] lands in [`Self::Unattributed`]: naming a dog sets
    /// `handshook`, and `silent_dogs` filters a handshook dog out before it
    /// can be seen quiet, so the only ways to reach it are pid reuse and a race
    /// with an eviction. Neither is attribution to trust.
    pub(super) fn of(pid: Option<u32>, contacts: &PeerContacts) -> Self {
        match (pid, contacts.from_pid(pid)) {
            (Some(pid), Contact::None) => Self::Unreachable { pid },
            (Some(pid), Contact::Anonymous) => Self::Anonymous { pid },
            _ => Self::Unattributed,
        }
    }
}

/// The one clause the first rung adds about what this shepherd has seen.
///
/// Short, because the restart it accompanies happens either way and the
/// operator has nothing to decide yet.
pub(super) fn first_rung_evidence(evidence: Silence) -> String {
    match evidence {
        Silence::Unreachable { pid } => {
            format!("nothing has connected to this shepherd from pid {pid}")
        }
        Silence::Anonymous { pid } => format!(
            "pid {pid} has connected to this shepherd without naming a dog, so the restart is unlikely to help"
        ),
        Silence::Unattributed => {
            "this shepherd cannot tell which process opened a connection".to_string()
        }
    }
}

/// The stale verdict, written from what this shepherd observed.
///
/// The claim that the binary on disk cannot talk to this shep either belongs
/// on exactly one path, the one where this shepherd watched nothing arrive:
/// asserting it about a connected but anonymous dog sends an operator to
/// reinstall a binary that reinstalling cannot fix. Every arm ends in a
/// command, since the reader is an operator mid-incident.
///
/// `pub(super)`: read by `dogs::silent`'s own ladder and by `dogs::contacts`'s
/// test, which checks the warm-up gate from the contacts side.
pub(super) fn stale_verdict(name: &str, evidence: Silence) -> String {
    let seen = "a dog restarted for never answering this shepherd has still not answered it";
    match evidence {
        Silence::Unreachable { pid } => format!(
            "{seen}, and nothing has ever connected to this shepherd's socket from its process (pid {pid}): \
             the binary on disk cannot reach this shep either, so this dog is stale and will not be \
             restarted again. Read its own log with `shep bleats {name}` for what it says about \
             connecting, then rebuild or reinstall it and run `shep restart {name}`. A dog \
             installed with cargo wants `cargo install <crate> --force`: its own version does \
             not change when the shep it was built against does, so a plain `cargo install` \
             reports the package already installed, builds nothing, and exits 0"
        ),
        Silence::Anonymous { pid } => format!(
            "{seen}, but its process (pid {pid}) HAS connected to this shepherd — every time without \
             naming a dog in its handshake, which is the only thing this shepherd waits for. The dog \
             is reaching shep and may be serving every request it is asked; reinstalling the same \
             build will NOT change that. It is built against shep-client older than 0.1.23, or it \
             connects with `Client::connect` instead of `ReconnectingClient::connect_as_dog`. Rebuild \
             it against shep-client 0.1.23 or newer, then run `shep restart {name}`. With cargo \
             that means `cargo install <crate> --force`: the dog's own version does not change \
             when its shep-client does, so a plain `cargo install` builds nothing and exits 0. \
             It will not be restarted again in the meantime, and it goes on running"
        ),
        Silence::Unattributed => format!(
            "{seen}, and this shepherd could not tell which process opened its connections, so it \
             cannot say which of two things is wrong. Either the dog is not reaching the socket at \
             all — rebuild or reinstall it — or it is reaching it and never names itself in the \
             handshake, which means a build against shep-client older than 0.1.23 and which \
             reinstalling the same build will not fix. Run `shep bleats {name}` to tell them apart: \
             a dog that cannot reach the socket says so in its own log, and one that is connected \
             and merely anonymous does not. It will not be restarted again"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The assertion is not that the wording is nice: it is that the
    /// stale-binary claim appears on the one path where nothing was ever seen
    /// to arrive, and that the connected-but-anonymous path says the opposite
    /// out loud.
    #[test]
    fn the_stale_verdict_claims_only_what_this_shepherd_watched() {
        let unreachable = stale_verdict("metrics", Silence::Unreachable { pid: 900 });
        assert!(
            unreachable.contains("nothing has ever connected"),
            "the reinstall advice has to be earned by an observation: {unreachable}"
        );
        assert!(unreachable.contains("pid 900"), "{unreachable}");
        assert!(
            unreachable.contains("rebuild or reinstall it"),
            "a dog that never reached the socket is the case reinstalling does fix: {unreachable}"
        );

        let anonymous = stale_verdict("log-rotate", Silence::Anonymous { pid: 901 });
        assert!(
            !anonymous.contains("cannot reach this shep"),
            "this dog reached shep; claiming otherwise is the whole defect: {anonymous}"
        );
        assert!(
            anonymous.contains("reinstalling the same build will NOT"),
            "the two days were spent on advice this line has to refuse: {anonymous}"
        );
        assert!(
            anonymous.contains("0.1.23"),
            "the fix is a newer shep-client, and the message has to name it: {anonymous}"
        );
        assert!(
            anonymous.contains("`shep restart log-rotate`"),
            "every verdict ends in something the reader can run: {anonymous}"
        );

        let unattributed = stale_verdict("metrics", Silence::Unattributed);
        // The whole command, not the flag on its own: `contains("--force")`
        // would pass on any sentence that mentioned it. A plain `cargo install
        // <crate>` on a dog whose version has not moved builds nothing and
        // exits 0.
        for verdict in [&unreachable, &anonymous] {
            assert!(
                verdict.contains("`cargo install <crate> --force`"),
                "an actionable verdict must carry the whole forced reinstall command: {verdict}"
            );
        }
        assert!(
            unattributed.contains("could not tell which process"),
            "not knowing has to be said rather than papered over: {unattributed}"
        );
        assert!(
            unattributed.contains("`shep bleats metrics`"),
            "the one command that separates the two candidates: {unattributed}"
        );

        for verdict in [&unreachable, &anonymous, &unattributed] {
            assert!(
                !verdict.contains("the binary on disk cannot talk to this shep either"),
                "the sentence that was asserted on every path is gone: {verdict}"
            );
        }
    }

    /// [`Contact::Named`] is the interesting row: a pid that named a dog and
    /// is judged silent anyway is a contradiction, since naming one sets
    /// `handshook` and `silent_dogs` filters a handshook dog out. The only
    /// honest reading is that the attribution cannot be trusted.
    #[tokio::test(start_paused = true)]
    async fn evidence_is_read_off_the_record_and_never_guessed() {
        let contacts = PeerContacts::new();
        // `Unreachable` is only ever read off a map that has been watching
        // long enough to claim it.
        tokio::time::advance(super::super::contacts::PEER_CONTACT_WARMUP * 2).await;
        contacts.connected(11);
        contacts.named_a_dog(12);

        assert_eq!(
            Silence::of(Some(10), &contacts),
            Silence::Unreachable { pid: 10 }
        );
        assert_eq!(
            Silence::of(Some(11), &contacts),
            Silence::Anonymous { pid: 11 }
        );
        assert_eq!(
            Silence::of(Some(12), &contacts),
            Silence::Unattributed,
            "a pid that named a dog and is silent anyway is a contradiction, not a diagnosis"
        );
        assert_eq!(
            Silence::of(None, &contacts),
            Silence::Unattributed,
            "no pid is no attribution, which is a different answer from no contact"
        );
    }
}
