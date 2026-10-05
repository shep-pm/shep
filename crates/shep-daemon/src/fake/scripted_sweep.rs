//! Scripted [`LambSweep`] for engine tests: canned snapshots, a survivor
//! list per look, and a ledger of every signal

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use crate::sweep::{LambSignal, LambSnapshot, LambSweep, SignalError};

/// A [`LambSweep`] that touches no process.
///
/// [`LambSweep::survivors`] replays one scripted list per call and repeats
/// the last once the script runs out. An empty script never reports a
/// survivor.
#[derive(Debug, Default)]
pub(crate) struct ScriptedSweep {
    fresh: HashMap<u32, LambSnapshot>,
    last: HashMap<u32, LambSnapshot>,
    survivors: Vec<Vec<u32>>,
    refusing: HashSet<u32>,
    looks: Mutex<Vec<LambSnapshot>>,
    signals: Mutex<Vec<(u32, LambSignal)>>,
}

impl ScriptedSweep {
    /// A sweep with no snapshots and no survivors.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// `snapshot` answers a fresh walk of `root_pid`.
    pub(crate) fn with_fresh(mut self, root_pid: u32, snapshot: LambSnapshot) -> Self {
        self.fresh.insert(root_pid, snapshot);
        self
    }

    /// `snapshot` is what the last tick recorded for `root_pid`.
    pub(crate) fn with_last(mut self, root_pid: u32, snapshot: LambSnapshot) -> Self {
        self.last.insert(root_pid, snapshot);
        self
    }

    /// One survivor list per [`LambSweep::survivors`] call, in order.
    pub(crate) fn with_survivors(mut self, script: Vec<Vec<u32>>) -> Self {
        self.survivors = script;
        self
    }

    /// Every signal to `pid` fails as the OS refusing it.
    pub(crate) fn refusing(mut self, pid: u32) -> Self {
        self.refusing.insert(pid);
        self
    }

    /// Every signal sent so far, in order, refused ones included.
    pub(crate) fn signals(&self) -> Vec<(u32, LambSignal)> {
        self.signals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The snapshot each [`LambSweep::survivors`] call was asked about.
    pub(crate) fn looks(&self) -> Vec<LambSnapshot> {
        self.looks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl LambSweep for ScriptedSweep {
    fn snapshot(&self, root_pid: u32) -> LambSnapshot {
        self.fresh
            .get(&root_pid)
            .cloned()
            .unwrap_or_else(|| LambSnapshot::new([], 0))
    }

    fn last_snapshot(&self, root_pid: u32) -> Option<LambSnapshot> {
        self.last.get(&root_pid).cloned()
    }

    fn survivors(&self, snapshot: &LambSnapshot) -> Vec<u32> {
        let mut looks = self.looks.lock().unwrap_or_else(PoisonError::into_inner);
        looks.push(snapshot.clone());
        let Some(last) = self.survivors.len().checked_sub(1) else {
            return Vec::new();
        };
        self.survivors[(looks.len() - 1).min(last)].clone()
    }

    fn signal(&self, pid: u32, signal: LambSignal) -> Result<(), SignalError> {
        self.signals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((pid, signal));
        if self.refusing.contains(&pid) {
            return Err(SignalError::Refused("scripted refusal".to_string()));
        }
        Ok(())
    }
}
