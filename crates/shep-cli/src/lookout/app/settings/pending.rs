//! The pending edit the settings screen holds: the candidate `space` arms, the
//! prompt a confirm shows, the sentence a write is waiting on, and what a reply
//! clears.

use super::*;

impl Settings {
    /// A freshly opened screen, cursor on the first row, every section open.
    pub(in crate::lookout::app) fn new(snapshot: SettingsSnapshot) -> Self {
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
                | Pending::DogSectionArmed { text, .. }
                | Pending::DogEnvArmed { text, .. },
            ) => Some(SettingsPrompt { text, sent: false }),
            Some(Pending::Sent { text, .. }) => Some(SettingsPrompt { text, sent: true }),
            Some(Pending::Typing { .. } | Pending::TypingEnv { .. }) | None => None,
        }
    }

    /// Whether `ticket` names the write this screen is still waiting on,
    /// clearing the prompt when it does.
    ///
    /// A reply whose ticket is not the one [`Pending::Sent`] holds belongs to
    /// a write the screen has moved on from, and its answer must not touch
    /// the edit that is here now.
    pub(in crate::lookout::app) fn resolve(&mut self, ticket: u64) -> bool {
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
    pub(in crate::lookout::app) fn is_armed(&self) -> bool {
        matches!(
            self.pending,
            Some(
                Pending::Armed { .. }
                    | Pending::DogArmed { .. }
                    | Pending::DogTuneArmed { .. }
                    | Pending::DogSectionArmed { .. }
                    | Pending::DogEnvArmed { .. }
            )
        )
    }

    /// The dog and the editor of an in-flight `env` edit, or `None`.
    #[must_use]
    pub fn env_typing(&self) -> Option<(&str, &KeyValues)> {
        match &self.pending {
            Some(Pending::TypingEnv { dog, editor }) => Some((dog.as_str(), editor)),
            _ => None,
        }
    }

    /// The label and buffer of an in-flight free-text edit, or `None`.
    #[must_use]
    pub fn typing(&self) -> Option<(String, &TextBox)> {
        match &self.pending {
            Some(Pending::Typing { target, buffer }) => Some((target.label(), buffer)),
            _ => None,
        }
    }

    /// Whether the free-text edit in flight is on a `x-shep-secret` field of a
    /// dog, which every place that draws its buffer masks.
    #[must_use]
    pub fn typing_is_secret(&self) -> bool {
        matches!(
            &self.pending,
            Some(Pending::Typing {
                target: Target::DogValue { secret: true, .. },
                ..
            })
        )
    }

    /// The next candidate for `field`, or `None` for a key that is not a
    /// cycle: only a boolean and a closed set of names are.
    ///
    /// Advances from a candidate already armed for this field. From nothing
    /// armed the base is what the file says, which for `[style] level` is not
    /// the level in force: cycling the resolved level could propose a write
    /// that changes nothing.
    pub(in crate::lookout::app) fn next_candidate(&self, field: &Field) -> Option<String> {
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
}
