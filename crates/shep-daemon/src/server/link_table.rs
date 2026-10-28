//! The dog connections this shepherd is serving, kept so a handover can carry
//! them.
//!
//! The server task that reads a connection owns its descriptor, and nothing
//! else can name it. This table holds a duplicate of each. It also holds the
//! two facts the exec would erase: the `Hello` and the subscriptions.
//!
//! Serving tasks keep running while a handover takes its snapshot. The table
//! has a freeze: [`DogLinks::freeze`] copies the rows and holds every writer
//! until [`DogLinks::thaw`].

use core::time::Duration;
use std::collections::BTreeMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::Notify;
use tokio::time::Instant;

use super::origin::Side;

use crate::handover::DogConnection;

/// What the table knows about one connection.
#[derive(Debug)]
struct Held {
    name: String,
    pid: u32,
    fd: OwnedFd,
    /// `None` until the dog's `Hello`; then its topic patterns.
    topics: Option<Vec<String>>,
    /// Whether the reader is held at a frame boundary by a freeze.
    reader_parked: bool,
    /// Whether the writer is held at a frame boundary by a freeze.
    writer_parked: bool,
}

impl Held {
    const fn parked(&self) -> bool {
        self.reader_parked && self.writer_parked
    }
}

/// The table's shared state: a key per connection, so two connections of one
/// dog (the old and the new across a respawn) never share a row.
#[derive(Debug, Default)]
struct Rows {
    /// Set by [`DogLinks::freeze`]: no row changes until it clears.
    frozen: bool,
    next: u64,
    held: BTreeMap<u64, Held>,
}

/// Every dog connection being served, by lease
///
/// Cheap to clone: the clones share one table. `Debug` is derived and prints
/// names, pids, descriptor numbers and topic patterns, all of which are in
/// `shep flock` already.
#[derive(Debug, Clone, Default)]
pub(crate) struct DogLinks {
    rows: Arc<Mutex<Rows>>,
    /// Woken when the table freezes or thaws, or a task parks or resumes.
    changed: Arc<Notify>,
}

impl DogLinks {
    /// Enters the connection `stream` into the table and returns the lease
    /// that keeps it there.
    ///
    /// `resumed` is the topics of a connection a predecessor already greeted,
    /// and `None` for a fresh one. A duplicate that cannot be made (out of
    /// descriptors) leaves the connection served and untracked: a handover
    /// then leaves it out and the dog falls back to the control socket.
    pub(super) fn register(
        &self,
        name: &str,
        pid: u32,
        stream: &impl AsFd,
        resumed: Option<Vec<String>>,
    ) -> Lease {
        let fd = match stream.as_fd().try_clone_to_owned() {
            Ok(fd) => fd,
            Err(error) => {
                tracing::warn!(dog = name, %error, "a dog connection could not be tracked for a handover");
                return Lease::untracked();
            }
        };
        let mut rows = self.lock();
        let key = rows.next;
        rows.next += 1;
        rows.held.insert(
            key,
            Held {
                name: name.to_owned(),
                pid,
                fd,
                topics: resumed,
                reader_parked: false,
                writer_parked: false,
            },
        );
        Lease {
            tracked: Some((self.clone(), key)),
        }
    }

    /// Freezes the table, waits up to `within` for every connection's reader
    /// and writer to park at a frame boundary, and returns a fresh duplicate
    /// of each connection that did, for a handover to carry.
    ///
    /// Duplicates, so the numbers the blob names stay valid for as long as the
    /// caller holds these. A connection still mid-frame when the wait ends is
    /// left out. A caller that does not exec calls [`thaw`](Self::thaw).
    pub(crate) async fn freeze(&self, within: Duration) -> Vec<DogConnection> {
        self.lock().frozen = true;
        self.changed.notify_waiters();
        let deadline = Instant::now() + within;
        loop {
            // Registered before the check, so a park between the two is not
            // missed.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.lock().held.values().all(Held::parked) {
                break;
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                break;
            }
        }
        let rows = self.lock();
        for held in rows.held.values().filter(|held| !held.parked()) {
            tracing::warn!(
                dog = held.name,
                "a dog connection was mid-frame at a handover and is not carried"
            );
        }
        Self::copy(&rows, Held::parked)
    }

    /// Ends a freeze, for a handover that was refused: the tasks it held
    /// carry on.
    pub(crate) fn thaw(&self) {
        self.lock().frozen = false;
        self.changed.notify_waiters();
    }

    /// [`freeze`](Self::freeze) without the freeze.
    #[cfg(test)]
    pub(super) fn snapshot(&self) -> Vec<DogConnection> {
        Self::copy(&self.lock(), |_| true)
    }

    fn copy(rows: &Rows, wanted: impl Fn(&Held) -> bool) -> Vec<DogConnection> {
        rows.held
            .values()
            .filter(|held| wanted(held))
            .filter_map(|held| {
                Some(DogConnection {
                    name: held.name.clone(),
                    pid: held.pid,
                    fd: held.fd.try_clone().ok()?,
                    topics: held.topics.clone(),
                })
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Rows> {
        self.rows.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One connection's place in the [`DogLinks`] table, removed when dropped
///
/// Held by the task serving the connection, so the row lives exactly as long
/// as the serving does. `Debug` is derived: a key and a handle to the table.
#[derive(Debug)]
pub(super) struct Lease {
    tracked: Option<(DogLinks, u64)>,
}

impl Lease {
    /// A lease on nothing, for a connection that is not being tracked.
    pub(super) const fn untracked() -> Self {
        Self { tracked: None }
    }

    /// Records that the dog has said `Hello`, so a successor treats its next
    /// frame as a request. Before the reply, and held through a freeze.
    pub(super) async fn greeted(&self) {
        self.update(|topics| {
            topics.get_or_insert_with(Vec::new);
        })
        .await;
    }

    /// Records the dog's current subscription, replacing the last: a
    /// connection has one topic list. Before the reply, and held through a
    /// freeze.
    pub(super) async fn subscribed(&self, patterns: &[String]) {
        self.update(|topics| *topics = Some(patterns.to_vec()))
            .await;
    }

    async fn update(&self, change: impl FnOnce(&mut Option<Vec<String>>)) {
        let Some((links, key)) = &self.tracked else {
            return;
        };
        let mut change = Some(change);
        loop {
            let changed = links.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut rows = links.lock();
                if !rows.frozen {
                    if let (Some(held), Some(change)) = (rows.held.get_mut(key), change.take()) {
                        change(&mut held.topics);
                    }
                    return;
                }
            }
            // Held here, not parked: the connection is mid-exchange, so a
            // freeze leaves it out rather than carry a dog that is waiting on
            // a reply.
            changed.await;
        }
    }

    /// Resolves once the table is frozen, and never for an untracked lease.
    pub(super) async fn frozen(&self) {
        let Some((links, _)) = &self.tracked else {
            return core::future::pending().await;
        };
        loop {
            let changed = links.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if links.lock().frozen {
                return;
            }
            changed.await;
        }
    }

    /// Holds `side` at a frame boundary until the table thaws, telling the
    /// freeze it is there.
    pub(super) async fn park(&self, side: Side) {
        let Some((links, _)) = &self.tracked else {
            return;
        };
        self.mark(side, true);
        loop {
            let changed = links.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !links.lock().frozen {
                break;
            }
            changed.await;
        }
        self.mark(side, false);
    }

    fn mark(&self, side: Side, parked: bool) {
        let Some((links, key)) = &self.tracked else {
            return;
        };
        if let Some(held) = links.lock().held.get_mut(key) {
            match side {
                Side::Reader => held.reader_parked = parked,
                Side::Writer => held.writer_parked = parked,
            }
        }
        links.changed.notify_waiters();
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some((links, key)) = &self.tracked {
            links.lock().held.remove(key);
        }
    }
}

#[cfg(test)]
mod tests;
