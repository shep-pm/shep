//! The settings screen's rows, its pending edit, and the sentence each confirm
//! shows.

use super::*;

/// One row the settings screen's cursor can sit on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    /// One of the six scalar fields, in [`Settings::rows`]'s fixed order.
    Scalar(SettingField),
    /// Index into [`SettingsSnapshot::dogs`].
    Dog(usize),
}

/// The settings screen's own state. `None` on [`App`] is the dashboard.
#[derive(Debug, Clone)]
pub struct Settings {
    pub(super) snapshot: SettingsSnapshot,
    /// The six scalars' shape: which they are, in what order, under which
    /// section. The screen reads its rows, labels and section headers off
    /// this rather than off a `match` per question, so a config pane and
    /// this screen answer them the same way.
    fields: FieldSet,
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
}

/// The settings screen's own in-flight edit.
#[derive(Debug, Clone)]
pub(super) enum Pending {
    /// A free-text edit under construction. Only [`SettingField::Socket`] and
    /// [`SettingField::MaxCronSleep`] reach this, seeded with the field's
    /// on-disk value.
    Typing {
        /// Which scalar.
        field: SettingField,
        /// What the operator has typed so far.
        buffer: String,
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
    /// A freshly opened screen, cursor on the first row.
    pub(super) fn new(snapshot: SettingsSnapshot) -> Self {
        Self {
            snapshot,
            fields: settings_field_set(),
            view: Viewport::new(),
            pending: None,
        }
    }

    /// The armed candidate and its prompt, or `None`.
    #[must_use]
    pub fn pending(&self) -> Option<SettingsPrompt<'_>> {
        match &self.pending {
            Some(Pending::Armed { text, .. } | Pending::DogArmed { text, .. }) => {
                Some(SettingsPrompt { text, sent: false })
            }
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
            Some(Pending::Armed { .. } | Pending::DogArmed { .. })
        )
    }

    /// The field and buffer of an in-flight free-text edit, or `None`.
    #[must_use]
    pub fn typing(&self) -> Option<(&SettingField, &str)> {
        match &self.pending {
            Some(Pending::Typing { field, buffer }) => Some((field, buffer.as_str())),
            _ => None,
        }
    }

    /// The next candidate for `field`, or `None` for the two free-text fields.
    ///
    /// Advances from a candidate already armed for this field, so a second
    /// `space` walks one step further along the cycle. From nothing armed the
    /// base is what the file says, which for `[style] level` is
    /// [`SettingsSnapshot::style_level_in_file`] rather than the level in
    /// force: cycling the resolved level could propose a write that changes
    /// nothing.
    pub(super) fn next_candidate(&self, field: SettingField) -> Option<String> {
        let armed_here = match &self.pending {
            Some(Pending::Armed {
                edit:
                    SettingEdit::Set {
                        field: armed_field,
                        value,
                    },
                ..
            }) if *armed_field == field => Some(value.as_str()),
            _ => None,
        };
        let in_file = (field == SettingField::StyleLevel)
            .then_some(self.snapshot.style_level_in_file.as_deref())
            .flatten();
        let base: String = match (armed_here, in_file) {
            (Some(value), _) | (None, Some(value)) => value.to_string(),
            // A `[style]` document declaring nothing falls back to
            // `StyleLevel`'s compiled default, as `style::resolve` does.
            (None, None) if field == SettingField::StyleLevel => STYLE_LEVEL_ORDER[0].to_string(),
            (None, None) => self.current_value(field)?.to_string(),
        };
        Some(match field {
            SettingField::LogLevel => next_log_level(&base),
            SettingField::LogJson | SettingField::AllowControl => next_bool(&base),
            SettingField::StyleLevel => next_style_level(&base),
            SettingField::Socket | SettingField::MaxCronSleep => return None,
        })
    }

    /// The snapshot's own rendered value for one of the four cycled scalars.
    /// `None` for the two free-text ones.
    fn current_value(&self, field: SettingField) -> Option<&str> {
        Some(match field {
            SettingField::LogLevel => self.snapshot.log_level.value.as_str(),
            SettingField::LogJson => self.snapshot.log_json.value.as_str(),
            SettingField::AllowControl => self.snapshot.allow_control.value.as_str(),
            SettingField::StyleLevel => self.snapshot.style_level.value.as_str(),
            SettingField::Socket | SettingField::MaxCronSleep => return None,
        })
    }

    /// Which layer `field`'s value came from. Only [`confirm_text`]'s `[style]`
    /// arm acts on it.
    pub(super) fn source_of(&self, field: SettingField) -> StyleSource {
        match field {
            SettingField::LogLevel => self.snapshot.log_level.source,
            SettingField::LogJson => self.snapshot.log_json.source,
            SettingField::Socket => self.snapshot.socket.source,
            SettingField::MaxCronSleep => self.snapshot.max_cron_sleep.source,
            SettingField::AllowControl => self.snapshot.allow_control.source,
            SettingField::StyleLevel => self.snapshot.style_level.source,
        }
    }

    /// The rendered value [`App::confirm_setting`] seeds [`Pending::Typing`]'s
    /// buffer with. Only the two free-text fields reach it.
    pub(super) fn text_seed(&self, field: SettingField) -> &str {
        match field {
            SettingField::Socket => self.snapshot.socket.value.as_str(),
            SettingField::MaxCronSleep => self.snapshot.max_cron_sleep.value.as_str(),
            SettingField::LogLevel
            | SettingField::LogJson
            | SettingField::AllowControl
            | SettingField::StyleLevel => {
                unreachable!("text_seed only ever reaches the two free-text fields")
            }
        }
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

    /// Every row the cursor can sit on: the six scalars in their fixed
    /// order, then one row per candidate dog.
    #[must_use]
    pub fn rows(&self) -> Vec<SettingsRow> {
        let mut rows: Vec<SettingsRow> = self
            .fields
            .fields()
            .iter()
            .filter_map(|f| SettingField::from_key(&f.key))
            .map(SettingsRow::Scalar)
            .collect();
        rows.extend((0..self.snapshot.dogs.len()).map(SettingsRow::Dog));
        rows
    }

    /// The field model behind the scalar rows.
    #[must_use]
    pub fn fields(&self) -> &FieldSet {
        &self.fields
    }

    /// The row the cursor sits on. `None` only if [`Self::rows`] is empty,
    /// which the six unconditional scalars make unreachable.
    #[must_use]
    pub fn cursor(&self) -> Option<SettingsRow> {
        let rows = self.rows();
        rows.get(self.view.cursor().min(rows.len().saturating_sub(1)))
            .copied()
    }

    /// Moves the cursor by `delta` rows, clamped to [`Self::rows`], never
    /// wrapping.
    pub(super) fn move_by(&mut self, delta: isize) {
        let len = self.rows().len();
        self.view.move_by(delta, len);
    }

    pub(super) fn move_to_first(&mut self) {
        let len = self.rows().len();
        self.view.move_to(0, len);
    }

    pub(super) fn move_to_last(&mut self) {
        let len = self.rows().len();
        self.view.move_to(len.saturating_sub(1), len);
    }

    /// The viewport, for the renderer.
    #[must_use]
    pub fn view(&self) -> &Viewport {
        &self.view
    }

    /// Records the terminal's height.
    pub fn set_rows(&mut self, rows: usize) {
        let len = self.rows().len();
        self.view.set_rows(rows, len);
    }
}

/// [`LogLevel`]'s own declared order, wrapping from `Trace` back to `Off`.
pub(crate) const LOG_LEVEL_ORDER: [LogLevel; 6] = [
    LogLevel::Off,
    LogLevel::Error,
    LogLevel::Warn,
    LogLevel::Info,
    LogLevel::Debug,
    LogLevel::Trace,
];

/// One step along [`LOG_LEVEL_ORDER`] from `current`. An unparseable value
/// reads as `Warn`, so it still produces a legal next one.
fn next_log_level(current: &str) -> String {
    let index = LogLevel::from_name(current)
        .and_then(|level| {
            LOG_LEVEL_ORDER
                .iter()
                .position(|candidate| *candidate == level)
        })
        .unwrap_or(2);
    LOG_LEVEL_ORDER[(index + 1) % LOG_LEVEL_ORDER.len()]
        .as_str()
        .to_string()
}

/// Flips `"true"`/`"false"`. Anything else reads as `false`.
fn next_bool(current: &str) -> String {
    (current != "true").to_string()
}

/// [`StyleLevel`]'s own declared order, wrapping from `Bare` back to `Full`.
pub(crate) const STYLE_LEVEL_ORDER: [StyleLevel; 3] =
    [StyleLevel::Full, StyleLevel::Plain, StyleLevel::Bare];

/// One step along [`STYLE_LEVEL_ORDER`] from `current`. An unparseable value
/// reads as `Full`.
fn next_style_level(current: &str) -> String {
    let index = StyleLevel::parse(current)
        .and_then(|level| {
            STYLE_LEVEL_ORDER
                .iter()
                .position(|candidate| *candidate == level)
        })
        .unwrap_or(0);
    STYLE_LEVEL_ORDER[(index + 1) % STYLE_LEVEL_ORDER.len()].to_string()
}

/// The confirm sentence for `field`'s candidate `value`, verbatim. `value` is
/// what a `next_*` function produced, never re-derived here.
///
/// Only [`SettingField::StyleLevel`] reads `source`: the other three can only
/// warn about the shepherd's env and flags, which lookout cannot see.
pub(super) fn confirm_text(field: SettingField, value: &str, source: StyleSource) -> String {
    match field {
        SettingField::LogLevel => format!(
            "set log_level to {value}? needs shep daemon reload, and will not apply if the shepherd was booted with SHEP_LOG_LEVEL or --log-level"
        ),
        SettingField::LogJson => format!(
            "set log_json to {value}? needs shep daemon reload, and will not apply if the shepherd was booted with SHEP_LOG_JSON or --log-json"
        ),
        SettingField::AllowControl => {
            let word = if value == "true" { "on" } else { "off" };
            format!("turn whistle control tools {word}? needs shep whistle restarted")
        }
        SettingField::StyleLevel => style_confirm_text(value, source),
        SettingField::Socket | SettingField::MaxCronSleep => unreachable!(
            "Settings::next_candidate never arms these two -- they are task 8's Pending::Typing"
        ),
    }
}

/// The `[style] level` half of [`confirm_text`].
///
/// Under `Env` or `Flag` the write lands and the level in force does not move,
/// so the sentence names the layer that keeps winning.
fn style_confirm_text(value: &str, source: StyleSource) -> String {
    match source {
        StyleSource::Config | StyleSource::Default => {
            format!("set style level to {value}? the next command reads it")
        }
        StyleSource::Env => format!(
            "set style level to {value}? it goes in the file, but $SHEP_STYLE is set and keeps winning until it is unset"
        ),
        StyleSource::Flag => format!(
            "set style level to {value}? it goes in the file, but --style was passed to this lookout and keeps winning for as long as it runs"
        ),
    }
}

/// The confirm sentence for a free-text edit, verbatim.
///
/// Only [`SettingField::Socket`] and [`SettingField::MaxCronSleep`] reach it;
/// the other four go through [`confirm_text`] and, not being optional, are
/// never [`SettingEdit::Unset`].
pub(super) fn confirm_text_for_edit(edit: &SettingEdit) -> String {
    match edit {
        SettingEdit::Set {
            field: SettingField::Socket,
            value,
        } => format!(
            "set socket to {value}? needs the shepherd stopped and started; a reload will not move it, and it will not apply if the shepherd was booted with SHEP_SOCKET or --socket"
        ),
        SettingEdit::Set {
            field: SettingField::MaxCronSleep,
            value,
        } => format!(
            "set max_cron_sleep to {value}? needs shep daemon reload, and will not apply if the shepherd was booted with SHEP_MAX_CRON_SLEEP or --max-cron-sleep"
        ),
        SettingEdit::Unset {
            field: SettingField::Socket,
        } => "unset socket? it goes back to the default under $SHEP_HOME, and needs the shepherd stopped and started"
            .to_string(),
        SettingEdit::Unset {
            field: SettingField::MaxCronSleep,
        } => "unset max_cron_sleep? it goes back to the daemon's own default, and needs shep daemon reload"
            .to_string(),
        SettingEdit::Set { .. } | SettingEdit::Unset { .. } => unreachable!(
            "on_settings_text_key only ever builds an edit for socket or max_cron_sleep"
        ),
    }
}

/// What `Msg::SettingWritten`'s `Err` arm reopens [`Pending::Typing`] with: the
/// field and the text the operator typed. `None` for the four cycled fields,
/// which have no editor to reopen.
pub(super) fn typed_text_of(edit: &SettingEdit) -> Option<(SettingField, String)> {
    match edit {
        SettingEdit::Set {
            field: field @ (SettingField::Socket | SettingField::MaxCronSleep),
            value,
        } => Some((*field, value.clone())),
        SettingEdit::Unset {
            field: field @ (SettingField::Socket | SettingField::MaxCronSleep),
        } => Some((*field, String::new())),
        _ => None,
    }
}

impl App {
    /// The settings screen's own state, or `None` while the dashboard is
    /// showing.
    #[must_use]
    pub fn settings(&self) -> Option<&Settings> {
        match &self.body {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// [`Self::settings`]'s mutable twin, for the settings keymap and the
    /// handlers that update a field or a pending edit in place.
    pub(super) fn settings_mut(&mut self) -> Option<&mut Settings> {
        match &mut self.body {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::view::fixtures;

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

        let row = &app.settings().unwrap().snapshot().style_level;
        assert_eq!(
            row.source,
            StyleSource::Flag,
            "the flag beats the file rather than being dropped by it"
        );
        assert_eq!(row.value, "plain");
    }
}
