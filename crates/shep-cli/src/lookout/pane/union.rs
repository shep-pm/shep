//! A tagged union on a dog's table on a sheep: the rows it opens to and the
//! one value every edit of them files.
//!
//! A union is a leaf of the outline whose value is a table. Its rows are
//! made from that table as the operator has it, the filed one else the
//! stored one: a `kind` row for the tag, then the fields of the arm the tag
//! names. They key the edit they file by a name of their own,
//! [`tag_key`] or [`arm_key`], which [`ConfigPane::file_field`] turns back
//! into the whole table filed under the union's key. One union, one edit, so
//! `u` takes the whole union back and a write carries a table that matches
//! one arm of the schema.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::super::edits::EditKey;
use super::super::field::{Bounds, Field, FieldKind, FieldSet, KEY_SEP as SEP, Union};
use super::fields::{UNSET, render_json};
use super::{ConfigPane, PaneEdit, PaneRow, PaneTarget};

/// The edit key of the `kind` row of the union filed under `union`.
fn tag_key(union: &str, tag: &str) -> String {
    format!("{union}{SEP}{tag}")
}

/// The edit key of `field` of the arm `arm` of the union filed under `union`.
fn arm_key(union: &str, arm: &str, field: &str) -> String {
    format!("{union}{SEP}{arm}{SEP}{field}")
}

/// Which row of a union a key from [`tag_key`] or [`arm_key`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Place<'a> {
    /// The `kind` row.
    Tag {
        /// The union's own key.
        union: &'a str,
    },
    /// A field of one arm.
    Field {
        /// The union's own key.
        union: &'a str,
        /// The tag of the arm.
        arm: &'a str,
        /// The field's property name.
        field: &'a str,
    },
}

impl<'a> Place<'a> {
    /// The place `key` names, or [`None`] for a key of an ordinary row.
    pub(super) fn of(key: &'a str) -> Option<Self> {
        let mut parts = key.splitn(3, SEP);
        let union = parts.next()?;
        let second = parts.next()?;
        Some(match parts.next() {
            Some(field) => Self::Field {
                union,
                arm: second,
                field,
            },
            None => Self::Tag { union },
        })
    }

    /// The key the union itself is filed under.
    pub(super) fn union(self) -> &'a str {
        match self {
            Self::Tag { union } | Self::Field { union, .. } => union,
        }
    }
}

/// What a union holds, as the operator has it.
enum Held<'a> {
    /// Absent or `null`.
    Unset,
    /// A table whose tag names arm `arm`.
    Arm {
        arm: usize,
        object: &'a Map<String, Value>,
    },
    /// A value no arm describes: a hand edit. `tag` is what it names, when
    /// it names anything.
    Unknown { tag: Option<&'a str> },
}

/// The rows the `arms` cache holds for every union among `fields`, each a
/// [`Field`] keyed by its [`tag_key`] or [`arm_key`] so that the editors
/// which read a field's key work on it unchanged.
pub(super) fn arm_fields(fields: &FieldSet) -> BTreeMap<String, Field> {
    let mut rows = BTreeMap::new();
    for field in fields.fields() {
        let FieldKind::Union(union) = &field.kind else {
            continue;
        };
        if field.key.contains(SEP) {
            continue;
        }
        let key = tag_key(&field.key, &union.tag);
        rows.insert(
            key.clone(),
            Field {
                key,
                kind: FieldKind::Choice(union.tags().map(str::to_owned).collect()),
                default: None,
                default_value: None,
                value_kind: None,
                bounds: Bounds::default(),
                ..field.clone()
            },
        );
        for arm in &union.arms {
            for member in &arm.fields {
                let key = arm_key(&field.key, &arm.tag, &member.key);
                rows.insert(
                    key.clone(),
                    Field {
                        key,
                        ..member.clone()
                    },
                );
            }
        }
    }
    rows
}

/// Why a field an arm requires cannot be left empty. The sentence is
/// [`crate::dog_settings::required_field`]'s, which is the one the table walk
/// gives for the same rule, so the pane and the gate cannot disagree in front
/// of the operator.
fn required_text(tag: &str, arm: &str, field: &str) -> String {
    crate::dog_settings::required_field(field, tag, arm).text
}

impl ConfigPane {
    /// Whether this pane opens a union to its rows and files edits to them:
    /// a dog's table on a sheep. A dog's own section is written by patching
    /// its TOML text, which has no cell for a table value.
    fn writes_unions(&self) -> bool {
        matches!(self.target, PaneTarget::SheepDog { .. })
    }

    /// The place `key` names, or [`None`] for a key the `arms` cache does
    /// not hold: an ordinary row's key is never read as a union's.
    pub(super) fn place<'a>(&self, key: &'a str) -> Option<Place<'a>> {
        self.arms
            .contains_key(key)
            .then(|| Place::of(key))
            .flatten()
    }

    /// Whether field `index` is a union drawn open-able.
    pub(super) fn is_union(&self, index: usize) -> bool {
        self.writes_unions()
            && self.fields.fields().get(index).is_some_and(|field| {
                matches!(&field.kind, FieldKind::Union(union)
                    if self.arms.contains_key(&tag_key(&field.key, &union.tag)))
            })
    }

    /// What is filed for `key`, else what the target holds.
    fn effective(&self, key: &str) -> Option<&Value> {
        match self.edits.get(&EditKey::Field(key.to_owned())) {
            Some(entry) => match entry.edit() {
                PaneEdit::Set { value, .. } => Some(value.as_value()),
                PaneEdit::SetEnv { .. } => None,
            },
            None => self.values.get(key),
        }
    }

    fn union_of(&self, key: &str) -> Option<&Union> {
        match &self.fields.by_key(key)?.kind {
            FieldKind::Union(union) => Some(union),
            _ => None,
        }
    }

    fn held<'a>(&'a self, key: &str, union: &Union) -> Held<'a> {
        let Some(value) = self.effective(key).filter(|value| !value.is_null()) else {
            return Held::Unset;
        };
        let Some(object) = value.as_object() else {
            return Held::Unknown { tag: None };
        };
        match object.get(&union.tag).and_then(Value::as_str) {
            Some(tag) => match union.arms.iter().position(|arm| arm.tag == tag) {
                Some(arm) => Held::Arm { arm, object },
                None => Held::Unknown { tag: Some(tag) },
            },
            None => Held::Unknown { tag: None },
        }
    }

    /// What a union's own row says: `kind = path`, or `(unset)`, or the tag
    /// a hand edit left marked as unknown. [`None`] for any other key.
    pub(super) fn union_text(&self, key: &str) -> Option<String> {
        let union = self.union_of(key)?;
        Some(match self.held(key, union) {
            Held::Unset => UNSET.to_owned(),
            Held::Arm { arm, .. } => format!("{} = {}", union.tag, union.arms[arm].tag),
            Held::Unknown { tag: Some(tag) } => format!("{} = {tag} (unknown)", union.tag),
            Held::Unknown { tag: None } => "(unknown)".to_owned(),
        })
    }

    /// What a row under a union says, as the operator has it, for a key from
    /// [`tag_key`] or [`arm_key`]. [`None`] for any other key.
    pub(super) fn arm_text(&self, key: &str) -> Option<String> {
        let place = self.place(key)?;
        let union = self.union_of(place.union())?;
        let held = self.held(place.union(), union);
        Some(match (place, held) {
            (Place::Tag { .. }, Held::Arm { arm, .. }) => union.arms[arm].tag.clone(),
            (Place::Tag { .. }, Held::Unknown { tag: Some(tag) }) => format!("{tag} (unknown)"),
            (Place::Tag { .. }, Held::Unknown { tag: None }) => "(unknown)".to_owned(),
            (Place::Field { field, .. }, Held::Arm { object, .. }) => object
                .get(field)
                .map_or_else(|| UNSET.to_owned(), render_json),
            _ => UNSET.to_owned(),
        })
    }

    /// The value of a row under a union, for [`Self::cycle`] to start from.
    pub(super) fn arm_value(&self, key: &str) -> Option<&Value> {
        let place = self.place(key)?;
        let union = self.union_of(place.union())?;
        match place {
            Place::Tag { .. } => self.effective(place.union())?.as_object()?.get(&union.tag),
            Place::Field { arm, field, .. } => match self.held(place.union(), union) {
                Held::Arm { arm: held, object } if union.arms[held].tag == arm => object.get(field),
                _ => None,
            },
        }
    }

    /// The editor field of a row under a union, by its key.
    pub(super) fn arm_field_of(&self, key: &str) -> Option<&Field> {
        self.arms.get(key)
    }

    /// The editor field of `row`, for a row under a union.
    pub(in crate::lookout) fn arm_field(&self, row: PaneRow) -> Option<&Field> {
        self.arm_field_of(&self.arm_row_key(row)?)
    }

    fn arm_row_key(&self, row: PaneRow) -> Option<String> {
        let PaneRow::Arm { field, item } = row else {
            return None;
        };
        let field = self.fields.fields().get(field)?;
        let union = self.union_of(&field.key)?;
        if item == 0 {
            return Some(tag_key(&field.key, &union.tag));
        }
        let Held::Arm { arm, .. } = self.held(&field.key, union) else {
            return None;
        };
        let arm = &union.arms[arm];
        Some(arm_key(
            &field.key,
            &arm.tag,
            &arm.fields.get(item - 1)?.key,
        ))
    }

    /// The name `row` draws as and how deep it sits.
    pub(super) fn arm_label(&self, row: PaneRow) -> Option<(usize, String)> {
        let PaneRow::Arm { field, item } = row else {
            return None;
        };
        let field = self.fields.fields().get(field)?;
        let depth = self.row_path(&field.key)?.len();
        let union = self.union_of(&field.key)?;
        if item == 0 {
            return Some((depth, union.tag.clone()));
        }
        let Held::Arm { arm, .. } = self.held(&field.key, union) else {
            return None;
        };
        Some((depth, union.arms[arm].fields.get(item - 1)?.key.clone()))
    }

    /// The rows under union `field`, pushed onto `rows`, while it is drawn
    /// open: the tag, then the fields of the arm it names.
    pub(super) fn push_union_rows(&self, field: usize, rows: &mut Vec<PaneRow>) {
        if !self.is_union(field) || !self.is_open(PaneRow::Field(field)) {
            return;
        }
        let Some(key) = self.fields.fields().get(field).map(|f| f.key.as_str()) else {
            return;
        };
        let Some(union) = self.union_of(key) else {
            return;
        };
        let members = match self.held(key, union) {
            Held::Arm { arm, .. } => union.arms[arm].fields.len(),
            Held::Unset | Held::Unknown { .. } => 0,
        };
        rows.extend((0..=members).map(|item| PaneRow::Arm { field, item }));
    }

    /// The description of the arm that the union filed under `key` holds,
    /// or that the union a row under it belongs to holds, for the blurb
    /// panel. [`None`] for any other key.
    #[must_use]
    pub(in crate::lookout) fn arm_description_of(&self, key: &str) -> Option<&str> {
        let key = self.place(key).map_or(key, Place::union);
        let union = self.union_of(key)?;
        match self.held(key, union) {
            Held::Arm { arm, .. } => union.arms[arm].description.as_deref(),
            Held::Unset | Held::Unknown { .. } => None,
        }
    }

    /// `key` as the panel names it: a row under a union by the union's key
    /// and its own name, since the key itself is not for reading.
    #[must_use]
    pub(in crate::lookout) fn shown_key(&self, key: &str) -> String {
        match self.place(key) {
            None => key.to_owned(),
            Some(Place::Tag { union }) => format!(
                "{union}.{}",
                self.union_of(union).map_or("", |shape| shape.tag.as_str())
            ),
            Some(Place::Field { union, field, .. }) => format!("{union}.{field}"),
        }
    }

    /// Files the union filed under `union` with tag `tag`: its table is
    /// dropped for `{ kind = tag }` and each default of the arm. A field the
    /// arm requires and has no default for is left out until it is given,
    /// and [`Self::incomplete_union`] holds the pane open until then.
    fn pick_tag(&mut self, union: &str, tag: &str) {
        let Some(shape) = self.union_of(union) else {
            return;
        };
        let Some(arm) = shape.arm(tag) else {
            return;
        };
        let mut object = Map::new();
        object.insert(shape.tag.clone(), Value::String(tag.to_owned()));
        for member in &arm.fields {
            if let Some(default) = &member.default_value {
                object.insert(member.key.clone(), default.clone());
            }
        }
        self.file_field(union.to_owned(), Value::Object(object));
    }

    /// Files the union with `field` of arm `arm` set to `value`, or removed
    /// for `null`. Does nothing while the union holds a different arm, and
    /// for removing a field the arm requires.
    fn set_arm_field(&mut self, union: &str, arm: &str, field: &str, value: Value) {
        let Some(shape) = self.union_of(union) else {
            return;
        };
        let Held::Arm { arm: held, object } = self.held(union, shape) else {
            return;
        };
        let held = &shape.arms[held];
        if held.tag != arm || (value.is_null() && held.requires(field)) {
            return;
        }
        let mut object = object.clone();
        if value.is_null() {
            object.remove(field);
        } else {
            object.insert(field.to_owned(), value);
        }
        self.file_field(union.to_owned(), Value::Object(object));
    }

    /// Files an edit of a row under a union as the whole union.
    pub(super) fn file_arm(&mut self, place: Place<'_>, value: Value) {
        match place {
            Place::Tag { union } => {
                if let Value::String(tag) = &value {
                    let tag = tag.clone();
                    self.pick_tag(union, &tag);
                }
            }
            Place::Field { union, arm, field } => self.set_arm_field(union, arm, field, value),
        }
    }

    /// The sentence that says why `key`, a field of an arm, cannot be left
    /// empty. [`None`] for a field that can be, and for any other key.
    pub(super) fn required_refusal(&self, key: &str) -> Option<String> {
        let Some(Place::Field { union, arm, field }) = self.place(key) else {
            return None;
        };
        let shape = self.union_of(union)?;
        let held = shape.arm(arm)?;
        let member = self.arm_field_of(key)?;
        (member.editable && held.requires(field) && member.default_value.is_none())
            .then(|| required_text(&shape.tag, arm, field))
    }

    /// The sentence that says why the filed edits cannot be written: a
    /// union among them holds an arm without an editable field the arm
    /// requires. A field with no editor is not asked for.
    /// [`None`] when every filed union is whole.
    ///
    /// The same rule [`crate::dog_settings::table`] applies to a proposed
    /// table, with the same sentence through [`required_text`]. This one runs
    /// on the filed edits, which is where the pane can hold an operator to a
    /// whole arm before anything goes out.
    #[must_use]
    pub(in crate::lookout) fn incomplete_union(&self) -> Option<String> {
        self.incomplete_own_union()
            .or_else(|| self.dogs_tab.tables().find_map(Self::incomplete_own_union))
    }

    fn incomplete_own_union(&self) -> Option<String> {
        self.edits.iter().find_map(|(key, _)| {
            let EditKey::Field(union) = key else {
                return None;
            };
            let shape = self.union_of(union)?;
            let Held::Arm { arm, object } = self.held(union, shape) else {
                return None;
            };
            let arm = &shape.arms[arm];
            arm.fields
                .iter()
                .find(|member| {
                    member.editable
                        && arm.requires(&member.key)
                        && !object.contains_key(&member.key)
                })
                .map(|member| required_text(&shape.tag, &arm.tag, &member.key))
        })
    }

    /// [`Self::required_refusal`] for the row under the cursor.
    #[must_use]
    pub(in crate::lookout) fn cursor_refusal(&self) -> Option<String> {
        let key = self.arm_row_key(self.cursor()?)?;
        self.required_refusal(&key)
    }

    /// `d` on a row under a union: removes the field from its arm's table,
    /// or for one the arm requires puts back its default. Does nothing on
    /// the `kind` row, and where a required field has no default.
    pub(super) fn restore_arm_field(&mut self, key: &str) {
        let Some(Place::Field { union, arm, field }) = self.place(key) else {
            return;
        };
        if self.arm_field_of(key).is_none_or(|member| !member.editable) {
            return;
        }
        let required = self
            .union_of(union)
            .and_then(|shape| shape.arm(arm))
            .is_some_and(|held| held.requires(field));
        let value = if required {
            match self.arm_field_of(key).and_then(|f| f.default_value.clone()) {
                Some(default) => default,
                None => return,
            }
        } else {
            Value::Null
        };
        let (union, arm, field) = (union.to_owned(), arm.to_owned(), field.to_owned());
        self.set_arm_field(&union, &arm, &field, value);
    }
}
