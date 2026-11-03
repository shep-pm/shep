//! Who a connection is, as far as the daemon knows without asking the peer.

use std::collections::BTreeSet;
#[cfg(unix)]
use std::sync::Arc;

use shep_core::protocol::Request;

#[cfg(unix)]
use super::link_table::Lease;
use crate::rpc::{Link, RpcContext, kind_of};

/// Which half of a connection a task serves, for a handover that parks both.
#[derive(Debug, Clone, Copy)]
pub(super) enum Side {
    /// The task reading frames off the socket.
    Reader,
    /// The task writing frames onto it.
    Writer,
}

/// How a connection reached the daemon
///
/// A connection on the control socket is whoever the uid check lets in, and
/// names itself in its `Hello`. A dog's own connection, opened at spawn, is
/// that dog and nobody else: the runner knew whom it handed the other end to.
/// A launch link is the same for a sheep a dog launches, and speaks for the
/// sheep alone.
#[derive(Debug, Clone)]
pub(super) enum Origin {
    /// Accepted on the control socket.
    Socket,
    /// The connection a dog was spawned holding: a socketpair on unix, a pipe
    /// the dog opened by name on Windows.
    Dog {
        /// The dog's name, from the spec that spawned it.
        name: String,
        /// The pid it was spawned under.
        pid: u32,
        /// This connection's row in the table a handover carries from. Unix
        /// only, as the handover is.
        #[cfg(unix)]
        lease: Arc<Lease>,
        /// The topics the dog had subscribed to, for a connection a
        /// predecessor had already greeted and handed over, and `None` for
        /// one opened at this spawn, whose dog has yet to say `Hello`. Always
        /// `None` on Windows.
        resumed: Option<Vec<String>>,
    },
    /// The connection a sheep was spawned holding because a dog launches it,
    /// a socketpair that works under any uid. Unix only: Windows refuses
    /// `launch`.
    Launch {
        /// The dog launching the sheep.
        dog: String,
        /// The sheep the link was opened for.
        sheep: String,
        /// This connection's row in the table a handover carries from.
        #[cfg(unix)]
        lease: Arc<Lease>,
        /// As [`Self::Dog`]'s `resumed`.
        resumed: Option<Vec<String>>,
    },
}

impl Origin {
    /// The dog this connection speaks for: the link's own name, or for the
    /// socket whatever the peer `claimed`.
    ///
    /// A claim on a link is dropped, so a dog cannot present as another. A
    /// launch link speaks for a sheep and no dog.
    pub(super) fn dog<'a>(&'a self, claimed: Option<&'a str>) -> Option<&'a str> {
        match self {
            Self::Socket => claimed,
            Self::Dog { name, .. } => Some(name),
            Self::Launch { .. } => None,
        }
    }

    /// What bounds this connection's requests: the dog's grants or a launch
    /// link's sheep, and never a socket's claim.
    ///
    /// A socket acts as the operator, so a `Hello` naming a dog buys it no
    /// grants and costs it none.
    pub(super) fn link(&self) -> Option<Link<'_>> {
        match self {
            Self::Socket => None,
            Self::Dog { name, .. } => Some(Link::Dog(name)),
            Self::Launch { dog, sheep, .. } => Some(Link::Launch { dog, sheep }),
        }
    }

    /// The pid to record contact against: the link's, or for the socket the
    /// one the OS reported, which `None` on Windows.
    ///
    /// A socketpair's peer credentials name the daemon itself. A launch link
    /// records none: the silence watch is about dogs.
    pub(super) fn pid(&self, reported: Option<u32>) -> Option<u32> {
        match self {
            Self::Socket => reported,
            Self::Dog { pid, .. } => Some(*pid),
            Self::Launch { .. } => None,
        }
    }

    /// The topics to re-subscribe a connection a predecessor handed over, or
    /// `None` for one that starts with a `Hello`.
    pub(super) fn resumed(&self) -> Option<&[String]> {
        match self {
            Self::Socket => None,
            Self::Dog { resumed, .. } | Self::Launch { resumed, .. } => resumed.as_deref(),
        }
    }

    /// Notes on the link's row that the dog has said `Hello`. A socket has no
    /// row.
    pub(super) async fn greeted(&self) {
        #[cfg(unix)]
        if let Self::Dog { lease, .. } | Self::Launch { lease, .. } = self {
            lease.greeted().await;
        }
    }

    /// Notes on the link's row what the dog now subscribes to. A socket has no
    /// row.
    #[cfg_attr(
        not(unix),
        allow(unused_variables, reason = "only a unix link has a row to note it on")
    )]
    pub(super) async fn subscribed(&self, patterns: &[String]) {
        #[cfg(unix)]
        if let Self::Dog { lease, .. } | Self::Launch { lease, .. } = self {
            lease.subscribed(patterns).await;
        }
    }

    /// Resolves once a handover has frozen this connection's row, and never for
    /// a connection no handover carries.
    pub(super) async fn frozen(&self) {
        #[cfg(unix)]
        if let Self::Dog { lease, .. } | Self::Launch { lease, .. } = self {
            return lease.frozen().await;
        }
        core::future::pending::<()>().await;
    }

    /// Holds `side` of this connection at a frame boundary until the freeze
    /// ends. A socket has no row to hold.
    #[cfg_attr(
        not(unix),
        allow(unused_variables, reason = "only a unix link has a row to park on")
    )]
    pub(super) async fn park(&self, side: Side) {
        #[cfg(unix)]
        if let Self::Dog { lease, .. } | Self::Launch { lease, .. } = self {
            lease.park(side).await;
        }
    }

    /// A link no handover can carry, for a test that serves one.
    #[cfg(all(test, unix))]
    pub(super) fn untracked_dog(name: &str, pid: u32) -> Self {
        Self::Dog {
            name: name.to_string(),
            pid,
            lease: Arc::new(Lease::untracked()),
            resumed: None,
        }
    }

    /// A launch link no handover can carry, for a test that serves one.
    #[cfg(all(test, unix))]
    pub(super) fn untracked_launch(dog: &str, sheep: &str) -> Self {
        Self::Launch {
            dog: dog.to_string(),
            sheep: sheep.to_string(),
            lease: Arc::new(Lease::untracked()),
            resumed: None,
        }
    }

    /// Whether reaching this daemon is the connection arriving, as on the
    /// socket. A link is opened by the daemon at spawn, so it proves nothing
    /// about the dog until the dog sends a frame.
    pub(super) fn contacts_on_open(&self) -> bool {
        matches!(self, Self::Socket)
    }

    /// Whether the control socket's same-uid check applies. A link needs none:
    /// only the process it was handed to holds the other end.
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "only the unix accept path checks the peer")
    )]
    pub(super) fn checks_peer(&self) -> bool {
        matches!(self, Self::Socket)
    }
}

/// Writes into a dog's own log what it asked of the shepherd
///
/// Once per kind of request per connection, so a dog polling every second
/// does not bury its own output in its own log.
#[derive(Debug, Default)]
pub(super) struct Attribution {
    dog: Option<String>,
    seen: BTreeSet<String>,
}

impl Attribution {
    /// Attributes requests to `origin`'s dog; a socket connection and a launch
    /// link have none.
    pub(super) fn new(origin: &Origin) -> Self {
        match origin {
            Origin::Socket => Self::default(),
            Origin::Dog { name, .. } => Self {
                dog: Some(name.clone()),
                seen: BTreeSet::new(),
            },
            Origin::Launch { .. } => Self::default(),
        }
    }

    /// Narrates `request` into the dog's log if this connection has not made
    /// one of its kind before.
    pub(super) fn note(&mut self, ctx: &RpcContext, request: &Request) {
        let Some(dog) = &self.dog else { return };
        let kind = kind_of(request);
        if self.seen.insert(kind.clone()) {
            crate::dogs::narrate_by_name(
                &ctx.supervisor,
                &ctx.events,
                dog,
                format!("this dog sent shep a `{kind}` request on its own connection"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_link_ignores_the_name_the_peer_claims() {
        let link = Origin::untracked_dog("metrics", 7);
        assert_eq!(link.dog(Some("bark")), Some("metrics"));
        assert_eq!(link.dog(None), Some("metrics"));
    }

    #[cfg(unix)]
    #[test]
    fn only_a_link_is_bounded_and_a_launch_link_by_its_sheep() {
        assert!(matches!(
            Origin::untracked_dog("metrics", 7).link(),
            Some(Link::Dog("metrics"))
        ));
        assert!(matches!(
            Origin::untracked_launch("sandbox", "web").link(),
            Some(Link::Launch {
                dog: "sandbox",
                sheep: "web"
            })
        ));
        assert!(Origin::Socket.link().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_launch_link_speaks_for_no_dog_and_records_no_contact() {
        let link = Origin::untracked_launch("sandbox", "web");
        assert_eq!(link.dog(Some("sandbox")), None);
        assert_eq!(link.pid(Some(99)), None);
        assert!(!link.checks_peer());
    }

    #[test]
    fn the_socket_takes_the_claim_as_given() {
        assert_eq!(Origin::Socket.dog(Some("bark")), Some("bark"));
        assert_eq!(Origin::Socket.dog(None), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_records_contact_against_its_own_pid() {
        let link = Origin::untracked_dog("metrics", 7);
        assert_eq!(link.pid(Some(99)), Some(7));
        assert_eq!(Origin::Socket.pid(Some(99)), Some(99));
    }

    #[cfg(unix)]
    #[test]
    fn only_the_socket_checks_its_peer() {
        assert!(Origin::Socket.checks_peer());
        assert!(!Origin::untracked_dog("metrics", 7).checks_peer());
    }

    #[test]
    fn a_request_is_named_by_its_variant_alone() {
        assert_eq!(kind_of(&Request::Ping), "ping");
        assert_eq!(kind_of(&Request::ListFlock), "list_flock");
    }
}
