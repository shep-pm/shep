//! Fixtures this module's test files share.
//!
//! One plain `ProcessEntry`, the two [`Candidate`] shapes built from it, and
//! the descriptor numbers a running sheep would carry. They live here rather
//! than in any one file because the gate, the blob and the exec all build
//! their cases from the same entry.

use std::os::fd::RawFd;
use std::path::PathBuf;

use shep_core::config::AppConfig;
use shep_core::status::ProcStatus;

use super::{Candidate, CarriedFds, CarriedSheep};
use crate::entry::{ProcessEntry, ReloadState, RestartBudget};
use crate::privilege::SpawnIdentity;
use crate::testing::app_with;

/// A plain, `Online` entry: no channel, not a dog, one instance, no
/// in-flight reload. Every field a real spawn would set is present.
pub(super) fn entry_fixture(mutate: impl FnOnce(&mut AppConfig)) -> ProcessEntry {
    let spec = app_with("web", mutate);
    ProcessEntry {
        id: 1,
        spec,
        pending: None,
        pending_reidentifies: false,
        overridden: Vec::new(),
        instance: 0,
        status: ProcStatus::Online,
        pid: Some(100),
        restarts: 0,
        started_at: None,
        budget: RestartBudget::default(),
        reload: ReloadState::None,
        credentials: SpawnIdentity::Resolved(None),
        out_file: PathBuf::from("/tmp/shep-handover-test-out.log"),
        err_file: PathBuf::from("/tmp/shep-handover-test-err.log"),
        dog: None,
        last_exit: None,
    }
}

pub(super) fn plain(entry: &ProcessEntry) -> Candidate<'_> {
    Candidate {
        entry,
        pump_unresponsive: false,
    }
}

/// A candidate whose log pump missed the snapshot's deadline, which is the
/// one thing that refuses a flock.
pub(super) fn wedged(entry: &ProcessEntry) -> Candidate<'_> {
    Candidate {
        entry,
        pump_unresponsive: true,
    }
}

/// The six descriptor numbers a running sheep would have, counting up
/// from `base`, so two instances can be given disjoint sets.
pub(super) const fn fds_at(base: RawFd) -> CarriedFds {
    CarriedFds {
        out_pipe: Some(base),
        err_pipe: Some(base + 1),
        out_log: Some(base + 2),
        err_log: Some(base + 3),
        stdin: Some(base + 4),
        channel: Some(base + 5),
    }
}

/// One carried sheep off `entry`, with the descriptor numbers a
/// running sheep would have.
pub(super) fn carried(entry: &ProcessEntry) -> CarriedSheep {
    CarriedSheep::from_entry(entry, 7, fds_at(11), false, None, false, None)
}
