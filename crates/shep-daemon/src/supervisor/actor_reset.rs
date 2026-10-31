//! Handing a sheep its restart count and budget back.

use super::actor_disable::Sweep;
use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// Zeroes the restart count and budget of every sheep `selector` reaches
    /// and answers with their rows in flock order.
    ///
    /// Status, pid, timers and the bus are left alone. `last_exit` goes too,
    /// except on a stopped dog: that record is what keeps it from reading as
    /// at rest (`is_at_rest`).
    pub(super) fn handle_reset(
        &mut self,
        selector: &ProcessSelector,
        reply: oneshot::Sender<Result<Vec<ProcessInfo>, SupervisorError>>,
    ) {
        let ids = self.reached_ids(selector, Sweep::Any);
        if ids.is_empty() {
            let _ = reply.send(Err(SupervisorError::NotFound));
            return;
        }
        for id in &ids {
            let Some(slot) = self.sheep.get_mut(id) else {
                continue;
            };
            let entry = &mut slot.entry;
            entry.restarts = 0;
            entry.budget.reset();
            if !(entry.dog.is_some() && entry.status == ProcStatus::Stopped) {
                entry.last_exit = None;
            }
        }
        let mut rows: Vec<ProcessInfo> = ids
            .iter()
            .filter_map(|id| self.sheep.get(id))
            .map(|slot| self.with_mark(to_info(&slot.entry, &self.smits)))
            .collect();
        sort_flock(&mut rows);
        let _ = reply.send(Ok(rows));
    }
}
