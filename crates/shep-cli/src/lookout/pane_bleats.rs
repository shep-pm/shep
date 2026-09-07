//! The full-screen bleats pane's own state: which sheep it is pinned to.

use super::app::RowKey;

/// The full-screen bleats pane's own state.
///
/// Holds the sheep it opened on rather than reading the dashboard's
/// selection: full screen leaves no table on which to change one, so the
/// pane describes a single sheep for as long as it is open.
///
/// `Debug` is derived. A row key and a cursor position carry no env, no
/// path and no argument vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleatsPane {
    sheep: RowKey,
}

impl BleatsPane {
    /// Opens the pane on one sheep.
    #[must_use]
    pub fn new(sheep: RowKey) -> Self {
        Self { sheep }
    }

    /// The sheep this pane describes, fixed for its lifetime.
    #[must_use]
    pub fn sheep(&self) -> &RowKey {
        &self.sheep
    }
}
