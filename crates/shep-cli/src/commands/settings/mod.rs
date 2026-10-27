//! The settings screen's own reader and writer: the one place `shep.toml`'s
//! raw text meets [`DaemonConfig`]'s opinion about whether a value is legal.
//!
//! Validation lives in [`apply_setting`], the only function that puts a
//! mutated [`ShepToml`] and a real [`DaemonConfig::load`] in one place.
//!
//! [`load_settings`] reads the document, never a loaded [`DaemonConfig`]:
//! every section is `#[serde(default)]`, so a loaded config cannot tell a
//! key written at its own default from a key nobody wrote, which is the
//! distinction this screen exists to show.
//!
//! Which keys exist is not decided here. The screen's rows come from
//! [`shep_toml_shape`], and an edit names its key by path.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use shep_core::config::{DaemonConfig, DaemonConfigError, parse_daemon_bool};

use crate::commands::shep_toml::{PathError, ShepToml, ShepTomlError};
use crate::dog::BUILT_IN_DOGS;
use crate::lookout::field::{Field, FieldKind, segment, shep_toml_shape};
use crate::style::{StyleLevel, StyleSource};

#[cfg(test)]
mod tests;

// `crate::style::StyleSource` is reused for "which layer decided". Only
// `style.level` ever reads as `Flag` or `Env`: the shepherd's own env and
// flags are invisible here, so every other key is `Config` or `Default`.
// `Config` says the key is in the file, not that the shepherd is using it.

/// One value as the screen shows it
///
/// `Debug` is derived, not redacted: a rendered string and a layer name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarView {
    /// Already rendered for display, defaults resolved.
    pub value: String,
    /// Which layer this value came from.
    pub source: StyleSource,
}

/// What kind of TOML value a typed edit becomes.
///
/// `Debug` is derived, not redacted: a bare variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `true` or `false`, in the daemon's own spellings.
    Bool,
    /// A whole number.
    Integer,
    /// A number with or without a fraction.
    Number,
    /// A string, as typed.
    Text,
    /// An array of strings, typed comma-separated.
    List,
}

impl Shape {
    /// The shape an edit of `kind` writes, or [`None`] for a kind the
    /// screen cannot write as one value.
    #[must_use]
    pub fn of(kind: &FieldKind) -> Option<Self> {
        match kind {
            FieldKind::Bool => Some(Self::Bool),
            FieldKind::Integer => Some(Self::Integer),
            FieldKind::Number => Some(Self::Number),
            FieldKind::Text | FieldKind::Choice(_) | FieldKind::Suggested(_) => Some(Self::Text),
            FieldKind::List(_) => Some(Self::List),
            FieldKind::Map | FieldKind::Entries(_) | FieldKind::Union(_) | FieldKind::Opaque => {
                None
            }
        }
    }

    /// `text` as a TOML value of this shape.
    fn parse(self, text: &str) -> Result<toml_edit::Value, SettingError> {
        let invalid = |what: &str| SettingError::Invalid(format!("{text} is not {what}"));
        Ok(match self {
            Self::Bool => parse_daemon_bool(text)
                .ok_or_else(|| invalid("a valid boolean"))?
                .into(),
            Self::Integer => text
                .trim()
                .parse::<i64>()
                .map_err(|_| invalid("a whole number"))?
                .into(),
            Self::Number => text
                .trim()
                .parse::<f64>()
                .map_err(|_| invalid("a number"))?
                .into(),
            Self::Text => text.into(),
            Self::List => text
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect::<toml_edit::Array>()
                .into(),
        })
    }
}

/// One edit, ready to apply
///
/// `Debug` is derived, not redacted: a key path and the text typed for it.
/// Nothing in `shep.toml`'s own schema is a secret: `secrets.allow_read`
/// names environment variables, never their values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingEdit {
    /// Write `value` at `path`.
    Set {
        /// The key, as its segments: `["daemon", "log_level"]`, or
        /// `["interpreters", "py"]` for one entry of a map.
        path: Vec<String>,
        /// What the text becomes.
        shape: Shape,
        /// The text as typed or chosen, unparsed until [`apply_setting`]
        /// validates it.
        value: String,
    },
    /// Remove the key at `path`, returning it to the compiled default.
    Unset {
        /// The key, as its segments.
        path: Vec<String>,
    },
}

impl SettingEdit {
    /// The key's segments.
    #[must_use]
    pub fn path(&self) -> &[String] {
        match self {
            Self::Set { path, .. } | Self::Unset { path } => path,
        }
    }

    /// The key as the screen names it: segments joined by dots, each
    /// quoted when TOML would quote it.
    #[must_use]
    pub fn key(&self) -> String {
        dotted(self.path())
    }
}

/// `path` as a dotted key.
pub(crate) fn dotted(path: &[String]) -> String {
    path.iter()
        .map(|key| segment(key))
        .collect::<Vec<_>>()
        .join(".")
}

/// Everything the screen reads off disk in one go
///
/// `Debug` is derived, not redacted: a dog's own webhook token lives in
/// `dogs.toml`, which this type never reads, and `shep.toml`'s schema names
/// no secret.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsSnapshot {
    /// The document as JSON. A section the file never wrote is absent.
    pub document: Map<String, Value>,
    /// The socket this lookout is connected over, which an absent
    /// `daemon.socket` shows.
    pub socket_default: PathBuf,
    /// `style.level`, resolved: the level in force and the layer that set it.
    ///
    /// `--style` and `$SHEP_STYLE` are lookout's own, so this can differ from
    /// what the document says. [`Self::file_value`] reads the document's half.
    pub style: (StyleLevel, StyleSource),
    /// Every candidate dog: [`BUILT_IN_DOGS`] plus every `adopted_dogs`
    /// key, sorted, deduplicated.
    pub dogs: Vec<DogView>,
}

impl SettingsSnapshot {
    /// What the document says at the dotted `key`, or [`None`] when it
    /// never wrote it.
    #[must_use]
    pub fn file_value(&self, key: &str) -> Option<&Value> {
        let path = shep_toml_shape().paths.get(key)?;
        let (first, rest) = path.split_first()?;
        let mut value = self.document.get(first)?;
        for segment in rest {
            value = value.as_object()?.get(segment)?;
        }
        (!value.is_null()).then_some(value)
    }

    /// The entries of the map at the dotted `key`, in key order.
    #[must_use]
    pub fn entries(&self, key: &str) -> Vec<(String, String)> {
        self.file_value(key)
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(name, value)| (name.clone(), render_value(value)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What the screen shows for `field`: the file's value, else the
    /// default.
    #[must_use]
    pub fn view(&self, field: &Field) -> ScalarView {
        if field.key == STYLE_LEVEL_KEY {
            return ScalarView {
                value: self.style.0.to_string(),
                source: self.style.1,
            };
        }
        if let Some(value) = self.file_value(&field.key) {
            return ScalarView {
                value: render_value(value),
                source: StyleSource::Config,
            };
        }
        let value = if field.key == SOCKET_KEY {
            self.socket_default.display().to_string()
        } else {
            field
                .default_value
                .as_ref()
                .map_or_else(|| "not set".to_owned(), render_value)
        };
        ScalarView {
            value,
            source: StyleSource::Default,
        }
    }

    /// The text an editor on `field` opens with, as it would be typed: what
    /// the file says, else the value the key has without it.
    #[must_use]
    pub fn seed(&self, field: &Field) -> String {
        if let Some(value) = self.file_value(&field.key) {
            return typed_text(value);
        }
        if field.key == SOCKET_KEY {
            return self.socket_default.display().to_string();
        }
        field
            .default_value
            .as_ref()
            .map(typed_text)
            .unwrap_or_default()
    }
}

/// `value` as one would type it into an editor: a string bare, a list comma
/// separated.
fn typed_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map_or_else(|| item.to_string(), str::to_owned)
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// The settings key whose value `--style` and `$SHEP_STYLE` can outrank.
pub(crate) const STYLE_LEVEL_KEY: &str = "style.level";
/// The settings key whose absence shows the socket lookout connected over.
pub(crate) const SOCKET_KEY: &str = "daemon.socket";

/// `value` as one cell: strings bare, a list joined, an empty one `none`.
fn render_value(value: &Value) -> String {
    match value {
        Value::Null => "not set".to_owned(),
        Value::String(text) => text.clone(),
        Value::Array(items) if items.is_empty() => "none".to_owned(),
        Value::Array(_) => typed_text(value),
        Value::Object(map) if map.is_empty() => "none".to_owned(),
        Value::Object(map) if map.len() == 1 => "1 entry".to_owned(),
        Value::Object(map) => format!("{} entries", map.len()),
        other => other.to_string(),
    }
}

/// One row of the dogs table
///
/// `Debug` is derived, not redacted: a name, a bool and a binary's path.
/// The dog's own config, which can hold a secret, lives in `dogs.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DogView {
    /// The dog's name.
    pub name: String,
    /// Whether `[daemon] enabled_dogs` names it.
    pub enabled: bool,
    /// `None` for a built-in dog; the adopted binary's path otherwise.
    pub adopted_path: Option<PathBuf>,
}

/// What [`apply_setting`] can fail with
///
/// `Debug` is derived, not redacted: [`Self::Config`] forwards to
/// [`ShepTomlError`]'s own redacted `Debug`, and [`Self::Invalid`] carries
/// a refusal message naming a key and a rule, never a value from the file.
#[derive(Debug)]
pub enum SettingError {
    /// [`ShepToml::try_edit`]'s setup or write failed: the lock, the read,
    /// or the rename.
    Config(ShepTomlError),
    /// [`DaemonConfig::load`] refused the document this edit would have
    /// written, or the value is not legal for its key. Carries the refusal
    /// message, so the operator is told which key and why.
    Invalid(String),
}

impl std::fmt::Display for SettingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(err) => write!(f, "{err}"),
            Self::Invalid(message) => write!(f, "{message}"),
        }
    }
}

impl core::error::Error for SettingError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Config(err) => Some(err),
            Self::Invalid(_) => None,
        }
    }
}

impl From<ShepTomlError> for SettingError {
    fn from(err: ShepTomlError) -> Self {
        Self::Config(err)
    }
}

impl From<PathError> for SettingError {
    fn from(err: PathError) -> Self {
        Self::Invalid(err.to_string())
    }
}

/// Reads the snapshot, taking no lock: see [`ShepToml::read_only`]
///
/// `socket_default` is the socket this lookout is connected over, so an
/// absent `daemon.socket` shows the live answer rather than a guess.
/// `style` is `resolve_style`'s already-resolved pair.
///
/// # Errors
/// [`ShepTomlError::Io`] if `path` exists and could not be read.
/// [`ShepTomlError::Parse`] if `path` exists and is not valid TOML.
pub fn load_settings(
    path: &Path,
    socket_default: &Path,
    style: (StyleLevel, StyleSource),
) -> Result<SettingsSnapshot, ShepTomlError> {
    let doc = ShepToml::read_only(path)?;
    Ok(snapshot_of(&doc, socket_default, style))
}

/// [`load_settings`] over a document already in hand.
pub(crate) fn snapshot_of(
    doc: &ShepToml,
    socket_default: &Path,
    style: (StyleLevel, StyleSource),
) -> SettingsSnapshot {
    SettingsSnapshot {
        document: doc.to_json(),
        socket_default: socket_default.to_path_buf(),
        style,
        dogs: dog_candidates(doc),
    }
}

/// Every candidate dog: [`BUILT_IN_DOGS`] plus every adopted name, sorted
/// and deduplicated, each paired with whether it is enabled and its path.
///
/// `pub(crate)`: lookout's dogs sub-screen probes the same list.
pub(crate) fn dog_candidates(doc: &ShepToml) -> Vec<DogView> {
    let enabled = doc.enabled_dog_names();
    let mut names: BTreeSet<String> = BUILT_IN_DOGS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    names.extend(doc.adopted_dog_names());
    names
        .into_iter()
        .map(|name| {
            let adopted_path = doc.adopted_dog_path(&name);
            let is_enabled = enabled.contains(&name);
            DogView {
                name,
                enabled: is_enabled,
                adopted_path,
            }
        })
        .collect()
}

/// Applies one edit under the config lock, validating before it saves
///
/// A value outside a key's closed set is refused first. The mutated
/// document is then loaded with `&|_| None` in place of an env layer: this
/// process's environment is not the shepherd's. A loader `Err` returns
/// before [`ShepToml::try_edit`] saves, leaving the file untouched down to
/// its inode.
///
/// # Errors
/// [`SettingError::Config`] if the lock, the read or the write failed.
/// [`SettingError::Invalid`] if the value is not legal for its key, a table
/// above the key is something else in the file, or [`DaemonConfig::load`]
/// refuses the document the edit would leave.
pub fn apply_setting(path: &Path, edit: &SettingEdit) -> Result<(), SettingError> {
    check_closed_set(edit)?;
    ShepToml::try_edit(path, |doc| -> Result<(), SettingError> {
        match edit {
            SettingEdit::Set { path, shape, value } => doc.set_path(path, shape.parse(value)?)?,
            SettingEdit::Unset { path } => doc.unset_path(path),
        }
        let rendered = doc.rendered();
        DaemonConfig::load(Some(&rendered), &|_| None)
            .map_err(|err: DaemonConfigError| SettingError::Invalid(err.to_string()))?;
        Ok(())
    })
}

/// Refuses a [`SettingEdit::Set`] whose key is a closed set and whose value
/// is not in it. The loader does not look at every such key: `[style]` is
/// parsed only so an unknown key is not an error.
fn check_closed_set(edit: &SettingEdit) -> Result<(), SettingError> {
    let SettingEdit::Set { value, .. } = edit else {
        return Ok(());
    };
    let Some(Field {
        kind: FieldKind::Choice(names),
        ..
    }) = shep_toml_shape().fields.by_key(&edit.key())
    else {
        return Ok(());
    };
    if names.contains(value) {
        Ok(())
    } else {
        Err(SettingError::Invalid(format!(
            "{value} is not one of {}",
            names.join(", ")
        )))
    }
}
