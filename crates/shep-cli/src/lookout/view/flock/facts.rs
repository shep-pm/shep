//! The flock-wide values one frame's rows all share.

use std::collections::HashSet;

use super::super::super::app::App;

/// The three values every row in a frame reads the same answer for.
///
/// Each one is a fact about the whole flock rather than about a row, and each
/// was once derived inside the per-row draw path, so a frame paid its
/// whole-flock scan once per visible row instead of once. `paint_frame` builds
/// this before the row loop and hands it to [`super::key_line`] and
/// [`super::fold_key_line`].
///
/// Built per frame rather than cached on [`App`]: the flock moves under a
/// snapshot, a sample pass and a filter edit, and a cache that misses one of
/// those draws a stale table. Three passes over the flock per frame is already
/// the whole of the win.
pub struct FrameFacts<'a> {
    /// The ceiling every CPU sparkline scales against, so the column stays
    /// comparable down the table. [`App::cpu_ceiling`] explains the floor.
    pub cpu_ceiling: f32,
    /// Every app name drawing under a [`crate::lookout::app::RowKey::Group`]
    /// header, by [`App::grouped_names`]'s rule.
    pub grouped: HashSet<&'a str>,
    /// The whole flock's memory, the denominator of a fold's share bar.
    ///
    /// Summed over [`App::all_rows`] rather than the name-filtered
    /// [`App::rows_len`]'s set, the same way `view::host::strip_line` sums it, so a name filter never
    /// changes what a share bar divides by. `None` only when nothing in the
    /// flock has reported a reading.
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
