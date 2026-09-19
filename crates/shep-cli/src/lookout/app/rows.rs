//! The small value types the flock table, the status bar and the wire share.

use super::*;

/// The connection's state, as the dashboard reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Connected and subscribed.
    Live,
    /// Re-dialling. `attempt` is 1-based and bounded by
    /// `crate::lookout::link::RECONNECT_ATTEMPTS`.
    Retrying {
        /// Which attempt is in flight.
        attempt: u32,
    },
    /// The ladder is exhausted. Terminal: nothing moves this state, and the
    /// values on screen stay as they were.
    Lost {
        /// When it was declared lost, already rendered for display.
        at_local: String,
        /// Why the last dial failed, in the error's own words.
        why: String,
    },
}

/// One sheep's row: what the shepherd said, and when it said it.
#[derive(Debug, Clone)]
pub struct Row {
    /// The shepherd's own snapshot of this sheep.
    pub info: ProcessInfo,
    /// When [`Self::info`] was received: the origin for this row's live
    /// uptime, so a value two seconds old is never rendered as current.
    pub anchor: Instant,
}

impl Row {
    /// What this row's STATUS cell reports: [`Self::info`]'s status, or
    /// [`Reported::Silent`] for a dog whose process is up and which has never
    /// handshook this shepherd.
    ///
    /// Mirrors `output::rows::reported`. The `dog` check is read here rather
    /// than left to [`Reported::of`], so a non-dog row can never be painted
    /// silent by a stray `handshook`.
    #[must_use]
    pub(crate) fn reported(&self) -> Reported {
        if self.info.dog.is_none() {
            return Reported::Live(self.info.status);
        }
        Reported::of(self.info.status, self.info.handshook)
    }
}

/// One app's rolled-up numbers, computed from its own instances.
///
/// The fields `output::rows`'s own `GroupTotals` sums for `shep flock`.
/// Restarts, cpu and memory are summed; uptime is the minimum, so a group reads
/// as time since the app was last disturbed.
#[derive(Debug, Clone)]
pub struct GroupTotals {
    /// How many instances make up this group.
    pub count: usize,
    /// Every instance's restarts, added up.
    pub restarts: u32,
    /// Every instance's CPU reading summed, `None` only when not one
    /// instance has a live reading.
    pub cpu: Option<f32>,
    /// Every instance's memory reading summed, `None` only when not one
    /// instance has a live reading.
    pub memory: Option<u64>,
    /// The minimum live uptime across instances, `None` only when the group
    /// has none.
    pub uptime_ms: Option<u64>,
}

/// One request the dashboard asked the link task to send, carried back on the
/// reply so it can be routed.
///
/// An echo tag rather than a correlation id: an `Err` reply carries no shape of
/// its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    /// The selected sheep's process tree.
    Lambs {
        /// Which sheep was asked about.
        id: u32,
    },
    /// One action against a target: one sheep, or every instance of a named
    /// app. `name` rides along so a reply can be reported by name even after
    /// the target has left the flock.
    Action {
        /// Which verb.
        verb: ActionVerb,
        /// The pinned target.
        target: RowKey,
        /// Its name at arm time.
        name: String,
    },
    /// One dog's daemon half, after its file half landed. `source` is what
    /// the write returned, so the request cannot disagree with the file.
    Dog {
        /// The dog's name.
        name: String,
        /// `true` to start it, `false` to stop and deregister it.
        enable: bool,
        /// Where the binary comes from, exactly as the config write
        /// answered.
        source: DogSource,
        /// The ticket the file half went out with, carried on so the
        /// shepherd's answer resolves the prompt that toggle raised and no
        /// other.
        ticket: u64,
    },
    /// One sheep's effective config, for the config pane. Raised by `e` on
    /// the dashboard and by `r` from inside an open pane.
    ///
    /// A name rather than a [`RowKey`], because [`Request::SheepConfig`]
    /// takes one: the pane is about an app's stored spec, which every
    /// instance of a multi-instance app shares, so an id would name a
    /// narrower thing than the config it would come back with.
    SheepConfig {
        /// Which sheep was asked about.
        name: String,
    },
    /// One field of one sheep's config, off the config pane's own `Enter`.
    ///
    /// [`Self::SetEnv`]'s twin, and it is a `Request::SetSheepField`
    /// rather than a one-key `Request::ApplyConfig` for the reason that
    /// variant's own doc gives: an `ApplyConfig` at `ResetDepth::File`
    /// moves the field and then spends the operator's override for it,
    /// because that merge is a template load. This pane's value is the
    /// operator's, so the sheep still differs from its file and the `*`
    /// marker has to keep saying so.
    ///
    /// The [`WriteAuthority`] is not decoration, for exactly the reason
    /// [`Effect::WriteSetting`]'s own doc gives: this variant cannot be
    /// named without having passed the gate, so a fifth door added
    /// tomorrow cannot walk around it.
    ApplyField {
        /// The sheep.
        name: String,
        /// Which write this is, so a reply names its own request and no
        /// other. Minted per send, so no two are ever equal.
        ticket: u64,
        /// The field that moves.
        key: String,
        /// Its new value, in the shape that field serializes as. Wrapped,
        /// so a `{:?}` of this enum cannot print it: `cwd` and `script`
        /// hold a home directory and `args` holds a token (IR-41).
        value: FieldValue,
        /// Proof the control gate was open.
        authority: WriteAuthority,
    },
    /// One dog's `[<name>]` section, for the dog config pane. Raised once
    /// the pane's schema probe has answered, and again by `r` from inside an
    /// open dog pane and after a landed write.
    ///
    /// The schema is not asked for here and never travels the wire: it comes
    /// off the dog's own binary ([`Effect::LoadDogPane`]), because nothing
    /// records one. `shep adopt` uses it for the vet and writes down only
    /// the path.
    DogSection {
        /// Which dog was asked about.
        name: String,
    },
    /// One dog's whole section, off the dog pane's own `Enter`.
    ///
    /// The whole section rather than one key, because
    /// `Request::SetDogConfig` replaces the table: `ConfigPane::edited_section_with`
    /// applies the batch through `toml_edit`, so the operator's comments and
    /// key order survive a write shep did not author.
    ///
    /// Not a `Request::ApplyConfig`, and not for [`Self::ApplyField`]'s
    /// reason either: a dog has no override store and no Flockfile. Its
    /// section is the operator's outright, and `dogs.toml` is the one copy
    /// of it.
    ///
    /// The [`WriteAuthority`] is not decoration, for the reason
    /// [`Effect::WriteSetting`]'s own doc gives.
    SetDogSection {
        /// The dog.
        name: String,
        /// Which write this is, so a reply names its own request and no
        /// other. Minted per send, so no two are ever equal.
        ticket: u64,
        /// The section, edit applied. [`DogSectionToml`] rather than a
        /// `String` so a `{:?}` of this enum cannot print the webhook
        /// credentials a dog's section routinely holds (IR-41).
        toml: DogSectionToml,
        /// Proof the control gate was open.
        authority: WriteAuthority,
    },
    /// One env key of one sheep, off the env sub-screen's own `Enter`.
    ///
    /// Its own variant beside [`Self::ApplyField`] rather than a value of
    /// it, because the two are different requests: a whole env map is
    /// never sent (a pane is not told the values), so `SetSheepField`
    /// refuses the `env` key outright and `SetSheepEnv` takes one key at a
    /// time.
    SetEnv {
        /// The sheep.
        name: String,
        /// Which write this is, for [`Self::ApplyField`]'s reason.
        ticket: u64,
        /// The env key.
        key: String,
        /// The value, or `None` to remove the key. [`EnvValue`] rather than
        /// a `String` so a `{:?}` of this enum cannot print it (IR-41).
        value: Option<EnvValue>,
        /// Proof the control gate was open.
        authority: WriteAuthority,
    },
}

impl Sent {
    /// The wire request this asks for.
    #[must_use]
    pub fn request(&self) -> Request {
        match self {
            Self::Lambs { id } => Request::Describe {
                selector: SelectorSpec::Id(*id),
            },
            Self::Action { verb, target, .. } => {
                let selector = match target {
                    RowKey::Sheep(id) => SelectorSpec::Id(*id),
                    RowKey::Group(name) => SelectorSpec::Name(name.clone()),
                    RowKey::Fold(name) => SelectorSpec::Fold(name.clone()),
                    RowKey::Section(_) => unreachable!("a header is never an action target"),
                };
                match verb {
                    ActionVerb::Stop => Request::Stop { selector },
                    ActionVerb::Restart => Request::Restart { selector },
                    ActionVerb::Reload => Request::Reload { selector },
                }
            }
            Self::Dog {
                name,
                enable: true,
                source,
                ..
            } => Request::EnableDog {
                name: name.clone(),
                source: source.clone(),
            },
            Self::Dog {
                name,
                enable: false,
                ..
            } => Request::DisableDog { name: name.clone() },
            Self::SheepConfig { name } => Request::SheepConfig { name: name.clone() },
            // One field, recorded as an operator override, not an
            // `ApplyConfig`: see this variant's own doc for the marker that
            // route silently spent.
            Self::ApplyField {
                name, key, value, ..
            } => Request::SetSheepField {
                name: name.clone(),
                key: key.clone(),
                // Unwrapped only here, at the wire. See `FieldValue` for
                // why the protocol's own field is a bare `Value` while
                // everything above it is not.
                value: value.as_value().clone(),
            },
            Self::DogSection { name } => Request::DogConfig { name: name.clone() },
            Self::SetDogSection { name, toml, .. } => Request::SetDogConfig {
                name: name.clone(),
                toml: toml.clone(),
            },
            Self::SetEnv {
                name, key, value, ..
            } => Request::SetSheepEnv {
                name: name.clone(),
                key: key.clone(),
                value: value.clone(),
            },
        }
    }

    /// The ticket a pane write minted itself with, or [`None`] for a
    /// variant no pane ever produces.
    ///
    /// What [`App::resolve_held_write`] matches a reply against, rather
    /// than trusting whatever a counter says is outstanding: two writes in
    /// flight from two different close-dialog answers are otherwise
    /// indistinguishable to a count, and a reply belonging to neither
    /// [`App::held`] batch must never be read as belonging to it.
    pub(super) fn ticket(&self) -> Option<u64> {
        match self {
            Self::ApplyField { ticket, .. }
            | Self::SetDogSection { ticket, .. }
            | Self::SetEnv { ticket, .. } => Some(*ticket),
            Self::Lambs { .. }
            | Self::Action { .. }
            | Self::Dog { .. }
            | Self::SheepConfig { .. }
            | Self::DogSection { .. } => None,
        }
    }
}

/// One dog toggle, ready for the file half: [`Effect::WriteDog`] carries one
/// and [`Msg::DogWritten`] echoes it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DogEdit {
    /// The dog's name.
    pub name: String,
    /// `true` to enable, `false` to disable.
    pub enable: bool,
}

/// A dog's schema, and the binary it was probed from.
///
/// Held on [`App`] between the probe answering ([`Msg::DogPane`]) and the
/// section landing ([`Sent::DogSection`]), and then for as long as the pane
/// is open, so a refresh re-reads the section without respawning the dog.
///
/// `Debug` is derived rather than redacted (IR-41): a name, a binary's path
/// and a JSON Schema. A schema describes values without carrying any, the
/// same argument `crate::lookout::field::Field`'s own derived `Debug` makes, and a
/// dog's defaults come from its binary describing itself rather than from
/// this flock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DogProbe {
    /// The dog.
    pub name: String,
    /// The adopted binary, or [`None`] for a built-in.
    pub adopted_path: Option<PathBuf>,
    /// What it answered the schema flag with.
    pub schema: serde_json::Value,
}

/// What the cursor can sit on: one sheep, or the header above an app's
/// instances.
///
/// A name earns a [`Self::Group`] only with more than one instance, every one
/// of them reporting its slot ([`App::grouped_names`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RowKey {
    /// One app's group header, carrying its name.
    Group(String),
    /// One sheep, by id.
    Sheep(u32),
    /// A header, never selectable. `&'static str` because the only two are
    /// written here.
    Section(&'static str),
    /// One fold's header, carrying its name.
    ///
    /// Selectable and actionable, unlike [`Self::Section`], because
    /// `SelectorSpec::Fold` can name its members on the wire.
    Fold(String),
}

/// How the flock table gathers its rows.
///
/// `Debug` is derived; it is two words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grouping {
    /// Every sheep in one list, apps rolled up by instance.
    #[default]
    Flat,
    /// Sheep gathered under their fold.
    ByFold,
}

/// What one lamb fetch came back with. The pane says a different sentence for
/// each variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LambWalk {
    /// The shepherd walked the process table. Possibly to no descendants.
    Walked(Vec<Lamb>),
    /// The reply carried no walk at all, which for a `Describe` means this
    /// sheep has no pid to walk from.
    NotWalked,
    /// The request did not come back, or came back as something this binary
    /// does not understand.
    Failed,
}

/// One lamb reading, and which sheep it was taken for.
#[derive(Debug, Clone)]
pub struct LambReading {
    pub(super) id: u32,
    pub(super) at: Instant,
    pub(super) walk: LambWalk,
}

/// A short line the status bar shows instead of the key hints, cleared by the
/// next keypress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub(super) text: String,
    /// True for a refusal or a damage report: the status bar picks
    /// [`Palette::refusal`] over [`Palette::attention`].
    pub(super) grave: bool,
}

impl Notice {
    /// Whether this notice is a refusal or a damage report rather than an
    /// informational one.
    #[must_use]
    pub fn is_grave(&self) -> bool {
        self.grave
    }
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}
