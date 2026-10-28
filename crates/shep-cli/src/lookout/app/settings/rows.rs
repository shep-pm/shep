//! The rows the settings screen's cursor walks: the schema's tree, then one
//! block per candidate dog, and the viewport that shows a slice of them.

use super::*;

impl Settings {
    /// What the screen reads off disk, and renders every row's value and source
    /// from.
    ///
    /// A landed write does not update this in place: it raises a fresh
    /// [`Effect::LoadSettings`], so `Set` and `Unset` land the same way and
    /// neither can drift from the rest of the document.
    #[must_use]
    pub fn snapshot(&self) -> &SettingsSnapshot {
        &self.snapshot
    }

    /// Every row the cursor can sit on: the sections and their keys in the
    /// order the screen draws them, a map's entries under it, then one row per
    /// candidate dog, with that dog's own rows under it while its row is open.
    #[must_use]
    pub fn rows(&self) -> Vec<SettingsRow> {
        let shape = shep_toml_shape();
        let mut rows = Vec::new();
        self.push_children(&shape.outline.roots, &mut rows);
        for index in 0..self.snapshot.dogs.len() {
            self.push_dog_rows(index, &mut rows);
        }
        rows
    }

    /// One dog's block: its header row, then, while its row is open, the four
    /// `[daemon] keys that belong to it, the checklist under `grants`, and the
    /// rows of its own config tree.
    fn push_dog_rows(&self, index: usize, rows: &mut Vec<SettingsRow>) {
        rows.push(SettingsRow::Dog(index));
        let Some(dog) = self.snapshot.dogs.get(index) else {
            return;
        };
        let Some(tree) = self.dog_trees.get(&dog.name) else {
            return;
        };
        if !tree.is_open() {
            return;
        }
        for key in DogKey::ALL {
            rows.push(SettingsRow::DogKey { dog: index, key });
            if key != DogKey::Grants {
                continue;
            }
            rows.extend((0..dog.offered.len()).map(|item| SettingsRow::DogGrant {
                dog: index,
                index: item,
            }));
        }
        let Some(pane) = tree.pane() else {
            return;
        };
        rows.extend((0..pane.rows().len()).map(|row| SettingsRow::DogField { dog: index, row }));
    }

    fn push_children(&self, children: &[Child], rows: &mut Vec<SettingsRow>) {
        let shape = shep_toml_shape();
        for child in children {
            match *child {
                Child::Table(index) => {
                    rows.push(SettingsRow::Table(index));
                    let table = &shape.outline.tables[index];
                    if !self.folded.contains(&table.path) {
                        self.push_children(&table.children, rows);
                    }
                }
                Child::Leaf(index) => {
                    rows.push(SettingsRow::Field(index));
                    let field = &shape.fields.fields()[index];
                    if field.kind == FieldKind::Map && !self.folded.contains(&field.key) {
                        let entries = self.snapshot.entries(&field.key).len();
                        rows.extend((0..entries).map(|entry| SettingsRow::Entry {
                            map: index,
                            index: entry,
                        }));
                        rows.push(SettingsRow::AddEntry(index));
                    }
                }
            }
        }
    }

    /// How many rows [`Self::rows`] would build.
    #[must_use]
    pub fn rows_len(&self) -> usize {
        self.rows().len()
    }

    /// The row the cursor sits on. `None` only if [`Self::rows`] is empty,
    /// which the schema's sections make unreachable.
    #[must_use]
    pub fn cursor(&self) -> Option<SettingsRow> {
        let rows = self.rows();
        rows.get(self.view.cursor().min(rows.len().saturating_sub(1)))
            .copied()
    }

    /// Folds the table or map under the cursor if it is open, opens it if it
    /// is folded. Answers whether the cursor was on one.
    pub(in crate::lookout::app) fn toggle_fold(&mut self) -> bool {
        let shape = shep_toml_shape();
        let key = match self.cursor() {
            Some(SettingsRow::Table(index)) => shape.outline.tables[index].path.clone(),
            Some(SettingsRow::Field(index))
                if shape.fields.fields()[index].kind == FieldKind::Map =>
            {
                shape.fields.fields()[index].key.clone()
            }
            _ => return false,
        };
        if !self.folded.remove(&key) {
            self.folded.insert(key);
        }
        true
    }

    /// Whether the table or map at the dotted `key` shows what is inside it.
    #[must_use]
    pub fn is_open(&self, key: &str) -> bool {
        !self.folded.contains(key)
    }

    /// Moves the cursor by `delta` rows, clamped to [`Self::rows`], never
    /// wrapping.
    pub(in crate::lookout::app) fn move_by(&mut self, delta: isize) {
        let len = self.rows_len();
        self.view.move_by(delta, len);
    }

    pub(in crate::lookout::app) fn move_to_first(&mut self) {
        let len = self.rows_len();
        self.view.move_to(0, len);
    }

    pub(in crate::lookout::app) fn move_to_last(&mut self) {
        let len = self.rows_len();
        self.view.move_to(len.saturating_sub(1), len);
    }

    /// The viewport, for the renderer.
    #[must_use]
    pub fn view(&self) -> &Viewport {
        &self.view
    }

    /// Records the terminal's height.
    pub fn set_rows(&mut self, rows: usize) {
        let len = self.rows_len();
        self.view.set_rows(rows, len);
    }
}
