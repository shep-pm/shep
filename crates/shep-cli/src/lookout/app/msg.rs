//! What arrives at the reducer and what leaves it.

use super::*;

/// Whether this lookout may act on a sheep.
///
/// Turned on by `--allow-control` or `lookout.allow_control` in the KV store.
/// A fat-finger catch, not a security boundary: anyone who can run lookout can
/// run `shep stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Actions refuse. Asked for by `--read-only`, or by
    /// `lookout.allow_control` being `false`.
    ReadOnly,
    /// Actions are permitted, and the default: `x`, `R` and `L` arm a
    /// confirm, Enter sends it. The apply menu is the one door that sends on
    /// the press, since it names its keys on screen.
    Allowed,
}

/// Which keymap is in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// The ordinary dashboard keys.
    Normal,
    /// The filter box is open and every printable key is text.
    Text,
}

/// The keys lookout binds, named by meaning rather than by keystroke.
///
/// `crate::lookout::input::map_key` builds these at the edge, so this module
/// never touches a terminal crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPress {
    /// `q` in normal mode, or `Ctrl-C` in either.
    Quit,
    /// `Esc` in normal mode: cancels an armed confirm, else clears the filter,
    /// else quits. The reducer decides; the keymap sees none of those states.
    Escape,
    /// `k` or `Up`: the selection moves up one row.
    SelectUp,
    /// `j` or `Down`: the selection moves down one row.
    SelectDown,
    /// `g` or `Home`: the first sheep in the flock.
    SelectFirst,
    /// `G` or `End`: the last one.
    SelectLast,
    /// `r`: poll now.
    Refresh,
    /// `x`, `R` or `L`: arms a confirm for the verb, or refuses and says why.
    Action(ActionVerb),
    /// `Enter` in normal mode. Sends an armed confirm; does nothing
    /// otherwise.
    Confirm,
    /// `/`: open the filter box, carrying whatever query is already set.
    FilterStart,
    /// One printable character typed into whichever text field is open.
    TextChar(char),
    /// `Backspace` in whichever text field is open.
    TextBackspace,
    /// `Enter` in whichever text field is open: apply and leave.
    TextApply,
    /// `Esc` in whichever text field is open: abandon the edit and leave.
    TextAbandon,
    /// `s`: opens the settings screen, or closes it from inside.
    Settings,
    /// `S`: opens the secrets pane, or closes it from inside.
    ///
    /// Picked rather than found. The design named `g`, which is
    /// [`Self::SelectFirst`]; `s` is the settings screen and `S` is its
    /// neighbour, both change-screens over the shepherd's own
    /// configuration.
    Secrets,
    /// `v`: shows the selected secret's value for [`REVEAL_HOLDS`], in the
    /// secrets pane. Refuses when `[secrets] allow_read` is off, naming the
    /// gate. Ignored on every other screen.
    Reveal,
    /// `y`: sends the already-revealed value to the terminal's clipboard
    /// over OSC 52, in the secrets pane. A reveal by another route, so it
    /// takes [`Self::Reveal`]'s own `[secrets] allow_read` gate rather than
    /// a second one, and copies what [`Reveal`] already holds on screen
    /// rather than reading the store afresh. Ignored on every other screen.
    Copy,
    /// `Left`: the previous environment tab, in the secrets pane. Stops at
    /// the first rather than wrapping. Ignored elsewhere.
    TabPrev,
    /// `Right`: the next one, stopping at the last.
    TabNext,
    /// `space`: cycles the value under the settings screen's cursor. Nothing on
    /// the dashboard; refuses like an action key when the gate is closed.
    Cycle,
    /// `e`: open the config pane for the selected sheep on the dashboard.
    /// Inside the pane or its env sub-screen, edits the row under the
    /// cursor or sends an armed edit, the same job `Confirm` already does
    /// there: an operator should not have to remember two keys for one
    /// job. `Escape` is the only key that closes a pane or a sub-screen.
    Edit,
    /// `h` or `?`: opens the keymap overlay, from any body. Pressing either
    /// again, or `Escape`, closes it. Refused while a close dialog is up,
    /// which owns the keyboard until it is answered, and consumed as a
    /// cancel instead of opening while an action or a settings candidate
    /// is armed, the same as every other key.
    ///
    /// The config pane's field help draws unconditionally, wherever the
    /// explanation panel cannot show it, so no key is needed for it: see
    /// `view::pane::top_lines`. That is why `h` is free to mean this here
    /// too.
    Help,
    /// `d`: arms the removal of the element under the cursor, on the config
    /// pane's list sub-screen. On a config field, restores the default by
    /// removing the operator's override so the default shows through. One
    /// verb, both places.
    Remove,
    /// `K`. What a step means is the body's to decide: a config pane
    /// reorders the list element under the cursor, the sheep pane steps to
    /// the previous sheep the flock table would show without leaving the
    /// pane, and every other body routes it to [`Effect::None`].
    StepUp,
    /// `J`, the twin of [`Self::StepUp`].
    StepDown,
    /// `F`: toggles the flock table between the flat list and grouping by
    /// fold.
    FoldView,
    /// `z`: hides the selected fold's members, leaving its header behind.
    /// Pressed again on the same fold, or anywhere while collapsed, it
    /// shows them again. Does nothing when the selection is not a
    /// [`RowKey::Fold`] header.
    Collapse,
    /// `b`: opens the full-screen bleats pane on the selected sheep. Does
    /// nothing with no sheep selected, the way `e` does.
    Bleats,
    /// `o`: cycles the bleats pane's stream axis, `None` (both) → `Out` →
    /// `Err` → `None`. A global binding, so the dashboard sees it too, and
    /// ignores it: there is no stream axis outside the pane.
    StreamCycle,
    /// `m`: cycles the bleats pane's minimum-level axis through every
    /// [`Level`] in ascending order, then back
    /// to `None`. Chosen over a design-named key because the status bar's own
    /// list names none for this axis; a global binding, ignored on the
    /// dashboard the same way [`Self::StreamCycle`] is.
    LevelCycle,
    /// `Ctrl-d`: scrolls the bleats pane forward (toward the newest line) by
    /// a body height. A global binding, ignored on the dashboard the same
    /// way [`Self::StreamCycle`] is.
    PageDown,
    /// `Ctrl-u`: scrolls the bleats pane backward (toward the oldest
    /// surviving line) by a body height, and stops it following the tail,
    /// the same way [`Self::SelectUp`] does. Ignored on the dashboard.
    PageUp,
    /// `f`: toggles whether the bleats pane follows the tail. Ignored on the
    /// dashboard.
    FollowToggle,
    /// `w`: toggles whether the bleats pane wraps long lines rather than
    /// truncating them. Ignored on the dashboard.
    WrapToggle,
    /// `n`: steps the bleats pane toward the newest line that matches the
    /// match axis. Does nothing with no match axis set, rather than
    /// quietly becoming a line-movement key. Ignored on the dashboard.
    MatchNext,
    /// `N`: the same, toward the oldest matching line. Ignored on the
    /// dashboard.
    MatchPrev,
    /// `D`: arms the removal of the selected key's value in the current
    /// tab's environment, in the secrets pane. `Enter` confirms.
    ///
    /// Capital, and `d` is left alone: `d` is already
    /// [`Self::Remove`], and `map_key` dispatches on mode rather than
    /// pane, so the two cannot share a key.
    SecretDelete,
    /// `Tab`: moves the config pane's focus to the next group.
    NextGroup,
    /// `1` through `8`: jumps the config pane's focus straight to that
    /// group, numbered in the order the pane lists them.
    Group(u8),
    /// `u`: undoes the config pane's last change.
    Undo,
    /// `c` in the close dialog: write the set and leave the sheep running.
    /// Bound nowhere else, the way [`Self::Undo`] is read only by the
    /// config pane.
    Continue,
}

/// Everything that can change the dashboard.
#[derive(Debug, Clone)]
pub enum Msg {
    /// A `Request::ListFlock` reply landed. `at` is when it was received, and
    /// becomes every row's uptime anchor.
    Snapshot {
        /// The flock as the shepherd reported it.
        rows: Vec<ProcessInfo>,
        /// When the reply was received.
        at: Instant,
    },
    /// One frame off the bus.
    Event(BusEvent),
    /// This client's own receiver fell behind and discarded frames.
    /// [`BusEvent::Dropped`] is the shepherd's queue instead.
    BusLagged {
        /// How many frames this process lost.
        count: u64,
    },
    /// The link task is re-dialling; `attempt` is 1-based.
    Retrying {
        /// Which attempt is in flight.
        attempt: u32,
    },
    /// The link task reconnected and re-subscribed.
    Relinked,
    /// The reconnect ladder is exhausted. Everything on screen is now frozen.
    Frozen {
        /// When the link was declared lost, already rendered for display.
        at_local: String,
        /// The last dial's own words, from
        /// [`LinkError`](crate::lookout::source::LinkError)'s `Display`.
        /// Carried rather than re-derived so the link panel quotes the
        /// failure instead of guessing at one.
        why: String,
    },
    /// One key.
    Key(KeyPress),
    /// The 1s heartbeat. `now` is what advances every running sheep's uptime.
    Tick {
        /// The current instant, read by the caller.
        now: Instant,
    },
    /// The terminal changed size; nothing to update but the frame is stale.
    Resize,
    /// One reading of the machine this lookout runs on, off the 1s heartbeat.
    /// `None` means `sysinfo` does not support this platform. Refused once the
    /// link is lost.
    Host {
        /// What the sampler saw, or `None` on an unsupported platform.
        sample: Option<crate::lookout::source::HostSample>,
    },
    /// How big the selected sheep's two log files are on disk, read on the
    /// same cadence as the lamb walk. Always yields [`Effect::None`].
    LogSize {
        /// The sheep the read was taken for.
        id: u32,
        /// `out` plus `err`, or `None` when either could not be read.
        total_bytes: Option<u64>,
    },
    /// One refresh of the selected sheep's log files, answering an
    /// [`Effect::RefreshFeed`]. Always yields [`Effect::None`].
    Bleats {
        /// What the read found, including what it could not show.
        tail: crate::lookout::tail::Tail,
    },
    /// A request this dashboard asked for came back. `sent` is the echo tag.
    Replied {
        /// What was asked.
        sent: Sent,
        /// What the shepherd said, or why it could not be asked.
        result: Result<Response, RequestError>,
    },
    /// A request the caller could not hand to the link task.
    ///
    /// The reducer is already in the in-flight state when `run_ui` tries to
    /// send, so a failed `try_send` has to come back, or the
    /// one-action-at-a-time guard refuses everything from then on.
    Unsent {
        /// What could not be sent.
        sent: Sent,
    },
    /// An [`Effect::SendAll`] batch finished going out to the link task.
    ///
    /// `None` means every entry reached the channel; `send` only ever fails
    /// there once the channel is closed, so the first casualty stands for
    /// the rest of that batch, the same way a lone [`Effect::Send`] reports
    /// only the one entry it carries.
    BatchSent {
        /// The first entry the channel refused, once closed.
        unsent: Option<Sent>,
    },
    /// The settings screen's read of `shep.toml` landed, answering an
    /// [`Effect::LoadSettings`]. A `String` error, since this reducer holds no
    /// error types from `commands`.
    Settings {
        /// The rendered snapshot, or why it could not be read.
        result: Result<SettingsSnapshot, String>,
    },
    /// An [`Effect::WriteSetting`] has landed.
    SettingWritten {
        /// The edit that was sent, echoed back: the cursor can have moved on
        /// while the write was in flight.
        edit: SettingEdit,
        /// The ticket it went out with, echoed back: the screen can have
        /// armed a second edit, or closed and reopened, while it was in
        /// flight.
        ticket: u64,
        /// Whether the write landed, or why it did not.
        result: Result<(), String>,
    },
    /// An [`Effect::LoadDogPane`]'s schema probe has answered.
    ///
    /// The schema half only: the section arrives separately over the wire
    /// as [`Sent::DogSection`], since it comes from the shepherd rather
    /// than the dog's own binary. This arm parks the schema and raises the
    /// request for the section; the pane is built once that lands.
    ///
    /// `Result<_, String>`, like [`Self::Settings`]: this reducer holds no
    /// error types from `commands`.
    DogPane {
        /// The dog.
        name: String,
        /// The adopted binary, or [`None`] for a built-in, echoed back so
        /// the pane records what it probed.
        adopted_path: Option<PathBuf>,
        /// The dog's schema, or why there is no pane for it.
        result: Result<serde_json::Value, String>,
    },
    /// An [`Effect::WriteDog`] has landed. `Ok` carries the [`DogSource`] the
    /// write resolved, which [`Sent::Dog`] then rides to the shepherd, so the
    /// request cannot disagree with the file.
    DogWritten {
        /// The toggle that was sent, echoed back.
        edit: DogEdit,
        /// The ticket it went out with, echoed back for
        /// [`Self::SettingWritten`]'s reason.
        ticket: u64,
        /// Whether the write landed and what it resolved to, or why it did
        /// not.
        result: Result<DogSource, String>,
    },
    /// An [`Effect::LoadSecrets`] has answered.
    ///
    /// `Result<_, String>`, like [`Self::Settings`]: `secrets::model` cannot
    /// fail, but the `spawn_blocking` it runs on can. Dropped when no pane
    /// is open, the way a late config reply is.
    ///
    /// `environment` is the tab this read was built for, echoed back
    /// because the pane's model is empty on the very first load and has no
    /// tab to read it from; every later load already knows it from
    /// [`SecretsPane::tab`], but carrying it here keeps this arm's rule
    /// (find the requested environment in the fresh model, or fall back to
    /// 0) the same on every load rather than only the first.
    Secrets {
        /// The environment [`crate::lookout::secrets::model`] was built for.
        environment: String,
        /// The model, boxed: it is much larger than every other variant.
        result: Result<Box<SecretsModel>, String>,
    },
    /// An [`Effect::RevealSecret`]'s read has answered.
    ///
    /// Nothing here reaches the screen on its own: the pane draws the value
    /// only while every reason it was asked for still holds, which is what
    /// the two echoes are for.
    Revealed {
        /// The key that was read, echoed back: the selection can have moved
        /// to another row while the read was in flight.
        key: String,
        /// The environment tab it was read for, echoed back for
        /// [`Self::Secrets`]'s reason.
        environment: String,
        /// The value, or `None` when the slot no longer resolves or the
        /// store would not read.
        value: Option<RevealedValue>,
    },
    /// An [`Effect::WriteSecret`] has landed.
    SecretWritten {
        /// What the write returned, its error already rendered: a
        /// `SecretError` names a key, never a value, but the pane has no
        /// use for the type.
        ///
        /// `Ok(true)` means the store changed. `Ok(false)` is `secrets::unset`
        /// reporting that there was no slot to remove, which a delete must
        /// say out loud rather than report as a success.
        result: Result<bool, String>,
    },
}

/// The one gate on writing `shep.toml` from the settings screen.
///
/// [`WriteAuthority`]'s field is private, so [`WriteAuthority::granted`] is the
/// only way to build one, and it hands back [`None`] under
/// [`Control::ReadOnly`]. [`Effect::WriteSetting`] and [`Effect::WriteDog`]
/// each carry one, so a handler cannot name a write without the check.
mod authority {
    use super::App;

    /// Proof that `--allow-control` was on when a settings write was built.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct WriteAuthority(());

    impl WriteAuthority {
        /// A token when `app`'s [`Control`](super::Control) permits writing,
        /// [`None`] otherwise.
        ///
        /// The sole constructor, taking `&App` rather than a `Control` so the
        /// answer can only be the app's own gate.
        #[must_use]
        pub fn granted(app: &App) -> Option<Self> {
            match app.control {
                super::Control::ReadOnly => None,
                super::Control::Allowed => Some(Self(())),
            }
        }
    }
}

pub use authority::WriteAuthority;

/// What the caller has to do after an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Nothing.
    None,
    /// Ask the link task for a `ListFlock` now, rather than at the next tick.
    PollNow,
    /// Re-read the selected sheep's log files and hand the result back as
    /// [`Msg::Bleats`]. The feed has no timer of its own; it rides this.
    RefreshFeed,
    /// Re-read the selected sheep's log files and ask the shepherd for its
    /// lambs. Raised only when the selection moved: a snapshot refreshes the
    /// feed alone, since it fires every two seconds.
    RefreshSelected,
    /// Send a request to the shepherd. Raised by [`App::confirm`] once an
    /// armed action's Enter lands; `crate::lookout::run_ui` sends it.
    Send(Sent),
    /// Send several requests, in the order given. Raised when a config
    /// pane closes: nothing it edited has gone out yet, so the whole set
    /// leaves on that one keypress.
    ///
    /// Its own variant rather than a `Vec` on [`Self::Send`], because
    /// every other sender raises exactly one request and would have to
    /// wrap it. `crate::lookout::run_ui` hands the whole batch to a spawned task
    /// that awaits each `send` in turn on a cloned sender, so a channel
    /// deeper than the batch never stalls the screen and a full one never
    /// drops an entry; order survives because one task sends the whole
    /// batch in sequence. Only a closed channel can still fail a send, and
    /// [`Msg::BatchSent`] carries that casualty back.
    SendAll(Vec<Sent>),
    /// Leave.
    Quit,
    /// Read `shep.toml`'s settings snapshot; the result lands as
    /// [`Msg::Settings`]. Raised by the dashboard's `s`; once the screen is
    /// open, `s` closes it instead.
    LoadSettings,
    /// Apply one edit to `shep.toml`; the result lands as
    /// [`Msg::SettingWritten`].
    ///
    /// Must run on `spawn_blocking`: `ConfigLock::acquire` blocks with no
    /// deadline, and the UI task's redraw, tick and bus drain would block with
    /// the write.
    WriteSetting {
        /// The edit to apply.
        edit: SettingEdit,
        /// Which write this is, so its reply can only resolve the prompt it
        /// belongs to. Minted per send, so no two are ever equal.
        ticket: u64,
        /// Proof the control gate was open.
        authority: WriteAuthority,
    },
    /// Probe one dog for its config schema; the answer lands as
    /// [`Msg::DogPane`].
    ///
    /// A dog's schema is not persisted anywhere, so the pane asks at open:
    /// `shep adopt` uses the answer for the vet and records only the path.
    /// A built-in dog is this binary, so it is asked in-process
    /// (`crate::dog::builtin_schema`); an adopted one is spawned with the
    /// schema flag, which is why `crate::lookout::run_ui` runs this on
    /// `spawn_blocking`: it costs up to `VERSION_BUDGET` of somebody
    /// else's binary starting up, and the redraw task cannot wait for that.
    ///
    /// No [`WriteAuthority`]: this reads. The pane it opens is gated on
    /// every keystroke that writes, the same as a sheep pane.
    LoadDogPane {
        /// The dog.
        name: String,
        /// The adopted binary, or [`None`] for a built-in.
        adopted_path: Option<PathBuf>,
    },
    /// Apply one dog's file half; the result lands as [`Msg::DogWritten`].
    ///
    /// Its own effect, not [`Self::WriteSetting`]: it ends in a request to the
    /// shepherd ([`Sent::Dog`]) where a scalar write ends in a notice.
    /// `spawn_blocking`, for [`Self::WriteSetting`]'s reason.
    WriteDog {
        /// The toggle to apply.
        edit: DogEdit,
        /// Which write this is, for [`Self::WriteSetting`]'s reason. It
        /// rides on to [`Sent::Dog`], so both halves of one toggle answer
        /// under the same ticket.
        ticket: u64,
        /// Proof the control gate was open.
        authority: WriteAuthority,
    },
    /// Read the secret store, the provider cache and the roll; the result
    /// lands as [`Msg::Secrets`].
    ///
    /// Runs on `spawn_blocking` for [`Self::WriteSetting`]'s reason: the
    /// store's own lock acquires with no deadline.
    LoadSecrets,
    /// Read the one value behind `row`; the answer lands as
    /// [`Msg::Revealed`].
    ///
    /// Runs on `spawn_blocking` for [`Self::LoadSettings`]'s reason: the
    /// read takes no lock, and a synchronous file read on the UI task stalls
    /// the redraw, the tick and the bus drain together.
    ///
    /// No [`WriteAuthority`]: the gate on this one is
    /// `[secrets] allow_read`, checked when it is raised and again when the
    /// answer lands.
    RevealSecret {
        /// The operator store to read an
        /// [`Operator`](crate::lookout::secrets::Source::Operator) row from.
        store: PathBuf,
        /// The provider cache to read a namespaced row from.
        provider_cache: PathBuf,
        /// The row, which names the key, the store and the slot.
        row: SecretRow,
        /// The environment tab it is being read for, echoed back by
        /// [`Msg::Revealed`].
        environment: String,
    },
    /// Apply one change to `secrets.json`; the result lands as
    /// [`Msg::SecretWritten`].
    ///
    /// Runs on `spawn_blocking` for [`Self::WriteSetting`]'s reason: the
    /// store's lock acquires with no deadline, and the UI task's redraw,
    /// tick and bus drain would block with the write.
    ///
    /// The [`WriteAuthority`] is not decoration: this variant cannot be
    /// named without having passed the gate.
    WriteSecret(SecretEdit, WriteAuthority),
    /// Sends the carried value to the terminal's clipboard over OSC 52, by
    /// [`KeyPress::Copy`]. `crate::lookout::run_ui` writes it straight to the same
    /// stdout handle `crate::lookout::term` uses, never through `Terminal<B>` (a
    /// `TestBackend` has none) and never through `tracing`: the value must
    /// not reach a log.
    ///
    /// No [`WriteAuthority`]: this never touches `shep.toml`, and the value
    /// it carries is one [`App::update`] already read off `pane.reveal`,
    /// past the same `[secrets] allow_read` gate a reveal takes.
    CopyToClipboard(ClipboardValue),
}
