//! What the pane is editing, what it will send, and what one row is.
//!
//! These carry no behaviour beyond their own invariants: the pane's state
//! machine lives beside [`super::ConfigPane`], and everything here is what
//! that machine reads and writes.

use std::path::PathBuf;

use serde_json::Value;

use shep_core::protocol::EnvValue;

// Link-only (IR-32): every type here is read and written by `ConfigPane`,
// and its docs name it.
#[cfg(doc)]
use super::ConfigPane;

/// Which thing the pane is editing.
///
/// Two things, and they are not the same shape of edit. A sheep's config is
/// shep's own document, so shep knows what every field costs; a dog's
/// section belongs to the dog, so shep publishes the change and the dog
/// decides what to reload, which is what [`ConfigPane::cost`]'s [`Option`]
/// is for.
///
/// `Debug` is derived (IR-41): a name and a binary's path, neither of which
/// is a value the pane withholds. A dog's section can carry a credential and
/// is held on [`ConfigPane`] instead, behind that type's own redacted
/// `Debug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneTarget {
    /// One sheep, by name.
    Sheep {
        /// The sheep.
        name: String,
    },
    /// One dog, by name, with the binary its schema was probed from.
    Dog {
        /// The dog.
        name: String,
        /// The adopted binary, or [`None`] for a built-in, whose schema
        /// comes from `crate::dog::builtin_schema`, since a built-in dog is
        /// this same binary. Kept so a re-probe asks the same path the pane
        /// opened on.
        adopted_path: Option<PathBuf>,
    },
}

impl PaneTarget {
    /// The target's name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Sheep { name } | Self::Dog { name, .. } => name,
        }
    }
}

/// Why a row cannot be edited from the pane.
///
/// Two different facts, and an operator has to be able to tell them apart:
/// one says the field is beyond editing anywhere, the other says only that
/// this screen has no widget for its shape and a Flockfile still can.
/// Collapsing them into `Field::editable` alone is what made six rows claim
/// the wrong one.
///
/// `Debug` is derived (IR-41): a bare variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    /// shep itself refuses a config write. Identity or flock shape rather
    /// than a runtime knob, so no surface changes it: `name` and
    /// `instances`, whose count moves through `shep stock` instead.
    Refused,
    /// The pane has no widget for this shape, and nothing more than that.
    /// `shep start <Flockfile>` writes these perfectly well, and
    /// [`ConfigPane::cost`] still reports what doing so would cost.
    NoWidget,
}

/// One row of the pane.
///
/// Three variants, not one: the env sub-screen is gone, and its keys now
/// walk the same cursor as every field, so this enum has to name a row in
/// either territory. Named rather than left as a bare index anyway, a
/// `usize` travelling between [`ConfigPane::rows`], the viewport and the
/// renderer says nothing about what it indexes, and this one says.
///
/// `Debug` is derived (IR-41): an index, or a bare variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneRow {
    /// Index into [`ConfigPane::fields`].
    Field(usize),
    /// Index into [`ConfigPane::env_key_names`].
    Env(usize),
    /// The row that adds a new env key.
    AddEnv,
}

/// One config field's new value, on its way out of the pane.
///
/// A newtype for the reason [`EnvValue`] is one: `cwd` and `script`
/// routinely hold a home directory and `args` holds a token, so
/// [`ConfigPane`]'s own `Debug` already withholds the map these come out
/// of. A bare [`Value`] travels on into `Sent::ApplyField`, which derives
/// `Debug`, so the newtype rides with the value wherever it goes rather
/// than depending on every type along the way to redact it separately.
///
/// The wire field itself (`Request::SetSheepField`) stays a bare
/// [`Value`] deliberately: `env` is the one field `AppConfig`'s own
/// `Debug` redacts, and `cwd` prints in the clear on every request that
/// carries a whole config, so a newtype there would protect one copy of
/// a value the protocol prints three other ways. This one guards
/// lookout, where a live value must never print.
///
/// `Debug` is manual and redacted (IR-41), exact-string-tested below. It
/// names the JSON type and nothing else, which is what a diagnostic
/// needs and is not a value.
#[derive(Clone, PartialEq, Eq)]
pub struct FieldValue(Value);

impl FieldValue {
    /// The value, for the request that carries it.
    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// The value, printed, when printing it cannot leak anything: a bool or
    /// a number can never hold a secret, a token or a home directory
    /// (IR-41). Every other kind stays [`None`], for the same reason
    /// [`Debug`](core::fmt::Debug) above never prints one either.
    ///
    /// Exists so a notice that reports a field being set can say which way
    /// it moved (`reuse_port set to true` versus `false`) without touching
    /// the kinds this type exists to guard.
    #[must_use]
    pub fn safe_summary(&self) -> Option<String> {
        match &self.0 {
            Value::Bool(value) => Some(value.to_string()),
            Value::Number(value) => Some(value.to_string()),
            Value::Null | Value::String(_) | Value::Array(_) | Value::Object(_) => None,
        }
    }
}

impl From<Value> for FieldValue {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

/// Prints the JSON type and never the value. See the type doc for why.
/// Exact-string-tested below (`a_field_values_debug_names_no_value`) so a
/// future `#[derive(Debug)]` fails that test instead of silently reopening
/// the leak.
impl core::fmt::Debug for FieldValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let kind = match &self.0 {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        write!(f, "FieldValue(<{kind}>)")
    }
}

/// One edit, ready to send.
///
/// Two variants, and they leave by their own doors: a [`Self::Set`] as a
/// `Request::SetSheepField` and a [`Self::SetEnv`] as a
/// `Request::SetSheepEnv`. Both record an operator override for one key,
/// never a template merge: a one-app `Request::ApplyConfig` at
/// `ResetDepth::File` would treat the edit as a template load, so it would
/// vanish from `overridden` the moment it landed and the pane's `*` marker
/// would never appear for it.
///
/// `Debug` is derived, safe because both value types redact themselves:
/// [`FieldValue`] names a JSON type and [`EnvValue`] names a byte count.
/// One mechanism on the value beats two on the types that carry it; see
/// [`FieldValue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEdit {
    /// Set the config field `key` to `value`.
    Set {
        /// The field.
        key: String,
        /// The new value, already typed to the field's kind.
        value: FieldValue,
    },
    /// Set the env key `key`, or with [`None`] remove it.
    SetEnv {
        /// The env key.
        key: String,
        /// The value, or [`None`] to remove the key.
        value: Option<EnvValue>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cwd` and `script` routinely hold a home directory and `args`
    /// holds a token, which is why `ConfigPane`'s own `Debug` withholds
    /// the map these come out of. Asserted on `FieldValue` itself, not
    /// on a type that carries it, since a wrapper could redact while the
    /// value underneath still prints.
    #[test]
    fn a_field_values_debug_names_no_value() {
        for (value, want) in [
            (serde_json::json!("/home/ada/secret-project"), "<string>"),
            (serde_json::json!(40), "<number>"),
            (serde_json::json!(false), "<bool>"),
            (serde_json::json!(null), "<null>"),
            (serde_json::json!(["--token", "hunter2"]), "<array>"),
            (serde_json::json!({ "a": 1 }), "<object>"),
        ] {
            let wrapped: FieldValue = value.into();
            assert_eq!(format!("{wrapped:?}"), format!("FieldValue({want})"));
        }
    }

    /// `Debug` is derived, safe only because both value types redact
    /// themselves; this test pins that fact.
    #[test]
    fn a_pane_edits_debug_names_no_value() {
        let set = PaneEdit::Set {
            key: "cwd".into(),
            value: serde_json::json!("/home/ada/secret-project").into(),
        };
        assert_eq!(
            format!("{set:?}"),
            r#"Set { key: "cwd", value: FieldValue(<string>) }"#
        );
        let env = PaneEdit::SetEnv {
            key: "DB_PASSWORD".into(),
            value: Some("hunter2".to_owned().into()),
        };
        assert_eq!(
            format!("{env:?}"),
            r#"SetEnv { key: "DB_PASSWORD", value: Some(EnvValue(<7 bytes>)) }"#
        );
    }
}
