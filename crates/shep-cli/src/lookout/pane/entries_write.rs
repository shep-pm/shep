//! The dog pane's write for a whole-value change to an array or map of
//! tables, which is what removing an entry or adding one files.
//!
//! The change is applied to the tables the section already holds, never by
//! rendering the new value. The tables that stay keep the operator's own
//! comments and key order. A table the section lacks joins after the last
//! one it has. An empty section gains the array or map.
//!
//! A table can only join an array at its end. A value whose new tables are
//! not its last is refused if the section then reads differently.

use serde_json::{Map, Value};

use super::super::field::Entries;
use super::PathStep;
use super::section_write::{Landed, implicit_table, inline_child, remove_at, set_at, toml_cell};

/// One leaf of an entry that differs, as the path inside the section to it
/// and what it becomes, or [`None`] for removed.
type Change = (Vec<PathStep>, Option<Value>);

/// Applies `new` as the whole value of the entries field at `path` in `doc`.
///
/// [`Landed::Already`] when the section already holds `new`. [`Landed::Refused`]
/// when `new` is not the shape `kind` holds, when a changed leaf or an added
/// table has no TOML type, and when the section would not read back as `new`
/// once a table is added.
pub(super) fn set_entries(
    doc: &mut toml_edit::DocumentMut,
    kind: Entries,
    path: &[String],
    new: &Value,
) -> Landed {
    let Ok(held) = doc.to_string().parse::<toml::Table>() else {
        return Landed::Refused;
    };
    let held = serde_json::to_value(held).unwrap_or(Value::Null);
    let held = path.iter().try_fold(Some(&held), |at, key| match at {
        Some(Value::Object(map)) => Ok(map.get(key)),
        Some(_) => Err(()),
        None => Ok(None),
    });
    let Ok(held) = held else {
        return Landed::Refused;
    };
    let base: Vec<PathStep> = path.iter().cloned().map(PathStep::Key).collect();
    let plan = match (kind, new) {
        (Entries::Array, Value::Array(new)) => match held {
            Some(Value::Array(held)) => plan_array(&base, held, new),
            None => plan_array(&base, &[], new),
            Some(_) => None,
        },
        (Entries::Map, Value::Object(new)) => match held {
            Some(Value::Object(held)) => plan_map(&base, held, new),
            None => plan_map(&base, &Map::new(), new),
            Some(_) => None,
        },
        _ => None,
    };
    let Some(plan) = plan else {
        return Landed::Refused;
    };
    let mut landed = Landed::Already;
    for (at, value) in &plan.changes {
        let done = match value {
            Some(value) => match toml_cell(value) {
                Some(cell) => set_at(doc.as_table_mut(), at, base.len() + 1, cell, false),
                None => Landed::Refused,
            },
            None => remove_at(doc.as_table_mut(), at, base.len() + 1),
        };
        match done {
            Landed::Changed => landed = Landed::Changed,
            Landed::Already => {}
            Landed::Refused => return Landed::Refused,
        }
    }
    if !plan.removed.is_empty() && drop_entries(doc, path, &plan.removed) {
        landed = Landed::Changed;
    }
    if !plan.added.is_empty() {
        if !add_entries(doc, kind, path, &plan.added) || !reads_as(doc, path, new) {
            return Landed::Refused;
        }
        landed = Landed::Changed;
    }
    landed
}

/// What turns the held entries into the new ones.
struct Plan {
    /// The leaves that differ in the entries that stay.
    changes: Vec<Change>,
    /// The entries that go.
    removed: Vec<PathStep>,
    /// The tables the section lacks, in the order they are written: each
    /// with its key in a map, and [`None`] in an array.
    added: Vec<(Option<String>, Value)>,
}

/// The entries of an array of tables, matched to the new ones.
///
/// Content first: a held table identical to a new one is that one. What is
/// left is matched by position, in order, and the new table's leaves that
/// differ become [`Plan::changes`]. The new tables past the ones matched are
/// [`Plan::added`]. [`None`] when a held table or a new one is not a table.
fn plan_array(base: &[PathStep], held: &[Value], new: &[Value]) -> Option<Plan> {
    let (matched, added) = align(held, new);
    let mut changes = Vec::new();
    let mut removed = Vec::new();
    for (index, (table, pair)) in held.iter().zip(&matched).enumerate() {
        match pair {
            Some(new) => {
                let mut at = base.to_vec();
                at.push(PathStep::Index(index));
                diff(table.as_object()?, new.as_object()?, &mut at, &mut changes);
            }
            None => removed.push(PathStep::Index(index)),
        }
    }
    if added.iter().any(|table| !table.is_object()) {
        return None;
    }
    Some(Plan {
        changes,
        removed,
        added: added.iter().map(|table| (None, table.clone())).collect(),
    })
}

/// The entries of a map of tables, matched to the new ones by key.
fn plan_map(
    base: &[PathStep],
    held: &Map<String, Value>,
    new: &Map<String, Value>,
) -> Option<Plan> {
    let mut changes = Vec::new();
    let mut removed = Vec::new();
    for (key, table) in held {
        match new.get(key) {
            Some(new) => {
                let mut at = base.to_vec();
                at.push(PathStep::Key(key.clone()));
                diff(table.as_object()?, new.as_object()?, &mut at, &mut changes);
            }
            None => removed.push(PathStep::Key(key.clone())),
        }
    }
    let mut added = Vec::new();
    for (key, table) in new.iter().filter(|(key, _)| !held.contains_key(*key)) {
        if !table.is_object() {
            return None;
        }
        added.push((Some(key.clone()), table.clone()));
    }
    Some(Plan {
        changes,
        removed,
        added,
    })
}

/// For each of `held`, the new table it becomes, or [`None`] for one that
/// goes, and the new tables left over, which are to be added. The new tables
/// keep their order: each is matched to a held one that comes later than the
/// one before it.
///
/// The match with the most keys in common wins, and a table identical to a
/// new one outweighs any partial match. Ties go to the earlier held table.
fn align<'a>(held: &[Value], new: &'a [Value]) -> (Vec<Option<&'a Value>>, &'a [Value]) {
    // `best[i][j]`: the most the first `j` new tables score against the first
    // `i` held ones, or [`None`] when they cannot all be placed.
    let mut best = vec![vec![None::<usize>; new.len() + 1]; held.len() + 1];
    for row in &mut best {
        row[0] = Some(0);
    }
    for i in 1..=held.len() {
        for j in 1..=new.len().min(i) {
            let skip = best[i - 1][j];
            let take = best[i - 1][j - 1].map(|score| score + likeness(&held[i - 1], &new[j - 1]));
            best[i][j] = skip.max(take);
        }
    }
    let mut placed = new.len().min(held.len());
    for count in (0..placed).rev() {
        if best[held.len()][count] > best[held.len()][placed] {
            placed = count;
        }
    }
    let mut matched = vec![None; held.len()];
    let (mut i, mut j) = (held.len(), placed);
    while j > 0 {
        if i > j && best[i][j] == best[i - 1][j] {
            i -= 1;
            continue;
        }
        matched[i - 1] = new.get(j - 1);
        i -= 1;
        j -= 1;
    }
    (matched, new.get(placed..).unwrap_or_default())
}

/// How alike two tables are: each key they share with the same value counts
/// twice, and identical tables count once more.
fn likeness(held: &Value, new: &Value) -> usize {
    let (Some(held), Some(new)) = (held.as_object(), new.as_object()) else {
        return 0;
    };
    let shared = new
        .iter()
        .filter(|(key, value)| held.get(*key) == Some(*value))
        .count();
    2 * shared + usize::from(held == new)
}

/// Pushes onto `out` every leaf under `at` that `new` has different from
/// `held`: a key `new` lacks is removed, and a table is entered rather than
/// replaced.
fn diff(
    held: &Map<String, Value>,
    new: &Map<String, Value>,
    at: &mut Vec<PathStep>,
    out: &mut Vec<Change>,
) {
    let none = Map::new();
    for (key, value) in new {
        let before = held.get(key);
        if before == Some(value) {
            continue;
        }
        at.push(PathStep::Key(key.clone()));
        match value {
            Value::Object(inner) => {
                diff(
                    before.and_then(Value::as_object).unwrap_or(&none),
                    inner,
                    at,
                    out,
                );
            }
            _ => out.push((at.clone(), Some(value.clone()))),
        }
        at.pop();
    }
    for key in held.keys().filter(|key| !new.contains_key(*key)) {
        let mut gone = at.clone();
        gone.push(PathStep::Key(key.clone()));
        out.push((gone, None));
    }
}

/// Deletes the entries `gone` names from the entries field at `path`, in
/// place, and reports whether there was anything to delete.
///
/// An array of tables left with none is removed from its parent, so the key
/// is written as no `[[x]]` at all.
fn drop_entries(doc: &mut toml_edit::DocumentMut, path: &[String], gone: &[PathStep]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let Some(parent) = table_at(doc.as_table_mut(), parents) else {
        return false;
    };
    let Some(item) = parent.get_mut(last) else {
        return false;
    };
    let mut changed = false;
    for step in gone.iter().rev() {
        changed |= match (&mut *item, step) {
            (toml_edit::Item::ArrayOfTables(tables), PathStep::Index(at)) if *at < tables.len() => {
                tables.remove(*at);
                true
            }
            (toml_edit::Item::Value(toml_edit::Value::Array(array)), PathStep::Index(at))
                if *at < array.len() =>
            {
                let gone = array.remove(*at);
                // The first element carries the space after `[`, so it moves
                // to the one that is first now.
                if *at == 0
                    && let (Some(prefix), Some(first)) =
                        (gone.decor().prefix().cloned(), array.get_mut(0))
                {
                    first.decor_mut().set_prefix(prefix);
                }
                true
            }
            (item, PathStep::Key(key)) => item
                .as_table_like_mut()
                .is_some_and(|table| table.remove(key).is_some()),
            _ => false,
        };
    }
    if matches!(item, toml_edit::Item::ArrayOfTables(tables) if tables.is_empty()) {
        parent.remove(last);
    }
    changed
}

/// The table `keys` name below `table`.
fn table_at<'a>(
    table: &'a mut dyn toml_edit::TableLike,
    keys: &[String],
) -> Option<&'a mut dyn toml_edit::TableLike> {
    match keys.split_first() {
        None => Some(table),
        Some((key, rest)) => table_at(table.get_mut(key)?.as_table_like_mut()?, rest),
    }
}

/// Adds the tables `added` to the entries field at `path` in `doc`, after the
/// ones it holds, creating the field when the section lacks it. Reports
/// whether they could all be added.
///
/// An array of tables takes a `[[x]]` per table, an array of inline tables an
/// inline table, and a map the key as a `[x.name]` header or an inline table
/// in the form the section already uses. A key that needs quoting is quoted.
fn add_entries(
    doc: &mut toml_edit::DocumentMut,
    kind: Entries,
    path: &[String],
    added: &[(Option<String>, Value)],
) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let Some((parent, inline)) = ensure_table(doc.as_table_mut(), parents, false) else {
        return false;
    };
    let item = parent.entry(last).or_insert(toml_edit::Item::None);
    if item.is_none() {
        *item = match (kind, inline) {
            (Entries::Array, false) => {
                toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new())
            }
            (Entries::Array, true) => {
                toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new()))
            }
            (Entries::Map, false) => implicit_table(),
            (Entries::Map, true) => inline_child(),
        };
    }
    match (kind, item) {
        (Entries::Array, toml_edit::Item::ArrayOfTables(tables)) => {
            for (_, value) in added {
                let Some(table) = table_of(value) else {
                    return false;
                };
                tables.push(table);
            }
            true
        }
        (Entries::Array, toml_edit::Item::Value(toml_edit::Value::Array(array))) => {
            for (_, value) in added {
                let Some(table) = inline_table_of(value) else {
                    return false;
                };
                array.push(toml_edit::Value::InlineTable(table));
            }
            true
        }
        (Entries::Map, item) => {
            let inline = item.is_inline_table();
            // The space before the closing brace belongs to the last table,
            // and moves to the new last one.
            let tail = item.as_inline_table_mut().and_then(|map| {
                let (_, last) = map.iter_mut().last()?;
                let tail = last.decor().suffix().cloned();
                last.decor_mut().set_suffix("");
                tail
            });
            let Some(map) = item.as_table_like_mut() else {
                return false;
            };
            for (key, value) in added {
                let (Some(key), Some(cell)) = (
                    key,
                    if inline {
                        inline_table_of(value).map(|table| {
                            toml_edit::Item::Value(toml_edit::Value::InlineTable(table))
                        })
                    } else {
                        table_of(value).map(toml_edit::Item::Table)
                    },
                ) else {
                    return false;
                };
                map.insert(key, cell);
            }
            if let (Some(tail), Some(map)) = (tail, item.as_inline_table_mut())
                && let Some((_, last)) = map.iter_mut().last()
            {
                last.decor_mut().set_suffix(tail);
            }
            true
        }
        (Entries::Array, _) => false,
    }
}

/// The table `keys` name below `table`, creating any the section lacks, and
/// whether it is an inline table. [`None`] when a step is something that is
/// not a table.
fn ensure_table<'a>(
    table: &'a mut dyn toml_edit::TableLike,
    keys: &[String],
    inline: bool,
) -> Option<(&'a mut dyn toml_edit::TableLike, bool)> {
    let Some((key, rest)) = keys.split_first() else {
        return Some((table, inline));
    };
    let entry = table.entry(key).or_insert(toml_edit::Item::None);
    let nested_inline = if entry.is_none() {
        *entry = if inline {
            inline_child()
        } else {
            implicit_table()
        };
        inline
    } else {
        entry.is_inline_table()
    };
    ensure_table(entry.as_table_like_mut()?, rest, nested_inline)
}

/// `value`, an object, as a table with a header: a table inside it is one
/// too. [`None`] for a leaf with no TOML type. A `null` leaf is left out.
fn table_of(value: &Value) -> Option<toml_edit::Table> {
    let mut table = toml_edit::Table::new();
    for (key, value) in value.as_object()? {
        match value {
            Value::Null => {}
            Value::Object(_) => {
                table.insert(key, toml_edit::Item::Table(table_of(value)?));
            }
            _ => {
                table.insert(key, toml_cell(value)?);
            }
        }
    }
    Some(table)
}

/// [`table_of`] as an inline table.
fn inline_table_of(value: &Value) -> Option<toml_edit::InlineTable> {
    let mut table = toml_edit::InlineTable::new();
    for (key, value) in value.as_object()? {
        match value {
            Value::Null => {}
            Value::Object(_) => {
                table.insert(key, toml_edit::Value::InlineTable(inline_table_of(value)?));
            }
            _ => {
                table.insert(key, toml_cell(value)?.into_value().ok()?);
            }
        }
    }
    Some(table)
}

/// Whether `doc` holds `new` at `path`.
fn reads_as(doc: &toml_edit::DocumentMut, path: &[String], new: &Value) -> bool {
    let Ok(read) = doc.to_string().parse::<toml::Table>() else {
        return false;
    };
    let read = serde_json::to_value(read).unwrap_or(Value::Null);
    path.iter()
        .try_fold(&read, |at, key| at.as_object()?.get(key))
        == Some(new)
}
