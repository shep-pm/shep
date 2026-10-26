//! Which rows the pane draws, and where the cursor is among them.
//!
//! The split from [`super::config`] is the one the pane's own controls make:
//! what a target holds and what it answers about a key stay there, while the
//! group the field list is drawing, the row list itself, and the cursor's walk
//! through it live here.

use shep_core::config::GROUP_ORDER;

use super::super::viewport::Viewport;
#[cfg(test)]
use super::entries::EntryBody;
use super::{ConfigPane, PaneRow, PaneTarget};

impl ConfigPane {
    /// The cursor and offset.
    #[must_use]
    pub fn view(&self) -> &Viewport {
        &self.view
    }

    /// Records the terminal's height, in rows of data.
    pub fn set_rows(&mut self, rows: usize) {
        let len = self.rows().len();
        self.view.set_rows(rows, len);
    }

    /// The active group's name, one of the nine in [`GROUP_ORDER`],
    /// defaulting to the first.
    ///
    /// Meaningless for a dog pane in the sense that nothing filters on it:
    /// a dog's schema carries no `init.group`, so every one of its fields
    /// is visible under any name this returns. See [`Self::rows`].
    #[must_use]
    pub fn group(&self) -> &'static str {
        GROUP_ORDER
            .get(self.group)
            .copied()
            .unwrap_or(GROUP_ORDER[0])
    }

    /// Walks to the next group, wrapping from the last back to the first.
    pub fn next_group(&mut self) {
        self.group = (self.group + 1) % GROUP_ORDER.len();
    }

    /// Walks to the previous group, wrapping from the first back to the last.
    pub fn prev_group(&mut self) {
        self.group = (self.group + GROUP_ORDER.len() - 1) % GROUP_ORDER.len();
    }

    /// Jumps to the `digit`th group, one-based, the way `1`..`9` name them
    /// on the tab row. A digit past [`GROUP_ORDER`]'s length is ignored, so
    /// a tenth group added later needs a key of its own before it is
    /// reachable.
    pub fn set_group(&mut self, digit: u8) {
        let Some(index) = usize::from(digit).checked_sub(1) else {
            return;
        };
        if index < GROUP_ORDER.len() {
            self.group = index;
        }
    }

    /// One row per field of the active group, in display order, then, on a
    /// sheep's `process` tab, its own env keys and the row that adds one. A field carrying no
    /// group at all (every field on a dog pane, whose schema declares
    /// none) is visible regardless of which group is active, which is
    /// what keeps a dog's flat list undisturbed by a control meant for a
    /// sheep's nine.
    ///
    /// The `env` field itself is left out of the field portion for a
    /// sheep: its own [`PaneRow::Env`] rows are what replaced the sub-screen
    /// it used to open, and a row that still showed the field's own "N
    /// keys" summary beside them would be the same fact said twice. A
    /// dog's schema is somebody else's and can declare a field named
    /// `env` of its own, which stays.
    ///
    /// A sheep's `dogs` tab draws no field at all: its rows are the dogs,
    /// and the table of each one that is open. See [`Self::dog_rows`].
    ///
    /// A dog's pane draws its outline instead: a table's row, then what is
    /// under it while it is open. See [`Self::open_table`].
    #[must_use]
    pub fn rows(&self) -> Vec<PaneRow> {
        if self.is_tree() {
            let mut rows = Vec::new();
            self.push_tree_rows(&self.outline.roots, &mut rows);
            return rows;
        }
        let group = self.group();
        let is_sheep = matches!(self.target, PaneTarget::Sheep { .. });
        if self.shows_dogs() {
            return self.dog_rows();
        }
        let mut rows: Vec<PaneRow> = self
            .fields
            .fields()
            .iter()
            .enumerate()
            .filter(|(_, field)| !is_sheep || field.key != "env")
            .filter(|(_, field)| field.group.as_deref().is_none_or(|g| g == group))
            .map(|(index, _)| PaneRow::Field(index))
            .collect();
        if self.shows_env() {
            rows.extend((0..self.env_keys.len()).map(PaneRow::Env));
            rows.push(PaneRow::AddEnv);
        }
        rows
    }

    /// Whether the env rows and the row that adds one belong to the tab the
    /// pane is on: a sheep's `process` tab, and no other.
    #[must_use]
    pub fn shows_env(&self) -> bool {
        matches!(self.target, PaneTarget::Sheep { .. }) && self.group() == GROUP_ORDER[0]
    }

    /// [`Self::rows`]'s own [`PaneRow::Field`] and [`PaneRow::Table`] entries, alone: what the
    /// field body's own scroll walk lays out. [`Self::rows`] is the
    /// cursor's whole walk, env rows included; this is the narrower list
    /// the renderer needs to lay the field portion out on its own, since
    /// `view::scroll::to_cursor` has to know how many rows exist
    /// in the body it is walking, not in the cursor's wider one.
    #[must_use]
    pub(in crate::lookout) fn field_rows(&self) -> Vec<PaneRow> {
        self.rows()
            .into_iter()
            .filter(|row| {
                matches!(
                    row,
                    PaneRow::Field(_)
                        | PaneRow::Table(_)
                        | PaneRow::Entry(_)
                        | PaneRow::Choice { .. }
                        | PaneRow::Arm { .. }
                )
            })
            .collect()
    }

    /// The row under the cursor, or `None` for an empty form.
    #[must_use]
    pub fn cursor(&self) -> Option<PaneRow> {
        let rows = self.rows();
        rows.get(self.view.cursor()).copied()
    }

    pub(in crate::lookout) fn move_by(&mut self, delta: isize) {
        let len = self.rows().len();
        self.view.move_by(delta, len);
        self.push_focus();
    }

    pub(in crate::lookout) fn move_to_first(&mut self) {
        let len = self.rows().len();
        self.view.move_to(0, len);
        self.push_focus();
    }

    pub(in crate::lookout) fn move_to_last(&mut self) {
        let len = self.rows().len();
        self.view.move_to(len.saturating_sub(1), len);
        self.push_focus();
    }

    /// Adopts a previous pane's cursor and offset, clamped to this one's
    /// own row count. What a refresh of an already-open pane rides on, so
    /// `r` does not throw the operator back to the first field.
    pub(in crate::lookout) fn adopt_view(&mut self, view: Viewport) {
        self.view = view;
        let len = self.rows().len();
        self.view.clamp(len);
    }

    /// Lands on the `index`th group, as carried from a pane this one replaces.
    pub(in crate::lookout) fn adopt_group(&mut self, index: usize) {
        if index < GROUP_ORDER.len() {
            self.group = index;
        }
    }

    /// The active group's position in [`GROUP_ORDER`].
    pub(in crate::lookout) fn group_index(&self) -> usize {
        self.group
    }

    /// Switches to `key`'s own group, then walks the cursor onto it, the
    /// way an operator reaches a field in a different group than the one
    /// the pane opened on, and opens the tables above it on a tree pane. A
    /// no-op for a name no field carries.
    #[cfg(test)]
    pub(crate) fn move_to_key(&mut self, key: &str) {
        // A field inside an entry takes the group of the entries field above
        // it, whose key it starts with.
        let group = match self.fields.by_key(key) {
            Some(field) => field.group.clone(),
            None if self.entries.leaf(key).is_some() => self
                .fields
                .fields()
                .iter()
                .filter(|field| key.starts_with(&format!("{}[", field.key)))
                .find_map(|field| field.group.clone()),
            None => return,
        };
        if let Some(group) = group
            && let Some(index) = GROUP_ORDER.iter().position(|known| *known == group)
        {
            self.group = index;
        }
        self.open_down_to(key);
        if let Some(row_index) = self.rows().iter().position(|row| match row {
            PaneRow::Field(field_index) => self.fields.fields()[*field_index].key == key,
            PaneRow::Entry(entry) => self.entries.row(*entry).is_some_and(
                |row| matches!(&row.body, EntryBody::Leaf { field, .. } if field.key == key),
            ),
            PaneRow::Table(_)
            | PaneRow::Choice { .. }
            | PaneRow::Arm { .. }
            | PaneRow::Env(_)
            | PaneRow::AddEnv
            | PaneRow::Dog(_)
            | PaneRow::Under { .. } => false,
        }) {
            let len = self.rows().len();
            self.view.move_to(row_index, len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::web;
    use super::*;

    #[test]
    fn the_cursor_walks_the_rows_and_clamps_at_both_ends() {
        let mut pane = ConfigPane::sheep(web());
        assert_eq!(pane.cursor(), Some(PaneRow::Field(0)));
        pane.move_by(1);
        assert_eq!(pane.cursor(), Some(PaneRow::Field(1)));
        pane.move_by(-5);
        assert_eq!(pane.cursor(), Some(PaneRow::Field(0)));
        pane.move_to_last();
        // `process`, the group a fresh pane opens on, has ten fields, at
        // indices `0..10` since it sorts first, then `web`'s own one env
        // key and the row that adds another.
        assert_eq!(pane.cursor(), Some(PaneRow::AddEnv));
        assert_eq!(
            pane.fields().fields()[9].key,
            "user",
            "the last field is process's own last one"
        );
        pane.move_to_first();
        assert_eq!(pane.cursor(), Some(PaneRow::Field(0)));
    }

    #[test]
    fn the_cursor_reaches_an_env_row_from_the_process_tab_and_no_other() {
        let mut pane = ConfigPane::sheep(web());
        for digit in 1..=9 {
            pane.set_group(digit);
            let on_env = |row: &PaneRow| matches!(row, PaneRow::Env(_) | PaneRow::AddEnv);
            assert_eq!(
                pane.rows().iter().any(on_env),
                digit == 1,
                "tab {digit} lists env rows"
            );
            pane.move_to_last();
            assert_eq!(
                pane.cursor().as_ref().is_some_and(on_env),
                digit == 1,
                "tab {digit}: the last row"
            );
        }
    }

    #[test]
    fn a_refreshed_pane_keeps_the_cursor_it_had() {
        let mut pane = ConfigPane::sheep(web());
        pane.set_rows(10);
        pane.move_to_last();
        let carried = pane.view().clone();
        let mut fresh = ConfigPane::sheep(web());
        fresh.adopt_view(carried);
        assert_eq!(fresh.cursor(), Some(PaneRow::AddEnv));
    }
}
