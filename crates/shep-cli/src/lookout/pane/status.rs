//! Which tab a dog's config pane shows, and the cursor on its status tab.
//!
//! The cursor is one flat position over the rows of every table, in order, so
//! `j` and `k` walk the whole status tab as one list.

use shep_core::dogs::pane::{Pane, Section};

#[cfg(test)]
mod tests;

/// Which of a pane dog's two tabs is open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    /// The sections the dog answered `shep.pane` with.
    #[default]
    Status,
    /// The settings form, today's pane.
    Settings,
}

/// A move of the status tab's cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::lookout) enum Step {
    /// One row up.
    Up,
    /// One row down.
    Down,
    /// The first row.
    First,
    /// The last row.
    Last,
}

/// The open tab and the cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct StatusView {
    tab: Tab,
    cursor: Option<usize>,
}

impl StatusView {
    /// The open tab.
    pub(super) fn tab(self) -> Tab {
        self.tab
    }

    /// The row the cursor is on, counted over every table's rows in order.
    /// [`None`] while no table holds a row.
    pub(super) fn cursor(self) -> Option<usize> {
        self.cursor
    }

    /// Opens the tab that is not open.
    pub(super) fn switch(&mut self) {
        self.tab = match self.tab {
            Tab::Status => Tab::Settings,
            Tab::Settings => Tab::Status,
        };
    }

    /// Moves the cursor over `rows` rows, stopping at either end.
    pub(super) fn step(&mut self, step: Step, rows: usize) {
        let Some(last) = rows.checked_sub(1) else {
            self.cursor = None;
            return;
        };
        let at = self.cursor.unwrap_or(0).min(last);
        self.cursor = Some(match step {
            Step::Up => at.saturating_sub(1),
            Step::Down => (at + 1).min(last),
            Step::First => 0,
            Step::Last => last,
        });
    }

    /// Keeps the cursor on a row of a pane that now holds `rows` rows.
    pub(super) fn settle(&mut self, rows: usize) {
        self.cursor = rows
            .checked_sub(1)
            .map(|last| self.cursor.unwrap_or(0).min(last));
    }
}

/// How many rows the cursor walks: every table's rows, summed.
pub(super) fn row_count(pane: &Pane) -> usize {
    pane.sections
        .iter()
        .map(|section| match section {
            Section::Table(table) => table.rows.len(),
            _other => 0,
        })
        .sum()
}
