//! [`LambSweep`] over the real process table

use std::sync::Arc;

use shep_tree::sweep::ProcessTable;

use super::{LambSignal, LambSnapshot, LambSweep, SignalError};
use crate::limits::stats::StatsState;

/// The real [`LambSweep`]: walks through [`StatsState`]'s sampler and reads
/// the snapshot its poll tick recorded.
#[derive(Debug)]
pub(crate) struct StatsSweep {
    stats: Arc<StatsState>,
    table: ProcessTable,
}

impl StatsSweep {
    /// A sweep over `stats`, the same state the poll tick records into.
    pub(crate) fn new(stats: Arc<StatsState>) -> Self {
        Self {
            stats,
            table: ProcessTable::new(),
        }
    }
}

impl LambSweep for StatsSweep {
    fn snapshot(&self, root_pid: u32) -> LambSnapshot {
        self.stats.lamb_snapshot_now(root_pid, std::process::id())
    }

    fn last_snapshot(&self, root_pid: u32) -> Option<LambSnapshot> {
        self.stats.last_lamb_snapshot(root_pid)
    }

    fn descendants(&self, roots: &[u32]) -> LambSnapshot {
        self.stats.lamb_walk_from(roots)
    }

    fn survivors(&self, snapshot: &LambSnapshot) -> Vec<u32> {
        self.table.survivors(snapshot)
    }

    fn signal(&self, pid: u32, signal: LambSignal) -> Result<(), SignalError> {
        self.table.signal(pid, signal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::sample::TreeIndex;
    use crate::proc_table::ProcInstant;
    use crate::testing::{ScriptedSampler, rss};

    #[test]
    fn the_real_sweep_walks_through_stats_and_reads_the_ticks_snapshot() {
        let table = vec![
            rss(100, Some(std::process::id()), 0),
            rss(101, Some(100), 0),
        ];
        let stats = Arc::new(StatsState::new(Arc::new(ScriptedSampler::new(vec![
            table.clone(),
        ]))));
        stats.watch(1, 100);
        let sweep: &dyn LambSweep = &StatsSweep::new(Arc::clone(&stats));

        assert_eq!(sweep.snapshot(100).pids().collect::<Vec<_>>(), vec![101]);
        assert_eq!(sweep.last_snapshot(100), None, "no tick has run");

        let taken_at = ProcInstant::from_raw(5);
        stats.record_lamb_snapshots(&TreeIndex::build(&table), taken_at, std::process::id());
        assert_eq!(
            sweep.last_snapshot(100),
            Some(LambSnapshot::new([101], taken_at))
        );
    }
}
