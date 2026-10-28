//! The settings screen's rows, its pending edit, and the sentence each confirm
//! shows.

use super::*;
use crate::commands::settings::{STYLE_LEVEL_KEY, Shape, dotted};
use crate::lookout::field::{Child, Field, FieldKind, shep_toml_shape};
use std::collections::{BTreeMap, BTreeSet};

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
/// prompt are the same for them as for every other row on this screen.
///
/// `Debug` is derived, not redacted: a dog's name, a boolean and a permission
/// name. A permission names what a dog may ask, never a value.
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
/// shepherd. Both are asked for when the row is opened and not before, so a
/// screen listing six dogs spends nothing on a dog nobody opened.
///
/// `Debug` is derived, not redacted: the pane inside carries its own redacted
/// `Debug`, and a schema describes values without holding any.
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
    /// several `Option`s, so typing, armed and sent cannot overlap on
    /// screen.
    ///
    /// Not the same as the one write in flight. [`Pending::Sent`] eats no
    /// key, so a second edit can be armed and sent over it, and closing the
    /// screen abandons it without cancelling anything: either leaves a
    /// write with no prompt waiting for it. [`Self::resolve`] is what keeps
    /// those answers off the edit that is here now.
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

impl Settings {
    /// A freshly opened screen, cursor on the first row, every section open.
    pub(super) fn new(snapshot: SettingsSnapshot) -> Self {
        Self {
            snapshot,
            folded: BTreeSet::new(),
            view: Viewport::new(),
            pending: None,
            dog_trees: BTreeMap::new(),
        }
    }

    /// The armed candidate and its prompt, or `None`.
    #[must_use]
    pub fn pending(&self) -> Option<SettingsPrompt<'_>> {
        match &self.pending {
            Some(
                Pending::Armed { text, .. }
                | Pending::DogArmed { text, .. }
                | Pending::DogTuneArmed { text, .. }
                | Pending::DogSectionArmed { text, .. },
            ) => Some(SettingsPrompt { text, sent: false }),
            Some(Pending::Sent { text, .. }) => Some(SettingsPrompt { text, sent: true }),
            Some(Pending::Typing { .. }) | None => None,
        }
    }

    /// Whether `ticket` names the write this screen is still waiting on,
    /// clearing the prompt when it does.
    ///
    /// A reply whose ticket is not the one [`Pending::Sent`] holds belongs
    /// to a write the screen has moved on from: a second edit armed over
    /// the first, or a screen closed and reopened while the first was still
    /// in flight. Both leave a write in flight with nothing on screen
    /// waiting for it, and neither lets its answer touch the edit that is.
    pub(super) fn resolve(&mut self, ticket: u64) -> bool {
        let mine = matches!(
            self.pending,
            Some(Pending::Sent { ticket: waiting, .. }) if waiting == ticket
        );
        if mine {
            self.pending = None;
        }
        mine
    }

    /// Whether a candidate is waiting on `Enter`: the one state a stray key
    /// (movement, `Escape`, `Settings`, `Refresh`) eats rather than also doing
    /// its ordinary job.
    pub(super) fn is_armed(&self) -> bool {
        matches!(
            self.pending,
            Some(
                Pending::Armed { .. }
                    | Pending::DogArmed { .. }
                    | Pending::DogTuneArmed { .. }
                    | Pending::DogSectionArmed { .. }
            )
        )
    }

    /// The label and buffer of an in-flight free-text edit, or `None`.
    #[must_use]
    pub fn typing(&self) -> Option<(String, &TextBox)> {
        match &self.pending {
            Some(Pending::Typing { target, buffer }) => Some((target.label(), buffer)),
            _ => None,
        }
    }

    /// The next candidate for `field`, or `None` for a key that is not a
    /// cycle: only a boolean and a closed set of names are.
    ///
    /// Advances from a candidate already armed for this field, so a second
    /// `space` walks one step further along the cycle. From nothing armed the
    /// base is what the file says, which for `[style] level` is the file's
    /// value rather than the level in force: cycling the resolved level could
    /// propose a write that changes nothing.
    pub(super) fn next_candidate(&self, field: &Field) -> Option<String> {
        let armed_here = match &self.pending {
            Some(Pending::Armed {
                edit: SettingEdit::Set { value, .. },
                ..
            }) if self.armed_for(field) => Some(value.clone()),
            _ => None,
        };
        let base = armed_here
            .or_else(|| {
                self.snapshot.file_value(&field.key).map(|value| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                })
            })
            .or_else(|| {
                field.default_value.as_ref().map(|value| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                })
            })
            // The schema carries no default for `[style] level`; `full` is
            // what `style::resolve` falls back to.
            .or_else(|| {
                (field.key == STYLE_LEVEL_KEY).then(|| crate::style::StyleLevel::Full.to_string())
            });
        match &field.kind {
            FieldKind::Bool => Some((base.as_deref() != Some("true")).to_string()),
            FieldKind::Choice(names) => {
                let at = base
                    .and_then(|base| names.iter().position(|name| *name == base))
                    .map_or(0, |index| index + 1);
                names.get(at % names.len().max(1)).cloned()
            }
            _ => None,
        }
    }

    /// Whether the armed candidate is an edit of `field`.
    fn armed_for(&self, field: &Field) -> bool {
        matches!(
            &self.pending,
            Some(Pending::Armed { edit, .. }) if edit.key() == field.key
        )
    }

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

    /// The tree the dog at `index` was opened onto, or [`None`] while its row
    /// is closed or has never been opened.
    #[must_use]
    pub fn dog_tree(&self, index: usize) -> Option<&DogTree> {
        self.snapshot
            .dogs
            .get(index)
            .and_then(|dog| self.dog_trees.get(&dog.name))
    }

    /// Whether the row of the dog named `name` is mid-read: this screen asked
    /// for that dog's schema or section and has not heard back.
    ///
    /// What routes a dog read to the settings screen rather than to a full
    /// dog pane, so a read asked for by `e` from the flock is never answered
    /// here.
    #[must_use]
    pub fn is_reading_a_dog(&self, name: &str) -> bool {
        self.dog_trees
            .get(name)
            .is_some_and(|tree| tree.is_reading())
    }

    /// The tree the row of the dog named `name` holds, by name rather than by
    /// index, for a reply that carries a name and no index.
    #[must_use]
    pub fn dog_tree_named(&self, name: &str) -> Option<&DogTree> {
        self.dog_trees.get(name)
    }

    /// Opens the dog at `index` if its row is closed, closes it if open.
    /// Answers what the caller has to do next: a dog opened for the first
    /// time has to be probed, and a dog whose probe was refused is probed
    /// again rather than left holding the refusal.
    pub(super) fn toggle_dog(&mut self, index: usize) -> DogFold {
        let Some(dog) = self.snapshot.dogs.get(index) else {
            return DogFold::Nothing;
        };
        let name = dog.name.clone();
        let adopted_path = dog.adopted_path.clone();
        let held = self.dog_trees.remove(&name);
        let (tree, fold) = match held {
            None | Some(DogTree::Refused { .. }) => (
                DogTree::Probing,
                DogFold::Probe {
                    name: name.clone(),
                    adopted_path,
                },
            ),
            Some(DogTree::Probing) => (DogTree::Probing, DogFold::Nothing),
            Some(DogTree::Reading { schema }) => (DogTree::Reading { schema }, DogFold::Nothing),
            Some(DogTree::Open { pane, schema }) => {
                (DogTree::Shut { pane, schema }, DogFold::Closed)
            }
            Some(DogTree::Shut { pane, schema }) => {
                (DogTree::Open { pane, schema }, DogFold::Opened)
            }
        };
        self.dog_trees.insert(name, tree);
        fold
    }

    /// Parks the schema the probe answered with. The row is still waiting on
    /// the section, which is what [`Self::is_reading_a_dog`] says until
    /// [`Self::open_dog`] replaces it.
    pub(super) fn dog_schema(&mut self, name: &str, schema: serde_json::Value) {
        self.dog_trees
            .insert(name.to_owned(), DogTree::Reading { schema });
    }

    /// Opens the dog at `index` onto `pane`, replacing whatever state its row
    /// was in.
    pub(super) fn open_dog(&mut self, index: usize, pane: ConfigPane, schema: serde_json::Value) {
        let Some(name) = self.snapshot.dogs.get(index).map(|dog| dog.name.clone()) else {
            return;
        };
        self.dog_trees.insert(
            name,
            DogTree::Open {
                pane: Box::new(pane),
                schema,
            },
        );
    }

    /// Puts the row of the dog named `name` back to waiting on its section,
    /// keeping the schema its probe already answered with, and answers whether
    /// this screen has a tree for it at all.
    ///
    /// What a landed section write raises: the same `Request::DogConfig` a dog
    /// pane re-reads with, and the same reason for not re-probing. A dog whose
    /// row this screen never opened answers `false`, so a write from a dog pane
    /// raises nothing here.
    pub(super) fn reload_dog(&mut self, name: &str) -> bool {
        let Some(tree) = self.dog_trees.get_mut(name) else {
            return false;
        };
        let schema = match tree {
            DogTree::Open { schema, .. } | DogTree::Shut { schema, .. } => schema.clone(),
            DogTree::Probing | DogTree::Reading { .. } | DogTree::Refused { .. } => {
                return true;
            }
        };
        *tree = DogTree::Reading { schema };
        true
    }

    /// Says on the dog at `index`'s own row that its config could not be
    /// read, and leaves the row closed.
    pub(super) fn refuse_dog(&mut self, index: usize, why: String) {
        let Some(name) = self.snapshot.dogs.get(index).map(|dog| dog.name.clone()) else {
            return;
        };
        self.dog_trees.insert(name, DogTree::Refused { why });
    }

    /// The dog at `index`'s snapshot row.
    #[must_use]
    pub fn dog(&self, index: usize) -> Option<&DogView> {
        self.snapshot.dogs.get(index)
    }

    /// The snapshot row of the dog named `name`, or `None` when this read no
    /// longer lists it.
    ///
    /// What a reply about a dog checks the screen against: a re-read can drop
    /// a dog, and a sentence about a dog the operator can no longer see has to
    /// say something true about a dog that is not on screen.
    #[must_use]
    pub fn dog_named(&self, name: &str) -> Option<&DogView> {
        self.snapshot.dogs.iter().find(|dog| dog.name == name)
    }

    /// What `space` on the dog at `dog`'s `boot first` row would write: the
    /// opposite of the bit the file has.
    #[must_use]
    pub fn next_boot_first(&self, dog: usize) -> Option<bool> {
        self.dog(dog).map(|dog| !dog.boot_first)
    }

    /// What `space` on item `index` of the dog at `dog`'s grants checklist
    /// would write: the permission, and whether it would be held or taken
    /// away. `None` for an index the checklist does not have.
    #[must_use]
    pub fn next_grant(&self, dog: usize, index: usize) -> Option<(String, bool)> {
        let dog = self.dog(dog)?;
        let permission = dog.offered.get(index)?;
        Some((permission.clone(), !dog.grants.grants(permission)))
    }

    /// The dog `name`'s whole section with `row` of its own tree set to `text`,
    /// or why there is none to send.
    ///
    /// Runs through a copy of the dog's own pane rather than a settings-screen
    /// writer of its own, so the gate is [`crate::dog_settings::refusal`], the
    /// value is typed to the field's kind, and the text that reaches
    /// `Request::SetDogConfig` is the operator's section with one line changed
    /// in it, comments and key order intact. That is the same path `e` from the
    /// flock writes through, which is the point: two surfaces cannot leave
    /// `dogs.toml` in two different shapes.
    ///
    /// `Ok(None)` for a value the section already carries, which is not a
    /// failure but is not a write either.
    ///
    /// [`DogSectionToml`] rather than `String`, so the armed candidate carries
    /// the section in the same redacted wrapper a dog pane's close sends it in.
    pub(super) fn dog_section_edit(
        &self,
        name: &str,
        row: usize,
        text: &str,
    ) -> Result<Option<DogSectionToml>, String> {
        let Some(pane) = self.dog_trees.get(name).and_then(DogTree::pane) else {
            return Err(format!("{name} has no open config tree on this screen"));
        };
        let mut copy = pane.clone();
        copy.move_to_index(row);
        copy.type_over(text);
        if let Some(refusal) = copy.apply_typing() {
            return Err(refusal.text);
        }
        let edits = copy.close();
        match copy.edited_section_with(&edits) {
            SectionWrite::Written(toml) => Ok(Some(toml)),
            SectionWrite::Nothing => Ok(None),
            SectionWrite::Refused => Err(format!(
                "{name}'s section holds something this screen would have to rewrite to change it"
            )),
        }
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
    pub(super) fn toggle_fold(&mut self) -> bool {
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

    /// Where an editor opened on the cursor's row writes, and what it opens
    /// with. `None` on a row that cycles, folds or is not a key.
    pub(super) fn editor_at_cursor(&self) -> Option<(Target, String)> {
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
    pub(super) fn removal_at_cursor(&self) -> Option<SettingEdit> {
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

    /// Whether the table or map at the dotted `key` shows what is inside it.
    #[must_use]
    pub fn is_open(&self, key: &str) -> bool {
        !self.folded.contains(key)
    }

    /// Moves the cursor by `delta` rows, clamped to [`Self::rows`], never
    /// wrapping.
    pub(super) fn move_by(&mut self, delta: isize) {
        let len = self.rows_len();
        self.view.move_by(delta, len);
    }

    pub(super) fn move_to_first(&mut self) {
        let len = self.rows_len();
        self.view.move_to(0, len);
    }

    pub(super) fn move_to_last(&mut self) {
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
