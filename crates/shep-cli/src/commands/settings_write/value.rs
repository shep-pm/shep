//! The value half of `shep set`: one string from the command line read as
//! TOML, and the two directions between a `toml_edit` item and the JSON the
//! shepherd's requests carry.

use serde_json::{Map, Value as Json};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value as TomlValue};

use super::{WriteRefusal, not_a_number};

/// `raw` as the value the shepherd receives: the TOML it parses as, or the
/// text itself when it parses as nothing.
///
/// A whole document is not a value, so `key = 1` is the string `key = 1`. The
/// value half of one line is what an operator has to type for a value to be
/// anything but a string, which is the rule kelpie's own `set` had before shep
/// took the verb over.
///
/// # Errors
/// [`WriteRefusal::Usage`] when the value puts a `nan` or an `inf` anywhere in
/// itself, which [`non_finite`] explains.
pub(super) fn value_of(raw: &str) -> Result<Json, WriteRefusal> {
    let line = format!("value = {raw}");
    if let Ok(doc) = line.parse::<DocumentMut>()
        && let Some(value) = doc.get("value").and_then(Item::as_value)
    {
        if let Some(path) = non_finite(value) {
            return Err(WriteRefusal::Usage(not_a_number(&path)));
        }
        return Ok(value_json(value));
    }
    Ok(Json::String(raw.to_string()))
}

/// One `toml_edit` item as the JSON the shepherd's requests carry.
///
/// `Item::None` is no value at all, which is a key the document does not hold,
/// and an array of tables is an array of objects.
fn item_json(item: &Item) -> Option<Json> {
    Some(match item {
        Item::None => return None,
        Item::Value(value) => value_json(value),
        Item::Table(table) => Json::Object(table_json(table)),
        Item::ArrayOfTables(tables) => Json::Array(
            tables
                .iter()
                .map(|table| Json::Object(table_json(table)))
                .collect(),
        ),
    })
}

/// One `toml_edit` value as JSON.
fn value_json(value: &TomlValue) -> Json {
    if let Some(flag) = value.as_bool() {
        return Json::Bool(flag);
    }
    if let Some(integer) = value.as_integer() {
        return Json::from(integer);
    }
    if let Some(float) = value.as_float() {
        if !float.is_finite() {
            // A value read out of a section, not one an operator is writing:
            // `dogs.toml` can hold `nan`, and the change list has to say what
            // is there rather than print a null for it. Writing one is refused
            // at [`value_of`].
            return Json::String(spelling(float).to_string());
        }
        return Json::from(float);
    }
    if let Some(text) = value.as_str() {
        return Json::String(text.to_string());
    }
    if let Some(array) = value.as_array() {
        return Json::Array(array.iter().map(value_json).collect());
    }
    if let Some(inline) = value.as_inline_table() {
        return Json::Object(
            inline
                .iter()
                .map(|(key, nested)| (key.to_string(), value_json(nested)))
                .collect(),
        );
    }
    if let Some(stamp) = value.as_datetime() {
        // shep's JSON plane has no datetime type, and [`shep_core::config::DogTable`]'s own
        // deserializer already rewrites a TOML date, time or datetime to its
        // plain string at any depth. Reading one as `null` would put a null in
        // the change list and drop the key from a table rebuilt whole.
        return Json::String(stamp.to_string());
    }
    Json::Null
}

/// `float` as the text TOML spells a non-finite number with.
///
/// Rust's own rendering is `NaN`, which is not a word TOML has: a change list
/// that printed it would name a value the file does not hold.
fn spelling(float: f64) -> &'static str {
    if float.is_nan() {
        "nan"
    } else if float.is_sign_negative() {
        "-inf"
    } else {
        "inf"
    }
}

/// The dotted path of the first `nan` or `inf` under `value`, `None` when it
/// holds none.
///
/// TOML spells `nan`, `inf` and `-inf`, and the JSON the shepherd's requests
/// carry have no cell: `serde_json` renders a non-finite number as `null`,
/// which clears the key it aimed at.
fn non_finite(value: &TomlValue) -> Option<String> {
    under("", value).map(|path| path.join("."))
}

/// [`non_finite`] walking `value`, with the path it has reached so far.
fn under(path: &str, value: &TomlValue) -> Option<Vec<String>> {
    if let Some(float) = value.as_float() {
        return (!float.is_finite()).then(|| vec![path.to_string()]);
    }
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .enumerate()
            .find_map(|(index, nested)| under(&format!("{path}[{index}]"), nested));
    }
    if let Some(inline) = value.as_inline_table() {
        return inline.iter().find_map(|(key, nested)| {
            let step = if path.is_empty() {
                key.to_string()
            } else {
                format!("{path}.{key}")
            };
            under(&step, nested)
        });
    }
    None
}

/// Every key a `toml_edit` table holds, as the JSON map a dog table request
/// carries.
pub(crate) fn table_json(table: &Table) -> Map<String, Json> {
    table
        .iter()
        .filter_map(|(key, item)| item_json(item).map(|value| (key.to_string(), value)))
        .collect()
}

/// `value` as the TOML cell it reads as, `None` for one with no TOML type.
pub(super) fn toml_value(value: &Json) -> Option<TomlValue> {
    Some(match value {
        Json::Null => return None,
        Json::Bool(flag) => TomlValue::from(*flag),
        Json::Number(number) => match number.as_i64() {
            Some(integer) => TomlValue::from(integer),
            None => TomlValue::from(number.as_f64()?),
        },
        Json::String(text) => TomlValue::from(text.as_str()),
        Json::Array(list) => {
            let mut array = Array::new();
            for element in list {
                array.push(toml_value(element)?);
            }
            TomlValue::Array(array)
        }
        Json::Object(map) => TomlValue::InlineTable(inline_table(map)),
    })
}

/// The dotted path of the first `null` leaf under `map`, `None` when it holds
/// none.
///
/// TOML has no null, so a JSON null has no cell to write at. A renderer that
/// skipped one would silently drop the key it sat at, which is a smaller write
/// than the operator asked for.
pub(super) fn null_leaf(map: &Map<String, Json>) -> Option<String> {
    map.iter()
        .find_map(|(key, value)| leaf(key.clone(), value))
        .map(|path| path.join("."))
}

/// [`null_leaf`] walking `value`, with the path it has reached so far.
fn leaf(path: String, value: &Json) -> Option<Vec<String>> {
    match value {
        Json::Null => Some(vec![path]),
        Json::Object(map) => map
            .iter()
            .find_map(|(key, nested)| leaf(format!("{path}.{key}"), nested)),
        Json::Array(list) => list
            .iter()
            .enumerate()
            .find_map(|(index, nested)| leaf(format!("{path}[{index}]"), nested)),
        _ => None,
    }
}

/// `map` as a TOML inline table.
pub(super) fn inline_table(map: &Map<String, Json>) -> InlineTable {
    let mut inline = InlineTable::new();
    for (key, value) in map {
        if let Some(cell) = toml_value(value) {
            inline.insert(key.as_str(), cell);
        }
    }
    inline
}
