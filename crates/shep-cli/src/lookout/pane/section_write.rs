//! The dog pane's write: every filed edit applied to the section's own TOML
//! text, ready for `Request::SetDogConfig`.
//!
//! `toml_edit` rather than a re-render of the pane's parsed values: the
//! request replaces the table wholesale, so a re-render would delete every
//! comment in it on the operator's own keystroke. `dogs.toml` is the
//! operator's file, and shep never writes it any other way.
//!
//! A row key is dotted (`models.worker.model`) when the dog's schema nests a
//! table, and the pane keeps the key-to-path map [`flattened`] beside the text.

use serde_json::Value;

use super::super::edits::{EditKey, Edits};
#[cfg(doc)]
use super::super::field::flattened;
use super::super::field::{Entries, FieldKind};
use super::entries_write::set_entries;
use super::{ConfigPane, PaneEdit, PathStep, SectionWrite};

impl ConfigPane {
    /// The section with every field edit in `edits` applied at its own path,
    /// comments and key order intact, ready for `Request::SetDogConfig`.
    ///
    /// This is what a close actually sends: the request replaces the table, so
    /// a batch of edits to one dog is one write, each applied to the document
    /// the previous one produced. A `null` removes the leaf and nothing above
    /// it, which is how the pane's empty buffer unsets one.
    #[must_use]
    pub fn edited_section_with(&self, edits: &Edits) -> SectionWrite {
        let Some(section) = self.section_text() else {
            return SectionWrite::Nothing;
        };
        let Ok(mut doc) = section.parse::<toml_edit::DocumentMut>() else {
            return SectionWrite::Refused;
        };
        let mut changed = false;
        for (key, edit) in edits.iter() {
            let EditKey::Field(name) = key else {
                continue;
            };
            let PaneEdit::Set { value, .. } = edit.edit() else {
                continue;
            };
            let value = value.as_value();
            if let Some((kind, path)) = self.entries_field(name) {
                match set_entries(&mut doc, kind, path, value) {
                    Landed::Changed => changed = true,
                    Landed::Already => {}
                    Landed::Refused => return SectionWrite::Refused,
                }
                continue;
            }
            let Some((path, existing)) = self.write_path(name) else {
                return SectionWrite::Refused;
            };
            let landed = match toml_cell(value) {
                Some(cell) => set_at(doc.as_table_mut(), &path, existing, cell, false),
                // `null` has no TOML cell: it is the removal, not a value.
                None if value.is_null() => remove_at(doc.as_table_mut(), &path, existing),
                None => Landed::Refused,
            };
            match landed {
                Landed::Changed => changed = true,
                Landed::Already => {}
                Landed::Refused => return SectionWrite::Refused,
            }
        }
        if changed {
            SectionWrite::Written(doc.to_string().into())
        } else {
            SectionWrite::Nothing
        }
    }
}

impl ConfigPane {
    /// The kind and section path of the array or map of tables `key` names,
    /// or [`None`] for any other key.
    pub(super) fn entries_field(&self, key: &str) -> Option<(Entries, &[String])> {
        let FieldKind::Entries(kind) = self.fields.by_key(key)?.kind else {
            return None;
        };
        Some((kind, self.section_paths.get(key)?))
    }

    /// Where the row `key` is in the section, and how many leading steps of
    /// that path the section must already hold. [`None`] for a key that
    /// names no row.
    fn write_path(&self, key: &str) -> Option<(Vec<PathStep>, usize)> {
        if let Some(leaf) = self.entries.leaf(key) {
            return Some((leaf.at.to_vec(), leaf.entry_len));
        }
        let path = self.section_paths.get(key)?;
        Some((path.iter().cloned().map(PathStep::Key).collect(), 0))
    }
}

/// What one edit left behind it in the document.
///
/// Three states because a batch is judged as a whole: one entry that cannot
/// land loses the whole close, while an entry the section already carries must
/// not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Landed {
    /// The document now says something it did not before.
    Changed,
    /// The document already said this, so it is unchanged: a `null` on a key
    /// the section does not carry.
    Already,
    /// The path lands on something other than the table or the key this row
    /// edits, so nothing was written anywhere in the document.
    Refused,
}

/// The TOML item a filed value becomes, or [`None`] for one this writer has
/// no arm for.
///
/// An array is a cell only when every element is a string or every element is
/// an `i64`, the two shapes the list sub-screen files; an empty one is both.
/// Any other array, and a map, is refused: writing one as JSON text would put
/// a string in `dogs.toml` where the dog reads a table or a list.
pub(super) fn toml_cell(value: &Value) -> Option<toml_edit::Item> {
    match value {
        Value::Bool(flag) => Some(toml_edit::value(*flag)),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                Some(toml_edit::value(int))
            } else if number.is_f64() {
                number.as_f64().map(toml_edit::value)
            } else {
                None
            }
        }
        Value::String(text) => Some(toml_edit::value(text.as_str())),
        Value::Array(elements) => {
            toml_array(elements).map(|array| toml_edit::Item::Value(toml_edit::Value::Array(array)))
        }
        Value::Null | Value::Object(_) => None,
    }
}

/// `elements` as a TOML array, or [`None`] unless they are all strings or all
/// integers.
fn toml_array(elements: &[Value]) -> Option<toml_edit::Array> {
    if elements.iter().all(Value::is_string) {
        elements
            .iter()
            .map(|element| element.as_str().map(toml_edit::Value::from))
            .collect()
    } else {
        elements
            .iter()
            .map(|element| element.as_i64().map(toml_edit::Value::from))
            .collect()
    }
}

/// Sets `cell` at `path`, creating any table on the path the section does not
/// carry yet.
///
/// A parent that is there takes the set where it is: a `[models.worker]`
/// header, a dotted key and an inline table are all [`toml_edit::TableLike`],
/// and each renders back in the form it came in. The first `existing` steps are
/// the exception: they name an entry the section either holds or does not.
pub(super) fn set_at(
    table: &mut dyn toml_edit::TableLike,
    path: &[PathStep],
    existing: usize,
    cell: toml_edit::Item,
    inline: bool,
) -> Landed {
    let Some((PathStep::Key(key), rest)) = path.split_first() else {
        return Landed::Refused;
    };
    if let Some((PathStep::Index(at), after)) = rest.split_first() {
        let Some((entry, entry_inline)) =
            table.get_mut(key).and_then(|array| element_at(array, *at))
        else {
            return Landed::Refused;
        };
        return set_at(entry, after, existing.saturating_sub(2), cell, entry_inline);
    }
    if existing > 0 && table.get(key).is_none_or(toml_edit::Item::is_none) {
        return Landed::Refused;
    }
    // `entry` rather than `insert`: `insert` re-formats the key it replaces,
    // and that is where a comment the operator wrote above the key goes.
    let entry = table.entry(key).or_insert(toml_edit::Item::None);
    if rest.is_empty() {
        if entry.is_table_like() {
            return Landed::Refused;
        }
        *entry = cell;
        return Landed::Changed;
    }
    let nested_inline = if entry.is_none() {
        *entry = if inline {
            inline_child()
        } else {
            implicit_table()
        };
        inline
    } else if entry.is_inline_table() {
        true
    } else if !entry.is_table_like() {
        return Landed::Refused;
    } else {
        false
    };
    let Some(nested) = entry.as_table_like_mut() else {
        return Landed::Refused;
    };
    set_at(
        nested,
        rest,
        existing.saturating_sub(1),
        cell,
        nested_inline,
    )
}

/// Removes the leaf `path` names, and nothing above it.
///
/// [`Landed::Already`] when the leaf, or a table on the way to it, is not in
/// the section: the key is already unset, which is what the edit wanted. An
/// entry that is not there is the exception, as it is for [`set_at`].
pub(super) fn remove_at(
    table: &mut dyn toml_edit::TableLike,
    path: &[PathStep],
    existing: usize,
) -> Landed {
    let Some((PathStep::Key(key), rest)) = path.split_first() else {
        return Landed::Refused;
    };
    if let Some((PathStep::Index(at), after)) = rest.split_first() {
        let Some((entry, _)) = table.get_mut(key).and_then(|array| element_at(array, *at)) else {
            return Landed::Refused;
        };
        return remove_at(entry, after, existing.saturating_sub(2));
    }
    if rest.is_empty() {
        return if table.remove(key).is_some() {
            Landed::Changed
        } else {
            Landed::Already
        };
    }
    let Some(item) = table.get_mut(key) else {
        return if existing > 0 {
            Landed::Refused
        } else {
            Landed::Already
        };
    };
    if matches!(item, toml_edit::Item::None) {
        return Landed::Already;
    }
    let Some(nested) = item.as_table_like_mut() else {
        return Landed::Refused;
    };
    remove_at(nested, rest, existing.saturating_sub(1))
}

/// Entry `at` of the array of tables `item` holds, and whether it is an inline
/// table. [`None`] for an index past the end, and for an item that is not an
/// array of tables.
fn element_at(
    item: &mut toml_edit::Item,
    at: usize,
) -> Option<(&mut dyn toml_edit::TableLike, bool)> {
    match item {
        toml_edit::Item::ArrayOfTables(tables) => tables
            .get_mut(at)
            .map(|table| (table as &mut dyn toml_edit::TableLike, false)),
        toml_edit::Item::Value(toml_edit::Value::Array(array)) => array
            .get_mut(at)?
            .as_inline_table_mut()
            .map(|table| (table as &mut dyn toml_edit::TableLike, true)),
        _ => None,
    }
}

/// A table `toml_edit` will not print a header for while it has no values of
/// its own, which is what keeps a created parent from showing as `[models]`.
pub(super) fn implicit_table() -> toml_edit::Item {
    let mut table = toml_edit::Table::new();
    table.set_implicit(true);
    toml_edit::Item::Table(table)
}

/// The kind of table to create inside an inline table. An inline table renders
/// its values and its inline tables and nothing else, so a regular
/// [`toml_edit::Item::Table`] inserted there reports a set and renders as
/// though nothing was written.
pub(super) fn inline_child() -> toml_edit::Item {
    toml_edit::Item::Value(toml_edit::Value::InlineTable(toml_edit::InlineTable::new()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::fixtures::{bark_pane, field, money_pane, web};
    use super::*;

    /// A decimal filed for a dog's section lands as a float, in the key's own
    /// place, with the comment the operator wrote above it. `toml_cell` already
    /// had the float arm; this pins that the float the pane now files reaches it
    /// rather than arriving as a string or being truncated to an integer.
    #[test]
    fn a_float_replaced_in_a_dogs_section_keeps_the_comment_above_it() {
        let mut pane = money_pane("# money cap\nper_item_usd = 10\nname = \"kelpie\"\n");
        pane.move_to_key("per_item_usd");
        pane.begin_typing();
        for _ in 0..10 {
            pane.type_backspace();
        }
        for c in "12.5".chars() {
            pane.type_char(c);
        }
        assert_eq!(pane.apply_typing(), None, "12.5 is above the floor");
        let SectionWrite::Written(section) = pane.edited_section_with(pane.edits()) else {
            panic!("the fixture section parses, and the replacement lands in it");
        };
        assert_eq!(
            section.as_str(),
            "# money cap\nper_item_usd = 12.5\nname = \"kelpie\"\n"
        );
    }

    /// A whole number typed into a decimal field is written back with its
    /// decimal point, which is what lets the dog read an `f64` where its own
    /// schema says `f64`.
    #[test]
    fn a_whole_number_filed_for_a_number_field_writes_a_float() {
        let mut pane = money_pane("per_item_usd = 12.5\n");
        pane.move_to_key("per_item_usd");
        pane.begin_typing();
        for _ in 0..4 {
            pane.type_backspace();
        }
        for c in "10".chars() {
            pane.type_char(c);
        }
        assert_eq!(pane.apply_typing(), None, "10 is a number");
        let SectionWrite::Written(section) = pane.edited_section_with(pane.edits()) else {
            panic!("the fixture section parses, and the replacement lands in it");
        };
        assert_eq!(section.as_str(), "per_item_usd = 10.0\n");
    }

    /// A dog whose schema nests one table inside another through a `$ref`
    /// chain, holds a secret beside a plain field in the inner one, and has
    /// a property literally named with a dot.
    fn nested_schema() -> Value {
        json!({
            "properties": {
                "name": { "type": "string" },
                "models": { "$ref": "#/$defs/Models" },
                "a.b": { "type": "string" },
            },
            "$defs": {
                "Models": {
                    "type": "object",
                    "properties": { "worker": { "$ref": "#/$defs/Worker" } },
                },
                "Worker": {
                    "type": "object",
                    "properties": {
                        "model": { "type": "string" },
                        "token": { "type": "string", "x-shep-secret": true },
                    },
                },
            },
        })
    }

    fn nested_pane(section: &str) -> ConfigPane {
        ConfigPane::dog("jobs".into(), None, nested_schema(), section.to_owned())
    }

    /// [`nested_schema`] with a list on the top level and one a table down.
    fn list_pane(section: &str) -> ConfigPane {
        let mut schema = nested_schema();
        schema["properties"]["hosts"] = json!({ "type": "array", "items": { "type": "string" } });
        schema["properties"]["codes"] = json!({ "type": "array", "items": { "type": "integer" } });
        schema["$defs"]["Worker"]["properties"]["models"] =
            json!({ "type": "array", "items": { "type": "string" } });
        ConfigPane::dog("jobs".into(), None, schema, section.to_owned())
    }

    /// The same data in the three forms `dogs.toml` allows for a nested
    /// table, each with a comment above the table and one inside it.
    const HEADER: &str = concat!(
        "# which model to run\n",
        "name = \"small\"\n",
        "\n",
        "[models.worker]\n",
        "# the credential\n",
        "token = \"sk-old\"\n",
        "model = \"gpt\"\n",
    );
    const DOTTED: &str = concat!(
        "# which model to run\n",
        "name = \"small\"\n",
        "models.worker.token = \"sk-old\"\n",
        "models.worker.model = \"gpt\"\n",
    );
    const INLINE: &str = concat!(
        "# which model to run\n",
        "name = \"small\"\n",
        "models = { worker = { token = \"sk-old\", model = \"gpt\" } }\n",
    );

    /// The header form with the credential left out, which is how an unset
    /// optional field reads in `dogs.toml`.
    const NO_TOKEN: &str = concat!(
        "# which model to run\n",
        "name = \"small\"\n",
        "\n",
        "[models.worker]\n",
        "model = \"gpt\"\n",
    );

    fn edits_of(pairs: &[(&str, Value)]) -> Edits {
        let mut edits = Edits::default();
        for (key, value) in pairs {
            edits.set(field(key, value.clone()), None);
        }
        edits
    }

    /// The section a write produced, panicking on either other answer so a
    /// test that wants text fails naming what it got instead.
    fn want_written(write: SectionWrite) -> String {
        match write {
            SectionWrite::Written(toml) => toml.as_str().to_owned(),
            other => panic!("wanted a section to send, got {other:?}"),
        }
    }

    /// Every form takes the set where it is, and neither comment survives
    /// anywhere but where the operator wrote it.
    #[test]
    fn a_nested_set_lands_in_the_form_the_section_already_uses() {
        for (section, want) in [
            (
                HEADER,
                concat!(
                    "# which model to run\n",
                    "name = \"small\"\n",
                    "\n",
                    "[models.worker]\n",
                    "# the credential\n",
                    "token = \"sk-old\"\n",
                    "model = \"large\"\n",
                ),
            ),
            (
                DOTTED,
                concat!(
                    "# which model to run\n",
                    "name = \"small\"\n",
                    "models.worker.token = \"sk-old\"\n",
                    "models.worker.model = \"large\"\n",
                ),
            ),
            (
                INLINE,
                concat!(
                    "# which model to run\n",
                    "name = \"small\"\n",
                    "models = { worker = { token = \"sk-old\", model = \"large\" } }\n",
                ),
            ),
        ] {
            let out = want_written(
                nested_pane(section)
                    .edited_section_with(&edits_of(&[("models.worker.model", json!("large"))])),
            );
            assert_eq!(out, want, "{section}");
        }
    }

    /// A set into a table the section does not carry writes the deepest
    /// header it needs and nothing above it.
    #[test]
    fn a_set_that_creates_a_table_writes_no_empty_parent_header() {
        let out = want_written(
            nested_pane("name = \"small\"\n")
                .edited_section_with(&edits_of(&[("models.worker.model", json!("large"))])),
        );
        assert_eq!(
            out,
            concat!(
                "name = \"small\"\n",
                "\n",
                "[models.worker]\n",
                "model = \"large\"\n",
            )
        );
    }

    /// Key order is the operator's: a set rewrites one line in place, and a
    /// new key joins its table at the end rather than at the top.
    #[test]
    fn key_order_survives_a_set() {
        let out = want_written(nested_pane(HEADER).edited_section_with(&edits_of(&[
            ("models.worker.token", json!("sk-new")),
            ("name", json!("large")),
        ])));
        assert_eq!(
            out,
            concat!(
                "# which model to run\n",
                "name = \"large\"\n",
                "\n",
                "[models.worker]\n",
                "# the credential\n",
                "token = \"sk-new\"\n",
                "model = \"gpt\"\n",
            )
        );
    }

    /// Two edits to one table are two lines of the same write.
    #[test]
    fn two_edits_in_one_table_both_land() {
        let out = want_written(nested_pane(HEADER).edited_section_with(&edits_of(&[
            ("models.worker.model", json!("large")),
            ("models.worker.token", json!("sk-new")),
        ])));
        assert_eq!(
            out,
            concat!(
                "# which model to run\n",
                "name = \"small\"\n",
                "\n",
                "[models.worker]\n",
                "# the credential\n",
                "token = \"sk-new\"\n",
                "model = \"large\"\n",
            )
        );
    }

    /// `null` is the leaf's own removal: its sibling and the table holding
    /// both stay, comment included.
    #[test]
    fn a_null_removes_only_its_leaf() {
        for (section, want) in [
            (
                HEADER,
                concat!(
                    "# which model to run\n",
                    "name = \"small\"\n",
                    "\n",
                    "[models.worker]\n",
                    "# the credential\n",
                    "token = \"sk-old\"\n",
                ),
            ),
            (
                DOTTED,
                concat!(
                    "# which model to run\n",
                    "name = \"small\"\n",
                    "models.worker.token = \"sk-old\"\n",
                ),
            ),
            (
                INLINE,
                concat!(
                    "# which model to run\n",
                    "name = \"small\"\n",
                    // The space the removed entry owned goes with it.
                    "models = { worker = { token = \"sk-old\"} }\n",
                ),
            ),
        ] {
            let out = want_written(
                nested_pane(section)
                    .edited_section_with(&edits_of(&[("models.worker.model", Value::Null)])),
            );
            assert_eq!(out, want, "{section}");
        }
    }

    /// A batch that changed nothing sends nothing: the removal found no
    /// parent, so the section it would have sent is the one it was handed.
    /// `Nothing`, not `Refused`: the file already says what the operator asked
    /// for.
    #[test]
    fn a_null_under_an_absent_parent_writes_nothing() {
        assert_eq!(
            nested_pane("name = \"small\"\n")
                .edited_section_with(&edits_of(&[("models.worker.model", Value::Null)])),
            SectionWrite::Nothing
        );
    }

    /// An unset field the operator unsets again is a no-op, not a failure: a
    /// refusal here would drop the sibling edit in the same close along with
    /// it.
    #[test]
    fn a_null_on_a_key_the_section_does_not_carry_keeps_the_rest_of_the_batch() {
        let out = want_written(nested_pane(NO_TOKEN).edited_section_with(&edits_of(&[
            ("models.worker.token", Value::Null),
            ("name", json!("large")),
        ])));
        assert_eq!(
            out,
            concat!(
                "# which model to run\n",
                "name = \"large\"\n",
                "\n",
                "[models.worker]\n",
                "model = \"gpt\"\n",
            )
        );
    }

    /// The same for a parent the section does not carry at all.
    #[test]
    fn a_null_under_an_absent_parent_keeps_the_rest_of_the_batch() {
        let out = want_written(
            nested_pane("name = \"small\"\n").edited_section_with(&edits_of(&[
                ("models.worker.model", Value::Null),
                ("name", json!("large")),
            ])),
        );
        assert_eq!(out, "name = \"large\"\n");
    }

    /// `models = "old"` is the operator's own line, and a row editing a key
    /// inside `models` is not licensed to trade it for a table. Refused whole:
    /// a batch this writer cannot finish sends no part of itself.
    #[test]
    fn a_set_onto_a_scalar_where_the_schema_has_a_table_writes_nothing() {
        for section in [
            "models = \"old\"\nname = \"small\"\n",
            "models.worker = \"old\"\nname = \"small\"\n",
            "models = { worker = \"old\" }\nname = \"small\"\n",
        ] {
            let pane = nested_pane(section);
            assert_eq!(
                pane.edited_section_with(&edits_of(&[("models.worker.model", json!("large"))])),
                SectionWrite::Refused,
                "{section}"
            );
            assert_eq!(
                pane.edited_section_with(&edits_of(&[
                    ("models.worker.model", json!("large")),
                    ("name", json!("large")),
                ])),
                SectionWrite::Refused,
                "{section}"
            );
        }
    }

    /// No row of this pane ends at a table, so this is the writer's own rule
    /// stated where it lives rather than a path an operator reaches.
    #[test]
    fn a_set_never_replaces_a_table_the_operator_wrote() {
        let mut doc: toml_edit::DocumentMut = "[models.worker]\nmodel = \"gpt\"\n"
            .parse()
            .expect("the section parses");
        let path = [PathStep::Key("models".to_owned())];
        assert_eq!(
            set_at(doc.as_table_mut(), &path, 0, toml_edit::value("old"), false),
            Landed::Refused
        );
        assert!(doc.to_string().contains("[models.worker]"), "{doc}");
    }

    /// A new table on the path takes the form of the table it goes into.
    /// Inserting a regular `Item::Table` into an inline table reports success
    /// and renders nothing at all: inline tables emit only their values, so
    /// the section comes back exactly as it went in and the edit is gone.
    #[test]
    fn a_set_inside_an_inline_table_creates_an_inline_table() {
        for (section, want) in [
            (
                "models = {}\nname = \"small\"\n",
                "models = { worker = { model = \"large\" } }\nname = \"small\"\n",
            ),
            (
                "models = { token = \"sk-old\" }\nname = \"small\"\n",
                // The space the new entry's separator takes sits before the
                // comma rather than after it, which is `toml_edit` keeping the
                // decor the operator's own entry had.
                concat!(
                    "models = { token = \"sk-old\" , worker = { model = \"large\" } }\n",
                    "name = \"small\"\n",
                ),
            ),
            (
                "models = { worker = {} }\nname = \"small\"\n",
                "models = { worker = { model = \"large\" } }\nname = \"small\"\n",
            ),
            (
                "models = { worker = { token = \"sk-old\" } }\nname = \"small\"\n",
                concat!(
                    "models = { worker = { token = \"sk-old\" , model = \"large\" } }\n",
                    "name = \"small\"\n",
                ),
            ),
        ] {
            let out = want_written(
                nested_pane(section)
                    .edited_section_with(&edits_of(&[("models.worker.model", json!("large"))])),
            );
            assert_eq!(out, want, "{section}");
            let doc: toml_edit::DocumentMut = out.parse().expect("the output parses");
            assert_eq!(
                doc["models"]["worker"]["model"].as_str(),
                Some("large"),
                "{out}"
            );
        }
    }

    /// A property literally named `a.b` is one key, and the section spells
    /// it quoted. The row key carries the quotes `segment` added to it.
    #[test]
    fn a_property_named_with_a_dot_stays_one_key() {
        let pane = nested_pane("\"a.b\" = \"literal\"\nname = \"small\"\n");
        assert_eq!(pane.value("\"a.b\""), "literal");
        let out = want_written(pane.edited_section_with(&edits_of(&[("\"a.b\"", json!("other"))])));
        assert_eq!(out, "\"a.b\" = \"other\"\nname = \"small\"\n");
    }

    /// A number this pane cannot name is refused rather than rounded: a
    /// `u64` above `i64::MAX` reaches `toml_edit` as a float, and a rounded
    /// number in `dogs.toml` is a wrong setting the dog reads back as valid.
    #[test]
    fn a_number_that_is_neither_an_integer_nor_a_float_writes_nothing() {
        let pane = nested_pane("name = \"small\"\n");
        assert_eq!(
            pane.edited_section_with(&edits_of(&[("name", json!(u64::MAX))])),
            SectionWrite::Refused
        );
    }

    /// An array that is not all strings or all integers has no TOML list the
    /// pane edits, and a map has no cell at all. JSON text written as a TOML
    /// string is a value the dog cannot read back as the shape it was.
    #[test]
    fn a_mixed_array_an_array_of_objects_or_a_map_writes_nothing() {
        let pane = nested_pane("name = \"small\"\n");
        for value in [
            json!(["a", 1]),
            json!([1.5, 2.5]),
            json!([true]),
            json!([null]),
            json!([["a"]]),
            json!([{ "model": "gpt" }]),
            json!({ "model": "gpt" }),
        ] {
            assert_eq!(
                pane.edited_section_with(&edits_of(&[("name", value.clone())])),
                SectionWrite::Refused,
                "{value}"
            );
        }
    }

    /// The array replaces the operator's line where it stands, and the
    /// comment above it is the operator's own.
    #[test]
    fn a_string_list_replaces_its_line_and_keeps_the_comment_above_it() {
        let out = want_written(
            list_pane("# who to ask\nhosts = [\"a\"]\nname = \"small\"\n")
                .edited_section_with(&edits_of(&[("hosts", json!(["a", "b"]))])),
        );
        assert_eq!(
            out,
            "# who to ask\nhosts = [\"a\", \"b\"]\nname = \"small\"\n"
        );
    }

    #[test]
    fn an_integer_list_writes_integers_not_text() {
        let out = want_written(
            list_pane("codes = [0]\nname = \"small\"\n")
                .edited_section_with(&edits_of(&[("codes", json!([0, 143]))])),
        );
        assert_eq!(out, "codes = [0, 143]\nname = \"small\"\n");
    }

    /// Removing the last element leaves the key set to nothing, which is not
    /// the same as unsetting it.
    #[test]
    fn an_empty_list_writes_an_empty_array() {
        let out = want_written(
            list_pane("hosts = [\"a\"]\n").edited_section_with(&edits_of(&[("hosts", json!([]))])),
        );
        assert_eq!(out, "hosts = []\n");
    }

    /// A list the section does not carry yet joins it at the end.
    #[test]
    fn a_list_the_section_lacks_is_added_after_the_keys_it_has() {
        let out = want_written(
            list_pane("name = \"small\"\n")
                .edited_section_with(&edits_of(&[("hosts", json!(["a"]))])),
        );
        assert_eq!(out, "name = \"small\"\nhosts = [\"a\"]\n");
    }

    /// A list one table down is written at its dotted path, in whichever of
    /// the three forms the section uses for the table.
    #[test]
    fn a_list_under_a_nested_table_lands_at_its_dotted_path() {
        for (section, want) in [
            (
                "[models.worker]\n# the pool\nmodels = [\"gpt\"]\n",
                "[models.worker]\n# the pool\nmodels = [\"gpt\", \"large\"]\n",
            ),
            (
                "models.worker.models = [\"gpt\"]\n",
                "models.worker.models = [\"gpt\", \"large\"]\n",
            ),
            (
                "[models.worker]\nmodel = \"gpt\"\n",
                "[models.worker]\nmodel = \"gpt\"\nmodels = [\"gpt\", \"large\"]\n",
            ),
        ] {
            let out = want_written(list_pane(section).edited_section_with(&edits_of(&[(
                "models.worker.models",
                json!(["gpt", "large"]),
            )])));
            assert_eq!(out, want, "{section}");
        }
    }

    /// The four shapes the writer does have an arm for, each in its own TOML
    /// type rather than as text.
    #[test]
    fn a_scalar_writes_its_own_toml_type() {
        for (value, want) in [
            (json!(-5), "name = -5\n"),
            (json!(1.5), "name = 1.5\n"),
            (json!(true), "name = true\n"),
            (json!("gpt"), "name = \"gpt\"\n"),
        ] {
            let pane = nested_pane("name = 0\n");
            let out = want_written(pane.edited_section_with(&edits_of(&[("name", value.clone())])));
            assert_eq!(out, want, "{value}");
        }
    }

    /// Re-rendering from parsed values would delete every comment in a file
    /// shep does not author.
    #[test]
    fn an_edited_section_keeps_its_comments_and_changes_one_key() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(field("poll", json!("30s")), None);
        let out = want_written(pane.edited_section_with(&edits));
        assert!(out.contains("# how often"), "{out}");
        assert!(out.contains("poll = \"30s\""), "{out}");
        assert!(out.contains("history_bytes = 4096"), "{out}");
        assert!(out.contains("url = \"https://hooks.example/x\""), "{out}");
    }

    /// An empty buffer unsets the key, putting it back on the dog's own
    /// default.
    #[test]
    fn a_null_edit_removes_the_key_from_the_section() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(field("history_bytes", Value::Null), None);
        let out = want_written(pane.edited_section_with(&edits));
        assert!(!out.contains("history_bytes"), "{out}");
        assert!(out.contains("# how often"), "{out}");
    }

    /// A dog section takes every filed edit in one write rather than one
    /// write per edit.
    #[test]
    fn a_dog_section_takes_every_filed_edit_in_one_write() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(field("sink_timeout", json!("5s")), None);
        edits.set(field("poll", json!("30s")), None);
        let section = want_written(pane.edited_section_with(&edits));
        assert!(section.contains("poll = \"30s\""), "{section}");
        assert!(section.contains("sink_timeout = \"5s\""), "{section}");
    }

    /// An env edit has no home in a dog's section and must not silently
    /// become a key in it.
    #[test]
    fn an_env_edit_is_ignored_by_a_dog_section() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(
            PaneEdit::SetEnv {
                key: "SECRET".to_owned(),
                value: None,
            },
            None,
        );
        assert_eq!(pane.edited_section_with(&edits), SectionWrite::Nothing);
    }

    /// A stray env edit in a set that also carries a field edit must not
    /// revive the old "one bad entry aborts the whole batch" bug: skipping
    /// the env edit and an early return on any non-field entry both leave
    /// `an_env_edit_is_ignored_by_a_dog_section` green, since an env-only
    /// set answers `Nothing` either way. Only a mixed set tells them apart.
    #[test]
    fn a_field_edit_lands_even_when_the_same_batch_carries_an_env_edit() {
        let pane = bark_pane();
        let mut edits = Edits::default();
        edits.set(field("poll", json!("30s")), None);
        edits.set(
            PaneEdit::SetEnv {
                key: "SECRET".to_owned(),
                value: None,
            },
            None,
        );
        let out = want_written(pane.edited_section_with(&edits));
        assert!(out.contains("poll = \"30s\""), "{out}");
        assert!(!out.contains("SECRET"), "{out}");
    }

    /// A section here would send a sheep's config out through a dog's door,
    /// and there is no section to send it from.
    #[test]
    fn a_sheep_pane_has_no_section_to_edit() {
        let pane = ConfigPane::sheep(web());
        let mut edits = Edits::default();
        edits.set(field("cwd", json!("/srv")), None);
        assert_eq!(pane.edited_section_with(&edits), SectionWrite::Nothing);
    }
}
