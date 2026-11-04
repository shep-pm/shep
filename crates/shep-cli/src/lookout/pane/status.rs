//! Which tab a dog's config pane shows, and the cursor on its status tab.
//!
//! The cursor is one flat position over the rows of every table, in order, so
//! `j` and `k` walk the whole status tab as one list. Past the first or last
//! row, or with no rows at all, the same keys scroll the tab a line at a time,
//! so a section no row can reach is still read.

use core::ops::Range;

use shep_core::dogs::pane::{Pane, PaneAction, Row, Section};

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

/// Where the lines of a ready pane's body fall, as the view lays them out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::lookout) struct Layout {
    /// How many lines the body holds.
    pub(in crate::lookout) total: usize,
    /// The line of each table row, in the cursor's order.
    row_lines: Vec<usize>,
}

impl Layout {
    /// The line the cursor is on, when it is on a row.
    pub(in crate::lookout) fn line_of(&self, cursor: Option<usize>) -> Option<usize> {
        cursor.and_then(|row| self.row_lines.get(row).copied())
    }
}

/// The layout of `pane`'s body: a blank line, then each section under its
/// title, a blank line between two of them.
pub(super) fn layout(pane: &Pane) -> Layout {
    let mut at = Layout {
        total: 1,
        row_lines: Vec::new(),
    };
    let mut blank_last = true;
    for section in &pane.sections {
        if !blank_last {
            at.total += 1;
        }
        blank_last = false;
        match section {
            Section::Fields(fields) => at.total += 1 + fields.fields.len().max(1),
            Section::Table(table) if table.rows.is_empty() => at.total += 2,
            Section::Table(table) => {
                // The title and the header come first.
                at.total += 2;
                for _ in &table.rows {
                    at.row_lines.push(at.total);
                    at.total += 1;
                }
            }
            // Drawn as nothing, so the blank line before it is still last.
            _unknown => blank_last = true,
        }
    }
    at
}

/// The letters lookout answers itself on a status tab: quit and the four
/// select keys. No action takes one.
pub(in crate::lookout) const RESERVED: [char; 5] = ['q', 'j', 'k', 'g', 'G'];

/// One action of a pane and the letter that runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::lookout) struct PaneKey {
    /// The letter that runs it. [`None`] for an action the manifest does not
    /// declare, or when all 22 free letters are taken.
    pub(in crate::lookout) letter: Option<char>,
    /// The action as the dog declared it.
    pub(in crate::lookout) action: PaneAction,
    /// The cursor positions of the rows it runs on, or [`None`] for a pane
    /// action.
    pub(in crate::lookout) rows: Option<Range<usize>>,
}

/// The actions of `pane` in declared order, each table's row actions in the
/// order of the sections, then the pane's own, with their letters.
///
/// An action takes the first free letter of its label, else the first free
/// one from `a` to `z`. A letter is free when no earlier action took it and
/// it is not [`RESERVED`]. An action absent from `declared` takes none.
pub(in crate::lookout) fn keyed(pane: &Pane, declared: &[String]) -> Vec<PaneKey> {
    let mut found = Vec::new();
    let mut first_row = 0;
    for section in &pane.sections {
        if let Section::Table(table) = section {
            let rows = first_row..first_row + table.rows.len();
            first_row = rows.end;
            for action in &table.row_actions {
                found.push((action, Some(rows.clone())));
            }
        }
    }
    found.extend(pane.actions.iter().map(|action| (action, None)));

    let mut taken = Vec::new();
    found
        .into_iter()
        .map(|(action, rows)| {
            let letter = declared
                .contains(&action.action)
                .then(|| free_letter(&action.label, &taken))
                .flatten();
            taken.extend(letter);
            PaneKey {
                letter,
                action: action.clone(),
                rows,
            }
        })
        .collect()
}

fn free_letter(label: &str, taken: &[char]) -> Option<char> {
    let free = |letter: &char| !RESERVED.contains(letter) && !taken.contains(letter);
    label
        .chars()
        .map(|letter| letter.to_ascii_lowercase())
        .filter(char::is_ascii_lowercase)
        .find(free)
        .or_else(|| ('a'..='z').find(free))
}

/// The row at cursor position `cursor`, counted over every table in order.
pub(in crate::lookout) fn row_at(pane: &Pane, cursor: usize) -> Option<&Row> {
    pane.sections
        .iter()
        .filter_map(|section| match section {
            Section::Table(table) => Some(table.rows.iter()),
            _fields_or_unknown => None,
        })
        .flatten()
        .nth(cursor)
}

/// The first body line a window of `room` lines shows so the cursor's line is
/// in it, a line short of either end for the marker that may sit there.
///
/// The view draws exactly this when nothing has scrolled past the cursor.
pub(in crate::lookout) fn follow_start(
    cursor_line: Option<usize>,
    total: usize,
    room: usize,
) -> usize {
    let Some(spare) = total.checked_sub(room) else {
        return 0;
    };
    (cursor_line.unwrap_or(0) + 2)
        .saturating_sub(room)
        .min(spare)
}

/// The open tab, the cursor, and how far the tab is scrolled past it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct StatusView {
    tab: Tab,
    cursor: Option<usize>,
    /// The first body line drawn, once a key scrolled the tab away from the
    /// cursor. [`None`] while the window follows the cursor.
    scroll: Option<usize>,
    /// How many body lines the tab has room for, or 0 before the first draw.
    room: usize,
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

    /// The first body line to draw when a key scrolled past the cursor.
    pub(super) fn scroll(self) -> Option<usize> {
        self.scroll
    }

    /// Opens the tab that is not open.
    pub(super) fn switch(&mut self) {
        self.tab = match self.tab {
            Tab::Status => Tab::Settings,
            Tab::Settings => Tab::Status,
        };
    }

    /// Records how many body lines the tab has room for.
    pub(super) fn set_room(&mut self, room: usize, layout: &Layout) {
        self.room = room;
        self.clamp_scroll(layout);
    }

    /// Moves the cursor over the rows of `layout`, stopping at either end. A
    /// step the cursor cannot take scrolls the tab a line instead.
    pub(super) fn step(&mut self, step: Step, layout: &Layout) {
        let Some(last) = layout.row_lines.len().checked_sub(1) else {
            self.cursor = None;
            self.scroll_by(step, layout);
            return;
        };
        let at = self.cursor.unwrap_or(0).min(last);
        let line = matches!(step, Step::Up | Step::Down);
        let blocked = match step {
            Step::Up => at == 0,
            Step::Down => at == last,
            Step::First | Step::Last => false,
        };
        if line && (blocked || self.scroll.is_some()) {
            self.cursor = Some(at);
            self.scroll_by(step, layout);
            return;
        }
        self.scroll = None;
        self.cursor = Some(match step {
            Step::Up => at.saturating_sub(1),
            Step::Down => (at + 1).min(last),
            Step::First => 0,
            Step::Last => last,
        });
    }

    /// Keeps the cursor on a row of a pane that now holds `layout`.
    pub(super) fn settle(&mut self, layout: &Layout) {
        self.cursor = layout
            .row_lines
            .len()
            .checked_sub(1)
            .map(|last| self.cursor.unwrap_or(0).min(last));
        self.clamp_scroll(layout);
    }

    fn furthest(self, layout: &Layout) -> usize {
        layout.total.saturating_sub(self.room)
    }

    /// Scrolls a line up or down, or to the top or the bottom. Does nothing
    /// before the first draw, when the room is not known.
    fn scroll_by(&mut self, step: Step, layout: &Layout) {
        if self.room == 0 {
            return;
        }
        let follows = follow_start(layout.line_of(self.cursor), layout.total, self.room);
        let from = self.scroll.unwrap_or(follows);
        let to = match step {
            Step::Up => from.saturating_sub(1),
            Step::Down => (from + 1).min(self.furthest(layout)),
            Step::First => 0,
            Step::Last => self.furthest(layout),
        };
        self.scroll = (to != follows).then_some(to);
    }

    fn clamp_scroll(&mut self, layout: &Layout) {
        if self.room == 0 {
            self.scroll = None;
            return;
        }
        let furthest = self.furthest(layout);
        self.scroll = self.scroll.map(|at| at.min(furthest));
    }
}
