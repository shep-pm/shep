//! The CPU and resident-memory series behind the sparklines.

use super::*;

/// The widest window any pane draws, in samples.
///
/// The landing pane's sparkline needs ten. 1d's charts want six minutes,
/// which is 180 at the two-second poll, and 140 is what fits the frames'
/// 140-cell chart body. Sized for the charts now so the sheep pane
/// inherits a filled buffer rather than starting cold on a pane the
/// operator has just opened.
pub(crate) const HISTORY: usize = 140;

/// The lowest ceiling [`App::cpu_ceiling`] will report, in percent of one
/// core.
///
/// Without it, a flock idling at a tenth of a percent would have its own
/// jitter scaled to full height, so the busiest thing on screen would be
/// noise. Two percent is low enough that any real work clears it and high
/// enough that nothing else does.
///
/// `pub(crate)`, not private: `pane_sheep::scale_top`'s own floor argument
/// is this same number for the sheep pane's CPU chart, and a second
/// constant carrying the value would be the thing that drifts.
pub(crate) const CPU_CEILING_FLOOR: f32 = 2.0;

impl App {
    /// Differences one CPU sample per sheep in the current flock against its
    /// last reading, buffers RSS as read, appends the flock-wide CPU sum,
    /// and drops every history entry (CPU, RSS and baseline) for a sheep the
    /// new snapshot no longer carries.
    ///
    /// Called after `self.flock` is replaced, so it reads the fresh
    /// snapshot rather than the one before it. A sheep with no CPU reading
    /// contributes `0.0` and forgets its baseline: skipping the sample would
    /// slide the whole window and make an old spike look recent, and
    /// keeping the baseline would difference the next live reading across
    /// the gap.
    ///
    /// Every touched deque is made contiguous here, while this method still
    /// holds `&mut self`, so [`Self::cpu_history`], [`Self::rss_history`]
    /// and [`Self::flock_cpu_history`] can hand out a slice from `&self`
    /// alone.
    pub(super) fn record_samples(&mut self, at: Instant) {
        // Collected first: the differencing below needs `&mut self.cpu_last`
        // while a walk of `self.flock` would still be borrowing it.
        let readings: Vec<(u32, Option<u32>, Option<u64>, u64)> = self
            .flock
            .values()
            .map(|row| {
                (
                    row.info.id,
                    row.info.pid,
                    row.info.cpu_ms,
                    row.info.memory_bytes.unwrap_or(0),
                )
            })
            .collect();
        let mut sum = 0.0;
        for (id, pid, cpu_ms, rss) in readings {
            let rss_history = self.rss_history.entry(id).or_default();
            rss_history.push_back(rss);
            if rss_history.len() > HISTORY {
                rss_history.pop_front();
            }
            rss_history.make_contiguous();

            let cpu = match cpu_ms {
                None => {
                    self.cpu_last.remove(&id);
                    0.0
                }
                Some(now_ms) => match self.cpu_last.insert(id, (pid, now_ms, at)) {
                    // Nothing behind this reading to difference. The buffer
                    // stays one short of the poll count rather than claiming
                    // an idle sample it never measured.
                    None => continue,
                    // A respawn keeps the sheep's id and takes a new pid, and
                    // `cpu_ms` counts the tree under whichever pid the
                    // shepherd is watching now. Differencing across that
                    // boundary subtracts a dead process's counter from a live
                    // one's: `saturating_sub` keeps it from ever reading as a
                    // spike, but it still underreports the new process by
                    // exactly what the old one had spent. A new process is a
                    // first reading, so it records a baseline and appends
                    // nothing, the same as a sheep the pane has never seen.
                    Some((then_pid, _, _)) if then_pid != pid => continue,
                    Some((_, then_ms, then)) => shep_core::values::cpu_percent(
                        now_ms.saturating_sub(then_ms),
                        at.saturating_duration_since(then),
                    )
                    .unwrap_or(0.0),
                },
            };
            sum += cpu;
            let history = self.cpu_history.entry(id).or_default();
            history.push_back(cpu);
            if history.len() > HISTORY {
                history.pop_front();
            }
            history.make_contiguous();
        }
        self.cpu_history.retain(|id, _| self.flock.contains_key(id));
        self.rss_history.retain(|id, _| self.flock.contains_key(id));
        // A departed sheep's baseline outlives its rows here unless dropped
        // too: without this, a later id reused by an unrelated sheep would
        // inherit a stranger's counter and difference its first honest
        // reading against it, breaking the exact guarantee
        // `Self::cpu_history`'s doc makes about a later id inheriting
        // nothing.
        self.cpu_last.retain(|id, _| self.flock.contains_key(id));
        self.flock_cpu.push_back(sum);
        if self.flock_cpu.len() > HISTORY {
            self.flock_cpu.pop_front();
        }
        self.flock_cpu.make_contiguous();
    }

    /// One sheep's CPU-percent samples, oldest first, newest last.
    ///
    /// Empty for a sheep with no history yet, and for one that has left the
    /// flock: [`Self::record_samples`] drops its entry entirely.
    #[must_use]
    pub fn cpu_history(&self, id: u32) -> &[f32] {
        self.cpu_history
            .get(&id)
            .map_or(&[][..], |history| history.as_slices().0)
    }

    /// `id`'s newest differenced CPU sample: [`Self::cpu_history`]'s last
    /// entry, the same number its sparkline's last cell draws.
    ///
    /// `None` in two cases, both honest gaps rather than a claimed zero:
    /// before a first difference exists (one poll after launch, on
    /// [`Self::cpu_history`]'s own terms), and when the current snapshot's
    /// `cpu_ms` is itself `None` (the sheep is not running, or the peer
    /// daemon predates the field). The second check matters because
    /// [`Self::record_samples`] still appends a zero to the history buffer
    /// in that case, to keep the sparkline's window from sliding; reading
    /// that zero back as a figure would report "0.0%" for a sheep whose CPU
    /// was never sampled, the same false claim `ProcessInfo::cpu_percent`'s
    /// own `None` exists to refuse.
    ///
    /// Every CPU figure lookout draws reads through here rather than
    /// `ProcessInfo::cpu_percent`, the shepherd's own mean over a window
    /// that resets independently of this pane's polls: reading both would
    /// put two different numbers under one label.
    #[must_use]
    pub fn cpu_now(&self, id: u32) -> Option<f32> {
        self.flock.get(&id)?.info.cpu_ms?;
        self.cpu_history(id).last().copied()
    }

    /// One sheep's RSS samples in bytes, oldest first, newest last.
    ///
    /// Empty for a sheep with no history yet and for one that has left the
    /// flock, on [`Self::cpu_history`]'s terms.
    #[must_use]
    pub fn rss_history(&self, id: u32) -> &[u64] {
        self.rss_history
            .get(&id)
            .map_or(&[][..], |series| series.as_slices().0)
    }

    /// The whole flock's summed CPU-percent samples, oldest first, newest
    /// last, same depth as [`Self::cpu_history`].
    pub fn flock_cpu_history(&self) -> &[f32] {
        self.flock_cpu.as_slices().0
    }

    /// The ceiling every row's CPU sparkline scales against: the busiest
    /// sample any sheep has recorded in the retained window.
    ///
    /// One ceiling shared by every row is what makes the column comparable
    /// down the table. Per-row peaks make an idle sheep and a busy one both
    /// fill their own cells; a fixed 100% of a core makes an ordinary flock,
    /// where nothing is above two percent, draw a screen of flat lines.
    ///
    /// Floored at [`CPU_CEILING_FLOOR`] so a flock that is genuinely doing
    /// nothing stays flat instead of having its rounding noise stretched
    /// into a shape. Below that floor there is nothing to see and saying so
    /// is the honest answer.
    #[must_use]
    pub fn cpu_ceiling(&self) -> f32 {
        self.cpu_history
            .values()
            .flat_map(|series| series.iter().copied())
            .fold(CPU_CEILING_FLOOR, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One snapshot row for `id`, reporting `cpu_ms` CPU-milliseconds.
    fn row_with_cpu_ms(id: u32, cpu_ms: u64) -> ProcessInfo {
        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online)
            .cpu_ms(Some(cpu_ms))
            .build()
    }

    /// A row naming its own pid, for the respawn case: one sheep id outlives
    /// the process under it, and `cpu_ms` counts whichever tree the shepherd
    /// watches now.
    fn row_with_pid_and_cpu_ms(id: u32, pid: u32, cpu_ms: u64) -> ProcessInfo {
        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online)
            .pid(Some(pid))
            .cpu_ms(Some(cpu_ms))
            .build()
    }

    /// The same row with no CPU reading, which is what a stopped sheep sends.
    fn row_without_cpu(id: u32) -> ProcessInfo {
        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Stopped).build()
    }

    /// One snapshot row for `id`, reporting `rss` bytes of resident memory.
    fn row_with_rss(id: u32, rss: u64) -> ProcessInfo {
        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online)
            .memory_bytes(Some(rss))
            .build()
    }

    /// A dashboard with an empty flock, for tests that only exercise the
    /// snapshot's history bookkeeping.
    fn fixture() -> App {
        App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        )
    }

    impl App {
        /// Drives `Msg::Snapshot` the way the poll does, two seconds after
        /// the last one. The gap is load-bearing: a differenced sample over
        /// a zero window has no honest value.
        fn on_snapshot(&mut self, rows: Vec<ProcessInfo>) {
            self.now += Duration::from_secs(2);
            let at = self.now;
            self.update(Msg::Snapshot { rows, at });
        }
    }

    /// The first reading has nothing behind it to difference, so it records a
    /// baseline and appends nothing. A zero would claim an idle sample that
    /// was never measured.
    #[test]
    fn the_first_reading_records_a_baseline_and_no_sample() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        assert!(app.cpu_history(1).is_empty());
    }

    /// 2000 CPU-milliseconds across a two-second poll is one core.
    #[test]
    fn two_readings_difference_into_one_sample() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        assert_eq!(app.cpu_history(1), &[100.0]);
    }

    /// The 15s baseline is what this whole change exists to stop mattering. A
    /// one-second burst reads once and then reads zero, rather than decaying
    /// across the next seven polls.
    #[test]
    fn a_burst_does_not_smear_across_later_polls() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 1_000)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 1_000)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 1_000)]);
        assert_eq!(app.cpu_history(1), &[50.0, 0.0, 0.0]);
    }

    /// A sheep with no reading appends a zero rather than a gap: the chart is
    /// one cell per sample, and a skipped sample would slide the whole window
    /// and make an old spike look recent. The stored reading goes with it, so
    /// the next live reading is not differenced across the stop.
    #[test]
    fn an_unsampled_sheep_appends_a_zero_and_forgets_its_baseline() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        app.on_snapshot(vec![row_without_cpu(1)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 9_000)]);
        assert_eq!(app.cpu_history(1), &[100.0, 0.0]);
    }

    /// `App::cpu_now` is the source every CPU figure lookout draws reads
    /// through, and it must agree with the sparkline beside it: `None`
    /// while one poll has nothing differenced yet, then the same newest
    /// sample [`App::cpu_history`] holds once a second poll has something
    /// to difference against.
    #[test]
    fn cpu_now_reads_none_after_one_poll_and_matches_cpu_history_after_two() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        assert_eq!(app.cpu_now(1), None, "one poll has nothing to difference");
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        assert_eq!(app.cpu_now(1), Some(100.0));
        assert_eq!(
            app.cpu_history(1).last().copied(),
            app.cpu_now(1),
            "the figure and the sparkline's newest cell must be the same number"
        );
    }

    /// `App::record_samples` still appends a zero to the history buffer for
    /// a sheep with no current reading, so the sparkline's window does not
    /// slide. `App::cpu_now` must not read that buffered zero back as a
    /// figure: a sheep whose `cpu_ms` is `None` this poll has nothing
    /// measured, and `0.0%` would claim otherwise.
    #[test]
    fn cpu_now_reads_none_for_a_sheep_with_no_current_reading() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        app.on_snapshot(vec![row_without_cpu(1)]);
        assert_eq!(
            app.cpu_history(1),
            &[100.0, 0.0],
            "sanity: the buffer still holds the appended zero"
        );
        assert_eq!(app.cpu_now(1), None);
    }

    /// A departed sheep's baseline must not survive to be inherited by an
    /// unrelated sheep that later reuses its id. Without
    /// [`App::record_samples`]'s `cpu_last.retain`, the third poll below
    /// would difference the new sheep's tiny counter against the departed
    /// sheep's much larger one and manufacture a sample, instead of
    /// recording an honest baseline and appending nothing.
    #[test]
    fn a_departed_sheeps_baseline_is_not_inherited_by_a_reused_id() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 9_000)]);
        // Sheep 1 leaves the flock entirely.
        app.on_snapshot(vec![]);
        // An unrelated sheep reuses id 1, with its own counter starting low.
        app.on_snapshot(vec![row_with_cpu_ms(1, 12)]);
        assert!(
            app.cpu_history(1).is_empty(),
            "the reused id's first reading should record a baseline and \
             append nothing, on `Self::cpu_history`'s own terms for a first \
             reading: {:?}",
            app.cpu_history(1)
        );
    }

    /// A respawn gives a new tree whose counter starts below the old one's.
    /// Clamped to zero, the same rule the daemon applies, and it costs one
    /// dropped sample rather than a negative spike.
    /// A respawn keeps the id and takes a new pid, so differencing across it
    /// would subtract a dead process's counter from a live one's.
    ///
    /// The counter rising across the boundary is the case `saturating_sub`
    /// cannot save: it reads as a real delta and underreports the new
    /// process by exactly what the old one had spent. A new process is a
    /// first reading, so it records a baseline and appends nothing.
    #[test]
    fn a_respawn_under_the_same_id_starts_a_new_baseline() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_pid_and_cpu_ms(1, 100, 50)]);
        app.on_snapshot(vec![row_with_pid_and_cpu_ms(1, 100, 2_050)]);
        assert_eq!(app.cpu_history(1), &[100.0], "the live process differences");
        app.on_snapshot(vec![row_with_pid_and_cpu_ms(1, 200, 3_000)]);
        assert_eq!(
            app.cpu_history(1),
            &[100.0],
            "the new pid appends nothing rather than differencing 3000 against 2050"
        );
    }

    #[test]
    fn a_counter_that_went_backwards_reads_zero() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 9_000)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 12)]);
        assert_eq!(app.cpu_history(1), &[0.0]);
    }

    /// RSS is sampled at an instant, so it is buffered as it arrives with no
    /// differencing at all.
    #[test]
    fn rss_is_buffered_as_read() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_rss(1, 1_024)]);
        app.on_snapshot(vec![row_with_rss(1, 2_048)]);
        assert_eq!(app.rss_history(1), &[1_024, 2_048]);
    }

    /// Same depth and same drop-on-leave rule as the CPU buffer.
    #[test]
    fn a_sheep_that_leaves_takes_its_rss_history_too() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_rss(1, 1_024), row_with_rss(2, 512)]);
        app.on_snapshot(vec![row_with_rss(1, 1_024)]);
        assert!(app.rss_history(2).is_empty());
    }

    #[test]
    fn the_buffer_holds_at_most_a_hundred_and_forty_samples() {
        // Each poll's counter climbs by a distinct step, so each differenced
        // percent is distinct too; a wrong-end eviction or a reversed order
        // fails this, not just a wrong length.
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        let mut counter: u64 = 0;
        for i in 1..=200u64 {
            counter += i * 20;
            app.on_snapshot(vec![row_with_cpu_ms(1, counter)]);
        }
        let history = app.cpu_history(1);
        assert_eq!(history.len(), 140);
        assert_eq!(history.first(), Some(&61.0), "oldest survivor");
        assert_eq!(history.last(), Some(&200.0), "newest sample");
    }

    #[test]
    fn a_sheep_that_leaves_the_flock_takes_its_history_with_it() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0), row_with_cpu_ms(2, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        assert!(
            app.cpu_history(2).is_empty(),
            "a deleted sheep leaves no history behind"
        );
    }

    /// The first poll only records a baseline for each sheep and contributes
    /// nothing to the sum, so the series starts with a zero.
    #[test]
    fn the_flock_series_is_the_sum_of_the_snapshot() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0), row_with_cpu_ms(2, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000), row_with_cpu_ms(2, 1_000)]);
        assert_eq!(app.flock_cpu_history(), &[0.0, 150.0]);
    }
}
