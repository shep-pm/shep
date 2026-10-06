//! Scripted [`LambSweep`] for engine tests: canned snapshots, a survivor
//! list per look, and a ledger of every signal

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use crate::sweep::{LambSignal, LambSnapshot, LambSweep, SignalError};

/// A sweep that finds no lambs, for a fixture's `Extras` to hold.
///
/// Never the real one: a scripted pid names a real process on the host.
pub(crate) fn idle_sweep() -> Arc<dyn LambSweep> {
    Arc::new(ScriptedSweep::new())
}

/// A [`LambSweep`] that touches no process.
///
/// [`LambSweep::survivors`] replays one scripted list per call and repeats
/// the last once the script runs out. An empty script never reports a
/// survivor. A delivered `Kill` removes its pid from every later list, as
/// the kernel would, unless the pid is [`Self::unkillable`].
#[derive(Debug, Default)]
pub(crate) struct ScriptedSweep {
    fresh: HashMap<u32, LambSnapshot>,
    last: HashMap<u32, LambSnapshot>,
    survivors: Vec<Vec<u32>>,
    born: Vec<Vec<u32>>,
    walks: Mutex<Vec<Vec<u32>>>,
    refusing: HashSet<u32>,
    unkillable: HashSet<u32>,
    killed: Mutex<HashSet<u32>>,
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

    /// One list per [`LambSweep::descendants`] call, in order, of what the
    /// survivors started since; repeats the last once the script runs out.
    pub(crate) fn with_born(mut self, script: Vec<Vec<u32>>) -> Self {
        self.born = script;
        self
    }

    /// The roots each [`LambSweep::descendants`] call walked from.
    pub(crate) fn walks(&self) -> Vec<Vec<u32>> {
        self.walks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Every signal to `pid` fails as the OS refusing it.
    pub(crate) fn refusing(mut self, pid: u32) -> Self {
        self.refusing.insert(pid);
        self
    }

    /// `pid` takes its `Kill` and stays in the survivor lists, like a
    /// process stuck in uninterruptible sleep.
    pub(crate) fn unkillable(mut self, pid: u32) -> Self {
        self.unkillable.insert(pid);
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
        let killed = self.killed.lock().unwrap_or_else(PoisonError::into_inner);
        self.survivors[(looks.len() - 1).min(last)]
            .iter()
            .copied()
            .filter(|pid| !killed.contains(pid))
            .collect()
    }

    fn descendants(&self, roots: &[u32]) -> LambSnapshot {
        let mut walks = self.walks.lock().unwrap_or_else(PoisonError::into_inner);
        walks.push(roots.to_vec());
        let Some(last) = self.born.len().checked_sub(1) else {
            return LambSnapshot::new([], 0);
        };
        LambSnapshot::new(self.born[(walks.len() - 1).min(last)].clone(), 0)
    }

    fn signal(&self, pid: u32, signal: LambSignal) -> Result<(), SignalError> {
        self.signals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((pid, signal));
        if self.refusing.contains(&pid) {
            return Err(SignalError::Refused("scripted refusal".to_string()));
        }
        if signal == LambSignal::Kill && !self.unkillable.contains(&pid) {
            self.killed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(pid);
        }
        Ok(())
    }
}
