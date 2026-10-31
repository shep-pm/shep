//! One proposed dog table, asked whether the schema that describes it takes
//! it: [`table_refusal`].
//!
//! It is the same door [`super::refusal`] walks one keystroke through, read
//! off the same [`crate::lookout::field::flattened`] walk: one module, not a
//! copy in the pane and a copy in the CLI.
//!
//! One rule belongs to a table alone: a value has to be the JSON type the
//! schema names before any rule on it is read. It refuses nothing a schema
//! says nothing about, and nothing for a key the table does not carry.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use shep_core::config::DogTable;
use shep_core::dog_defaults::layered;

use crate::lookout::field::{
    Entries, Field, FieldKind, Flattened, ListItem, Union, flattened, segment,
};

use super::numbers::{integer_refusal, number_refusal};
use super::{
    item_refusal, not_a_number, not_a_whole_number, refusal, required_field, shown_value,
    takes_one_of,
};

/// Why a proposed table is not one the dog's schema takes.
///
/// `path` is the property path the value sits at, one segment per table, so
/// a caller can name the key it came from even when the sentence spells it
/// dotted.
///
/// `Debug` is derived (IR-41), safe for [`super::Refusal`]'s reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRefusal {
    /// The key path, from the top of the table down, in property names.
    pub path: Vec<String>,
    /// The sentence to print.
    pub text: String,
}

impl TableRefusal {
    fn at(path: &[String], text: String) -> Self {
        Self {
            path: path.to_vec(),
            text,
        }
    }
}

/// Which keys of the proposed table the walk answers for.
///
/// [`Scope::Whole`] is what a caller that chose every key needs.
/// [`Scope::Touched`] is what a pane needs: it writes a whole table, but the
/// only keys it chose are the ones an operator edited, and a value the
/// operator wrote into `dogs.toml` or the Flockfile by hand goes back out as
/// it came. Whether lookout should ever refuse one is a separate decision.
#[derive(Clone, Copy)]
enum Scope<'a> {
    /// Every key the schema names.
    Whole,
    /// Only the keys whose property path is in `touched`.
    Touched(&'a BTreeSet<Vec<String>>),
}

impl Scope<'_> {
    fn holds(self, path: &[String]) -> bool {
        match self {
            Scope::Whole => true,
            Scope::Touched(touched) => touched.contains(path),
        }
    }
}

/// The refusal for `path`, or [`None`] when `scope` does not cover that key.
fn report(scope: Scope<'_>, path: &[String], text: String) -> Option<TableRefusal> {
    scope.holds(path).then(|| TableRefusal::at(path, text))
}

/// Why `table` is not a table `schema` takes, or [`None`] when nothing in it
/// disagrees with the schema.
///
/// `schema` is the whole answer, not a `properties` map, and the walk is
/// [`crate::lookout::field::flattened`], so the leaves this checks are the
/// leaves the pane edits. The first refusal in row order, not every one.
/// [`None`] says no published rule is broken, nothing more.
#[must_use]
pub fn table_refusal(schema: &Value, table: &Map<String, Value>) -> Option<TableRefusal> {
    flattened_refusal(&flattened(schema), table, &[], "", Scope::Whole)
}

/// [`table_refusal`] over only the keys in `touched`.
///
/// `touched` holds property paths, the same paths
/// [`crate::lookout::field::flattened`] built the row keys from. A refusal
/// found at any other key is not reported, and does not end the walk: a bad
/// value the caller did not choose rides along either way.
#[must_use]
pub fn touched_refusal(
    schema: &Value,
    table: &Map<String, Value>,
    touched: &BTreeSet<Vec<String>>,
) -> Option<TableRefusal> {
    flattened_refusal(&flattened(schema), table, &[], "", Scope::Touched(touched))
}

/// Why a default is turned down: the sheep that would read a bad value, and
/// the default's own value where no sheep names it.
///
/// `Debug` is derived (IR-41), safe for [`TableRefusal`]'s reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultRefusals {
    /// The default's own value at a touched path, set only when no entry of
    /// `sheep` already names that path. A sheep that holds its own value
    /// there hides the default's today, and a reset of it exposes the default
    /// again.
    pub default: Option<TableRefusal>,
    /// One refusal per sheep that would read a bad value, by sheep name.
    pub sheep: Vec<(String, TableRefusal)>,
}

impl DefaultRefusals {
    /// Whether nothing is refused.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.sheep.is_empty()
    }
}

/// Why a default is not one the schema takes, over the sheep it reaches.
///
/// Each of `sheep`'s tables is checked as the dog reads it, layered over
/// `default`, at the paths in `touched`: one refusal per sheep, the first in
/// row order. The default's own table is checked there too, since resetting a
/// sheep's own value exposes it. Nothing touched, or no sheep, checks nothing.
#[must_use]
pub fn default_refusals(
    schema: &Value,
    default: &DogTable,
    sheep: &BTreeMap<String, DogTable>,
    touched: &BTreeSet<Vec<String>>,
) -> DefaultRefusals {
    if touched.is_empty() || sheep.is_empty() {
        return DefaultRefusals::default();
    }
    let sheep: Vec<(String, TableRefusal)> = sheep
        .iter()
        .filter_map(|(name, own)| {
            let layered = layered(&[default, own]);
            let refused = touched_refusal(schema, layered.as_map(), touched);
            Some((name.clone(), refused?))
        })
        .collect();
    let default = touched_refusal(schema, default.as_map(), touched)
        .filter(|refused| sheep.iter().all(|(_, named)| named.path != refused.path));
    DefaultRefusals { default, sheep }
}

/// Every leaf of `flat`, read out of `table`, in row order.
///
/// `base_path` and `base_key` are where `table` sits inside the table the
/// caller started at: empty at the top, and one entry deep for each table of
/// an [`FieldKind::Entries`] row. A shape's own `shapes` is empty, so this
/// descends one entry deep and stops.
fn flattened_refusal(
    flat: &Flattened,
    table: &Map<String, Value>,
    base_path: &[String],
    base_key: &str,
    scope: Scope<'_>,
) -> Option<TableRefusal> {
    for field in flat.fields.fields() {
        let Some(rest) = flat.paths.get(&field.key) else {
            continue;
        };
        if let Some(refused) = leaf_refusal(flat, field, rest, base_path, base_key, table, scope) {
            return Some(refused);
        }
    }
    None
}

/// One leaf row, and the value `table` holds at `rest`.
///
/// `rest` is the row's own path below `base_path`, which is what both the
/// lookup and the sentence key are built from. Only the reported path and
/// key carry `base_path`: a nested table is looked up from its own top.
fn leaf_refusal(
    flat: &Flattened,
    field: &Field,
    rest: &[String],
    base_path: &[String],
    base_key: &str,
    table: &Map<String, Value>,
    scope: Scope<'_>,
) -> Option<TableRefusal> {
    let path: Vec<String> = base_path.iter().chain(rest.iter()).cloned().collect();
    let key = below(base_key, &field.key);
    match held(table, rest) {
        Held::Unset => None,
        // A scalar where the schema has a table: every leaf below it reads
        // as unset, so the key the operator has to fix is this one.
        Held::Blocked { value, depth } => {
            let prefix: Vec<String> = rest
                .iter()
                .take(depth + 1)
                .map(|name| segment(name))
                .collect();
            report(
                scope,
                &base_path
                    .iter()
                    .chain(rest[..=depth].iter())
                    .cloned()
                    .collect::<Vec<_>>(),
                not_a_table(
                    &below(base_key, &prefix.join(".")),
                    &shown_for(field.secret, value),
                ),
            )
        }
        Held::Leaf(value) => match &field.kind {
            FieldKind::Entries(kind) => {
                entries_refusal(flat, field, kind, value, &path, &key, scope)
            }
            FieldKind::Union(union) => union_refusal(union, value, &path, &key, scope),
            _ => scalar_refusal(field, value, &key, &path, scope),
        },
    }
}

/// `value` against `field`, as a refusal naming `key` and the path `path` it
/// sits at, for every kind that holds a value rather than a table of its own.
///
/// The arms are [`super`]'s, with the row's key replaced by the full path.
/// Each arm is reached through the value's JSON type first, and a value of
/// the wrong one is refused rather than handed to a parse that would read it
/// as text.
fn scalar_refusal(
    field: &Field,
    value: &Value,
    key: &str,
    path: &[String],
    scope: Scope<'_>,
) -> Option<TableRefusal> {
    let shown = shown_for(field.secret, value);
    match &field.kind {
        // The parse is for a value that is a number. A JSON string is not one,
        // however it reads: `keep = "8"` is a string in the table the section
        // writer hands on, and the pane files an integer as `Value::from(i64)`
        // and a decimal as `Value::from(f64)`, never as a string.
        FieldKind::Integer if !value.is_number() => {
            report(scope, path, not_a_whole_number(key, &shown).text)
        }
        FieldKind::Integer => integer_refusal(key, &field.bounds, &text_of(value), &shown)
            .and_then(|r| report(scope, path, r.text)),
        FieldKind::Number if !value.is_number() => {
            report(scope, path, not_a_number(key, &shown).text)
        }
        FieldKind::Number => number_refusal(key, &field.bounds, &text_of(value), &shown)
            .and_then(|r| report(scope, path, r.text)),
        FieldKind::Bool => value
            .as_bool()
            .is_none()
            .then(|| not_a_toggle(key, &shown))
            .and_then(|text| report(scope, path, text)),
        FieldKind::Text | FieldKind::Suggested(_) => match value.as_str() {
            Some(text) => {
                refusal(&named(field, key), text).and_then(|r| report(scope, path, r.text))
            }
            None => report(scope, path, not_a_string(key, &shown)),
        },
        // A closed set on a scalar is chosen from rather than typed, so it
        // never reaches [`refusal`] at all. A table can hold anything.
        FieldKind::Choice(names) => match value.as_str() {
            Some(text) if names.iter().any(|name| name == text) => None,
            _ => report(scope, path, takes_one_of(key, &shown, names).text),
        },
        FieldKind::List(item) => {
            let Some(elements) = value.as_array() else {
                return report(scope, path, not_a_list(key, &shown));
            };
            let bounds = field.bounds.items.as_deref().cloned().unwrap_or_default();
            for (index, element) in elements.iter().enumerate() {
                // A `null` element is the unset spelling, not a value to
                // hold to a rule. A list whose items may be `null` is not a
                // list row in the first place: it flattens to one read-only
                // row, and this arm never sees it.
                if element.is_null() {
                    continue;
                }
                let slot = below(key, &index.to_string());
                let element_path = below_path(path, &index.to_string());
                // The element's own type before its item schema's rules, for
                // [`FieldKind::Integer`]'s reason: [`text_of`] would hand `8`
                // to a string list's `pattern` as "8", and
                // `ListPane::apply_typing` files it as `Value::from(i64)`.
                let shown_element = shown_for(field.secret, element);
                // An element this caller did not choose is not an answer to
                // give, and not a reason to stop looking at the ones it did:
                // the bad element rides along in the write either way, so the
                // walk only stops on a refusal it is allowed to report.
                let wrong_type = match item {
                    ListItem::Integer if !element.is_number() => {
                        Some(not_a_whole_number(&slot, &shown_element).text)
                    }
                    ListItem::Text | ListItem::Choice(_) if element.as_str().is_none() => {
                        Some(not_a_string(&slot, &shown_element))
                    }
                    ListItem::Integer | ListItem::Text | ListItem::Choice(_) => None,
                };
                if let Some(refused) =
                    wrong_type.and_then(|text| report(scope, &element_path, text))
                {
                    return Some(refused);
                }
                let item_rule = item_refusal(&slot, item, &bounds, &text_of(element), field.secret)
                    .and_then(|refused| report(scope, &element_path, refused.text));
                if let Some(refused) = item_rule {
                    return Some(refused);
                }
            }
            None
        }
        // A map of arbitrary keys, a table this pane cannot walk, and a
        // read-only row each hold whatever the schema said nothing about.
        FieldKind::Map | FieldKind::Opaque | FieldKind::Entries(_) | FieldKind::Union(_) => None,
    }
}

/// An array or a map of tables: every entry checked against the item schema
/// [`crate::lookout::field::flattened`] flattened beside the row, the same
/// shape the pane draws an entry's rows from.
fn entries_refusal(
    flat: &Flattened,
    field: &Field,
    kind: &Entries,
    value: &Value,
    path: &[String],
    key: &str,
    scope: Scope<'_>,
) -> Option<TableRefusal> {
    let shape = flat.shapes.get(&field.key)?;
    let shown = shown_for(field.secret, value);
    match kind {
        Entries::Array => {
            let Some(entries) = value.as_array() else {
                return report(scope, path, not_a_list(key, &shown));
            };
            for (index, entry) in entries.iter().enumerate() {
                let slot = below(key, &index.to_string());
                let entry_path = below_path(path, &index.to_string());
                // An entry this caller did not choose is skipped, not reported,
                // and the entries after it are still checked.
                let Some(table) = entry.as_object() else {
                    let refused = not_a_table(&slot, &shown_for(field.secret, entry));
                    if let Some(refused) = report(scope, &entry_path, refused) {
                        return Some(refused);
                    }
                    continue;
                };
                if let Some(refused) = flattened_refusal(shape, table, &entry_path, &slot, scope) {
                    return Some(refused);
                }
            }
            None
        }
        Entries::Map => {
            let Some(entries) = value.as_object() else {
                return report(scope, path, not_a_table(key, &shown));
            };
            for (name, entry) in entries {
                let slot = below(key, &segment(name));
                let entry_path = below_path(path, name);
                let Some(table) = entry.as_object() else {
                    let refused = not_a_table(&slot, &shown_for(field.secret, entry));
                    if let Some(refused) = report(scope, &entry_path, refused) {
                        return Some(refused);
                    }
                    continue;
                };
                if let Some(refused) = flattened_refusal(shape, table, &entry_path, &slot, scope) {
                    return Some(refused);
                }
            }
            None
        }
    }
}

/// A tagged union: the table has to name one of the arms, every field of the
/// arm it names is held to its own bounds, and the arm's own `required` list
/// is enforced.
///
/// A required field the table does not hold is the one refusal this walk
/// gives for an absent key, reported at the union, because that is the table
/// the operator has to fix. A union is never a secret row.
fn union_refusal(
    union: &Union,
    value: &Value,
    path: &[String],
    key: &str,
    scope: Scope<'_>,
) -> Option<TableRefusal> {
    let Some(table) = value.as_object() else {
        return report(scope, path, not_a_table(key, &text_of(value)));
    };
    let tags: Vec<&str> = union.tags().collect();
    let Some(tag) = table.get(&union.tag).and_then(Value::as_str) else {
        return report(
            scope,
            path,
            format!(
                "{key} names no {} of its own; try one of {}",
                union.tag,
                tags.join(", ")
            ),
        );
    };
    let Some(arm) = union.arm(tag) else {
        return report(
            scope,
            path,
            format!(
                "{key} is \"{tag}\", which is not a kind {key} takes; try one of {}",
                tags.join(", ")
            ),
        );
    };
    let tag_key = below(key, &segment(&union.tag));
    for arm_field in &arm.fields {
        let arm_key = below(key, &segment(&arm_field.key));
        let Some(held) = table.get(&arm_field.key) else {
            if arm_field.editable && arm.requires(&arm_field.key) {
                let refused = required_field(&arm_key, &tag_key, &arm.tag);
                if let Some(refused) = report(scope, path, refused.text) {
                    return Some(refused);
                }
            }
            continue;
        };
        if held.is_null() {
            continue;
        }
        let arm_path = below_path(path, &arm_field.key);
        if let Some(refused) = scalar_refusal(arm_field, held, &arm_key, &arm_path, scope) {
            return Some(refused);
        }
    }
    None
}

/// Where `path` leaves `table`: nothing there, the value, or a non-table
/// sitting partway down.
enum Held<'a> {
    /// No such key, or a `null`: unset, and nothing to check.
    Unset,
    /// The value the whole path names.
    Leaf(&'a Value),
    /// A value that is not a table at `path[depth]`, where the schema has
    /// one more table to walk.
    Blocked { value: &'a Value, depth: usize },
}

fn held<'a>(table: &'a Map<String, Value>, path: &[String]) -> Held<'a> {
    let mut nested = Some(table);
    for (depth, name) in path.iter().enumerate() {
        let Some(value) = nested.and_then(|table| table.get(name)) else {
            return Held::Unset;
        };
        if value.is_null() {
            return Held::Unset;
        }
        if depth + 1 == path.len() {
            return Held::Leaf(value);
        }
        match value.as_object() {
            Some(next) => nested = Some(next),
            None => return Held::Blocked { value, depth },
        }
    }
    Held::Unset
}

/// `key` with one more dotted segment. The segment is already quoted the way
/// a row key is, by [`segment`], or is an index.
fn below(key: &str, segment: &str) -> String {
    if key.is_empty() {
        segment.to_owned()
    } else {
        format!("{key}.{segment}")
    }
}

fn below_path(path: &[String], segment: &str) -> Vec<String> {
    let mut out = path.to_vec();
    out.push(segment.to_owned());
    out
}

/// The field with its row key replaced by the full path, so a sentence built
/// by [`super`]'s arms names the key the operator has to fix.
fn named(field: &Field, key: &str) -> Field {
    let mut clone = field.clone();
    clone.key = key.to_owned();
    clone
}

/// What may be printed for `value`: [`crate::dog_settings::REDACTED`] for a
/// secret field, the value itself otherwise.
fn shown_for(secret: bool, value: &Value) -> String {
    shown_value(secret, &text_of(value)).to_owned()
}

/// `value` as the text each arm of [`super`] parses and quotes: bare for a
/// string, compact JSON for anything else.
fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn not_a_string(key: &str, shown: &str) -> String {
    format!("{key} is \"{shown}\", which is not a string shep accepts")
}

fn not_a_toggle(key: &str, shown: &str) -> String {
    format!("{key} is \"{shown}\", which is not true or false")
}

fn not_a_list(key: &str, shown: &str) -> String {
    format!("{key} is \"{shown}\", which is not a list shep accepts")
}

fn not_a_table(key: &str, shown: &str) -> String {
    format!("{key} is \"{shown}\", which is not a table shep accepts")
}
