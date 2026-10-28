//! The flock-wide values one frame's rows all share.

use std::collections::HashSet;

use super::super::super::app::App;

/// The three values every row in a frame reads the same answer for.
///
/// Each is a fact about the whole flock rather than about a row. Built per
/// frame rather than cached on [`App`]: a cache that misses a snapshot, a
/// sample pass or a filter edit draws a stale table.
#[derive(Debug)]
pub struct FrameFacts<'a> {
    /// The ceiling every CPU sparkline scales against, so the column stays
    /// comparable down the table. [`App::cpu_ceiling`] explains the floor.
    pub cpu_ceiling: f32,
    /// Every app name drawing under a [`crate::lookout::app::RowKey::Group`]
    /// header, by [`App::grouped_names`]'s rule.
    pub grouped: HashSet<&'a str>,
    /// The whole flock's memory, the denominator of a fold's share bar.
    ///
    /// Summed over [`App::all_rows`] rather than the set [`App::rows_len`]
    /// counts, the same way `view::host::strip_line` sums it, so a name filter
    /// never changes what a share bar divides by. `None` only when nothing in
    /// the flock has reported a reading.
    pub flock_memory: Option<u64>,
}

impl<'a> FrameFacts<'a> {
    /// Reads all three off `app`.
    #[must_use]
    pub fn new(app: &'a App) -> Self {
        Self {
            cpu_ceiling: app.cpu_ceiling(),
            grouped: app.grouped_names(),
            flock_memory: app
                .all_rows()
                .iter()
                .filter_map(|row| row.info.memory_bytes)
                .fold(None, |sum, value| Some(sum.unwrap_or(0) + value)),
        }
    }

    /// Whether `name`'s instances draw under a group header.
    #[must_use]
    pub fn is_grouped(&self, name: &str) -> bool {
        self.grouped.contains(name)
    }
}
