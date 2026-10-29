//! The settings screen's rows, its pending edit, and the sentence each confirm
//! shows.
//!
//! Four modules under this one, one per reason to change: [`rows`] builds the
//! sequence the cursor walks and holds the viewport over it, [`pending`] holds
//! the candidate a write is waiting on, [`edit`] answers where a keypress opens
//! an editor and what it removes, and [`dogs`] holds each dog's opened tree and
//! the two `[daemon] keys` this screen can write.

use super::*;
use crate::commands::settings::{STYLE_LEVEL_KEY, Shape, dotted};
use crate::lookout::field::{Child, Field, FieldKind, shep_toml_shape};
use std::collections::{BTreeMap, BTreeSet};

mod dogs;
mod edit;
mod pending;
mod rows;

/// One row the settings screen's cursor can sit on.
///
/// The indexes are into [`shep_toml_shape`]: a table of its outline, or a
/// field of its field set. The dog rows index [`SettingsSnapshot::dogs`] and
/// what [`DogTree`] holds under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    /// A section, which folds.
    Table(usize),
    /// One key, or a map, which folds over its entries.
    Field(usize),
    /// One entry of the map at field `map`, by its place in key order.
    Entry {
        /// The map's field.
        map: usize,
        /// Which entry, in key order.
        index: usize,
    },
    /// The row under a map's entries that adds one.
    AddEntry(usize),
    /// A dog's header row: index into [`SettingsSnapshot::dogs`]. It folds,
    /// and `space` toggles `[daemon] enabled_dogs` for it.
    Dog(usize),
    /// One of the dog at `dog`'s other `[daemon] keys`, under its header.
    DogKey {
        /// Which dog.
        dog: usize,
        /// Which key.
        key: DogKey,
    },
    /// One permission in the grants checklist under the dog at `dog`'s
    /// `grants` row, by its place in the checklist.
    DogGrant {
        /// Which dog.
        dog: usize,
        /// Which permission.
        index: usize,
    },
    /// The `env` table under an adopted dog's header, beside its keys. It
    /// folds.
    DogEnv(usize),
    /// One key of the dog at `dog`'s `env`, by its place in
    /// [`DogView::env_keys`]. Its value is never held here.
    DogEnvKey {
        /// Which dog.
        dog: usize,
        /// Which key, in key order.
        index: usize,
    },
    /// The row under a dog's `env` keys that adds one.
    DogEnvNew(usize),
    /// One row of the dog at `dog`'s own config tree, by its place in that
    /// tree's [`ConfigPane::rows`].
    DogField {
        /// Which dog.
        dog: usize,
        /// Which row of its tree.
        row: usize,
    },
}

/// Which of a dog's `[daemon] keys a [`SettingsRow::DogKey`] names.
///
/// `enabled` is absent: the dog's header row carries that toggle, and its
/// `IN FILE` cell is where it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DogKey {
    /// `[daemon] boot_first_dogs`: a toggle.
    BootFirst,
    /// `[daemon] dog_grants`: the row its checklist hangs under.
    Grants,
    /// `[daemon] adopted_dogs`: read-only, because changing the path means
    /// vetting a new binary, which is `shep adopt`.
    Source,
    /// `[daemon] channel_dogs`: read-only, because it is the dog's own answer
    /// when it was adopted rather than a setting.
    Channel,
}

impl DogKey {
    /// Every key the screen draws under a dog's header, in the order it draws
    /// them.
    const ALL: [Self; 4] = [Self::BootFirst, Self::Grants, Self::Source, Self::Channel];

    /// The word the row's NAME cell shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::BootFirst => "boot first",
            Self::Grants => "grants",
            Self::Source => "source",
            Self::Channel => "channel",
        }
    }

    /// Why `space` on this row writes nothing, in the words the row itself
    /// gives. `None` for a row that does change the file.
    #[must_use]
    pub const fn read_only_reason(self) -> Option<&'static str> {
        match self {
            Self::BootFirst | Self::Grants => None,
            Self::Source => Some(
                "the path is `shep adopt`'s to change: it vets the binary before the \
                 shepherd will run it",
            ),
            Self::Channel => Some(
                "the dog asked for the channel itself when `shep adopt` ran it; this is what \
                 it answered, not a setting",
            ),
        }
    }
}

/// One of a dog's `[daemon] keys` the settings screen changes on its own row.
///
/// Neither is a [`SettingEdit`]: `boot_first_dogs` is a name in or out of an
/// array rather than a value at a path, and a grant is a read-modify-write of
/// a record the CLI owns. Both are named here so the gate, the ticket and the
/// prompt are the same for them as for every other row. `Debug` is derived,
/// not redacted: a name, a boolean and a permission name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DogTune {
    /// `[daemon] boot_first_dogs`: `name` in or out.
    BootFirst {
        /// Which dog.
        name: String,
        /// `true` to put it in.
        first: bool,
    },
    /// `[daemon] dog_grants`: one permission of `name` held or taken away.
    Grant {
        /// Which dog.
        name: String,
        /// Which permission.
        permission: String,
        /// `true` to grant it, `false` to revoke it.
        grant: bool,
    },
}

impl DogTune {
    /// The dog this change names.
    #[must_use]
    pub fn dog(&self) -> &str {
        match self {
            Self::BootFirst { name, .. } | Self::Grant { name, .. } => name,
        }
    }

    /// The question the confirm asks, which is also what the status bar and
    /// the sent line repeat while the write is in flight.
    ///
    /// Each names what applying it does rather than promising a reload:
    /// `boot_first_dogs` is read at boot and a grant is read when the dog is
    /// started, and neither is a key the shepherd reloads.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::BootFirst { name, first: true } => {
                format!("boot {name} first? the next shepherd boot starts it ahead of the flock")
            }
            Self::BootFirst { name, first: false } => {
                format!("stop booting {name} first? it starts with the rest of the flock")
            }
            Self::Grant {
                name,
                permission,
                grant: true,
            } => format!(
                "grant {name} `{permission}`? it may ask the shepherd for it from its next start"
            ),
            Self::Grant {
                name,
                permission,
                grant: false,
            } => format!("revoke `{permission}` from {name}? it stops being able to ask for it"),
        }
    }
}

/// One dog the operator opened on the settings screen.
///
/// A dog's own config needs two reads the screen cannot do inline: its schema
/// comes from running the dog's binary, and its section comes from the
/// shepherd. Both are asked for when the row is opened and not before. `Debug`
/// is derived, not redacted: the pane inside carries its own redacted `Debug`.
#[derive(Debug, Clone)]
pub enum DogTree {
    /// The schema probe is out. Nothing to draw yet, and the row stays closed.
    Probing,
    /// The schema landed and the section read is out. Carries the schema,
    /// which is what the tree is built from once the section answers, and
    /// which is never re-probed while this screen holds it.
    Reading {
        /// What the dog's binary answered the schema flag with.
        schema: serde_json::Value,
    },
    /// One of the two reads failed. The dog's header row says why, naming
    /// `dogs.toml` as the place to edit instead, and the row stays closed.
    Refused {
        /// The refusal, already worded.
        why: String,
    },
    /// Open: the rows under the dog's header come from this pane, which is
    /// the same [`ConfigPane::dog`] an `e` from the flock builds. The schema
    /// rides beside it for the same reason [`DogProbe`] does for a dog pane:
    /// a landed write re-reads the section and rebuilds the tree from it, and
    /// nothing else holds a copy of the schema.
    Open {
        /// The dog's own tree.
        pane: Box<ConfigPane>,
        /// What the dog's binary answered the schema flag with.
        schema: serde_json::Value,
    },
    /// Closed after a successful open. The pane is kept so re-opening the row
    /// does not respawn the dog's binary, the same reason `r` inside a dog
    /// pane re-reads the section without re-probing.
    Shut {
        /// The dog's own tree, kept for the next opening.
        pane: Box<ConfigPane>,
        /// What the dog's binary answered the schema flag with.
        schema: serde_json::Value,
    },
}

impl DogTree {
    /// The schema this tree's pane was built from, or [`None`] while there is
    /// none yet.
    #[must_use]
    pub fn schema(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Reading { schema } | Self::Open { schema, .. } | Self::Shut { schema, .. } => {
                Some(schema)
            }
            Self::Probing | Self::Refused { .. } => None,
        }
    }

    /// The pane this tree draws its rows from, or [`None`] while there is
    /// nothing to draw.
    #[must_use]
    pub fn pane(&self) -> Option<&ConfigPane> {
        match self {
            Self::Open { pane, .. } | Self::Shut { pane, .. } => Some(pane),
            Self::Probing | Self::Reading { .. } | Self::Refused { .. } => None,
        }
    }

    /// Whether the row shows what is under it.
    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open { .. })
    }

    /// Whether this tree is waiting on a read it already asked for.
    #[must_use]
    pub fn is_reading(&self) -> bool {
        matches!(self, Self::Probing | Self::Reading { .. })
    }
}

/// What opening or closing a dog's row asks the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DogFold {
    /// Nothing to do: the row is mid-read already.
    Nothing,
    /// The row opened for the first time, or reopened after a refusal, and
    /// needs its schema probe.
    Probe {
        /// Which dog.
        name: String,
        /// The adopted binary, or [`None`] for a built-in.
        adopted_path: Option<PathBuf>,
    },
    /// The row opened onto a tree this screen already read.
    Opened,
    /// The row closed.
    Closed,
}

/// The settings screen's own state. `None` on [`App`] is the dashboard.
#[derive(Debug, Clone)]
pub struct Settings {
    pub(super) snapshot: SettingsSnapshot,
    /// The dotted keys of the tables and maps the operator folded. Everything
    /// else is open, so a key added to the schema shows up unfolded.
    pub(super) folded: BTreeSet<String>,
    /// The cursor and, once a terminal has said how tall the body is, the
    /// scroll offset. Clamped on every read rather than kept pre-clamped: a
    /// refresh can shrink the dog list out from under a cursor already
    /// sitting past its new end.
    pub(super) view: Viewport,
    /// The edit this screen is showing, or `None`. One field rather than
    /// several `Option`s, so typing, armed and sent cannot overlap on screen.
    ///
    /// Not the same as the one write in flight. [`Pending::Sent`] eats no key,
    /// so a second edit can be armed and sent over it. [`Self::resolve`] is
    /// what keeps those answers off the edit that is here now.
    pub(super) pending: Option<Pending>,
    /// Each dog whose row the operator opened, by name, with the two reads
    /// its own tree needs. Keyed by name rather than by index because a
    /// re-read can reorder or drop a dog, and an index that survived would
    /// name a different one.
    pub(super) dog_trees: BTreeMap<String, DogTree>,
}

/// The settings screen's own in-flight edit.
#[derive(Debug, Clone)]
pub(super) enum Pending {
    /// A free-text edit under construction, seeded with the key's on-disk
    /// value.
    Typing {
        /// Where the text goes.
        target: Target,
        /// What the operator has typed so far.
        buffer: TextBox,
    },
    /// Armed: waiting for the operator's `Enter`. Nothing has gone out yet.
    Armed {
        /// The candidate, ready to send.
        edit: SettingEdit,
        /// The question this candidate reads as, rendered once at arm time.
        text: String,
        /// When it was armed. Only an armed edit expires.
        at: Instant,
    },
    /// [`Self::Armed`] for a [`DogEdit`] on a [`SettingsRow::Dog`] row, which
    /// [`App::confirm_setting`] sends through [`Effect::WriteDog`].
    DogArmed {
        /// The candidate toggle, ready to send.
        edit: DogEdit,
        /// The question this candidate reads as, rendered once at arm time.
        text: String,
        /// When it was armed. Only an armed edit expires.
        at: Instant,
    },
    /// [`App::confirm_setting`]'s `Enter` for a [`SettingsRow::DogKey`] or
    /// [`SettingsRow::DogGrant`] row: one of the dog's `[daemon] keys, sent
    /// through [`Effect::WriteDogTune`].
    DogTuneArmed {
        /// The change, ready to send.
        tune: DogTune,
        /// The question this candidate reads as, rendered once at arm time.
        text: String,
        /// When it was armed. Only an armed edit expires.
        at: Instant,
    },
    /// [`App::confirm_setting`]'s `Enter` for a [`SettingsRow::DogField`] row:
    /// the dog's whole section with that one row's change applied, sent as
    /// [`Sent::SetDogSection`], the same request a dog pane's close sends.
    DogSectionArmed {
        /// Which dog.
        name: String,
        /// The section, edit applied. Redacted in every `{:?}`.
        toml: DogSectionToml,
        /// The question this candidate reads as, rendered once at arm time.
        text: String,
        /// When it was armed. Only an armed edit expires.
        at: Instant,
    },
    /// [`Effect::WriteSetting`] or [`Effect::WriteDog`] is in flight. Carries
    /// no `edit`: every match site reads the landing message's own copy.
    Sent {
        /// The same rendered question, so the prompt line does not change
        /// wording between the question and its own answer.
        text: String,
        /// The write this is waiting on, by the ticket it went out with.
        /// What [`Settings::resolve`] matches a reply against, so a reply
        /// from a write the screen has moved on from cannot answer this
        /// one.
        ticket: u64,
    },
}

/// What a free-text edit writes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Target {
    /// The value at `path`, parsed as `shape`.
    Value {
        /// The key's segments.
        path: Vec<String>,
        /// What the typed text becomes.
        shape: Shape,
    },
    /// A new entry of the map at `path`, typed as `name=value`.
    NewEntry {
        /// The map's segments.
        path: Vec<String>,
    },
    /// One field of one dog's own section, which leaves as the whole section
    /// with that field changed.
    DogValue {
        /// Which dog.
        name: String,
        /// Which field of its section.
        key: String,
        /// Which row of the dog's tree it sits on, which is what the pane copy
        /// is pointed at before the text is handed to it.
        row: usize,
    },
}

impl Target {
    /// What the status bar calls the edit.
    pub(super) fn label(&self) -> String {
        match self {
            Self::Value { path, .. } => dotted(path),
            Self::NewEntry { path } => format!("new {} entry", dotted(path)),
            Self::DogValue { name, key, .. } => format!("{name} {key}"),
        }
    }

    /// The edit `text` stands for, or why it stands for none.
    ///
    /// Takes `settings` because a dog's own field has no meaning on its own:
    /// what one field's text becomes is that dog's whole section with the
    /// field changed, which is the dog pane's [`ConfigPane::edited_section_with`]
    /// answering, gate and all. A refusal there is the field's own schema
    /// refusing the text, worded by [`crate::dog_settings`].
    pub(super) fn edit(&self, settings: &Settings, text: &str) -> Result<TargetEdit, String> {
        match self {
            Self::Value { path, .. } if text.is_empty() => {
                Ok(TargetEdit::Setting(SettingEdit::Unset {
                    path: path.clone(),
                }))
            }
            Self::Value { path, shape } => Ok(TargetEdit::Setting(SettingEdit::Set {
                path: path.clone(),
                shape: *shape,
                value: text.to_owned(),
            })),
            Self::NewEntry { path } => {
                let (name, value) = text.split_once('=').ok_or("type name=value".to_owned())?;
                let (name, value) = (name.trim(), value.trim());
                if name.is_empty() || value.is_empty() {
                    return Err("type name=value".to_owned());
                }
                let mut path = path.clone();
                path.push(name.to_owned());
                Ok(TargetEdit::Setting(SettingEdit::Set {
                    path,
                    shape: Shape::Text,
                    value: value.to_owned(),
                }))
            }
            Self::DogValue { name, key, row } => {
                match settings.dog_section_edit(name, *row, text) {
                    Ok(Some(toml)) => Ok(TargetEdit::DogSection {
                        name: name.clone(),
                        toml,
                    }),
                    Ok(None) => Err(format!(
                        "{name} already says {text} for {key}, so there is nothing to write"
                    )),
                    Err(why) => Err(why),
                }
            }
        }
    }
}

/// What a finished editor stands for.
///
/// Two shapes because the screen writes two files: `shep.toml`, one key at a
/// time, and a dog's section, which `Request::SetDogConfig` replaces whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TargetEdit {
    /// One key of `shep.toml`.
    Setting(SettingEdit),
    /// One dog's whole section, with the edited field changed.
    DogSection {
        /// Which dog.
        name: String,
        /// The section to send. Redacted in every `{:?}`.
        toml: DogSectionToml,
    },
}

/// What the settings screen's status line shows for its one in-flight edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsPrompt<'a> {
    /// The confirm sentence: what will change, and what applying it does and
    /// does not do.
    pub text: &'a str,
    /// False while it is a question, true once it has gone out.
    pub sent: bool,
}

/// Whether a key of this kind is typed into an editor, rather than flipped,
/// walked through a closed set, or folded.
fn is_typed(kind: &FieldKind) -> bool {
    matches!(
        kind,
        FieldKind::Integer
            | FieldKind::Number
            | FieldKind::Text
            | FieldKind::Suggested(_)
            | FieldKind::List(_)
    )
}

/// What a refused write reopens the editor with: where the text goes and the
/// text the operator typed. `None` for an edit that had no editor to reopen:
/// a flip, a walk through names, or a removed entry.
pub(super) fn reopened_editor(edit: &SettingEdit) -> Option<(Target, TextBox)> {
    let shape = shep_toml_shape();
    let field = shape.fields.by_key(&edit.key());
    match (edit, field) {
        (
            SettingEdit::Set {
                path,
                shape: kind,
                value,
            },
            Some(field),
        ) if is_typed(&field.kind) => Some((
            Target::Value {
                path: path.clone(),
                shape: *kind,
            },
            TextBox::from(value.as_str()),
        )),
        (
            SettingEdit::Set {
                path,
                shape: kind,
                value,
            },
            None,
        ) if is_entry(path) => Some((
            Target::Value {
                path: path.clone(),
                shape: *kind,
            },
            TextBox::from(value.as_str()),
        )),
        (SettingEdit::Unset { path }, Some(field)) if is_typed(&field.kind) => Some((
            Target::Value {
                path: path.clone(),
                shape: Shape::of(&field.kind)?,
            },
            TextBox::new(),
        )),
        _ => None,
    }
}

/// Whether `path` names one entry of a map.
fn is_entry(path: &[String]) -> bool {
    let [map, _] = path else {
        return false;
    };
    shep_toml_shape()
        .fields
        .by_key(map)
        .is_some_and(|field| field.kind == FieldKind::Map)
}

impl App {
    /// The settings screen's own state, or `None` while the dashboard is
    /// showing: [`Body::settings`] over this app's body.
    #[must_use]
    pub fn settings(&self) -> Option<&Settings> {
        self.body.settings()
    }

    /// [`Self::settings`]'s mutable twin, for the settings keymap and the
    /// handlers that update a field or a pending edit in place.
    ///
    /// A caller that still needs another `App` field while it holds the
    /// screen calls [`Body::settings_mut`] on `self.body` instead, which
    /// borrows no more than the body.
    pub(super) fn settings_mut(&mut self) -> Option<&mut Settings> {
        self.body.settings_mut()
    }

    /// The resolved style level and which layer chose it, which the STYLE LEVEL
    /// row reads rather than re-resolving.
    #[must_use]
    pub fn style(&self) -> (StyleLevel, StyleSource) {
        self.style
    }

    /// Sets the resolved style level and its source. `App` reads no files, so
    /// it cannot resolve this itself.
    pub(crate) fn set_style(&mut self, style: (StyleLevel, StyleSource)) {
        self.style = style;
    }

    /// Marks this lookout as watching the shepherd on `dest` over `--host`.
    ///
    /// From here the screens that read this machine say so instead of opening:
    /// see [`NOT_OVER_HOST`].
    pub(crate) fn set_remote_host(&mut self, dest: &str) {
        self.remote_host = Some(dest.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::field::PER_DOG_KEYS;
    use crate::lookout::view::fixtures;

    /// Every leaf of `shep.toml`'s schema, as a dotted key, walked straight
    /// off the JSON: a section is a property that resolves to an object with
    /// properties, anything else (a map, a list, a scalar) is one leaf.
    fn schema_leaves(node: &serde_json::Value, defs: &serde_json::Value, at: &str) -> Vec<String> {
        let mut leaves = Vec::new();
        let properties = node["properties"].as_object().into_iter().flatten();
        for (name, property) in properties {
            let key = if at.is_empty() {
                name.clone()
            } else {
                format!("{at}.{name}")
            };
            let target = property["$ref"]
                .as_str()
                .and_then(|reference| reference.strip_prefix("#/$defs/"))
                .map_or(property, |def| &defs[def]);
            if target["properties"].is_object() {
                leaves.extend(schema_leaves(target, defs, &key));
            } else {
                leaves.push(key);
            }
        }
        leaves
    }

    /// A key added to `DaemonConfig` shows up in the schema, so this fails
    /// until the screen draws it. The four per-dog keys are the exception:
    /// the dog's own row carries them.
    #[test]
    fn the_tree_has_a_row_for_every_leaf_of_the_schema_but_the_per_dog_keys() {
        let schema = shep_core::config::daemon_config_schema_json().to_value();
        let mut expected = schema_leaves(&schema, &schema["$defs"], "");
        expected.retain(|key| !PER_DOG_KEYS.contains(&key.as_str()));
        expected.sort();

        let app = fixtures::app_in_settings();
        let settings = app.settings().unwrap();
        let fields = crate::lookout::field::shep_toml_shape().fields.fields();
        let mut drawn: Vec<String> = settings
            .rows()
            .into_iter()
            .filter_map(|row| match row {
                SettingsRow::Field(index) => Some(fields[index].key.clone()),
                _ => None,
            })
            .collect();
        drawn.sort();

        assert!(
            expected.len() > 10,
            "the walk found the schema: {expected:?}"
        );
        assert_eq!(drawn, expected);
    }

    #[test]
    fn set_style_round_trips_exactly() {
        let mut app = fixtures::full_app();
        assert_eq!(
            app.style(),
            (StyleLevel::Full, StyleSource::Default),
            "the default before anyone calls set_style"
        );
        app.set_style((StyleLevel::Bare, StyleSource::Flag));
        assert_eq!(app.style(), (StyleLevel::Bare, StyleSource::Flag));
    }

    /// Against a real file whose `[style] level` names a third, different
    /// level: the row reports the value threaded onto `App`, not one re-derived
    /// from the file.
    #[test]
    fn the_style_set_on_the_app_reaches_the_settings_row_undropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shep.toml");
        std::fs::write(&path, "[style]\nlevel = \"bare\"\n").unwrap();
        let socket_default = dir.path().join("run").join("shep.sock");

        let mut app = fixtures::full_app();
        app.set_style((StyleLevel::Plain, StyleSource::Flag));

        let result = crate::commands::settings::load_settings(&path, &socket_default, app.style())
            .map_err(|err| err.to_string());
        let _ = app.update(Msg::Settings { result });

        let field = crate::lookout::field::shep_toml_shape()
            .fields
            .by_key("style.level")
            .unwrap();
        let row = app.settings().unwrap().snapshot().view(field);
        assert_eq!(
            row.source,
            StyleSource::Flag,
            "the flag beats the file rather than being dropped by it"
        );
        assert_eq!(row.value, "plain");
    }
}
