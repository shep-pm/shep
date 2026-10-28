//! The editor a keypress opens on the cursor's row: where it writes, what it
//! opens with, and the edit that removes the row.

use super::*;

impl Settings {
    /// Where an editor opened on the cursor's row writes, and what it opens
    /// with. `None` on a row that cycles, folds or is not a key.
    pub(in crate::lookout::app) fn editor_at_cursor(&self) -> Option<(Target, String)> {
        let shape = shep_toml_shape();
        let fields = shape.fields.fields();
        match self.cursor()? {
            SettingsRow::Field(index) => {
                let field = &fields[index];
                if !field.editable || !is_typed(&field.kind) {
                    return None;
                }
                let path = shape.paths.get(&field.key)?.clone();
                let kind = Shape::of(&field.kind)?;
                Some((
                    Target::Value { path, shape: kind },
                    self.snapshot.seed(field),
                ))
            }
            SettingsRow::Entry { map, index } => {
                let key = &fields[map].key;
                let (name, value) = self.snapshot.entries(key).into_iter().nth(index)?;
                Some((
                    Target::Value {
                        path: vec![key.clone(), name],
                        shape: Shape::Text,
                    },
                    value,
                ))
            }
            SettingsRow::AddEntry(map) => Some((
                Target::NewEntry {
                    path: shape.paths.get(&fields[map].key)?.clone(),
                },
                String::new(),
            )),
            SettingsRow::DogField { dog, row } => {
                let (name, key, seed) = self.dog_field_editor(dog, row)?;
                Some((Target::DogValue { name, key, row }, seed))
            }
            SettingsRow::Table(_)
            | SettingsRow::Dog(_)
            | SettingsRow::DogKey { .. }
            | SettingsRow::DogGrant { .. } => None,
        }
    }

    /// What an editor opened on one row of a dog's own tree writes, and what
    /// it opens with. `None` for a row that folds, cycles, is locked, or is a
    /// secret, which is seeded empty rather than with a value the screen is
    /// not allowed to show.
    fn dog_field_editor(&self, dog: usize, row: usize) -> Option<(String, String, String)> {
        let name = self.snapshot.dogs.get(dog)?.name.clone();
        let pane = self.dog_tree(dog)?.pane()?;
        let PaneRow::Field(index) = pane.rows().get(row)?.to_owned() else {
            return None;
        };
        let field = pane.fields().fields().get(index)?;
        if !field.editable || !is_typed(&field.kind) || pane.lock(&field.key).is_some() {
            return None;
        }
        // A secret's value is never held here, so the editor opens empty, the
        // same way a dog pane's does.
        let seed = if field.secret {
            String::new()
        } else {
            pane.value(&field.key)
        };
        Some((name, field.key.clone(), seed))
    }

    /// The edit that removes the cursor's row from `shep.toml`, or `None`
    /// where there is nothing to remove: a section, a dog, or a key the file
    /// never wrote.
    pub(in crate::lookout::app) fn removal_at_cursor(&self) -> Option<SettingEdit> {
        let shape = shep_toml_shape();
        let fields = shape.fields.fields();
        match self.cursor()? {
            SettingsRow::Field(index) => {
                let field = &fields[index];
                self.snapshot.file_value(&field.key)?;
                Some(SettingEdit::Unset {
                    path: shape.paths.get(&field.key)?.clone(),
                })
            }
            SettingsRow::Entry { map, index } => {
                let key = &fields[map].key;
                let (name, _) = self.snapshot.entries(key).into_iter().nth(index)?;
                Some(SettingEdit::Unset {
                    path: vec![key.clone(), name],
                })
            }
            SettingsRow::Table(_)
            | SettingsRow::AddEntry(_)
            | SettingsRow::Dog(_)
            | SettingsRow::DogKey { .. }
            | SettingsRow::DogGrant { .. }
            | SettingsRow::DogField { .. } => None,
        }
    }
}
