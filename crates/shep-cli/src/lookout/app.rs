//! The lookout's state and its reducer: `Msg` in, `Effect` out.
//!
//! No I/O, no terminal types, no clock. [`App::update`] is synchronous, work
//! for the outside comes back as an [`Effect`] the caller runs, and every
//! `Instant` arrives on a message.
//!
//! The bus is lossy, so [`Msg::Event`] upserts and [`Msg::Snapshot`] replaces
//! the whole flock map. The cursor is a [`RowKey`], not a row index: the map is
//! replaced wholesale every two seconds, and [`App::reseat`] puts the cursor
//! back on a real row.

use core::fmt;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use shep_client::RequestError;
use shep_core::config::LogLevel;
use shep_core::protocol::{
    BusEvent, DogSectionToml, DogSource, EnvValue, Lamb, ProcessEventKind, ProcessInfo, Request,
    Response, SelectorSpec, SheepConfigView, SheepRefusal,
};
use shep_core::secrets::ALL_ENVIRONMENTS;
use shep_core::status::ProcStatus;

use super::field::{FieldKind, FieldSet};
use super::level::Level;
use super::pane::{ConfigPane, FieldValue, Lock, PaneEdit, PaneRow, PaneTarget, ReloadKind};
use super::pane_bleats::BleatsPane;
use super::pane_sheep::SheepPane;
use super::secrets::{SecretRow, SecretsModel, Source};
use super::tail::Stream;
use super::theme::Palette;
use super::viewport::Viewport;
use crate::commands::settings::{SettingEdit, SettingField, SettingsSnapshot, settings_field_set};
use crate::style::{StyleLevel, StyleSource};
use crate::vocabulary::Reported;

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
/// `super::input::map_key` builds these at the edge, so this module never
/// touches a terminal crate.
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
    /// `h`: shows the selected field's own help text, in the config pane.
    /// Pressing it again, or `Escape`, dismisses it. Bound nowhere else.
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
    /// [`super::level::Level`] in ascending order, then back to `None`.
    /// Chosen over a design-named key because the status bar's own list
    /// names none for this axis; a global binding, ignored on the
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
        /// The last dial's own words, from [`super::source::LinkError`]'s
        /// `Display`. Carried rather than re-derived so the link panel
        /// quotes the failure instead of guessing at one.
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
        sample: Option<super::source::HostSample>,
    },
    /// One refresh of the selected sheep's log files, answering an
    /// [`Effect::RefreshFeed`]. Always yields [`Effect::None`].
    Bleats {
        /// What the read found, including what it could not show.
        tail: super::tail::Tail,
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
    /// armed action's Enter lands; `super::run_ui` sends it.
    Send(Sent),
    /// Send several requests, in the order given. Raised when a config
    /// pane closes: nothing it edited has gone out yet, so the whole set
    /// leaves on that one keypress.
    ///
    /// Its own variant rather than a `Vec` on [`Self::Send`], because
    /// every other sender raises exactly one request and would have to
    /// wrap it. `super::run_ui` hands the whole batch to a spawned task
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
    /// schema flag, which is why `super::run_ui` runs this on
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
        /// The operator store to read an [`crate::lookout::secrets::Source::Operator`] row from.
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
    /// [`KeyPress::Copy`]. `super::run_ui` writes it straight to the same
    /// stdout handle `super::term` uses, never through `Terminal<B>` (a
    /// `TestBackend` has none) and never through `tracing`: the value must
    /// not reach a log.
    ///
    /// No [`WriteAuthority`]: this never touches `shep.toml`, and the value
    /// it carries is one [`App::update`] already read off `pane.reveal`,
    /// past the same `[secrets] allow_read` gate a reveal takes.
    CopyToClipboard(ClipboardValue),
}

/// The connection's state, as the dashboard reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Connected and subscribed.
    Live,
    /// Re-dialling. `attempt` is 1-based and bounded by
    /// `super::link::RECONNECT_ATTEMPTS`.
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
/// same argument `super::field::Field`'s own derived `Debug` makes, and a
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
/// of them reporting its slot ([`App::is_grouped`]).
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
    id: u32,
    at: Instant,
    walk: LambWalk,
}

/// A short line the status bar shows instead of the key hints, cleared by the
/// next keypress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    text: String,
    /// True for a refusal or a damage report: the status bar picks
    /// [`Palette::refusal`] over [`Palette::attention`].
    grave: bool,
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
    snapshot: SettingsSnapshot,
    /// The six scalars' shape: which they are, in what order, under which
    /// section. The screen reads its rows, labels and section headers off
    /// this rather than off a `match` per question, so a config pane and
    /// this screen answer them the same way.
    fields: FieldSet,
    /// The cursor and, once a terminal has said how tall the body is, the
    /// scroll offset. Clamped on every read rather than kept pre-clamped: a
    /// refresh can shrink the dog list out from under a cursor already
    /// sitting past its new end.
    view: Viewport,
    /// The edit this screen is showing, or `None`. One field rather than
    /// several `Option`s, so typing, armed and sent cannot overlap on
    /// screen.
    ///
    /// Not the same as the one write in flight. [`Pending::Sent`] eats no
    /// key, so a second edit can be armed and sent over it, and closing the
    /// screen abandons it without cancelling anything: either leaves a
    /// write with no prompt waiting for it. [`Self::resolve`] is what keeps
    /// those answers off the edit that is here now.
    pending: Option<Pending>,
}

/// The settings screen's own in-flight edit.
#[derive(Debug, Clone)]
enum Pending {
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
    fn new(snapshot: SettingsSnapshot) -> Self {
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
    fn resolve(&mut self, ticket: u64) -> bool {
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
    fn is_armed(&self) -> bool {
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
    fn next_candidate(&self, field: SettingField) -> Option<String> {
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
    fn source_of(&self, field: SettingField) -> StyleSource {
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
    fn text_seed(&self, field: SettingField) -> &str {
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
    fn move_by(&mut self, delta: isize) {
        let len = self.rows().len();
        self.view.move_by(delta, len);
    }

    fn move_to_first(&mut self) {
        let len = self.rows().len();
        self.view.move_to(0, len);
    }

    fn move_to_last(&mut self) {
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
fn confirm_text(field: SettingField, value: &str, source: StyleSource) -> String {
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
fn confirm_text_for_edit(edit: &SettingEdit) -> String {
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
fn typed_text_of(edit: &SettingEdit) -> Option<(SettingField, String)> {
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

/// What an action key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionVerb {
    /// `x`. Stops the sheep; it stays registered.
    Stop,
    /// `R`, on shift because `r` is refresh.
    Restart,
    /// `L`, on shift for symmetry with `R`.
    Reload,
}

impl ActionVerb {
    /// The word the prompt and every outcome sentence begin with.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Reload => "reload",
        }
    }
}

/// Whether an action has been sent yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Armed, waiting for the operator's Enter. Nothing has gone out.
    Armed,
    /// Sent, waiting for the shepherd.
    Sent,
}

/// The one action this dashboard is in the middle of.
///
/// The target is captured at arm time and never re-read from the selection: a
/// snapshot can land between the keypress and the Enter.
///
/// One field on [`App`] rather than two `Option`s, so "armed" and "in flight"
/// cannot both be true.
#[derive(Debug, Clone)]
struct Action {
    verb: ActionVerb,
    target: RowKey,
    name: String,
    /// How many processes [`Self::target`] reaches, captured at arm time: 1 for
    /// a sheep, the group's own size for a [`RowKey::Group`], and the fold's
    /// membership for a [`RowKey::Fold`].
    count: usize,
    /// When it was armed. Only an armed action expires.
    at: Instant,
    stage: Stage,
}

/// The offer a pane makes on its way out when the running sheep has not
/// taken every field yet.
///
/// Nothing is at risk while it is up: a pane edit reaches the override
/// store on the keystroke that makes it, so leaving costs nothing and the
/// menu says so. What it buys is the operator not walking away from parked
/// config without knowing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneMenu {
    parked: usize,
    reload: ReloadKind,
    at: Instant,
}

impl PaneMenu {
    /// One, over `parked` fields and the reload `reload`.
    #[must_use]
    pub(super) const fn new(parked: usize, reload: ReloadKind, at: Instant) -> Self {
        Self { parked, reload, at }
    }

    /// When it opened. A menu that outlives `CONFIRM_EXPIRY` is dropped by
    /// the tick, so a later keypress cannot answer a question nobody is
    /// still looking at.
    #[must_use]
    pub const fn at(self) -> Instant {
        self.at
    }

    /// How many fields the running sheep has not taken yet.
    #[must_use]
    pub const fn parked(self) -> usize {
        self.parked
    }

    /// Which reload this sheep would get, so `L` can name its cost.
    #[must_use]
    pub const fn reload(self) -> ReloadKind {
        self.reload
    }
}

/// What the status bar needs to know about the action in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionState<'a> {
    /// Which verb.
    pub verb: ActionVerb,
    /// The pinned target.
    pub target: &'a RowKey,
    /// The pinned target's name, as it was when the key was pressed.
    pub name: &'a str,
    /// How many processes [`Self::target`] reaches.
    pub count: usize,
    /// False while it is a question, true once it has gone out.
    pub sent: bool,
}

/// How long an armed confirm waits for its Enter.
///
/// Ten seconds: a prompt left armed while the operator walks away is the same
/// fat finger by a slower route. Rides `Msg::Tick`, so it needs no timer.
pub const CONFIRM_EXPIRY: Duration = Duration::from_secs(10);

/// The sentence `r` and the action keys both give when the link is gone.
const LINK_GONE: &str = "the shepherd is gone: nothing left to ask";

/// The redial sentence, for the status bar and for a refused action key.
///
/// Both render on one frame, so they must agree exactly.
pub(super) fn retrying_sentence(attempt: u32) -> String {
    format!("the shepherd stopped answering: reconnecting (attempt {attempt})")
}

/// The sentence every closed-gate refusal gives, dashboard and settings alike.
const READ_ONLY_REFUSAL: &str = "read-only: from --read-only or lookout.allow_control";

/// `Enter`'s refusal on a provider row: pushed by a dog, so nothing here is
/// this operator's to set.
const PROVIDER_ROW_REFUSAL: &str = "read-only: pushed by a dog, not set here";

/// `D`'s refusal on a row whose value comes from the `all` slot while a
/// named environment's tab is open.
///
/// Refuses rather than retargets, both ways round. Unsetting the tab's own
/// environment would remove nothing and report success, and unsetting `all`
/// from here would change every environment at once, which is not what a row
/// reading `all` under `IN FORCE` invites anyone to expect.
const ALL_SLOT_REFUSAL: &str = "this value comes from the `all` slot: removing it \
     affects every environment, so switch to the `all` tab to mean it";

/// [`Msg::SecretWritten`]'s answer to an unset that found no slot, whether
/// the store changed under the pane or the row never resolved in this tab.
const NOTHING_REMOVED: &str = "nothing to remove: this key holds no value in this environment";

/// Whether deleting `row` from the tab named `environment` would have to
/// remove the `all` slot, which only the `all` tab may do.
fn deletes_the_all_slot(row: &SecretRow, environment: &str) -> bool {
    row.in_force.as_deref() == Some(ALL_ENVIRONMENTS) && environment != ALL_ENVIRONMENTS
}

/// The grammar a new key's name is checked against, matching `shep secret`'s
/// own `--help` wording (`cli.rs`) rather than a second copy of it.
const NEW_KEY_GRAMMAR: &str =
    "letters, digits, `.`, `_` and `-`, up to 128 bytes, not starting with a dot";

/// `y`'s notice on a successful copy.
///
/// Says the value was sent, never that it arrived: OSC 52 is write-only, the
/// terminal never replies, and many terminals refuse the sequence by
/// default, so no caller can confirm anything landed. Names the system
/// clipboard's own reach in the same breath, since sending a value there is
/// handing it to every process on the desktop, not just the terminal.
const COPY_SENT_NOTICE: &str = "sent to the terminal's clipboard over OSC 52 \u{b7} readable there by every process on the desktop";

/// How long a revealed value stays on screen.
///
/// The pane prints this number, so the two cannot drift.
pub(crate) const REVEAL_HOLDS: Duration = Duration::from_secs(10);

/// The widest window any pane draws, in samples.
///
/// The landing pane's sparkline needs ten. 1d's charts want six minutes,
/// which is 180 at the two-second poll, and 140 is what fits the frames'
/// 140-cell chart body. Sized for the charts now so the sheep pane
/// inherits a filled buffer rather than starting cold on a pane the
/// operator has just opened.
pub(crate) const HISTORY: usize = 140;

/// The lowest ceiling [`App::cpu_ceiling`] will report, in percent of one
/// core.
///
/// Without it, a flock idling at a tenth of a percent would have its own
/// jitter scaled to full height, so the busiest thing on screen would be
/// noise. Two percent is low enough that any real work clears it and high
/// enough that nothing else does.
///
/// `pub(crate)`, not private: `pane_sheep::scale_top`'s own floor argument
/// is this same number for the sheep pane's CPU chart, and a second
/// constant carrying the value would be the thing that drifts.
pub(crate) const CPU_CEILING_FLOOR: f32 = 2.0;

/// What occupies the body between the title band and the status bar.
///
/// A variant per pane rather than a shared "which pane is open" enum plus
/// separate state for each: the flock table needs no state of its own, and
/// the other two carry their screen's whole state directly, so there is
/// nowhere for a stale value to survive a switch. See [`App::body`]'s doc
/// comment for why this replaced two `Option` fields.
///
/// One field, one variant at a time, which is what used to be two
/// independent `Option`s (`settings`, `config_pane`) kept disjoint only by
/// convention. That made the pair's own history reachable: opening a config
/// pane never cleared a settings screen still parked underneath it, so an
/// `Escape` from a pane that outraced a slower settings read could resurface
/// a screen the operator had already walked past. With one field there is
/// nothing left underneath to resurface — see
/// `escape_from_a_config_pane_that_outraced_a_settings_read_lands_on_the_dashboard`.
#[derive(Debug)]
pub(crate) enum Body {
    /// The dashboard: flock table, host strip, sheep detail, bleats feed.
    FlockTable,
    /// The settings screen, opened by [`KeyPress::Settings`].
    Settings(Settings),
    /// An open config pane, for a sheep or a dog, opened by
    /// [`KeyPress::Edit`].
    ConfigPane(ConfigPane),
    /// The bleats feed given the whole screen, opened by
    /// [`KeyPress::Bleats`].
    Bleats(BleatsPane),
    /// The secrets pane, opened by [`KeyPress::Secrets`].
    Secrets(SecretsPane),
    /// One sheep given the whole screen, opened by [`KeyPress::Confirm`].
    Sheep(Box<SheepPane>),
}

/// The secrets pane's state.
///
/// `Debug` is manual (IR-41): [`Self::reveal`] and [`Self::typing`] carry
/// an operator's plaintext.
pub(crate) struct SecretsPane {
    /// Everything drawn, rebuilt by every load.
    pub model: Box<SecretsModel>,
    /// Which environment tab is showing, an index into
    /// [`SecretsModel::environments`].
    pub tab: usize,
    /// Which row the panels describe, an index into
    /// [`SecretsModel::rows`].
    pub selected: usize,
    /// Namespace groups `z` has collapsed, mirroring the flock table's own
    /// collapsed-fold set for this pane's rows.
    pub collapsed: HashSet<String>,
    /// The value on screen and when it leaves, or `None`.
    pub reveal: Option<Reveal>,
    /// The key an [`Effect::RevealSecret`] is reading for, or `None`. The
    /// answer is drawn only while this still names its key, so every
    /// trigger that clears a reveal also drops one in flight.
    pub pending_reveal: Option<String>,
    /// The armed delete, or `None`. While this is set, `Enter` confirms the
    /// delete rather than opening the value input.
    pub armed: Option<ArmedDelete>,
    /// The open text input, or `None`.
    pub typing: Option<Typing>,
}

/// A delete armed on the secrets pane: the key and when it armed, one value
/// rather than two so a caller cannot set one without the other, the same
/// pairing [`PanePending::Armed`] keeps for the config pane.
#[derive(Debug, Clone)]
pub(crate) struct ArmedDelete {
    /// The key waiting on `Enter` to confirm the delete.
    pub key: String,
    /// When it armed, for the expiry the tick runs.
    pub at: Instant,
}

impl SecretsPane {
    /// Takes the value off the screen, and abandons a read still in flight
    /// so its answer cannot put one back.
    ///
    /// One method rather than an assignment at each trigger: a trigger added
    /// later has one thing to call, and the ones that exist cannot drift
    /// apart.
    pub(crate) fn hide(&mut self) {
        self.reveal = None;
        self.pending_reveal = None;
    }

    /// The environment tab showing, or `None` before the first load.
    pub(crate) fn environment(&self) -> Option<&str> {
        self.model.environments.get(self.tab).map(String::as_str)
    }

    /// Whether `source`'s rows are folded away: only a provider namespace
    /// can be, mirroring `on_secrets_key`'s `Collapse` arm.
    ///
    /// `pub(crate)` so `view::secrets::draw` reads the same answer this
    /// pane's own cursor does, rather than a second copy of the match.
    pub(crate) fn is_collapsed(&self, source: &Source) -> bool {
        match source {
            Source::Operator => false,
            Source::Namespace(namespace) => self.collapsed.contains(namespace),
        }
    }

    /// Every index into `model.rows` this pane currently draws: a
    /// collapsed namespace's members contribute none, the same rows
    /// `view::secrets::draw` skips on screen. Never the `+ new key` row,
    /// which is not a `model.rows` index: [`Self::reveal_selected`] reads
    /// this to decide whether anything real is even on screen, so it stays
    /// real-rows-only rather than growing the affordance into it.
    fn visible_row_indices(&self) -> Vec<usize> {
        self.model
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !self.is_collapsed(&row.source))
            .map(|(index, _)| index)
            .collect()
    }

    /// Whether `selected` names the `+ new key` row rather than a real one:
    /// one past every index [`Self::visible_row_indices`] can ever hand
    /// back.
    pub(crate) fn selected_is_new_key_row(&self) -> bool {
        self.selected == self.model.rows.len()
    }

    /// The `model.rows` index the `+ new key` affordance sits after: the
    /// highest-index operator row, or `None` when the store holds none, in
    /// which case the affordance is first on screen instead.
    ///
    /// The single source of truth for where the affordance goes.
    /// [`Self::screen_slots`] (the cursor) and [`view::secrets::draw`] (the
    /// render) both derive their placement from this rather than each
    /// running its own scan, so the two cannot disagree about which line
    /// the affordance is on.
    pub(crate) fn new_key_anchor(&self) -> Option<usize> {
        self.model
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.source == Source::Operator)
            .map(|(index, _)| index)
            .max()
    }

    /// Every screen slot `j`/`k`/`g`/`G` can land the cursor on, in the
    /// order [`view::secrets::draw`] draws them: `None` is the `+ new key`
    /// affordance, placed right after [`Self::new_key_anchor`], or first on
    /// screen when there is none, matching `view::secrets::draw`'s own
    /// placement.
    ///
    /// Real rows never move relative to each other here, so a row hidden by
    /// a fold is simply absent, the same as [`Self::visible_row_indices`].
    fn screen_slots(&self) -> Vec<Option<usize>> {
        let anchor = self.new_key_anchor();
        let visible = self.visible_row_indices();
        let insert_at = match anchor {
            Some(anchor) => visible
                .iter()
                .position(|&index| index > anchor)
                .unwrap_or(visible.len()),
            None => 0,
        };
        let mut slots: Vec<Option<usize>> = visible.into_iter().map(Some).collect();
        slots.insert(insert_at, None);
        slots
    }

    /// Moves `selected` by `delta` positions over [`Self::screen_slots`],
    /// clamped rather than wrapping: the same rule the flock table and every
    /// other pane's cursor follows. A no-op with nothing on screen (there is
    /// always at least the affordance, so this is unreachable in practice).
    ///
    /// A selection `z` just folded away is not itself in [`Self::screen_slots`]:
    /// rather than guess where inside it the old position belonged, this
    /// lands on the nearest surviving *real* row in the direction `delta`
    /// points, over [`Self::visible_row_indices`] alone (never the
    /// affordance), so a reload or a fold can never strand the cursor on it
    /// by accident.
    pub(crate) fn move_by(&mut self, delta: isize) {
        let slots = self.screen_slots();
        if slots.is_empty() {
            return;
        }
        let current = (!self.selected_is_new_key_row()).then_some(self.selected);
        if let Some(position) = slots.iter().position(|&slot| slot == current) {
            let next = position.saturating_add_signed(delta).min(slots.len() - 1);
            self.selected = slots[next].unwrap_or(self.model.rows.len());
            return;
        }
        // The selection itself just went hidden (`z` folded its own group
        // away, or a reload's clamp landed on a row a standing fold hides):
        // land on the nearest visible neighbour in the direction requested,
        // rather than guessing a position inside a list the old selection
        // is not part of. `j`/`k` reach this arm with `delta` of 1 or -1;
        // the `Collapse` arm and the `Msg::Secrets` clamp call with `delta`
        // 0 to reuse the same landing logic without moving the selection
        // themselves.
        let visible = self.visible_row_indices();
        if visible.is_empty() {
            return;
        }
        let boundary = visible.partition_point(|&index| index < self.selected);
        self.selected = if delta < 0 {
            visible[boundary.saturating_sub(1).min(visible.len() - 1)]
        } else {
            visible[boundary.min(visible.len() - 1)]
        };
    }

    /// Jumps `selected` to the first screen slot, `g`'s effect: a real row
    /// unless the operator store holds none, in which case the affordance
    /// itself is first on screen.
    pub(crate) fn move_to_first(&mut self) {
        if let Some(&slot) = self.screen_slots().first() {
            self.selected = slot.unwrap_or(self.model.rows.len());
        }
    }

    /// Jumps `selected` to the last screen slot, `G`'s effect: the
    /// affordance itself when no namespace group follows the operator rows,
    /// otherwise the last namespace row, exactly what is last on screen.
    pub(crate) fn move_to_last(&mut self) {
        if let Some(&slot) = self.screen_slots().last() {
            self.selected = slot.unwrap_or(self.model.rows.len());
        }
    }
}

/// Redacted (IR-41): `reveal` and `typing` hold a plaintext value.
impl fmt::Debug for SecretsPane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretsPane")
            .field("rows", &self.model.rows.len())
            .field("tab", &self.tab)
            .field("selected", &self.selected)
            .field("collapsed", &self.collapsed.len())
            .field("revealing", &self.reveal.is_some())
            .field("pending_reveal", &self.pending_reveal)
            .field("armed", &self.armed.as_ref().map(|a| &a.key))
            .field("typing", &self.typing.is_some())
            .finish()
    }
}

/// A value on screen, and the instant it leaves.
pub(crate) struct Reveal {
    /// The key it belongs to.
    pub key: String,
    /// The plaintext.
    pub value: String,
    /// When it clears, [`REVEAL_HOLDS`] after the keypress.
    pub until: Instant,
}

/// Redacted (IR-41): `value` is the secret.
impl fmt::Debug for Reveal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reveal")
            .field("key", &self.key)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .finish_non_exhaustive()
    }
}

/// A plaintext value on its way from the store to the pane, in
/// [`Msg::Revealed`].
///
/// A type of its own rather than a `String` field: [`Msg`] derives `Debug`,
/// so the redaction has to travel with the value (IR-41).
#[derive(Clone)]
pub struct RevealedValue(pub(crate) String);

/// Redacted (IR-41): the field is the secret.
impl fmt::Debug for RevealedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RevealedValue(<{} bytes>)", self.0.len())
    }
}

/// A plaintext value on its way to the terminal's clipboard, in
/// [`Effect::CopyToClipboard`].
///
/// A type of its own rather than a bare `String`: [`Effect`] derives
/// `Debug`, so the redaction has to travel with the value, the same reason
/// [`RevealedValue`] exists (IR-41).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ClipboardValue(pub(crate) String);

/// Redacted (IR-41): the field is the secret.
impl fmt::Debug for ClipboardValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClipboardValue(<{} bytes>)", self.0.len())
    }
}

/// One change to the operator's store, carried by [`Effect::WriteSecret`].
///
/// `Debug` is manual (IR-41): `value` is the operator's plaintext.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SecretEdit {
    /// The key.
    pub key: String,
    /// Which environment's slot moves.
    pub environment: String,
    /// The new value, or `None` to remove the slot. Task 8's door: nothing
    /// in this task builds `None`.
    pub value: Option<String>,
}

/// Redacted (IR-41), matching `SecretCommand::Set`: a length, never a
/// value.
impl fmt::Debug for SecretEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.value.as_ref().map_or_else(
            || "None".to_string(),
            |v| format!("Some(<{} bytes>)", v.len()),
        );
        f.debug_struct("SecretEdit")
            .field("key", &self.key)
            .field("environment", &self.environment)
            .field("value", &format_args!("{value}"))
            .finish()
    }
}

/// An open text input in the secrets pane: the `+ new key` row's name step,
/// or a key's value step.
pub(crate) struct Typing {
    /// What is being typed: a new key's name, or a value for a key.
    pub what: TypingWhat,
    /// The buffer.
    pub buffer: String,
}

/// Redacted (IR-41): a value buffer is the secret being typed.
impl fmt::Debug for Typing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Typing")
            .field("what", &self.what)
            .field("buffer", &format_args!("<{} bytes>", self.buffer.len()))
            .finish()
    }
}

/// Which of the pane's two inputs is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypingWhat {
    /// The `+ new key` row's name input.
    NewKey,
    /// A value for the named key, in the current tab's environment.
    ValueFor(String),
}

/// Which screen asked for a sheep's config.
///
/// Both [`Sent::SheepConfig`]'s callers send the exact same request, and the
/// reply cannot tell them apart on its own: `e` pressed inside the sheep
/// pane leaves that pane on screen while the reply is in flight, so
/// [`App::on_sheep_config`] cannot route by the current [`Body`] the way it
/// could if only one screen ever asked. This is read instead, and it is set
/// in the same step as [`App::config_target`], by whichever door sent the
/// request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFor {
    /// The editing pane, opened by `e`.
    Editor,
    /// The sheep pane's read-only left column.
    SheepPane,
}

/// The whole dashboard's state.
#[derive(Debug)]
pub struct App {
    flock: BTreeMap<u32, Row>,
    /// Which row the detail pane and the bleats feed describe. `None` only for
    /// an empty flock.
    ///
    /// A [`RowKey`], not an index: the flock map is replaced wholesale every
    /// two seconds, and an index would silently start pointing at a different
    /// sheep. The viewport offset is derived from this
    /// ([`super::view::flock::scroll_offset`]).
    selected: Option<RowKey>,
    /// The live substring filter over sheep names, empty when there is none.
    ///
    /// Case-insensitive `contains`, taken literally with no trimming. Not the
    /// CLI's selector grammar, which is exact-match and so cannot narrow as you
    /// type.
    filter: String,
    /// Which keymap [`super::input::map_key`] is called with. Normal until `/`
    /// opens the box; the reducer, not the keymap, owns this state.
    mode: InputMode,
    /// The next write ticket, shared by the config pane's batches and the
    /// settings screen's one edit. Monotonic and never reused, so a reply
    /// can only name the write it belongs to.
    next_write_ticket: u64,
    link: Link,
    notice: Option<Notice>,
    palette: Palette,
    control: Control,
    /// The `$SHEP_HOME` this lookout watches, for the title line.
    home: String,
    /// The clock the view reads. Advanced by [`Msg::Tick`], and never once the
    /// link is [`Link::Lost`], so a frozen dashboard's uptime column stops.
    now: Instant,
    /// When the link was declared lost, on [`Self::now`]'s clock. `None`
    /// while it is still up.
    froze_at: Option<Instant>,
    /// How long ago that was, as of the last [`Msg::Tick`].
    ///
    /// The one number on a frozen dashboard that still moves. [`Self::now`]
    /// stops, so every uptime stops with it; this rides the tick separately,
    /// because how long the shepherd has been gone is a fact about now
    /// rather than a value the shepherd reported.
    frozen_for: Duration,
    /// The last host reading, or `None` before the first heartbeat and on a
    /// platform `sysinfo` does not support. [`Self::host_unsupported`] tells
    /// the strip which of the two it is looking at.
    host: Option<super::source::HostSample>,
    /// True once a sample has come back `None`, which the strip says a
    /// different sentence for than a heartbeat that has not fired yet.
    host_unsupported: bool,
    /// The selected sheep's most recent output, as of the last refresh. An
    /// empty, unlabelled tail before the first one, which the feed reads the
    /// same way as a sheep that has written nothing.
    feed: super::tail::Tail,
    /// The last lamb reading, or `None` before there has been one. Keyed by the
    /// id it was taken for, so a stale reading and a dropped request both read
    /// as "not read yet".
    lambs: Option<LambReading>,
    /// The one action this dashboard is in the middle of, or `None`.
    action: Option<Action>,
    /// What the body between the title band and the status bar is showing.
    ///
    /// One field rather than the two `Option`s this replaced: see [`Body`]'s
    /// doc for why.
    body: Body,
    /// Which sheep a config pane is open for, or wanted for.
    ///
    /// Set when the read goes out, cleared when the pane closes, checked
    /// when a reply lands. Without it, two `e` presses and an `Esc` could
    /// reopen a closed pane on the late reply. [`Self::config_pane`] alone
    /// cannot tell those two `None` states apart.
    config_target: Option<String>,
    /// Which screen [`Self::config_target`] is standing in for, while it is
    /// `Some`. `None` whenever `config_target` is: the two are set and
    /// cleared together, always in the same step that sends
    /// [`Sent::SheepConfig`]. See [`ConfigFor`].
    config_for: Option<ConfigFor>,
    /// The dog a config pane is open for, or wanted for, and the schema its
    /// binary answered with.
    ///
    /// [`Self::config_target`]'s twin. Carries a schema because it is
    /// probed once at open and reused on every re-read, so `r` never
    /// respawns the dog's binary. Cleared alongside `config_target`.
    dog_target: Option<DogProbe>,
    /// The apply offer over the open pane, or `None`.
    ///
    /// Opened by `Escape` on a pane with parked fields and the gate open,
    /// and it owns the keyboard while it is up. Cleared with the pane, so
    /// no menu can outlive the fields it counted.
    pane_menu: Option<PaneMenu>,
    /// The resolved style level and which layer chose it. Defaulted here and
    /// overridden through [`Self::set_style`], so the STYLE LEVEL row reads the
    /// same answer the rest of the CLI does.
    style: (StyleLevel, StyleSource),
    /// Each sheep's last [`HISTORY`] CPU-percent samples, oldest first,
    /// keyed by [`ProcessInfo::id`].
    ///
    /// Populated by [`Msg::Snapshot`] only: the bus's per-event
    /// [`Msg::Event`] carries no CPU reading, so a sample is one point per
    /// poll, not per bus message. A row missing from the latest snapshot
    /// loses its entry entirely, so a sheep that left the flock leaves no
    /// history behind for a later id to inherit.
    cpu_history: HashMap<u32, VecDeque<f32>>,
    /// The whole flock's summed CPU percent, one sample per poll, same
    /// depth and ordering as [`Self::cpu_history`].
    flock_cpu: VecDeque<f32>,
    /// Each sheep's last [`HISTORY`] RSS samples, oldest first, keyed by
    /// [`ProcessInfo::id`], on the same terms as [`Self::cpu_history`].
    ///
    /// Buffered as read rather than differenced: RSS is a reading at an
    /// instant, and only CPU arrives as a counter.
    rss_history: HashMap<u32, VecDeque<u64>>,
    /// The previous CPU counter and the instant it was read, per sheep.
    ///
    /// What makes a sample a mean over one poll rather than over the
    /// shepherd's own baseline window. Dropped when a sheep reports no
    /// reading, so a stop is never differenced across.
    cpu_last: HashMap<u32, (Option<u32>, u64, Instant)>,
    /// How [`Self::visible_rows`] gathers the flock table, toggled by `F`.
    grouping: Grouping,
    /// Fold names `z` has collapsed: [`Self::visible_rows`] skips a
    /// collapsed fold's members, leaving its own [`RowKey::Fold`] header
    /// behind.
    ///
    /// Names, not [`RowKey`]s: a fold has no id to key on, and a name
    /// surviving one poll to the next is exactly what keeps a collapse in
    /// place while the flock underneath it changes shape.
    collapsed_folds: HashSet<String>,
}

/// One flock entry as `visible_rows` sorts and partitions it: name, instance
/// slot, id, and whether it names a dog.
type RowEntry<'a> = (&'a str, Option<u32>, u32, bool);

/// Splits `entries` into contiguous runs sharing a name, in the order they
/// already sit in (name-sorted, so a run is always one unbroken slice).
/// [`App::push_fold_group_rows`] and [`App::push_grouped_rows`] walk the same
/// runs and then disagree on what to do with one, which is the two-levels
/// rule itself and stays out of this helper.
fn name_runs<'a>(entries: &'a [RowEntry<'a>]) -> impl Iterator<Item = &'a [RowEntry<'a>]> {
    let mut rest = entries;
    std::iter::from_fn(move || {
        let name = rest.first()?.0;
        let end = rest
            .iter()
            .position(|entry| entry.0 != name)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some(run)
    })
}

/// What a partly refused walk should say, or `None` when nothing was
/// refused.
///
/// Names the apps rather than counting them, because "2 refused" sends an
/// operator to the shepherd's log to find out which. One app's reason is
/// worth carrying; several would outrun the status bar, so past the first
/// the sentence names the apps alone.
fn refusal_sentence(refused: &[SheepRefusal]) -> Option<String> {
    match refused {
        [] => None,
        [only] => Some(format!("{} refused it: {}", only.name, only.reason)),
        many => {
            let names: Vec<&str> = many.iter().map(|one| one.name.as_str()).collect();
            Some(format!("{} refused it", names.join(", ")))
        }
    }
}

impl App {
    /// A dashboard with an empty flock, a live link, and no notice.
    #[must_use]
    pub fn new(palette: Palette, control: Control, home: String, now: Instant) -> Self {
        Self {
            flock: BTreeMap::new(),
            selected: None,
            filter: String::new(),
            mode: InputMode::Normal,
            next_write_ticket: 0,
            link: Link::Live,
            notice: None,
            palette,
            control,
            home,
            now,
            froze_at: None,
            frozen_for: Duration::ZERO,
            host: None,
            host_unsupported: false,
            feed: super::tail::Tail::default(),
            lambs: None,
            action: None,
            body: Body::FlockTable,
            config_target: None,
            config_for: None,
            dog_target: None,
            pane_menu: None,
            style: (StyleLevel::Full, StyleSource::Default),
            cpu_history: HashMap::new(),
            flock_cpu: VecDeque::new(),
            rss_history: HashMap::new(),
            cpu_last: HashMap::new(),
            grouping: Grouping::Flat,
            collapsed_folds: HashSet::new(),
        }
    }

    /// Applies one message and reports what the caller must do next.
    pub fn update(&mut self, msg: Msg) -> Effect {
        match msg {
            Msg::Snapshot { rows, at } => {
                // The link task has ended, so nothing is left to produce a
                // snapshot. Accepting one would un-freeze the dashboard.
                if matches!(self.link, Link::Lost { .. }) {
                    return Effect::None;
                }
                let previous = self.selected_index();
                self.flock = rows
                    .into_iter()
                    .map(|info| (info.id, Row { info, anchor: at }))
                    .collect();
                self.record_samples(at);
                self.reseat(previous);
                self.forget_missing_target();
                // Unconditional: the selected row's log paths can change even
                // when the selection does not, and this is the feed's cadence.
                Effect::RefreshFeed
            }
            Msg::Event(event) => self.on_event(event),
            Msg::BusLagged { count } => {
                self.notice = Some(Notice {
                    text: format!(
                        "lookout fell behind and lost {count} events; re-reading the flock"
                    ),
                    grave: false,
                });
                Effect::PollNow
            }
            Msg::Retrying { attempt } => {
                if !matches!(self.link, Link::Lost { .. }) {
                    self.link = Link::Retrying { attempt };
                    self.disarm_on_link_change();
                }
                Effect::None
            }
            Msg::Relinked => {
                if !matches!(self.link, Link::Lost { .. }) {
                    self.link = Link::Live;
                }
                Effect::None
            }
            Msg::Frozen { at_local, why } => {
                self.link = Link::Lost { at_local, why };
                self.froze_at = Some(self.now);
                self.frozen_for = Duration::ZERO;
                self.disarm_on_link_change();
                // Every notice is about a shepherd that no longer exists,
                // and none of them can be acted on. Left standing, the last
                // one outranks the key hint for the rest of the session
                // (`view::status::status_line`'s own ordering), so an
                // operator reads `the shepherd is shutting down` where the
                // bar should be naming the keys that still work. Whether the
                // shutdown was clean is on the screen either way: the link
                // panel quotes an error that says the socket was removed
                // rather than refusing.
                self.notice = None;
                Effect::None
            }
            Msg::Tick { now } => {
                if !matches!(self.link, Link::Lost { .. }) {
                    self.now = now;
                    let expired = self.action.as_ref().is_some_and(|action| {
                        action.stage == Stage::Armed
                            && now.saturating_duration_since(action.at) >= CONFIRM_EXPIRY
                    });
                    if expired {
                        self.action = None;
                    }
                    let stale = self.pane_menu.as_ref().is_some_and(|menu| {
                        now.saturating_duration_since(menu.at()) >= CONFIRM_EXPIRY
                    });
                    if stale {
                        self.pane_menu = None;
                    }
                }
                // The tick's own `now` again, for the same reason: how long
                // the shepherd has been gone is the one number a frozen
                // dashboard keeps counting.
                if let Some(at) = self.froze_at {
                    self.frozen_for = now.saturating_duration_since(at);
                }
                // Against the tick's own `now`, not `self.now`, which stops on
                // a dead link: a settings edit describes a local file that is
                // no staler for the shepherd being gone.
                if let Some(settings) = self.settings_mut() {
                    let expired = matches!(
                        settings.pending,
                        Some(Pending::Armed { at, .. } | Pending::DogArmed { at, .. })
                            if now.saturating_duration_since(at) >= CONFIRM_EXPIRY
                    );
                    if expired {
                        settings.pending = None;
                    }
                }
                // The config pane has no expiry of its own any more:
                // nothing on it is a question waiting for an answer. Its
                // edits sit until the operator writes them or undoes them,
                // and a set that timed out would throw work away silently.
                // Outside the link guard for the same reason, and one more:
                // `until` was set off `self.now`, which a dead link stops
                // advancing, so the tick's own `now` is what expires a
                // reveal at all once the link has gone.
                if let Some(pane) = self.secrets_pane_mut()
                    && pane
                        .reveal
                        .as_ref()
                        .is_some_and(|reveal| now >= reveal.until)
                {
                    pane.hide();
                }
                // Outside the link guard for the same reason as the reveal
                // above: an armed delete that can never be sent is no less
                // stale for the shepherd being gone. `now`, not `self.now`.
                if let Some(pane) = self.secrets_pane_mut()
                    && pane
                        .armed
                        .as_ref()
                        .is_some_and(|a| now.saturating_duration_since(a.at) >= CONFIRM_EXPIRY)
                {
                    pane.armed = None;
                }
                // Neither full-screen feed has a timer of its own; each
                // rides every tick instead of the dashboard's own cadence,
                // which is fixed for the connection's lifetime (see
                // `RefreshFeed`'s own doc). The dashboard raises nothing
                // here, or every lookout would poll twice as often for
                // nothing. `Link::Lost` too: `Msg::Bleats` throws the tail
                // away while the link is down, so every read would be work
                // done and discarded once a second. `Msg::Snapshot` and
                // `select_at` guard on the same thing, and
                // `a_frozen_dashboard_does_not_re_read_anything` states the
                // rule.
                if matches!(self.body, Body::Bleats(_) | Body::Sheep(_))
                    && !matches!(self.link, Link::Lost { .. })
                {
                    Effect::RefreshFeed
                } else {
                    Effect::None
                }
            }
            Msg::Resize => Effect::None,
            Msg::Key(key) => self.on_key(key),
            Msg::Host { sample } => {
                // A strip ticking over under a banner saying the values are
                // frozen contradicts it on one frame.
                if matches!(self.link, Link::Lost { .. }) {
                    return Effect::None;
                }
                self.host_unsupported = sample.is_none();
                self.host = sample;
                Effect::None
            }
            // Always `Effect::None`: answering a feed update with another
            // refresh would spin the UI task. The guard catches a read `run_ui`
            // armed before the freeze landed.
            Msg::Bleats { tail } => {
                if matches!(self.link, Link::Lost { .. }) {
                    return Effect::None;
                }
                self.feed = tail;
                Effect::None
            }
            Msg::Replied { sent, result } => match sent {
                Sent::Lambs { id } => self.on_lambs(id, result),
                Sent::Action { verb, target, name } => {
                    self.on_action_reply(verb, target, &name, result)
                }
                Sent::Dog {
                    name,
                    enable,
                    ticket,
                    ..
                } => self.on_dog_reply(name, enable, ticket, result),
                Sent::SheepConfig { name } => self.on_sheep_config(&name, result),
                Sent::DogSection { name } => self.on_dog_section(&name, result),
                Sent::SetDogSection { name, .. } => self.on_dog_section_set(&name, result),
                Sent::ApplyField {
                    name, key, value, ..
                } => self.on_field_applied(&name, &key, &value, result),
                Sent::SetEnv {
                    name, key, value, ..
                } => self.on_env_set(&name, &key, value.is_some(), result),
            },
            Msg::Unsent { sent } => match sent {
                Sent::Action { verb, target, name } => {
                    self.action = None;
                    self.notice = Some(Notice {
                        // No cause: `Full` is reachable while the shepherd is
                        // merely slow, so naming one would invent it.
                        text: format!("{}: it was not sent", target_prefix(verb, &target, &name)),
                        grave: true,
                    });
                    Effect::None
                }
                // A dropped lamb fetch already reads as "not read yet".
                Sent::Lambs { .. } => Effect::None,
                // A config read nobody took, reported rather than
                // swallowed: silence here looks like a key that is not
                // bound. Nothing was armed, so this is the whole report.
                Sent::SheepConfig { name } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: its config was not asked for"),
                        grave: true,
                    });
                    Effect::None
                }
                // Both write arms name the field, and neither reaches for
                // the pane: a close sends the whole set and leaves, so
                // every one of these lands with no pane on screen. The
                // notice is the whole report.
                Sent::ApplyField { name, key, .. } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: {key} was not sent"),
                        grave: true,
                    });
                    Effect::None
                }
                Sent::SetEnv { name, key, .. } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: env {key} was not sent"),
                        grave: true,
                    });
                    Effect::None
                }
                // The dog twins of the two arms above: a read nobody took
                // is reported, and so is a write nobody took.
                Sent::DogSection { name } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: its config was not asked for"),
                        grave: true,
                    });
                    Effect::None
                }
                Sent::SetDogSection { name, .. } => {
                    self.notice = Some(Notice {
                        text: format!("{name}: its config was not sent"),
                        grave: true,
                    });
                    Effect::None
                }
                // The arm above, against the settings screen's pending line.
                Sent::Dog {
                    name,
                    enable,
                    ticket,
                    ..
                } => {
                    if let Some(settings) = self.settings_mut() {
                        settings.resolve(ticket);
                    }
                    let verb = if enable { "enable" } else { "disable" };
                    self.notice = Some(Notice {
                        text: format!("{verb} {name}: it was not sent"),
                        grave: true,
                    });
                    Effect::None
                }
            },
            // Delegates to `Msg::Unsent`'s own match rather than repeating
            // it: every arm there already reports the right notice for the
            // `Sent` it carries, and returns `Effect::None`.
            Msg::BatchSent { unsent } => match unsent {
                Some(sent) => self.update(Msg::Unsent { sent }),
                None => Effect::None,
            },
            // The screen opens on what this read found; a failed read leaves
            // the dashboard up. A landed write's re-read and `r` land here too,
            // with `body` already `Body::Settings`, so `opening` is false and
            // the cursor survives.
            Msg::Settings { result } => {
                let opening = self.settings().is_none();
                match result {
                    // A config pane opened while this read was in flight, so
                    // the operator asked for the pane AFTER asking for
                    // settings and this reply is the stale one. Adopting it
                    // would replace the pane with a settings screen the
                    // operator has moved on from, and leave `config_target`
                    // and `pane_menu` describing a screen that is no longer
                    // up. The `Body` enum stops the two coexisting; it does
                    // not stop this handler overwriting one with the other,
                    // which is the same race `on_sheep_config` had in the
                    // opposite direction.
                    Ok(_) if self.config_pane().is_some() => {}
                    Ok(snapshot) => {
                        // An action armed while the read was in flight: once
                        // the screen is up, `on_settings_key` no-ops `Confirm`
                        // and the prompt would be unreachable.
                        self.action = None;
                        // A filter box left open would keep eating every
                        // keystroke the settings keymap owns: `on_key` checks
                        // the mode first. The query itself is kept.
                        self.mode = InputMode::Normal;
                        // Reset to the top only while `opening`.
                        // `Settings::cursor` clamps on every read, so a
                        // preserved `Viewport` past a shorter dogs list
                        // still lands somewhere real.
                        let view = self.settings().map(|settings| settings.view.clone());
                        let mut settings = Settings::new(snapshot);
                        if !opening {
                            if let Some(view) = view {
                                settings.view = view;
                            }
                            let len = settings.rows().len();
                            settings.view.clamp(len);
                        }
                        self.body = Body::Settings(settings);
                    }
                    Err(message) => {
                        self.notice = Some(Notice {
                            text: message,
                            grave: true,
                        });
                    }
                }
                Effect::None
            }
            // `Ok` re-reads rather than folding the write into the row, which
            // covers `Unset` too. `Err` reopens the editor for the two
            // free-text fields, so a long path need not be retyped.
            //
            // Both arms act on the screen only when this reply is the one it
            // is waiting on. `Settings::resolve` answers that; a reply it
            // refuses still reports itself and still re-reads, but leaves
            // whatever is on screen alone. A dog section can also have
            // replaced the settings screen with a config pane while the
            // write was in flight, which `resolve` refuses for the same
            // reason: `InputMode::Text` with no editor behind it sends every
            // later keystroke to a text handler that owns nothing.
            Msg::SettingWritten {
                edit,
                ticket,
                result,
            } => {
                let mine = self
                    .settings_mut()
                    .is_some_and(|settings| settings.resolve(ticket));
                match result {
                    Ok(()) => self.reread_settings(),
                    Err(message) => {
                        // Split so no borrow of `self.body` is held across
                        // the `self.notice` assignment below.
                        if mine && let Some((field, buffer)) = typed_text_of(&edit) {
                            if let Some(settings) = self.settings_mut() {
                                settings.pending = Some(Pending::Typing { field, buffer });
                            }
                            self.mode = InputMode::Text;
                        }
                        self.notice = Some(Notice {
                            text: message,
                            grave: true,
                        });
                        Effect::None
                    }
                }
            }
            // A dog's schema probe answered. `Ok` parks the schema and asks
            // the shepherd for the section; the pane is built once that
            // lands. `Err` gets no pane, and the refusal names the file to
            // edit instead. The settings screen stays open until then.
            Msg::DogPane {
                name,
                adopted_path,
                result,
            } => match result {
                Ok(schema) => {
                    self.dog_target = Some(DogProbe {
                        name: name.clone(),
                        adopted_path,
                        schema,
                    });
                    Effect::Send(Sent::DogSection { name })
                }
                Err(message) => {
                    self.notice = Some(Notice {
                        text: message,
                        grave: true,
                    });
                    Effect::None
                }
            },
            // `Ok` raises the daemon half: `Cycle` arms, `Confirm` writes the
            // file, this arm asks the shepherd. `Err` never reaches it, since
            // there is nothing for the daemon half to agree with.
            //
            // `Ok` asks the shepherd whether or not the screen is still
            // waiting on this ticket: the file already says the dog is on or
            // off, so a daemon half dropped here would leave the two
            // disagreeing. `Err` clears only the prompt this ticket raised.
            Msg::DogWritten {
                edit,
                ticket,
                result,
            } => match result {
                Ok(source) => Effect::Send(Sent::Dog {
                    name: edit.name,
                    enable: edit.enable,
                    source,
                    ticket,
                }),
                Err(message) => {
                    if let Some(settings) = self.settings_mut() {
                        settings.resolve(ticket);
                    }
                    self.notice = Some(Notice {
                        text: message,
                        grave: true,
                    });
                    Effect::None
                }
            },
            // Dropped when the pane has since closed, the way a settings
            // read that outraced a config pane is: adopting it would reopen
            // a screen the operator has already left.
            Msg::Secrets {
                environment,
                result,
            } => {
                if let Body::Secrets(pane) = &mut self.body {
                    match result {
                        Ok(model) => {
                            // A fresh read describes the store as it is now;
                            // an arm from before it landed named a row this
                            // model may no longer even have.
                            pane.armed = None;
                            // Only on the very first load, where the pane's
                            // model is still the empty default and so has no
                            // tab yet: the daemon's own default environment
                            // wins the tab it lands on.
                            let first_load = pane.model.environments.is_empty();
                            if first_load {
                                pane.tab = model
                                    .environments
                                    .iter()
                                    .position(|candidate| candidate == &environment)
                                    .unwrap_or(0);
                            }
                            // The environments list is a union recomputed on
                            // every load and can shrink, so clamp a tab past
                            // the new end onto the last surviving one
                            // instead of dangling.
                            pane.tab = pane.tab.min(model.environments.len().saturating_sub(1));
                            // A selection on the trailing `+ new key`
                            // sentinel stays on it under the fresh model's
                            // own row count, rather than the ordinary clamp
                            // below, which would otherwise land it on the
                            // new model's last real row. Excludes the first
                            // load, whose own empty model reads `selected`
                            // (`0`) as that same sentinel by coincidence,
                            // having no rows yet either.
                            pane.selected = if !first_load && pane.selected_is_new_key_row() {
                                model.rows.len()
                            } else {
                                pane.selected.min(model.rows.len().saturating_sub(1))
                            };
                            pane.model = model;
                            // The row count clamp above says nothing about
                            // collapse state, and `collapsed` survives a
                            // reload: the surviving index can still name a
                            // row a still-folded namespace hides. Same
                            // fallback the `Collapse` arm uses.
                            if pane
                                .model
                                .rows
                                .get(pane.selected)
                                .is_some_and(|row| pane.is_collapsed(&row.source))
                            {
                                pane.move_by(0);
                            }
                        }
                        Err(message) => {
                            self.notice = Some(Notice {
                                text: message,
                                grave: true,
                            });
                        }
                    }
                }
                Effect::None
            }
            Msg::Revealed {
                key,
                environment,
                value,
            } => {
                self.on_revealed(&key, &environment, value);
                Effect::None
            }
            // `Ok` re-reads (like `Msg::SettingWritten`) so the table shows
            // the file's new contents rather than what was typed, and clears
            // any revealed value, which belonged to the prior store. `Err`
            // raises no reload, so the table keeps its last known-good read.
            Msg::SecretWritten { result } => match result {
                Ok(true) => {
                    self.hide_revealed();
                    Effect::LoadSecrets
                }
                // `secrets::unset` found no slot. Nothing changed, so
                // nothing is re-read, and the operator hears about it: a
                // destructive action reporting success over a no-op is the
                // one answer this pane must never give.
                Ok(false) => {
                    self.notice = Some(Notice {
                        text: NOTHING_REMOVED.to_string(),
                        grave: true,
                    });
                    Effect::None
                }
                Err(message) => {
                    self.notice = Some(Notice {
                        text: message,
                        grave: true,
                    });
                    Effect::None
                }
            },
        }
    }

    /// Records one lamb reading. Always [`Effect::None`]: a reducer that
    /// answered a reading with another request would spin the UI task.
    fn on_lambs(&mut self, id: u32, result: Result<Response, RequestError>) -> Effect {
        // Armed before the freeze could land: a reading reaching the frame now
        // would be newer than the banner over it.
        if matches!(self.link, Link::Lost { .. }) {
            return Effect::None;
        }
        let walk = match result {
            Ok(Response::Described(rows)) => rows
                .into_iter()
                .find(|info| info.id == id)
                .map_or(LambWalk::Failed, |info| {
                    info.lambs.map_or(LambWalk::NotWalked, LambWalk::Walked)
                }),
            // Neither an `Err` nor an unrecognised reply is an empty walk:
            // reporting one would say "none found" about nothing read.
            _ => LambWalk::Failed,
        };
        self.lambs = Some(LambReading {
            id,
            at: self.now,
            walk,
        });
        Effect::None
    }

    /// One action's answer: the shepherd's rows upserted, and one sentence.
    /// Nothing provisional is invented; all three replies carry the rows.
    fn on_action_reply(
        &mut self,
        verb: ActionVerb,
        target: RowKey,
        name: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        self.action = None;
        let prefix = target_prefix(verb, &target, name);
        // Each verb accepts its own reply and no other: a `Stopped` answering
        // a `Restart` carries rows and would upsert happily.
        let mut refusal: Option<String> = None;
        let rows = match result {
            Ok(Response::Stopped(rows)) if verb == ActionVerb::Stop => rows,
            // `SelectorSpec::Fold` names every app in a fold, so this is the
            // multi-app walk that fills `refused`. Dropping it would tell an
            // operator the whole fold restarted when some of it did not.
            Ok(Response::Restarted { accepted, refused }) if verb == ActionVerb::Restart => {
                refusal = refusal_sentence(&refused);
                accepted
            }
            Ok(Response::Reloading { accepted, refused }) if verb == ActionVerb::Reload => {
                refusal = refusal_sentence(&refused);
                accepted
            }
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{prefix}: the shepherd answered something this lookout does not understand"
                    ),
                    grave: true,
                });
                return Effect::None;
            }
            // The daemon's own message: `RequestError`'s `Display` would put a
            // Rust identifier on an operator's screen.
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: {}", err.message),
                    grave: true,
                });
                return Effect::None;
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: {other}"),
                    grave: true,
                });
                return Effect::None;
            }
        };
        let anchor = self.now;
        let was_empty = self.flock.is_empty();
        for info in rows {
            self.flock.insert(info.id, Row { info, anchor });
        }
        // A partial walk is not a success sentence. `grave` follows, so a
        // fold that half refused reads as a problem rather than as a done
        // thing an operator scrolls past.
        self.notice = Some(match &refusal {
            Some(sentence) => Notice {
                text: format!("{prefix}: {}, but {sentence}", outcome(verb)),
                grave: true,
            },
            None => Notice {
                text: format!("{prefix}: {}", outcome(verb)),
                grave: false,
            },
        });
        if was_empty && self.reseat(None) {
            return Effect::RefreshSelected;
        }
        Effect::None
    }

    /// One dog toggle's answer: the `Pending::Sent` line this ticket raised
    /// clears, one sentence lands in the status bar, and the screen re-reads
    /// `shep.toml`.
    ///
    /// The sentence lands whether or not the screen is still waiting on this
    /// ticket, since the operator asked for this toggle either way. Only the
    /// prompt is `ticket`'s to clear, and only [`Self::reread_settings`]
    /// decides whether the re-read runs: the file half has already landed, so
    /// `DogView.enabled` is stale whatever the shepherd said. No row is
    /// upserted; the next `ListFlock` repairs RUNNING.
    fn on_dog_reply(
        &mut self,
        name: String,
        enable: bool,
        ticket: u64,
        result: Result<Response, RequestError>,
    ) -> Effect {
        if let Some(settings) = self.settings_mut() {
            settings.resolve(ticket);
        }
        let verb = if enable { "enable" } else { "disable" };
        let prefix = format!("{verb} {name}");
        // `EnableDog` answers `Response::DogStarted`; `DisableDog` answers
        // `Response::Deleted`, the same reply `Delete` gives.
        match result {
            Ok(Response::DogStarted(_)) if enable => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: the shepherd started it"),
                    grave: false,
                });
            }
            Ok(Response::Deleted(_)) if !enable => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: the shepherd stopped and deregistered it"),
                    grave: false,
                });
            }
            // Also a mismatched guard above: an `EnableDog` answered by
            // `Response::Deleted`, or the reverse.
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{prefix}: the shepherd answered something this lookout does not understand"
                    ),
                    grave: true,
                });
            }
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: {}", err.message),
                    grave: true,
                });
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: {other}"),
                    grave: true,
                });
            }
        }
        self.reread_settings()
    }

    /// What a landed settings write answers with: re-read `shep.toml`, unless
    /// the screen has an edit of its own in flight.
    ///
    /// [`Msg::Settings`] rebuilds the whole screen, so a re-read raised by a
    /// reply the screen was no longer waiting on would throw away the edit it
    /// is waiting on instead. That edit's own reply re-reads a moment later,
    /// and carries both writes' work with it.
    fn reread_settings(&self) -> Effect {
        match self.settings() {
            Some(settings) if settings.pending.is_some() => Effect::None,
            _ => Effect::LoadSettings,
        }
    }

    /// One `Request::SheepConfig` reply. Opens the pane, or refreshes an
    /// open one in place.
    ///
    /// The cursor and offset are carried across a refresh rather than reset
    /// to the first field, the same rule `Msg::Settings` follows and for the
    /// same reason: `r` from inside the pane, and every later re-read, must
    /// not throw an operator who was reading `cron_restart` back to `name`.
    /// [`ConfigPane::adopt_view`] clamps what it adopts, so a field list
    /// that came back shorter cannot leave the cursor past its end.
    ///
    /// A failed read leaves whatever is on screen exactly as it was and
    /// raises a grave notice, so a refusal is reported rather than
    /// swallowed, and a refresh that fails does not blank a pane that was
    /// showing something real.
    fn on_sheep_config(&mut self, name: &str, result: Result<Response, RequestError>) -> Effect {
        // A reply nobody is waiting for any more is dropped in silence:
        // nothing went wrong, and the operator asked for nothing that is
        // still outstanding.
        if self.config_target.as_deref() != Some(name) {
            return Effect::None;
        }
        match result {
            Ok(Response::SheepConfig(view)) => match self.config_for {
                Some(ConfigFor::SheepPane) => {
                    if let Some(pane) = self.sheep_pane_mut() {
                        pane.adopt_config(*view);
                    }
                }
                // `None` only if something outside this file set
                // `config_target` without `config_for`, which nothing does:
                // [`Self::ask_for_sheep_config`] is the one place both are
                // set, always together. Falling back to the editor is the
                // pre-[`ConfigFor`] behaviour, not a guess this reply
                // belongs to a screen that never asked for it.
                None | Some(ConfigFor::Editor) => self.open_or_refresh_config_pane(*view),
            },
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{name}: the shepherd answered something this lookout does not understand"
                    ),
                    grave: true,
                });
            }
            // Prefixed, like every other reply handler in this file: the
            // shepherd's refusals mostly name their own subject, but
            // `EngineStopped` renders as `the supervisor engine has
            // stopped`, naming neither the sheep nor the screen it came
            // from.
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {}", err.message),
                    grave: true,
                });
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {other}"),
                    grave: true,
                });
            }
        }
        Effect::None
    }

    /// Opens the config pane on `view`, or refreshes one already open in
    /// place, carrying across everything a rebuild would otherwise drop.
    /// Split out of [`Self::on_sheep_config`] so that method can route a
    /// [`ConfigFor::SheepPane`] reply to [`Self::sheep_pane_mut`] instead
    /// without repeating its guard or its error arms.
    fn open_or_refresh_config_pane(&mut self, view: SheepConfigView) {
        let carried = self.config_pane().map(|pane| pane.view().clone());
        // An env row's own cursor is carried by key, not index: a
        // set re-reads the whole config, and a removal shortens the
        // list, so an index that survived would name a different
        // key. See `ConfigPane::cursor_env_key`.
        let carried_env_key = self.config_pane().and_then(ConfigPane::cursor_env_key);
        // The list sub-screen rides across for the same reason,
        // and by index rather than by name: an element has no
        // name. See `ListPane::adopt_view`.
        let carried_list = self
            .config_pane()
            .and_then(ConfigPane::list)
            .map(|list| (list.key().to_owned(), list.view().clone()));
        // The pending set survives the rebuild: the values are
        // the shepherd's and the edits are the operator's. An open
        // editor is dropped. See `ConfigPane::adopt_edits`.
        let carried_edits = self
            .config_pane()
            .map(|pane| pane.edits().clone())
            .unwrap_or_default();
        // Carried for the same reason as the cursor: a re-read must not
        // dismiss a help note the operator has not dismissed.
        let carried_help = self.config_pane().is_some_and(ConfigPane::help_open);
        let mut pane = ConfigPane::sheep(view);
        pane.adopt_edits(carried_edits);
        if let Some(carried) = carried {
            pane.adopt_view(carried);
        }
        if let Some(env_key) = carried_env_key {
            pane.adopt_env_cursor(env_key.as_deref());
        }
        if let Some((key, carried)) = carried_list {
            pane.adopt_list_view(&key, carried);
        }
        pane.set_help_open(carried_help);
        self.body = Body::ConfigPane(pane);
        // The rebuilt pane carries no editor, so the keyboard must not
        // still think one is open.
        self.release_text_mode_if_unowned();
    }

    fn on_event(&mut self, event: BusEvent) -> Effect {
        match event {
            BusEvent::Process { event, info, .. } => {
                if matches!(event, ProcessEventKind::Delete) {
                    let previous = self.selected_index();
                    self.flock.remove(&info.id);
                    self.forget_missing_target();
                    return if self.reseat(previous) {
                        Effect::RefreshSelected
                    } else {
                        Effect::None
                    };
                }
                // An upsert can orphan the selection from `visible_rows()`
                // without touching `self.flock`: a rename can move the selected
                // row out of the filter. `reseat` is a no-op read while the
                // selection is still seated.
                let previous = self.selected_index();
                let anchor = self.now;
                self.flock.insert(info.id, Row { info, anchor });
                if self.reseat(previous) {
                    return Effect::RefreshSelected;
                }
                Effect::None
            }
            // The shepherd's own outbound queue overflowed. Worded differently
            // from `Msg::BusLagged`: an operator cannot tell which end of the
            // connection to investigate if the two read the same.
            BusEvent::Dropped { count } => {
                self.notice = Some(Notice {
                    text: format!("the shepherd dropped {count} events; re-reading the flock"),
                    grave: false,
                });
                Effect::PollNow
            }
            // A notice, not an exit: a dashboard that vanished would take the
            // last known state with it.
            BusEvent::DaemonShutdown => {
                self.notice = Some(Notice {
                    text: "the shepherd is shutting down".to_string(),
                    grave: true,
                });
                Effect::None
            }
            // `BusEvent` is `#[non_exhaustive]`: a newer shepherd's variant
            // must not take the dashboard down.
            _ => Effect::None,
        }
    }

    /// One `Request::DogConfig` reply: the dog's section, which is the
    /// second half of an open.
    ///
    /// Guarded on [`Self::dog_target`] the way [`Self::on_sheep_config`] is
    /// guarded on `config_target`, and for the same reason: a reply for a
    /// dog the operator has already left must not re-open a pane over the
    /// screen they went back to.
    ///
    /// The schema comes off `dog_target` rather than off the wire. Nothing
    /// records a dog's schema, so the probe that ran at open is the only
    /// copy, and a refresh reuses it rather than respawning the binary.
    fn on_dog_section(&mut self, name: &str, result: Result<Response, RequestError>) -> Effect {
        let Some(probe) = self.dog_target.clone().filter(|probe| probe.name == name) else {
            return Effect::None;
        };
        match result {
            Ok(Response::DogSection { toml }) => {
                // Everything a refresh has to carry across, read before the
                // rebuild replaces the pane: see `Self::on_sheep_config`,
                // which states the argument for each. A dog pane has no env
                // sub-screen, so only two of the three apply.
                let carried = self.config_pane().map(|pane| pane.view().clone());
                let carried_edits = self
                    .config_pane()
                    .map(|pane| pane.edits().clone())
                    .unwrap_or_default();
                let carried_help = self.config_pane().is_some_and(ConfigPane::help_open);
                let mut pane = ConfigPane::dog(
                    probe.name,
                    probe.adopted_path,
                    probe.schema,
                    toml.as_str().to_owned(),
                );
                pane.adopt_edits(carried_edits);
                if let Some(carried) = carried {
                    pane.adopt_view(carried);
                }
                pane.set_help_open(carried_help);
                // The settings screen is what a dog pane opens over, and this
                // one assignment is what closes it: `Body` holds one
                // variant, so the pane replaces it once there is something
                // to look at.
                self.body = Body::ConfigPane(pane);
                self.release_text_mode_if_unowned();
            }
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{name}: the shepherd answered something this lookout does not understand"
                    ),
                    grave: true,
                });
            }
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {}", err.message),
                    grave: true,
                });
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {other}"),
                    grave: true,
                });
            }
        }
        Effect::None
    }

    /// One `Request::SetDogConfig` reply.
    ///
    /// Reaches for no pane: a write goes out when the pane closes, so
    /// this lands with the dashboard on screen. A success still re-reads
    /// the section, the same shape [`Self::on_field_applied`] has, and
    /// [`Self::on_dog_section`] drops the answer when nobody is waiting
    /// for it.
    ///
    /// The sentence says the change was published and stops there.
    /// Whether the dog acted on it is the dog's own answer, which this
    /// reply does not carry and shep cannot predict.
    fn on_dog_section_set(&mut self, name: &str, result: Result<Response, RequestError>) -> Effect {
        match result {
            Ok(Response::DogConfigSet { .. }) => {
                self.notice = Some(Notice {
                    text: format!("{name}: its config is written, and {name} is told"),
                    grave: false,
                });
                Effect::Send(Sent::DogSection {
                    name: name.to_owned(),
                })
            }
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{name}: the shepherd answered something this lookout does not understand"
                    ),
                    grave: true,
                });
                Effect::None
            }
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {}", err.message),
                    grave: true,
                });
                Effect::None
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {other}"),
                    grave: true,
                });
                Effect::None
            }
        }
    }

    /// One `Request::SetSheepField` reply.
    ///
    /// Reaches for no pane: the write went out on the keypress that closed
    /// it, so this lands with the dashboard on screen. A success still
    /// re-reads the config, using `Effect::Send` so the re-read gets
    /// `Msg::Unsent` handling for free, and [`Self::on_sheep_config`]
    /// drops the answer when nobody is waiting for it.
    ///
    /// Every sentence names the field, refusals included. A close sends
    /// the whole set at once, so several of these can land in a row and a
    /// refusal that named only the sheep would not say which write failed.
    ///
    /// The success sentence also names the new value when
    /// [`FieldValue::safe_summary`] says that is safe: without it, setting
    /// `reuse_port` to `true` and setting it back to `false` print the
    /// identical sentence, which is wrong on its own rather than merely
    /// incomplete.
    ///
    /// `pending` is the shepherd's answer, not this pane's guess: it knows
    /// about fields like `autostart` that `apply_group` cannot derive.
    fn on_field_applied(
        &mut self,
        name: &str,
        key: &str,
        value: &FieldValue,
        result: Result<Response, RequestError>,
    ) -> Effect {
        match result {
            Ok(Response::SheepFieldSet { pending, .. }) => {
                let key_text = match value.safe_summary() {
                    Some(v) => format!("{key} set to {v}"),
                    None => format!("{key} is set"),
                };
                self.notice = Some(Notice {
                    text: if pending {
                        format!("{name}: {key_text}, and waits for `shep reload {name}`")
                    } else {
                        format!("{name}: {key_text}")
                    },
                    grave: false,
                });
                Effect::Send(Sent::SheepConfig {
                    name: name.to_owned(),
                })
            }
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{name}: {key}: the shepherd answered something this lookout does not \
                         understand"
                    ),
                    grave: true,
                });
                Effect::None
            }
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {key}: {}", err.message),
                    grave: true,
                });
                Effect::None
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{name}: {key}: {other}"),
                    grave: true,
                });
                Effect::None
            }
        }
    }

    /// One `Request::SetSheepEnv` reply.
    ///
    /// `was_set` comes off the request rather than the reply: the answer
    /// names the key and deliberately never the value, so it cannot say
    /// which of the two things happened.
    ///
    /// Env is spawn-time in every case (`AppConfig::env` is
    /// `ApplyGroup::NeedsRespawn`), so the sentence says so unconditionally
    /// rather than reading a list that this reply does not carry.
    fn on_env_set(
        &mut self,
        name: &str,
        key: &str,
        was_set: bool,
        result: Result<Response, RequestError>,
    ) -> Effect {
        match result {
            Ok(Response::SheepEnvSet { .. }) => {
                let verb = if was_set { "set" } else { "removed" };
                self.notice = Some(Notice {
                    text: format!("{name}: env {key} {verb}, and waits for `shep reload {name}`"),
                    grave: false,
                });
                Effect::Send(Sent::SheepConfig {
                    name: name.to_owned(),
                })
            }
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{name}: env {key}: the shepherd answered something this lookout does not \
                         understand"
                    ),
                    grave: true,
                });
                Effect::None
            }
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{name}: env {key}: {}", err.message),
                    grave: true,
                });
                Effect::None
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{name}: env {key}: {other}"),
                    grave: true,
                });
                Effect::None
            }
        }
    }

    fn on_key(&mut self, key: KeyPress) -> Effect {
        // While the box is open every key is text.
        if self.mode == InputMode::Text {
            return self.on_text_key(key);
        }
        // The config pane owns the keyboard while it is open, ahead of the
        // settings screen and the armed-confirm check below. The two
        // screens cannot coexist, so this ordering is a documentation
        // choice, not a correctness one.
        if self.config_pane().is_some() {
            return self.on_pane_key(key);
        }
        // The settings screen owns its own keymap while it is open.
        if self.settings().is_some() {
            return self.on_settings_key(key);
        }
        // The bleats pane owns the keyboard while it is open, the same as
        // the other two full-screen panes above.
        if self.bleats_pane().is_some() {
            return self.on_bleats_key(key);
        }
        // The secrets pane, the same as the three panes above.
        if matches!(self.body, Body::Secrets(_)) {
            return self.on_secrets_key(key);
        }
        // The sheep pane owns the keyboard while it is open, the same as
        // the three full-screen panes above. `Body` holds only one at a
        // time, so this ordering is documentation, not correctness, the
        // same as theirs.
        if self.sheep_pane().is_some() {
            return self.on_sheep_pane_key(key);
        }
        // A cancelling keypress is consumed: a stray `j` cancels the confirm
        // and does not also move the selection, or the next reflexive Enter
        // acts on a target the operator lost track of. Cancelling is silent.
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.stage == Stage::Armed)
        {
            if key == KeyPress::Confirm {
                return self.confirm();
            }
            // The one key the cancel does not consume: an operator whose
            // Ctrl-C does nothing reaches for `kill -9`, past every restore
            // path `super::term` has. Quitting discards the confirm.
            if key == KeyPress::Quit {
                return Effect::Quit;
            }
            self.action = None;
            return Effect::None;
        }
        self.notice = None;
        match key {
            KeyPress::Quit => Effect::Quit,
            // The one key whose meaning depends on state, and the bar reads
            // `esc clear` for exactly as long as clearing is what it does.
            KeyPress::Escape => {
                if self.filter.is_empty() {
                    Effect::Quit
                } else {
                    self.set_filter(String::new())
                }
            }
            // Once the link task has ended its poll receiver is gone, so an
            // `Effect::PollNow` would be silence with no reason for it.
            KeyPress::Refresh => {
                if matches!(self.link, Link::Lost { .. }) {
                    self.notice = Some(Notice {
                        text: LINK_GONE.to_string(),
                        grave: true,
                    });
                    return Effect::None;
                }
                Effect::PollNow
            }
            KeyPress::SelectUp => self.select_by(-1),
            KeyPress::SelectDown => self.select_by(1),
            KeyPress::SelectFirst => self.select_at(0, 1),
            KeyPress::SelectLast => self.select_at(self.visible_len().saturating_sub(1), -1),
            KeyPress::Action(verb) => self.arm(verb),
            // An armed confirm (including one already in flight) owns
            // `Enter` before it ever reaches here: the routing rule above
            // fires only on `Stage::Armed`. With nothing armed, `Enter`
            // opens the sheep pane on the selected row, or does nothing on
            // a dog, a group or a fold header, none of which is a sheep to
            // open one on.
            KeyPress::Confirm => self.open_sheep_pane(),
            KeyPress::FilterStart => {
                self.mode = InputMode::Text;
                Effect::None
            }
            // `TextChar`/`TextBackspace`/`TextApply`/`TextAbandon` reach here
            // only from text mode, already branched above. `map_key` also
            // sends `Remove`/`StepUp`/`StepDown` from Normal mode
            // (`d`/`K`/`J`), so those land here too, just inert.
            // `NextGroup`/`Group`/`Undo` belong to the config pane: no other
            // screen has groups to walk or a filed edit set to undo.
            KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo => Effect::None,
            // The read, not the open: the screen opens only once
            // `Msg::Settings` lands.
            KeyPress::Settings => Effect::LoadSettings,
            // Opens with an empty model; `Msg::Secrets` fills it once the
            // read lands. Closing again is `on_secrets_key`'s job, reached
            // only once `self.body` is already `Body::Secrets`, the same
            // split `KeyPress::Settings`/`on_settings_key` uses.
            KeyPress::Secrets => {
                self.body = Body::Secrets(SecretsPane {
                    model: Box::default(),
                    tab: 0,
                    selected: 0,
                    collapsed: HashSet::new(),
                    reveal: None,
                    pending_reveal: None,
                    armed: None,
                    typing: None,
                });
                Effect::LoadSecrets
            }
            // Meaningful only inside the secrets pane, which owns the
            // keyboard while `self.body` is `Body::Secrets`; reached here
            // only from the dashboard, where there is no tab to move and no
            // secret selected to show.
            KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::SecretDelete => Effect::None,
            // Also the read, not the open: the pane shows the shepherd's
            // answer or nothing. `selected_row` is `None` for a group too,
            // but a group's name is what `Request::SheepConfig` wants, so
            // `e` still works on a multi-instance app's default row.
            KeyPress::Edit => self.ask_for_config(),
            // `space` acts only on the settings screen.
            KeyPress::Cycle => Effect::None,
            // `h` names a field's help text, and the dashboard has no
            // field selected.
            KeyPress::Help => Effect::None,
            // Toggles rather than opening: pressing it twice is where it
            // began. But `ByFold` collapses a grouped app to its
            // `RowKey::Group` header alone (`push_fold_group_rows`), so a
            // `RowKey::Sheep` row the flat view was pointing at can vanish
            // from `visible_rows()` even though the id survives in
            // `self.selected`. `reseat` reads `selected_index`, not id
            // survival, so the same fixup `set_filter` uses applies here.
            KeyPress::FoldView => {
                let previous = self.selected_index();
                self.grouping = match self.grouping {
                    Grouping::Flat => Grouping::ByFold,
                    Grouping::ByFold => Grouping::Flat,
                };
                if self.reseat(previous) && !matches!(self.link, Link::Lost { .. }) {
                    // The cursor moved to a different sheep (or a group
                    // standing in for several), so the feed and lambs panes
                    // are about to describe someone else.
                    return Effect::RefreshSelected;
                }
                Effect::None
            }
            // Only a `RowKey::Fold` header answers to this key; anything
            // else, including no selection at all, is a no-op rather than a
            // refusal, the same silence `Cycle` and `Help` fall back to
            // outside their own screen.
            KeyPress::Collapse => {
                if let Some(RowKey::Fold(name)) = self.selected()
                    && !self.collapsed_folds.remove(&name)
                {
                    self.collapsed_folds.insert(name);
                }
                Effect::None
            }
            // Opens on the selected sheep, or does nothing without one, the
            // same shape `KeyPress::Edit` follows above.
            KeyPress::Bleats => self.ask_for_bleats(),
            // All four scroll or cycle a bleats-pane axis, and the
            // dashboard has neither a filter axis nor a feed to scroll;
            // named here rather than left to fall through the arm above,
            // the same way `KeyPress::Bleats` would be ignored on a screen
            // with no bleats pane.
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev => Effect::None,
        }
    }

    /// The secrets pane's own match box, in force while `self.body` is
    /// `Body::Secrets`.
    ///
    /// `S` and `Escape` both close it: `S` toggles, the way `s` toggles the
    /// settings screen, and `Escape` is the uniform "leave whatever is
    /// open" key every other pane answers to. A tab move reloads, because
    /// `in_force`, the value and the byte length on every row belong to one
    /// environment. `z` toggles the selected row's namespace rather than a
    /// fold, since the flock table is not what is on screen.
    fn on_secrets_key(&mut self, key: KeyPress) -> Effect {
        // `view::status`'s armed prompt promises "enter confirms, any other
        // key cancels", so every other key cancels here, the way the
        // dashboard's own armed confirm already does. Unlike the dashboard
        // the key is not also swallowed: a cursor move that disarms still
        // moves, which is what the pane has always done.
        //
        // `Quit` is the exception the dashboard makes too: an operator whose
        // ctrl-c does nothing reaches for `kill -9`.
        let was_armed =
            !matches!(key, KeyPress::Confirm | KeyPress::Quit) && self.disarm_secret_delete();
        match key {
            // Mirrors `on_bleats_key`'s own arm: every full-screen pane
            // answers `q`/`ctrl-c`, the one key a cancelling armed action
            // does not swallow either (`on_key`'s own comment on that).
            // The `hide` here buys nothing on screen, since nothing is drawn
            // after this: it drops the plaintext a moment before the whole
            // `App` goes.
            KeyPress::Quit => {
                self.hide_revealed();
                Effect::Quit
            }
            KeyPress::Secrets => {
                self.hide_revealed();
                self.body = Body::FlockTable;
                Effect::None
            }
            // An armed delete eats the first `Escape` rather than also
            // closing the pane: a delete waiting on a confirm is a state
            // the operator should see cleared before anything else moves.
            KeyPress::Escape => {
                if was_armed {
                    return Effect::None;
                }
                self.hide_revealed();
                self.body = Body::FlockTable;
                Effect::None
            }
            KeyPress::Reveal => self.reveal_selected(),
            KeyPress::Copy => self.copy_revealed(),
            KeyPress::TabPrev | KeyPress::TabNext => {
                self.hide_revealed();
                let Some(pane) = self.secrets_pane_mut() else {
                    return Effect::None;
                };
                let last = pane.model.environments.len().saturating_sub(1);
                pane.tab = if key == KeyPress::TabPrev {
                    pane.tab.saturating_sub(1)
                } else {
                    (pane.tab + 1).min(last)
                };
                Effect::LoadSecrets
            }
            KeyPress::Collapse => {
                if let Some(pane) = self.secrets_pane_mut()
                    && let Some(row) = pane.model.rows.get(pane.selected)
                    && let Source::Namespace(namespace) = &row.source
                {
                    let namespace = namespace.clone();
                    if !pane.collapsed.remove(&namespace) {
                        pane.collapsed.insert(namespace);
                        // Folding away the group `selected` sits in leaves
                        // no marked row on screen, and a `v` past this
                        // point would reveal a value nobody can see. Reuse
                        // `move_by`'s hidden-selection fallback rather than
                        // duplicate the boundary search here.
                        pane.move_by(0);
                    }
                }
                Effect::None
            }
            // `j`/`k`/`g`/`G` move over the pane's visible rows
            // ([`SecretsPane::move_by`] and friends), the same clamped
            // rule every other pane's cursor follows. The value on screen
            // belongs to the row it was revealed from, so every one of
            // these clears it first, the way a tab move already does.
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                self.hide_revealed();
                if let Some(pane) = self.secrets_pane_mut() {
                    match key {
                        KeyPress::SelectUp => pane.move_by(-1),
                        KeyPress::SelectDown => pane.move_by(1),
                        KeyPress::SelectFirst => pane.move_to_first(),
                        KeyPress::SelectLast => pane.move_to_last(),
                        _ => unreachable!(),
                    }
                }
                Effect::None
            }
            // `r`: re-reads the store, the provider cache and the roll,
            // the same effect a tab move already returns and for the same
            // reason: `in_force`, the value and the byte length are read
            // off disk, not derived from what is already on screen.
            KeyPress::Refresh => {
                self.hide_revealed();
                Effect::LoadSecrets
            }
            // `Enter` means two things here: armed, it confirms the delete;
            // otherwise, it opens an input. Armed wins, or a delete waiting
            // on a confirm would silently reopen the value box instead.
            KeyPress::Confirm => {
                if self
                    .secrets_pane_mut()
                    .is_some_and(|pane| pane.armed.is_some())
                {
                    self.confirm_secret_delete()
                } else {
                    self.secrets_confirm()
                }
            }
            KeyPress::SecretDelete => self.arm_secret_delete(),
            // Nothing else means anything here. Listed rather than a
            // wildcard, so a new `KeyPress` variant cannot fall silently
            // into an arm that ignores it.
            KeyPress::Action(_)
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Settings
            | KeyPress::Cycle
            | KeyPress::Edit
            | KeyPress::Help
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Bleats
            | KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            // The config pane's own three. This pane groups its rows by
            // source rather than by a field group, files nothing, and so
            // has neither a group to walk nor an edit set to take back.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo => Effect::None,
        }
    }

    /// `Enter` on the secrets pane while nothing is armed: opens the
    /// `+ new key` row's name input, or the selected key's value input.
    /// Refuses a provider row (read-only here) and a read-only lookout,
    /// each with its own reason, through [`Self::authorize_write`] for the
    /// second.
    ///
    /// The gate is checked before the input opens, not just before the
    /// write: a refusal that arrived only once a whole value was typed
    /// would waste every one of those keystrokes for nothing.
    fn secrets_confirm(&mut self) -> Effect {
        // `environments` empty is the placeholder model `KeyPress::Secrets`
        // opens with, before the first `Msg::Secrets` lands: `selected` (0)
        // and `model.rows.len()` (also 0) coincide there by having no rows
        // yet either, the same trap `Msg::Secrets`'s own clamp guards
        // against. A real load never has an empty `environments`: the model
        // always carries `all`, even over an empty store.
        let is_new_key_row = matches!(
            &self.body,
            Body::Secrets(pane)
                if !pane.model.environments.is_empty() && pane.selected_is_new_key_row()
        );
        if is_new_key_row {
            if self.authorize_write().is_none() {
                return Effect::None;
            }
            let Some(pane) = self.secrets_pane_mut() else {
                return Effect::None;
            };
            pane.typing = Some(Typing {
                what: TypingWhat::NewKey,
                buffer: String::new(),
            });
            self.mode = InputMode::Text;
            return Effect::None;
        }
        let Some(row) = (match &self.body {
            Body::Secrets(pane) => pane.model.rows.get(pane.selected).cloned(),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Sheep(_) => None,
        }) else {
            return Effect::None;
        };
        if matches!(row.source, Source::Namespace(_)) {
            self.notice = Some(Notice {
                text: PROVIDER_ROW_REFUSAL.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        // Seeded empty, never with the stored value: showing it here would
        // put a secret on screen with none of the reveal gate's ten-second
        // limit or its own `[secrets] allow_read` check.
        pane.typing = Some(Typing {
            what: TypingWhat::ValueFor(row.key),
            buffer: String::new(),
        });
        self.mode = InputMode::Text;
        Effect::None
    }

    /// `D`: arms the removal of the selected key's value in the current
    /// tab's environment. Refuses a provider row and a read-only lookout,
    /// mirroring [`Self::secrets_confirm`]'s own two checks, and refuses
    /// silently on the `+ new key` affordance and on a selection folded out
    /// of view: neither names a real, visible key to delete, the same gate
    /// [`Self::reveal_selected`] applies before a read.
    ///
    /// A row taking its value from the `all` slot refuses too
    /// ([`ALL_SLOT_REFUSAL`]), before it arms rather than after the write.
    fn arm_secret_delete(&mut self) -> Effect {
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        if !pane.visible_row_indices().contains(&pane.selected) {
            return Effect::None;
        }
        let Some(row) = pane.model.rows.get(pane.selected).cloned() else {
            return Effect::None;
        };
        let environment = pane.environment().map(str::to_string);
        if matches!(row.source, Source::Namespace(_)) {
            self.notice = Some(Notice {
                text: PROVIDER_ROW_REFUSAL.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        if environment.is_some_and(|tab| deletes_the_all_slot(&row, &tab)) {
            self.notice = Some(Notice {
                text: ALL_SLOT_REFUSAL.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let now = self.now;
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        pane.armed = Some(ArmedDelete {
            key: row.key,
            at: now,
        });
        Effect::None
    }

    /// `Enter` on the secrets pane while [`SecretsPane::armed`] holds a key:
    /// sends the delete and disarms.
    fn confirm_secret_delete(&mut self) -> Effect {
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        let Some(ArmedDelete { key, .. }) = pane.armed.take() else {
            return Effect::None;
        };
        let Some(environment) = pane.environment().map(str::to_string) else {
            return Effect::None;
        };
        // The same refusal the arm already made, taken again on the last
        // step before an unrecoverable write. No keypress reaches here with
        // an `all` row armed today, since a tab move and a reload both
        // disarm, so this is depth rather than a live path.
        let refuses = pane
            .model
            .rows
            .iter()
            .find(|row| row.key == key)
            .is_some_and(|row| deletes_the_all_slot(row, &environment));
        if refuses {
            self.notice = Some(Notice {
                text: ALL_SLOT_REFUSAL.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(authority) = self.authorize_write() else {
            return Effect::None;
        };
        Effect::WriteSecret(
            SecretEdit {
                key,
                environment,
                value: None,
            },
            authority,
        )
    }

    /// Clears an armed delete, and says whether one was there.
    ///
    /// Called once per keypress, from [`Self::on_secrets_key`]'s own head,
    /// so every key but the confirm and the quit cancels. Its answer is
    /// `Escape`'s cue not to also close the pane on the same press.
    fn disarm_secret_delete(&mut self) -> bool {
        self.secrets_pane_mut()
            .is_some_and(|pane| pane.armed.take().is_some())
    }

    /// The secrets pane's own text keymap, in force while
    /// [`SecretsPane::typing`] owns [`InputMode::Text`].
    fn on_secrets_text_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => return Effect::Quit,
            KeyPress::TextChar(typed) => {
                if let Some(typing) = self
                    .secrets_pane_mut()
                    .and_then(|pane| pane.typing.as_mut())
                {
                    typing.buffer.push(typed);
                }
            }
            KeyPress::TextBackspace => {
                if let Some(typing) = self
                    .secrets_pane_mut()
                    .and_then(|pane| pane.typing.as_mut())
                {
                    typing.buffer.pop();
                }
            }
            KeyPress::TextApply => return self.apply_secrets_text(),
            KeyPress::TextAbandon => {
                if let Some(pane) = self.secrets_pane_mut() {
                    pane.typing = None;
                }
                self.mode = InputMode::Normal;
            }
            _ => {}
        }
        Effect::None
    }

    /// `TextApply` on the secrets pane's own input.
    ///
    /// The name step hands straight to the value step rather than writing
    /// anything on its own. The value step validates against the store's
    /// own rules ([`shep_core::secrets::is_name`],
    /// [`shep_core::secrets::MAX_VALUE_BYTES`]) rather than a second copy of
    /// either, and raises [`Effect::WriteSecret`] only once both the
    /// grammar and the control gate hold; a refusal reopens the same input
    /// with what was typed still in it, so neither costs a retype.
    fn apply_secrets_text(&mut self) -> Effect {
        let Some(typing) = self.secrets_pane_mut().and_then(|pane| pane.typing.take()) else {
            return Effect::None;
        };
        match typing.what {
            TypingWhat::NewKey => {
                let name = typing.buffer;
                if shep_core::secrets::is_name(&name) {
                    if let Some(pane) = self.secrets_pane_mut() {
                        pane.typing = Some(Typing {
                            what: TypingWhat::ValueFor(name),
                            buffer: String::new(),
                        });
                    }
                    return Effect::None;
                }
                self.notice = Some(Notice {
                    text: format!("{name:?} is not a valid key: {NEW_KEY_GRAMMAR}"),
                    grave: true,
                });
                if let Some(pane) = self.secrets_pane_mut() {
                    pane.typing = Some(Typing {
                        what: TypingWhat::NewKey,
                        buffer: name,
                    });
                }
                Effect::None
            }
            TypingWhat::ValueFor(key) => {
                let value = typing.buffer;
                if value.len() > shep_core::secrets::MAX_VALUE_BYTES {
                    self.notice = Some(Notice {
                        text: format!(
                            "value is {} bytes, over the {}-byte limit",
                            value.len(),
                            shep_core::secrets::MAX_VALUE_BYTES
                        ),
                        grave: true,
                    });
                    if let Some(pane) = self.secrets_pane_mut() {
                        pane.typing = Some(Typing {
                            what: TypingWhat::ValueFor(key),
                            buffer: value,
                        });
                    }
                    return Effect::None;
                }
                let environment = match &self.body {
                    Body::Secrets(pane) => pane.environment().unwrap_or_default().to_string(),
                    Body::FlockTable
                    | Body::Settings(_)
                    | Body::ConfigPane(_)
                    | Body::Bleats(_)
                    | Body::Sheep(_) => return Effect::None,
                };
                let Some(authority) = self.authorize_write() else {
                    return Effect::None;
                };
                self.mode = InputMode::Normal;
                Effect::WriteSecret(
                    SecretEdit {
                        key,
                        environment,
                        value: Some(value),
                    },
                    authority,
                )
            }
        }
    }

    /// `b`: opens the full-screen bleats pane on the selected sheep. A
    /// group row or an empty selection is refused rather than opening on a
    /// stand-in: the pane is pinned to one sheep for its whole lifetime
    /// ([`BleatsPane`]), and a group has no single sheep to pin it to.
    fn ask_for_bleats(&mut self) -> Effect {
        if let Some(sheep @ RowKey::Sheep(_)) = self.selected() {
            self.body = Body::Bleats(BleatsPane::new(sheep));
        }
        Effect::None
    }

    /// `b`, from inside the sheep pane: hands the embedded feed's own state
    /// to `Body::Bleats` rather than [`BleatsPane::new`]ing a fresh one, so
    /// a filter narrowed in the pane's own column survives going full
    /// screen. A no-op on any other screen; `on_sheep_pane_key` only reaches
    /// this while [`Self::sheep_pane`] is `Some`, but the match on `body`
    /// stays defensive rather than assuming that.
    fn promote_feed_to_full_screen(&mut self) -> Effect {
        if let Body::Sheep(pane) = &self.body {
            self.body = Body::Bleats(pane.feed().clone());
        }
        // The embedded feed never clamps its own offset: it draws no
        // scrollback, so `N` can walk the stored value past anything the
        // full screen can scroll back to. Clamping on arrival rather than
        // leaving it is what `scroll_bleats_back` already documents, in
        // those words: the render clamps while the stored value keeps
        // climbing, so `j` stops appearing to work until the operator has
        // pressed it as many times as `N` was pressed before.
        let ceiling = self.bleats_pane().map_or(0, |pane| {
            super::view::bleats_full::max_scroll_offset(self, pane)
        });
        if let Some(pane) = self.bleats_pane_mut() {
            pane.clamp_scroll(ceiling);
        }
        Effect::None
    }

    /// `Enter`'s own handler on the dashboard: opens the sheep pane on the
    /// selected sheep and asks for its config in the same step, since the
    /// pane's own left column has nothing to draw without it.
    ///
    /// [`Self::selected_row`], not [`Self::selected_name`]: a group row has
    /// no single sheep to open the pane on, and a dog runs no config the
    /// pane's own `SheepConfigView` can show (its section is a TOML table,
    /// not a Flockfile's `AppConfig`). Both are silently refused, the same
    /// silence [`Self::ask_for_bleats`] falls back to for a group.
    fn open_sheep_pane(&mut self) -> Effect {
        let Some(row) = self.selected_row() else {
            return Effect::None;
        };
        if row.info.dog.is_some() {
            return Effect::None;
        }
        let sheep = self
            .selected()
            .expect("selected_row answered, so a selection exists");
        let name = row.info.name.clone();
        self.body = Body::Sheep(Box::new(SheepPane::new(sheep)));
        self.ask_for_sheep_config(name, ConfigFor::SheepPane)
    }

    /// `J`/`K` from inside the sheep pane: steps to the next or previous
    /// sheep the flock table would show, skipping a dog, a group header and
    /// a fold header (none of which is a sheep the pane can open on), and
    /// asks for the new sheep's config in the same step.
    ///
    /// Silent past either end of the list, and silent if the pane's own
    /// sheep has already left the flock: there is nothing to step from.
    fn step_sheep_pane(&mut self, delta: isize) -> Effect {
        let Body::Sheep(pane) = &self.body else {
            return Effect::None;
        };
        let current = pane.sheep().clone();
        let sheep_rows: Vec<RowKey> = self
            .visible_rows()
            .into_iter()
            .filter(|key| match key {
                RowKey::Sheep(id) => self.flock.get(id).is_some_and(|row| row.info.dog.is_none()),
                RowKey::Group(_) | RowKey::Fold(_) | RowKey::Section(_) => false,
            })
            .collect();
        let Some(index) = sheep_rows.iter().position(|key| *key == current) else {
            return Effect::None;
        };
        let next_index = index
            .saturating_add_signed(delta)
            .min(sheep_rows.len().saturating_sub(1));
        let next = sheep_rows[next_index].clone();
        if next == current {
            return Effect::None;
        }
        let RowKey::Sheep(id) = &next else {
            unreachable!("the filter above admits only `RowKey::Sheep`")
        };
        let Some(name) = self.flock.get(id).map(|row| row.info.name.clone()) else {
            return Effect::None;
        };
        self.selected = Some(next.clone());
        if let Some(pane) = self.sheep_pane_mut() {
            pane.set_sheep(next);
        }
        self.ask_for_sheep_config(name, ConfigFor::SheepPane)
    }

    /// The sheep pane's own keymap, in force while [`Self::sheep_pane`] is
    /// `Some`. `esc` closes it; `e` opens the config editor over it,
    /// routed by [`ConfigFor`] once the reply lands, since both send the
    /// same request; `J`/`K` step to the next or previous sheep without
    /// leaving the pane; `x`/`R`/`L` arm a confirm against the pane's own
    /// pinned sheep ([`Self::arm_sheep_pane`]), `↵` confirms it and any
    /// other key cancels it, the same as the dashboard's own armed check
    /// just below in [`Self::on_key`], needed here too, in its own copy,
    /// because `on_key` routes to this method ahead of that check, so an
    /// action armed from inside this pane never reaches it. `j`/`k` and
    /// `g`/`G` scroll the config/env column through its own `Viewport`.
    /// `/`, `o`, `m`, `f`, `w`, `n` and `N` belong to the embedded feed
    /// ([`Self::sheep_feed_mut`]), the same axes [`Self::on_bleats_key`]
    /// wires for the full-screen pane, and `b` hands that same feed to
    /// [`Self::promote_feed_to_full_screen`] rather than opening a fresh one.
    /// Every other key is inert.
    fn on_sheep_pane_key(&mut self, key: KeyPress) -> Effect {
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.stage == Stage::Armed)
        {
            if key == KeyPress::Confirm {
                return self.confirm();
            }
            if key == KeyPress::Quit {
                return Effect::Quit;
            }
            self.action = None;
            return Effect::None;
        }
        self.notice = None;
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::Escape => {
                self.close_pane();
                Effect::None
            }
            KeyPress::Edit => self.ask_for_sheep_pane_config(),
            KeyPress::StepDown => self.step_sheep_pane(1),
            KeyPress::StepUp => self.step_sheep_pane(-1),
            KeyPress::Action(verb) => self.arm_sheep_pane(verb),
            KeyPress::SelectUp => {
                if let Some(pane) = self.sheep_pane_mut() {
                    pane.move_by(-1);
                }
                Effect::None
            }
            KeyPress::SelectDown => {
                if let Some(pane) = self.sheep_pane_mut() {
                    pane.move_by(1);
                }
                Effect::None
            }
            KeyPress::SelectFirst => {
                if let Some(pane) = self.sheep_pane_mut() {
                    pane.move_to_first();
                }
                Effect::None
            }
            KeyPress::SelectLast => {
                if let Some(pane) = self.sheep_pane_mut() {
                    pane.move_to_last();
                }
                Effect::None
            }
            // `b`: hands the embedded feed's own state to `Body::Bleats`
            // rather than rebuilding one, so a filter narrowed here survives
            // going full screen.
            KeyPress::Bleats => self.promote_feed_to_full_screen(),
            // Opens the feed's own match box: `on_text_key` routes the
            // keystrokes that follow to `on_sheep_feed_text_key` once this
            // pane owns `InputMode::Text`, the same shape
            // `on_bleats_key`'s own `FilterStart` arm follows.
            KeyPress::FilterStart => {
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.begin_match_edit();
                }
                self.mode = InputMode::Text;
                Effect::None
            }
            // `o`: cycles the embedded feed's stream axis, the same cycle
            // `on_bleats_key`'s own arm follows.
            KeyPress::StreamCycle => {
                if let Some(feed) = self.sheep_feed_mut() {
                    let next = match feed.filters().stream {
                        None => Some(Stream::Out),
                        Some(Stream::Out) => Some(Stream::Err),
                        Some(Stream::Err) => None,
                    };
                    feed.set_stream(next);
                }
                Effect::None
            }
            // `m`: cycles the embedded feed's minimum-level axis, the same
            // cycle `on_bleats_key`'s own arm follows.
            KeyPress::LevelCycle => {
                if let Some(feed) = self.sheep_feed_mut() {
                    let next = match feed.filters().min_level {
                        None => Some(Level::Trace),
                        Some(Level::Trace) => Some(Level::Debug),
                        Some(Level::Debug) => Some(Level::Info),
                        Some(Level::Info) => Some(Level::Warn),
                        Some(Level::Warn) => Some(Level::Error),
                        Some(Level::Error) => None,
                    };
                    feed.set_min_level(next);
                }
                Effect::None
            }
            // `f`: toggles following explicitly.
            KeyPress::FollowToggle => {
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.toggle_follow();
                }
                Effect::None
            }
            // `w`: toggles whether a long line wraps or truncates.
            KeyPress::WrapToggle => {
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.toggle_wrap();
                }
                Effect::None
            }
            // `n`: one match toward the newest line. A no-op with no match
            // axis set: see `BleatsPane::match_next`.
            KeyPress::MatchNext => {
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.match_next();
                }
                Effect::None
            }
            // `N`: the same, toward the oldest matching line. Unlike
            // `on_bleats_key`'s own `MatchPrev` arm, this does not clamp
            // through `bleats_full::max_scroll_offset`: the embedded feed
            // draws no scrollback of its own (there is no `j`/`k` for it
            // here, unlike the full-screen pane), and `window_range`
            // saturates a stale offset rather than reading past the end.
            // `promote_feed_to_full_screen` clamps on arrival, which is
            // where an unclamped value would otherwise be felt.
            KeyPress::MatchPrev => {
                let stepping = self
                    .sheep_pane()
                    .is_some_and(|pane| pane.feed().filters().matcher.is_some());
                if stepping && let Some(feed) = self.sheep_feed_mut() {
                    feed.scroll_up(1);
                }
                Effect::None
            }
            KeyPress::Refresh
            | KeyPress::Confirm
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Settings
            | KeyPress::Cycle
            | KeyPress::Help
            | KeyPress::Remove
            | KeyPress::FoldView
            | KeyPress::Collapse
            | KeyPress::PageDown
            | KeyPress::PageUp
            // The secrets pane's own six. `S` opens that pane from the
            // dashboard, not from here, and the other five mean nothing
            // outside it.
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete => Effect::None,
            // The groups and the filed edit set belong to the config pane.
            // This pane lists a sheep's fields read-only, so it has no
            // group to switch to and nothing filed to take back.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo => Effect::None,
        }
    }

    /// The bleats pane's own keymap, in force while [`Self::bleats_pane`] is
    /// `Some`. `Escape` drops the newest filter chip first, one axis at a
    /// time, and only closes the pane once none are left. `j`/`k` scroll a
    /// line, `ctrl-d`/`ctrl-u` a page, `G` jumps to the tail and resumes
    /// following, `f` toggles following explicitly, `w` toggles wrapping,
    /// and `n`/`N` step toward the newest or oldest matching line.
    fn on_bleats_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::Escape => {
                let dropped_a_chip = self
                    .bleats_pane_mut()
                    .is_some_and(BleatsPane::drop_newest_chip);
                if !dropped_a_chip {
                    self.close_pane();
                }
                Effect::None
            }
            // Opens the match box: `on_text_key` routes the keystrokes that
            // follow to `on_bleats_text_key` once this pane owns
            // `InputMode::Text`. `begin_match_edit` remembers what the axis
            // held, so an abandoned edit restores it rather than losing it.
            KeyPress::FilterStart => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.begin_match_edit();
                }
                self.mode = InputMode::Text;
                Effect::None
            }
            // `o`: `None` (both streams) -> `Out` -> `Err` -> `None`.
            KeyPress::StreamCycle => {
                if let Some(pane) = self.bleats_pane_mut() {
                    let next = match pane.filters().stream {
                        None => Some(Stream::Out),
                        Some(Stream::Out) => Some(Stream::Err),
                        Some(Stream::Err) => None,
                    };
                    pane.set_stream(next);
                }
                Effect::None
            }
            // `m`: every `Level` in ascending order, then back to `None`.
            // The cycle must reach `None` again, or an operator who sets a
            // minimum can never see an unclassifiable line again without
            // closing the pane.
            KeyPress::LevelCycle => {
                if let Some(pane) = self.bleats_pane_mut() {
                    let next = match pane.filters().min_level {
                        None => Some(Level::Trace),
                        Some(Level::Trace) => Some(Level::Debug),
                        Some(Level::Debug) => Some(Level::Info),
                        Some(Level::Info) => Some(Level::Warn),
                        Some(Level::Warn) => Some(Level::Error),
                        Some(Level::Error) => None,
                    };
                    pane.set_min_level(next);
                }
                Effect::None
            }
            // `k`/`Up`: one line toward older lines.
            KeyPress::SelectUp => {
                self.scroll_bleats_back(1);
                Effect::None
            }
            // `j`/`Down`: one line toward the newest.
            KeyPress::SelectDown => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.scroll_down(1);
                }
                Effect::None
            }
            // `G`/`End`: the tail, and following resumes — that is what an
            // operator means by "go to the end".
            KeyPress::SelectLast => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.jump_to_end();
                }
                Effect::None
            }
            // `ctrl-u`: a page toward older lines. Sized through
            // `page_amount_up` rather than `pane.body_rows()` directly: see
            // that function's own doc for why a page is a line count under
            // wrap, not a raw row count.
            KeyPress::PageUp => {
                let amount = self
                    .bleats_pane()
                    .map_or(1, |pane| super::view::bleats_full::page_amount_up(self, pane));
                self.scroll_bleats_back(amount);
                Effect::None
            }
            // `ctrl-d`: toward the newest line, and sized by its own
            // function. The backward count `ctrl-u` uses drops lines when
            // applied forward; see `page_amount_up`'s doc.
            KeyPress::PageDown => {
                let amount = self
                    .bleats_pane()
                    .map_or(1, |pane| super::view::bleats_full::page_amount_down(self, pane));
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.page_down(amount);
                }
                Effect::None
            }
            // `f`: toggles following explicitly.
            KeyPress::FollowToggle => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.toggle_follow();
                }
                Effect::None
            }
            // `w`: toggles whether a long line wraps or truncates.
            KeyPress::WrapToggle => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.toggle_wrap();
                }
                Effect::None
            }
            // `n`: one match toward the newest line. A no-op with no match
            // axis set: see `BleatsPane::match_next`.
            KeyPress::MatchNext => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.match_next();
                }
                Effect::None
            }
            // `N`: the same, toward the oldest matching line, through
            // `scroll_bleats_back` so it takes the ceiling `k` and `ctrl-u`
            // take. Calling `match_prev` directly would climb past the
            // oldest surviving match and then `n`, `j` and `ctrl-d` would
            // all stop appearing to work until the offset drained. The
            // matcher guard is read here because the scroll happens here.
            KeyPress::MatchPrev => {
                let stepping = self
                    .bleats_pane()
                    .is_some_and(|pane| pane.filters().matcher.is_some());
                if stepping {
                    self.scroll_bleats_back(1);
                }
                Effect::None
            }
            // There is nothing on this screen `g`/`Home` can move to: the
            // window has no fixed start, only a tail. Left unbound rather
            // than aliased to `ctrl-u`'s page, which would give one key two
            // different meanings depending on how far a page happens to be.
            KeyPress::SelectFirst
            | KeyPress::Refresh
            | KeyPress::Action(_)
            | KeyPress::Confirm
            // Reach here only from text mode, already branched above in
            // `on_key`; listed so a new `KeyPress` variant cannot fall
            // silently into an arm that ignores it.
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Settings
            | KeyPress::Cycle
            | KeyPress::Edit
            | KeyPress::Help
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            // `F` and `z` belong to the flock table. Regrouping a table the
            // operator cannot see, while a log pane owns the screen, is a
            // change they would meet on closing it.
            | KeyPress::FoldView
            | KeyPress::Collapse
            | KeyPress::Bleats
            // The secrets pane's own keys; nothing to move or reload while
            // the bleats pane owns the screen instead.
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete => Effect::None,
            // `NextGroup`/`Group`/`Undo` belong to the config pane: no other
            // screen has groups to walk or a filed edit set to undo.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo => Effect::None,
        }
    }

    /// The bleats pane's match box, in force while it owns
    /// [`InputMode::Text`]. Follows the flock table's own text keymap
    /// ([`Self::on_filter_text_key`]) with one difference the design calls
    /// for: typing narrows live through [`BleatsPane::set_match`], but
    /// `TextAbandon` restores whatever [`BleatsPane::begin_match_edit`] saw
    /// rather than clearing the axis outright, since the axis may already
    /// have held a chip from an earlier edit.
    fn on_bleats_text_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::TextChar(typed) => {
                if let Some(pane) = self.bleats_pane_mut() {
                    let mut text = pane.filters().matcher.clone().unwrap_or_default();
                    text.push(typed);
                    pane.set_match(text);
                }
                Effect::None
            }
            KeyPress::TextBackspace => {
                if let Some(pane) = self.bleats_pane_mut() {
                    let mut text = pane.filters().matcher.clone().unwrap_or_default();
                    text.pop();
                    pane.set_match(text);
                }
                Effect::None
            }
            KeyPress::TextApply => {
                self.mode = InputMode::Normal;
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.commit_match_edit();
                }
                Effect::None
            }
            KeyPress::TextAbandon => {
                self.mode = InputMode::Normal;
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.abandon_match_edit();
                }
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// The embedded feed's own match box, in force while it owns
    /// [`InputMode::Text`]. [`Self::on_bleats_text_key`]'s own body, against
    /// [`Self::sheep_feed_mut`] instead of [`Self::bleats_pane_mut`]: the two
    /// panes never coexist, but each opens its match box against its own
    /// filter state.
    fn on_sheep_feed_text_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::TextChar(typed) => {
                if let Some(feed) = self.sheep_feed_mut() {
                    let mut text = feed.filters().matcher.clone().unwrap_or_default();
                    text.push(typed);
                    feed.set_match(text);
                }
                Effect::None
            }
            KeyPress::TextBackspace => {
                if let Some(feed) = self.sheep_feed_mut() {
                    let mut text = feed.filters().matcher.clone().unwrap_or_default();
                    text.pop();
                    feed.set_match(text);
                }
                Effect::None
            }
            KeyPress::TextApply => {
                self.mode = InputMode::Normal;
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.commit_match_edit();
                }
                Effect::None
            }
            KeyPress::TextAbandon => {
                self.mode = InputMode::Normal;
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.abandon_match_edit();
                }
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// The settings screen's own keymap, in force while [`Self::settings`] is
    /// `Some`. Everything not named here is ignored, an action key included.
    fn on_settings_key(&mut self, key: KeyPress) -> Effect {
        self.notice = None;
        match key {
            KeyPress::Quit => return Effect::Quit,
            // Both close, but an armed confirm eats the first one, the
            // cancel-before-act rule the dashboard follows. `Escape` closing
            // rather than quitting is where this screen swaps that cascade.
            KeyPress::Settings | KeyPress::Escape => {
                let armed = self.settings().is_some_and(Settings::is_armed);
                if armed {
                    if let Some(settings) = self.settings_mut() {
                        settings.pending = None;
                    }
                } else {
                    self.body = Body::FlockTable;
                }
            }
            // An armed candidate eats the first movement key rather than also
            // moving: the next reflexive Enter would otherwise apply an edit to
            // a row the operator lost track of. `Sent` is untouched.
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                if let Some(settings) = self.settings_mut() {
                    if settings.is_armed() {
                        settings.pending = None;
                    } else {
                        match key {
                            KeyPress::SelectUp => settings.move_by(-1),
                            KeyPress::SelectDown => settings.move_by(1),
                            KeyPress::SelectFirst => settings.move_to_first(),
                            KeyPress::SelectLast => settings.move_to_last(),
                            _ => unreachable!(),
                        }
                    }
                }
            }
            KeyPress::Cycle => return self.cycle_setting(),
            KeyPress::Confirm => return self.confirm_setting(),
            // Re-reads `shep.toml`, so another process's write shows up, and
            // the cursor survives. An armed candidate eats this key too.
            KeyPress::Refresh => {
                if let Some(settings) = self.settings_mut()
                    && settings.is_armed()
                {
                    settings.pending = None;
                    return Effect::None;
                }
                return Effect::LoadSettings;
            }
            // The probe, not the open, same reasoning as `on_key`'s `e`:
            // the pane shows the dog's real schema and section or nothing.
            // An armed candidate eats it first, like every other key here.
            KeyPress::Edit => {
                if let Some(settings) = self.settings_mut()
                    && settings.is_armed()
                {
                    settings.pending = None;
                    return Effect::None;
                }
                return self.probe_dog_schema();
            }
            // Unreachable from here, named so a new variant cannot fall
            // silently into an arm that ignores it.
            KeyPress::Action(_)
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Help
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete
            | KeyPress::Collapse => {}
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::Bleats
            // `NextGroup`/`Group`/`Undo` belong to the config pane: no other
            // screen has groups to walk or a filed edit set to undo.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo => {}
        }
        Effect::None
    }

    /// `e` on the settings screen: probe the dog under the cursor for its
    /// schema.
    ///
    /// Silent on a scalar row, not refused: `space` and `Enter` already
    /// edit those, and a refusal that was never going to act trains an
    /// operator to ignore the status bar.
    ///
    /// The dogs this can reach are exactly the rows already shown, so no
    /// listing request is needed. An adopted-but-disabled dog stays in
    /// that list: configure-then-enable is the ordinary order.
    fn probe_dog_schema(&mut self) -> Effect {
        let Some(settings) = self.settings() else {
            return Effect::None;
        };
        let Some(SettingsRow::Dog(index)) = settings.cursor() else {
            return Effect::None;
        };
        let Some(dog) = settings.snapshot().dogs.get(index) else {
            return Effect::None;
        };
        Effect::LoadDogPane {
            name: dog.name.clone(),
            adopted_path: dog.adopted_path.clone(),
        }
    }

    /// `e`'s own handler: open a dog's pane directly, else ask the shepherd.
    ///
    /// [`Self::selected_row`] rather than [`Self::selected_name`]: a dog
    /// runs one process and is never a group row, but a group row must
    /// still work with `e`.
    fn ask_for_config(&mut self) -> Effect {
        if let Some(row) = self.selected_row()
            && let Some(source) = row.info.dog.as_ref()
        {
            let adopted_path = match source {
                DogSource::Adopted { path } => Some(PathBuf::from(path)),
                _ => None,
            };
            return Effect::LoadDogPane {
                name: row.info.name.clone(),
                adopted_path,
            };
        }
        match self.selected_name() {
            Some(name) => self.ask_for_sheep_config(name, ConfigFor::Editor),
            None => Effect::None,
        }
    }

    /// `e`'s own handler from inside the sheep pane: targets the pane's own
    /// pinned sheep, never [`Self::selected_row`]/[`Self::selected_name`].
    ///
    /// The same reasoning [`Self::arm_sheep_pane`]'s own doc gives:
    /// `Msg::Snapshot` reseats the dashboard's selection whatever screen is
    /// showing, so reading the selection here would open a neighbour's
    /// config under the pane's own title the instant the pinned sheep left
    /// the flock. Refuses instead of substituting. A pane is never pinned
    /// on a dog, so [`Self::ask_for_config`]'s dog branch has no twin here.
    fn ask_for_sheep_pane_config(&mut self) -> Effect {
        let Some(row) = self.sheep_pane_row() else {
            self.notice = Some(Notice {
                text: "that sheep is no longer in the flock".to_string(),
                grave: true,
            });
            return Effect::None;
        };
        self.ask_for_sheep_config(row.info.name.clone(), ConfigFor::Editor)
    }

    /// Sends `Request::SheepConfig` for `name`, recording which screen it is
    /// for so [`Self::on_sheep_config`] can route the reply once it lands.
    ///
    /// The one place [`Self::config_target`] and [`Self::config_for`] are
    /// set for a sheep-config read, so the two can never disagree about
    /// which request is outstanding.
    fn ask_for_sheep_config(&mut self, name: String, for_screen: ConfigFor) -> Effect {
        self.config_target = Some(name.clone());
        self.config_for = Some(for_screen);
        Effect::Send(Sent::SheepConfig { name })
    }

    /// Puts the keyboard back to [`InputMode::Normal`] when no pane editor
    /// owns it any more.
    ///
    /// [`InputMode::Text`] is remembered on `App` while the buffer it
    /// belongs to lives on the pane, so anything that drops or replaces the
    /// pane can leave the two disagreeing, and a lookout in `Text` mode
    /// with nothing to type into eats every keystroke until `Esc`. A
    /// re-read rebuilds the whole `ConfigPane`, which is exactly that, and
    /// a landed write asks for one.
    ///
    /// Called only from paths where the config pane is the screen in
    /// question. The filter box owns `Text` with no marker but the mode
    /// itself, and it cannot be open while the pane is.
    fn release_text_mode_if_unowned(&mut self) {
        if self.mode != InputMode::Text {
            return;
        }
        let owned = self
            .config_pane()
            .is_some_and(|pane| pane.typing().is_some() || pane.env_typing().is_some());
        if !owned {
            self.mode = InputMode::Normal;
        }
    }

    /// The config pane's own keymap, in force for as long as
    /// [`Self::config_pane`] is `Some`.
    ///
    /// Movement walks fields, `r` re-reads, `space` cycles the row under
    /// the cursor, `Enter` or `e` edits it, `u` undoes the newest edit,
    /// `h` toggles the selected field's own help text, and `Escape`
    /// closes help if it is open, else writes everything filed and
    /// leaves. Everything else is named rather than wildcarded, so a
    /// stray variant cannot fall silently into an arm that ignores it.
    ///
    /// Nothing is armed here and no key is eaten. A keystroke that edits
    /// files into the pane's own set and sends nothing, so a stray one
    /// costs an `u` rather than a write to a running sheep.
    fn on_pane_key(&mut self, key: KeyPress) -> Effect {
        self.notice = None;
        if self.pane_menu.is_some() {
            return self.on_pane_menu_key(key);
        }
        if self.config_pane().is_some_and(|pane| pane.list().is_some()) {
            return self.on_list_key(key);
        }
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        match key {
            KeyPress::Quit => return Effect::Quit,
            // Backs out one level at a time: help first, if it is open,
            // else the pane. `Escape` closes rather than cascading to a
            // filter clear or a quit, exactly as it does on the settings
            // screen.
            //
            // This is the one door a config edit leaves by. The set goes
            // out whether the pane leaves the screen on this keypress or
            // stops to offer the parked-field menu, because the operator
            // asked to write on this key and a menu about the shepherd's
            // own parked fields is a separate question.
            KeyPress::Escape => {
                let help_open = self.config_pane().is_some_and(ConfigPane::help_open);
                if help_open {
                    if let Some(pane) = self.config_pane_mut() {
                        pane.close_help();
                    }
                    return Effect::None;
                }
                let writes = self.take_pane_writes();
                if let Some(menu) = self.apply_offer() {
                    self.pane_menu = Some(menu);
                } else {
                    self.close_pane();
                }
                if !writes.is_empty() {
                    return Effect::SendAll(writes);
                }
            }
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                if let Some(pane) = self.config_pane_mut() {
                    match key {
                        KeyPress::SelectUp => pane.move_by(-1),
                        KeyPress::SelectDown => pane.move_by(1),
                        KeyPress::SelectFirst => pane.move_to_first(),
                        KeyPress::SelectLast => pane.move_to_last(),
                        _ => unreachable!(),
                    }
                }
            }
            // Re-reads the same sheep, so an override applied from another
            // window shows up. The cursor survives it: see
            // `Self::on_sheep_config`.
            KeyPress::Refresh => return self.reread_pane(),
            KeyPress::Cycle => return self.cycle_field(),
            // `e` does exactly what `Enter` does here: an operator who
            // opened the pane with `e` should not have to learn a second
            // key to use it.
            KeyPress::Confirm | KeyPress::Edit => return self.confirm_field(),
            KeyPress::Help => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.toggle_help();
                }
            }
            // `d` restores the field under the cursor to its default. Does
            // nothing on an env row or `+ add a key`: unsetting a key
            // entirely is a different act from restoring a default, and
            // the spec does not ask for it.
            KeyPress::Remove => return self.restore_default(),
            KeyPress::Action(_)
            | KeyPress::Settings
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete
            | KeyPress::Collapse => {}
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::Bleats => {}
            // Drops the newest edit and nothing else. No control gate: a
            // key that unfiles something cannot write, and refusing it
            // behind a closed gate would leave an edit the gate already
            // refused to file.
            KeyPress::Undo => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.undo_edit();
                }
            }
            // `tab` walks the groups; a digit jumps straight to one. Both
            // reset the cursor to the group's first field, so `j`/`k` never
            // start on a row the new tab does not draw.
            KeyPress::NextGroup => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.next_group();
                    pane.move_to_first();
                }
            }
            KeyPress::Group(digit) => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.set_group(digit);
                    pane.move_to_first();
                }
            }
        }
        Effect::None
    }

    /// Everything the open pane has filed, as the requests that carry it,
    /// leaving the pane holding nothing.
    ///
    /// Empty for no pane, for an empty set, and for a dog whose section
    /// stopped parsing between the read and the keystroke, which is
    /// reported rather than sent as an empty table.
    ///
    /// A sheep's set is one request per entry and a dog's is one request
    /// for the lot: `Request::SetDogConfig` replaces the whole table, so
    /// a batch of edits to one dog is one write. See
    /// One write ticket, and the counter moved past it.
    ///
    /// [`Self::take_pane_writes`] mints a batch's worth inline rather than
    /// calling this per entry, because it holds a borrow of `self.body`
    /// across the loop.
    fn take_write_ticket(&mut self) -> u64 {
        let ticket = self.next_write_ticket;
        self.next_write_ticket += 1;
        ticket
    }

    /// `ConfigPane::edited_section_with`.
    fn take_pane_writes(&mut self) -> Vec<Sent> {
        // `WriteAuthority::granted`, not `Self::authorize_write`: the gate
        // is checked on the keystroke that files an edit, so a read-only
        // pane reaches here with an empty set and must not be told off for
        // leaving.
        let Some(authority) = WriteAuthority::granted(self) else {
            return Vec::new();
        };
        // A direct field match, not `Self::config_pane_mut`: that helper
        // borrows the whole struct for as long as `pane` lives, and this
        // function still needs `self.next_write_ticket` while it is in
        // scope.
        let Some(pane) = (match &mut self.body {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }) else {
            return Vec::new();
        };
        let edits = pane.close();
        if edits.is_empty() {
            return Vec::new();
        }
        let mut ticket = self.next_write_ticket;
        let sent = match pane.target().clone() {
            PaneTarget::Dog { name, .. } => match pane.edited_section_with(&edits) {
                Some(toml) => {
                    let section = vec![Sent::SetDogSection {
                        name,
                        ticket,
                        toml: toml.into(),
                        authority,
                    }];
                    ticket += 1;
                    section
                }
                None => {
                    self.notice = Some(Notice {
                        text: format!("{name}: its section in dogs.toml does not parse"),
                        grave: true,
                    });
                    return Vec::new();
                }
            },
            PaneTarget::Sheep { name } => {
                let writes = edits.into_writes();
                let mut requests = Vec::with_capacity(writes.len());
                for edit in writes {
                    let this = ticket;
                    ticket += 1;
                    requests.push(match edit {
                        PaneEdit::SetEnv { key, value } => Sent::SetEnv {
                            name: name.clone(),
                            ticket: this,
                            key,
                            value,
                            authority,
                        },
                        // No client-side validation, deliberately: the
                        // daemon already re-normalizes untrusted input, so
                        // a weaker copy here would drift the moment
                        // `AppConfig` grows a field. An empty buffer on a
                        // non-nullable field files `null`, refused as
                        // `InvalidConfig`.
                        PaneEdit::Set { key, value } => Sent::ApplyField {
                            name: name.clone(),
                            ticket: this,
                            key,
                            value,
                            authority,
                        },
                    });
                }
                requests
            }
        };
        // One ticket per request that goes out, and never reused: the
        // counter is what keeps two `Sent` values for the same field
        // distinguishable.
        self.next_write_ticket = ticket;
        sent
    }

    /// `r` from inside a pane: ask for the same target again, by whichever
    /// of the two doors it came in.
    ///
    /// A dog's schema is not re-probed. It came from the dog's binary at
    /// open and is parked on [`Self::dog_target`]; re-probing would respawn
    /// somebody else's binary on a keystroke whose job is to re-read a file.
    fn reread_pane(&mut self) -> Effect {
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        let name = pane.target().name().to_owned();
        match pane.target() {
            PaneTarget::Sheep { .. } => Effect::Send(Sent::SheepConfig { name }),
            PaneTarget::Dog { .. } => Effect::Send(Sent::DogSection { name }),
        }
    }

    /// Drops the open pane and everything a reply for it would re-open.
    ///
    /// Both targets are cleared, always, and not only the one the pane
    /// happens to hold: a read for the other kind can be in flight when this
    /// runs (`e` on a dog, then the settings screen closes and `e` opens a
    /// sheep), and a stale one left set is exactly the re-open behind the
    /// operator's back `config_target` exists to prevent.
    ///
    /// This always lands on [`Body::FlockTable`], never on whatever screen
    /// preceded the pane. That used to be reachable the other way: a
    /// settings screen and a config pane were once two independent
    /// `Option`s, so closing the pane only cleared its own field and a
    /// still-`Some` settings screen underneath resurfaced — the dashboard is
    /// what `Escape` is supposed to reach, not a screen the operator asked
    /// for two actions ago. `Body` makes that unrepresentable: there is only
    /// ever one screen to close to, and it is this one.
    fn close_pane(&mut self) {
        self.body = Body::FlockTable;
        self.pane_menu = None;
        self.config_target = None;
        self.config_for = None;
        self.dog_target = None;
        self.release_text_mode_if_unowned();
    }

    /// The offer this pane's `Escape` makes, or [`None`] when it just leaves.
    ///
    /// Silent with nothing parked, so reading a pane never costs a
    /// keystroke, and silent behind a closed gate, where the two keys it
    /// offers would be refused anyway.
    fn apply_offer(&self) -> Option<PaneMenu> {
        if self.control == Control::ReadOnly {
            return None;
        }
        let pane = self.config_pane()?;
        let parked = pane.parked_count();
        (parked > 0).then(|| PaneMenu::new(parked, pane.reload_kind(), self.now))
    }

    /// The menu's own keymap: `L` reloads, `R` restarts, and anything else
    /// that backs out leaves the fields parked.
    ///
    /// `Escape` closes the pane rather than only the menu: it is the second
    /// press of the two the operator meant as "leave", and a menu that ate
    /// it would need a third.
    fn on_pane_menu_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::Action(verb @ (ActionVerb::Reload | ActionVerb::Restart)) => {
                self.apply_parked(verb)
            }
            KeyPress::Escape => {
                self.close_pane();
                Effect::None
            }
            KeyPress::Action(ActionVerb::Stop)
            | KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast
            | KeyPress::Refresh
            | KeyPress::Confirm
            | KeyPress::Edit
            | KeyPress::Cycle
            | KeyPress::Help
            | KeyPress::Settings
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete
            | KeyPress::Collapse => Effect::None,
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::Bleats
            // `NextGroup`/`Group`/`Undo` belong to the config pane: no other
            // screen has groups to walk or a filed edit set to undo.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo => Effect::None,
        }
    }

    /// Sends the menu's chosen verb against the pane's own sheep and closes
    /// the pane behind it.
    ///
    /// The same [`Sent::Action`] the dashboard's `arm` and `confirm` build,
    /// so [`Self::on_action_reply`] answers it unchanged. The menu is the
    /// confirm, so there is no second one.
    fn apply_parked(&mut self, verb: ActionVerb) -> Effect {
        // Read rather than left to `apply_offer`: the gate is one write to
        // `pane_menu` away from not covering this, and a send is not the
        // place to find that out.
        if self.control == Control::ReadOnly {
            self.notice = Some(Notice {
                text: READ_ONLY_REFUSAL.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        if let Some(text) = self.link_refusal() {
            self.notice = Some(Notice { text, grave: true });
            return Effect::None;
        }
        if self.action.is_some() {
            self.notice = Some(Notice {
                text: "one action is already in flight".to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(name) = self
            .config_pane()
            .map(|pane| pane.target().name().to_owned())
        else {
            return Effect::None;
        };
        let Some((target, count)) = self.flock_target(&name) else {
            self.notice = Some(Notice {
                text: format!("{name}: it is no longer in the flock"),
                grave: true,
            });
            self.close_pane();
            return Effect::None;
        };
        self.action = Some(Action {
            verb,
            target: target.clone(),
            name: name.clone(),
            count,
            at: self.now,
            stage: Stage::Sent,
        });
        self.close_pane();
        Effect::Send(Sent::Action { verb, target, name })
    }

    /// The row key `name` reaches, and how many processes that is.
    ///
    /// A name rather than [`Self::selected`]: a pane is opened per name and
    /// survives the table underneath it changing. [`None`] when the flock
    /// has no such sheep left.
    fn flock_target(&self, name: &str) -> Option<(RowKey, usize)> {
        let ids: Vec<u32> = self
            .flock
            .values()
            .filter(|row| row.info.name == name)
            .map(|row| row.info.id)
            .collect();
        match ids.as_slice() {
            [] => None,
            [id] => Some((RowKey::Sheep(*id), 1)),
            _ => Some((RowKey::Group(name.to_owned()), ids.len())),
        }
    }

    /// The refusal a locked row gets, in that row's own words.
    ///
    /// Two sentences for two facts, never one for both: shep refusing a
    /// config write is not the same as this pane having no widget for a
    /// shape a Flockfile writes perfectly well.
    ///
    /// A refused Structural field names the verb that moves it instead.
    /// The wildcard keeps a generic sentence for a Structural field this
    /// binary has no remedy for, rather than guessing a verb.
    fn lock_refusal(key: &str, lock: Lock) -> String {
        match lock {
            Lock::Refused => match key {
                "instances" => {
                    format!("{key} is not a config write; `shep stock` moves an instance count")
                }
                "name" => {
                    format!("{key} is not a config write; a name change is a different sheep")
                }
                _ => {
                    format!("{key} is not something a config write changes, from here or anywhere")
                }
            },
            Lock::NoWidget => {
                format!("{key} has no editor in this pane; a Flockfile still sets it")
            }
        }
    }

    /// `space` on the config pane. Arms the next value for the row under
    /// the cursor, or refuses and says why.
    ///
    /// The gate is [`Self::authorize_write`], the same one every settings
    /// write passes and for the same reason: a keystroke that changes a
    /// running flock's config needs the fat-finger catch a keystroke that
    /// stops a sheep has.
    fn cycle_field(&mut self) -> Effect {
        // The lock is checked ahead of the control gate, the same order
        // `confirm_field` takes: it is the more specific fact, and
        // `--allow-control` would not change it. A screen that answers
        // one question two ways teaches an operator to believe neither.
        if let Some((key, lock)) = self
            .config_pane()
            .and_then(ConfigPane::cursor_lock)
            .map(|(key, lock)| (key.to_owned(), lock))
        {
            self.notice = Some(Notice {
                text: Self::lock_refusal(&key, lock),
                grave: true,
            });
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        if let Some(pane) = self.config_pane_mut() {
            pane.cycle();
        }
        Effect::None
    }

    /// The operator's `Enter` on the config pane. Three meanings, picked in
    /// this order:
    ///
    /// - The cursor is on an env row or `+ add a key`: opens the env
    ///   editor, in place, on the same row.
    /// - The cursor is on an array field: opens the list sub-screen.
    /// - The cursor is on a typed field: opens the editor and switches
    ///   [`InputMode::Text`] on.
    ///
    /// All three go through [`Self::authorize_write`], the editor included,
    /// for the reason [`Self::confirm_setting`]'s own doc gives: the gate
    /// is checked on the keystroke that would file an edit, not on the
    /// close that writes them.
    fn confirm_field(&mut self) -> Effect {
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        if matches!(pane.cursor(), Some(PaneRow::Env(_) | PaneRow::AddEnv)) {
            if self.authorize_write().is_none() {
                return Effect::None;
            }
            if let Some(pane) = self.config_pane_mut() {
                pane.begin_env_typing();
                self.mode = InputMode::Text;
            }
            return Effect::None;
        }
        let Some(kind) = pane.cursor_kind().cloned() else {
            return Effect::None;
        };
        let locked = pane.cursor_lock().map(|(key, lock)| (key.to_owned(), lock));
        let opens = matches!(
            kind,
            FieldKind::List(_) | FieldKind::Text | FieldKind::Integer | FieldKind::Suggested(_)
        );
        // A row `Enter` was never going to open raises nothing at all: a
        // refusal about a key that was never going to act trains an
        // operator to ignore the status bar. A bool and a choice are
        // `space`'s job, and `space` works.
        if !opens && locked.is_none() {
            return Effect::None;
        }
        // The lock is checked ahead of the control gate: it is the more
        // specific of the two answers, and `--allow-control` would not
        // help. Each lock says its own thing; see [`Self::lock_refusal`].
        if let Some((key, lock)) = locked {
            self.notice = Some(Notice {
                text: Self::lock_refusal(&key, lock),
                grave: true,
            });
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let Some(pane) = self.config_pane_mut() else {
            return Effect::None;
        };
        if matches!(kind, FieldKind::List(_)) {
            pane.open_list();
        } else {
            pane.begin_typing();
            self.mode = InputMode::Text;
        }
        Effect::None
    }

    /// `d` on the config pane's field list. Files an edit that removes the
    /// operator's value for the field under the cursor, so the stored
    /// default shows through once sent: the same verb the list sub-screen's
    /// `d` performs on an element, which is why both carry
    /// [`KeyPress::Remove`].
    ///
    /// Same lock-then-control order as [`Self::cycle_field`] and
    /// [`Self::confirm_field`]: the lock names the more specific reason and
    /// `--allow-control` would not change it. Does nothing on an env row,
    /// on `+ add a key`, or with no row at all: [`ConfigPane::cursor_kind`]
    /// is `None` for exactly those, and there is no field to restore.
    fn restore_default(&mut self) -> Effect {
        if let Some((key, lock)) = self
            .config_pane()
            .and_then(ConfigPane::cursor_lock)
            .map(|(key, lock)| (key.to_owned(), lock))
        {
            self.notice = Some(Notice {
                text: Self::lock_refusal(&key, lock),
                grave: true,
            });
            return Effect::None;
        }
        if self
            .config_pane()
            .and_then(ConfigPane::cursor_kind)
            .is_none()
        {
            return Effect::None;
        }
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        if let Some(pane) = self.config_pane_mut() {
            pane.file_default();
        }
        Effect::None
    }

    /// The list sub-screen's own keymap, in force for as long as the pane
    /// holds one.
    ///
    /// `Escape` closes the sub-screen, not the pane, the same
    /// innermost-first rule the env screen follows. `Enter` or `e` opens
    /// the editor on the element under the cursor, or adds one on
    /// `+ new`. `d` removes, and `K`/`J` move the element one place.
    ///
    /// A removal and a move file the whole array, since that is what the
    /// write carries. Nothing goes out here: the pane's own `Escape` is
    /// what writes the set.
    fn on_list_key(&mut self, key: KeyPress) -> Effect {
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        match key {
            KeyPress::Quit => return Effect::Quit,
            KeyPress::Escape => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.close_list();
                }
                self.release_text_mode_if_unowned();
            }
            KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast => {
                if let Some(list) = self.config_pane_mut().and_then(ConfigPane::list_mut) {
                    match key {
                        KeyPress::SelectUp => list.move_by(-1),
                        KeyPress::SelectDown => list.move_by(1),
                        KeyPress::SelectFirst => list.move_to_first(),
                        KeyPress::SelectLast => list.move_to_last(),
                        _ => unreachable!(),
                    }
                }
            }
            KeyPress::Refresh => return self.reread_pane(),
            KeyPress::Confirm | KeyPress::Edit => {
                if self.authorize_write().is_none() {
                    return Effect::None;
                }
                if let Some(list) = self.config_pane_mut().and_then(ConfigPane::list_mut) {
                    list.begin_typing();
                    self.mode = InputMode::Text;
                }
            }
            KeyPress::Remove => {
                if self.authorize_write().is_none() {
                    return Effect::None;
                }
                if let Some(pane) = self.config_pane_mut() {
                    pane.file_list_removal();
                }
            }
            KeyPress::StepUp | KeyPress::StepDown => {
                if self.authorize_write().is_none() {
                    return Effect::None;
                }
                let delta = if key == KeyPress::StepUp { -1 } else { 1 };
                if let Some(pane) = self.config_pane_mut() {
                    pane.file_list_reorder(delta);
                }
            }
            KeyPress::Action(_)
            | KeyPress::Cycle
            | KeyPress::Settings
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Help
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete
            | KeyPress::Collapse => {}
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::Bleats => {}
            // Drops the newest edit, the same key the field list answers
            // and on the same terms: no control gate, since a key that
            // unfiles something cannot write. The sub-screen re-reads the
            // array from what is left filed, so the rows show what was
            // restored. The set holds one entry per field, so this takes
            // the whole array back to the shepherd's rather than one
            // keystroke of it.
            KeyPress::Undo => {
                if let Some(pane) = self.config_pane_mut() {
                    pane.undo_edit();
                }
            }
            // The groups belong to the field list; the sub-screen is one
            // field's own array and has none to walk.
            KeyPress::NextGroup | KeyPress::Group(_) => {}
        }
        Effect::None
    }

    /// The config pane's own text keymap, in force for as long as one of
    /// its two editors owns [`InputMode::Text`].
    ///
    /// Does not trim the buffer, for the reason
    /// [`Self::on_settings_text_key`]'s own doc gives: this repository does
    /// not widen an accepted input grammar without a basis in the spec.
    ///
    /// Both editors file on `TextApply`, and neither sends: the pane's own
    /// `Escape` writes the whole set.
    fn on_pane_text_key(&mut self, key: KeyPress) -> Effect {
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        if self.config_pane().is_some_and(|pane| pane.list().is_some()) {
            return self.on_list_text_key(key);
        }
        if self
            .config_pane()
            .is_some_and(|pane| pane.env_typing().is_some())
        {
            return self.on_env_text_key(key);
        }
        let Some(pane) = self.config_pane_mut() else {
            return Effect::None;
        };
        match key {
            KeyPress::TextChar(typed) => pane.type_char(typed),
            KeyPress::TextBackspace => pane.type_backspace(),
            KeyPress::TextApply => {
                pane.apply_typing();
                // `apply_typing` keeps the editor open on an integer buffer
                // that does not parse, so the mode follows what the pane
                // actually did rather than what the key asked for.
                if pane.typing().is_none() {
                    self.mode = InputMode::Normal;
                }
            }
            KeyPress::TextAbandon => {
                pane.abandon_typing();
                self.mode = InputMode::Normal;
            }
            _ => {}
        }
        Effect::None
    }

    /// The list sub-screen's own text keymap.
    ///
    /// `TextApply` files the whole array. An integer element whose buffer
    /// does not parse keeps the editor open, which is why the mode follows
    /// what the sub-screen did rather than what the key asked for.
    fn on_list_text_key(&mut self, key: KeyPress) -> Effect {
        // See the comment in `Self::take_pane_writes`: a direct field
        // match, not `Self::config_pane_mut`, so `self.mode` stays
        // reachable below.
        let Some(pane) = (match &mut self.body {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }) else {
            return Effect::None;
        };
        let Some(list) = pane.list_mut() else {
            return Effect::None;
        };
        match key {
            KeyPress::TextChar(typed) => list.type_char(typed),
            KeyPress::TextBackspace => list.type_backspace(),
            KeyPress::TextApply => {
                let applied = list.apply_typing();
                if list.typing().is_none() {
                    self.mode = InputMode::Normal;
                }
                if let Some(text) = applied {
                    pane.file_list_element(text);
                }
            }
            KeyPress::TextAbandon => {
                list.abandon_typing();
                self.mode = InputMode::Normal;
            }
            _ => {}
        }
        Effect::None
    }

    /// The env editor's own text keymap, in force for as long as
    /// [`ConfigPane::env_typing`] is `Some`.
    ///
    /// `TextApply` files, exactly as the field editor's does: nothing on
    /// an env row reaches the shepherd before the pane closes, so an env
    /// key typed by mistake costs an `u` rather than an override the
    /// operator cannot read back.
    fn on_env_text_key(&mut self, key: KeyPress) -> Effect {
        // See the comment in `Self::take_pane_writes`: a direct field
        // match, not `Self::config_pane_mut`, so `self.mode` stays
        // reachable below.
        let Some(pane) = (match &mut self.body {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }) else {
            return Effect::None;
        };
        match key {
            KeyPress::TextChar(typed) => pane.type_env_char(typed),
            KeyPress::TextBackspace => pane.type_env_backspace(),
            KeyPress::TextApply => {
                pane.apply_env_typing();
                self.mode = InputMode::Normal;
            }
            KeyPress::TextAbandon => {
                pane.abandon_env_typing();
                self.mode = InputMode::Normal;
            }
            _ => {}
        }
        Effect::None
    }

    /// The [`WriteAuthority`] every settings write path has to hold, or
    /// [`None`] with the refusal already raised.
    ///
    /// The one place [`Control`] is read on this screen: `space` on a scalar or
    /// a dog, and `Enter` opening or applying an edit, all come through here.
    fn authorize_write(&mut self) -> Option<WriteAuthority> {
        let authority = WriteAuthority::granted(self);
        if authority.is_none() {
            self.notice = Some(Notice {
                text: READ_ONLY_REFUSAL.to_string(),
                grave: true,
            });
        }
        authority
    }

    /// `space` on the settings screen: arms a candidate for the cursor's row,
    /// or refuses through [`Self::authorize_write`].
    fn cycle_setting(&mut self) -> Effect {
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let Some(cursor) = self.settings().and_then(Settings::cursor) else {
            return Effect::None;
        };
        match cursor {
            SettingsRow::Scalar(field) => self.cycle_scalar(field),
            SettingsRow::Dog(index) => self.cycle_dog(index),
        }
    }

    /// `space` on one of the six scalar rows. Re-arms when a candidate is
    /// already armed, so a second `space` walks one step further along the
    /// cycle. Does nothing on the two free-text fields.
    ///
    /// Replaces a [`Pending::Sent`] outright rather than refusing over it:
    /// the write it names is local file I/O the operator need not wait on,
    /// and its answer can no longer reach the edit armed here. See
    /// [`Settings::pending`].
    fn cycle_scalar(&mut self, field: SettingField) -> Effect {
        // See the comment in `Self::take_pane_writes`: a direct field
        // match, not `Self::settings_mut`, so `self.now` stays reachable
        // below.
        let Some(settings) = (match &mut self.body {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }) else {
            return Effect::None;
        };
        let Some(value) = settings.next_candidate(field) else {
            return Effect::None;
        };
        let source = settings.source_of(field);
        let text = confirm_text(field, &value, source);
        settings.pending = Some(Pending::Armed {
            edit: SettingEdit::Set { field, value },
            text,
            at: self.now,
        });
        Effect::None
    }

    /// `space` on a [`SettingsRow::Dog`] row: arms the opposite of the file's
    /// `enabled` bit, refusing in [`LINK_GONE`]'s words while the link is gone.
    ///
    /// The link check is this row's own, unlike [`Self::cycle_scalar`]: a
    /// confirmed toggle ends in a request to the shepherd. Replaces a
    /// [`Pending::Sent`] for that method's reason.
    fn cycle_dog(&mut self, index: usize) -> Effect {
        if matches!(self.link, Link::Lost { .. }) {
            self.notice = Some(Notice {
                text: LINK_GONE.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        // See the comment in `Self::take_pane_writes`: a direct field
        // match, not `Self::settings_mut`, so `self.now` stays reachable
        // below.
        let Some(settings) = (match &mut self.body {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }) else {
            return Effect::None;
        };
        let Some(dog) = settings.snapshot.dogs.get(index) else {
            return Effect::None;
        };
        let name = dog.name.clone();
        let enable = !dog.enabled;
        let text = if enable {
            format!("enable {name}? it starts now, no reload")
        } else {
            format!("disable {name}? it stops now and is deregistered")
        };
        settings.pending = Some(Pending::DogArmed {
            edit: DogEdit { name, enable },
            text,
            at: self.now,
        });
        Effect::None
    }

    /// The operator's `Enter` on the settings screen. On a free-text row with
    /// nothing pending it opens [`Pending::Typing`] and switches
    /// [`InputMode::Text`] on; on an armed candidate it sends and moves to
    /// [`Pending::Sent`]; anything else is untouched.
    ///
    /// Both acting cases go through [`Self::authorize_write`]. Opening the
    /// editor is gated as well as applying it, so the refusal arrives before a
    /// whole socket path is typed.
    fn confirm_setting(&mut self) -> Effect {
        let Some(settings) = self.settings() else {
            return Effect::None;
        };
        let opens_editor = settings.pending.is_none()
            && matches!(
                settings.cursor(),
                Some(SettingsRow::Scalar(
                    SettingField::Socket | SettingField::MaxCronSleep
                ))
            );
        if !opens_editor && !settings.is_armed() {
            return Effect::None;
        }
        let Some(authority) = self.authorize_write() else {
            return Effect::None;
        };
        let Some(settings) = self.settings_mut() else {
            return Effect::None;
        };
        if opens_editor {
            let Some(SettingsRow::Scalar(field)) = settings.cursor() else {
                return Effect::None;
            };
            let buffer = settings.text_seed(field).to_string();
            settings.pending = Some(Pending::Typing { field, buffer });
            self.mode = InputMode::Text;
            return Effect::None;
        }
        // Minted before the borrow below, and spent on either arm that
        // sends: past the gate above, the pending edit is armed.
        let ticket = self.take_write_ticket();
        let Some(settings) = self.settings_mut() else {
            return Effect::None;
        };
        match settings.pending.take() {
            Some(Pending::Armed { edit, text, .. }) => {
                settings.pending = Some(Pending::Sent { text, ticket });
                Effect::WriteSetting {
                    edit,
                    ticket,
                    authority,
                }
            }
            Some(Pending::DogArmed { edit, text, .. }) => {
                settings.pending = Some(Pending::Sent { text, ticket });
                Effect::WriteDog {
                    edit,
                    ticket,
                    authority,
                }
            }
            other => {
                settings.pending = other;
                Effect::None
            }
        }
    }

    /// Why the shepherd cannot be sent to right now, if it cannot.
    ///
    /// Shared by the dashboard's action keys and the pane's apply menu: a
    /// dead link refuses the same way whichever door an operator used.
    fn link_refusal(&self) -> Option<String> {
        match self.link {
            // Not `LINK_GONE`: the ladder is still running.
            Link::Retrying { attempt } => Some(retrying_sentence(attempt)),
            // The ladder is exhausted, so the shepherd really is gone.
            Link::Lost { .. } => Some(LINK_GONE.to_string()),
            _ => None,
        }
    }

    /// The refusal ladder shared by [`Self::arm`] and [`Self::arm_sheep_pane`]:
    /// the gate, the link. Neither caller's own target-specific refusal
    /// (nothing selected, one action already in flight, the pane's pinned
    /// sheep is gone) lives here, since the two callers order those three
    /// differently: `arm` asks "nothing selected" before "one already in
    /// flight", `arm_sheep_pane` cannot ask the first (the pane would not be
    /// open without a sheep) so only asks the second. Folding "in flight"
    /// in here once put it ahead of `arm`'s "nothing selected" for every
    /// caller, which is the bug this comment now exists to keep out.
    fn confirm_refusal(&self) -> Option<String> {
        if self.control == Control::ReadOnly {
            Some(READ_ONLY_REFUSAL.to_string())
        } else {
            self.link_refusal()
        }
    }

    /// Arms a confirm, or refuses and says why.
    ///
    /// Every refusal happens here rather than at confirm time, so an operator
    /// never answers a question that was never going to be honoured. The
    /// ladder is [`Self::confirm_refusal`]'s own gate and link, then nothing
    /// selected, then one action already in flight, but this method checks
    /// nothing selected first, since a keypress with no target asked a
    /// question that was never about the in-flight action at all.
    fn arm(&mut self, verb: ActionVerb) -> Effect {
        if let Some(text) = self.confirm_refusal() {
            self.notice = Some(Notice { text, grave: true });
            return Effect::None;
        }
        let Some(key) = self.selected.clone() else {
            self.notice = Some(Notice {
                text: "no sheep is selected".to_string(),
                grave: true,
            });
            return Effect::None;
        };
        if self.action.is_some() {
            self.notice = Some(Notice {
                text: "one action is already in flight".to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let (target, name, count) = match &key {
            RowKey::Sheep(id) => {
                let row = self
                    .flock
                    .get(id)
                    .expect("a selected sheep is in the flock");
                (RowKey::Sheep(*id), row.info.name.clone(), 1)
            }
            RowKey::Group(group_name) => {
                let count = self
                    .flock
                    .values()
                    .filter(|row| &row.info.name == group_name)
                    .count();
                (RowKey::Group(group_name.clone()), group_name.clone(), count)
            }
            RowKey::Fold(fold_name) => {
                let count = self
                    .flock
                    .values()
                    .filter(|row| row.info.fold.as_deref() == Some(fold_name.as_str()))
                    .count();
                (RowKey::Fold(fold_name.clone()), fold_name.clone(), count)
            }
            RowKey::Section(_) => unreachable!("a header is never selectable"),
        };
        self.action = Some(Action {
            verb,
            target,
            name,
            count,
            at: self.now,
            stage: Stage::Armed,
        });
        Effect::None
    }

    /// `x`/`R`/`L` from inside the sheep pane: arms a confirm against the
    /// pane's own pinned sheep, never [`Self::selected`].
    ///
    /// Arming against the selection here would be the same mistake
    /// [`Self::sheep_pane_row`]'s own doc explains: `Msg::Snapshot` reseats
    /// the selection whatever screen is showing, so a pinned sheep that
    /// leaves the flock would arm an action against whichever sheep
    /// replaced it while the pane still names the first. Refuses instead.
    ///
    /// The ladder is [`Self::confirm_refusal`]'s own gate and link, then one
    /// action already in flight, same order [`Self::arm`] uses for those
    /// two; [`Self::arm`]'s "nothing selected" case cannot happen here,
    /// since the pane would not be open without a sheep, so its place is
    /// taken by the pinned sheep having left instead.
    fn arm_sheep_pane(&mut self, verb: ActionVerb) -> Effect {
        if let Some(text) = self.confirm_refusal() {
            self.notice = Some(Notice { text, grave: true });
            return Effect::None;
        }
        if self.action.is_some() {
            self.notice = Some(Notice {
                text: "one action is already in flight".to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(row) = self.sheep_pane_row() else {
            self.notice = Some(Notice {
                text: "that sheep is no longer in the flock".to_string(),
                grave: true,
            });
            return Effect::None;
        };
        self.action = Some(Action {
            verb,
            target: RowKey::Sheep(row.info.id),
            name: row.info.name.clone(),
            count: 1,
            at: self.now,
            stage: Stage::Armed,
        });
        Effect::None
    }

    /// The operator's Enter. Sends, or refuses because the target left.
    fn confirm(&mut self) -> Effect {
        let Some(action) = self.action.take() else {
            return Effect::None;
        };
        // The whole flock, not the visible set: a filter typed after arming
        // hides a sheep, it does not remove it.
        if !self.target_present(&action.target) {
            self.notice = Some(Notice {
                text: format!(
                    "{}: it is no longer in the flock",
                    target_prefix(action.verb, &action.target, &action.name)
                ),
                grave: true,
            });
            return Effect::None;
        }
        let sent = Sent::Action {
            verb: action.verb,
            target: action.target.clone(),
            name: action.name.clone(),
        };
        self.action = Some(Action {
            stage: Stage::Sent,
            ..action
        });
        Effect::Send(sent)
    }

    /// Whether `target` still has at least one process in the flock: a
    /// single sheep by id, or a group by whether any instance of its name
    /// remains.
    fn target_present(&self, target: &RowKey) -> bool {
        match target {
            RowKey::Sheep(id) => self.flock.contains_key(id),
            RowKey::Group(name) => self.flock.values().any(|row| &row.info.name == name),
            RowKey::Fold(name) => self
                .flock
                .values()
                .any(|row| row.info.fold.as_deref() == Some(name.as_str())),
            RowKey::Section(_) => unreachable!("a header is never an action target"),
        }
    }

    /// Differences one CPU sample per sheep in the current flock against its
    /// last reading, buffers RSS as read, appends the flock-wide CPU sum,
    /// and drops every history entry (CPU, RSS and baseline) for a sheep the
    /// new snapshot no longer carries.
    ///
    /// Called after `self.flock` is replaced, so it reads the fresh
    /// snapshot rather than the one before it. A sheep with no CPU reading
    /// contributes `0.0` and forgets its baseline: skipping the sample would
    /// slide the whole window and make an old spike look recent, and
    /// keeping the baseline would difference the next live reading across
    /// the gap.
    ///
    /// Every touched deque is made contiguous here, while this method still
    /// holds `&mut self`, so [`Self::cpu_history`], [`Self::rss_history`]
    /// and [`Self::flock_cpu_history`] can hand out a slice from `&self`
    /// alone.
    fn record_samples(&mut self, at: Instant) {
        // Collected first: the differencing below needs `&mut self.cpu_last`
        // while a walk of `self.flock` would still be borrowing it.
        let readings: Vec<(u32, Option<u32>, Option<u64>, u64)> = self
            .flock
            .values()
            .map(|row| {
                (
                    row.info.id,
                    row.info.pid,
                    row.info.cpu_ms,
                    row.info.memory_bytes.unwrap_or(0),
                )
            })
            .collect();
        let mut sum = 0.0;
        for (id, pid, cpu_ms, rss) in readings {
            let rss_history = self.rss_history.entry(id).or_default();
            rss_history.push_back(rss);
            if rss_history.len() > HISTORY {
                rss_history.pop_front();
            }
            rss_history.make_contiguous();

            let cpu = match cpu_ms {
                None => {
                    self.cpu_last.remove(&id);
                    0.0
                }
                Some(now_ms) => match self.cpu_last.insert(id, (pid, now_ms, at)) {
                    // Nothing behind this reading to difference. The buffer
                    // stays one short of the poll count rather than claiming
                    // an idle sample it never measured.
                    None => continue,
                    // A respawn keeps the sheep's id and takes a new pid, and
                    // `cpu_ms` counts the tree under whichever pid the
                    // shepherd is watching now. Differencing across that
                    // boundary subtracts a dead process's counter from a live
                    // one's: `saturating_sub` keeps it from ever reading as a
                    // spike, but it still underreports the new process by
                    // exactly what the old one had spent. A new process is a
                    // first reading, so it records a baseline and appends
                    // nothing, the same as a sheep the pane has never seen.
                    Some((then_pid, _, _)) if then_pid != pid => continue,
                    Some((_, then_ms, then)) => shep_core::values::cpu_percent(
                        now_ms.saturating_sub(then_ms),
                        at.saturating_duration_since(then),
                    )
                    .unwrap_or(0.0),
                },
            };
            sum += cpu;
            let history = self.cpu_history.entry(id).or_default();
            history.push_back(cpu);
            if history.len() > HISTORY {
                history.pop_front();
            }
            history.make_contiguous();
        }
        self.cpu_history.retain(|id, _| self.flock.contains_key(id));
        self.rss_history.retain(|id, _| self.flock.contains_key(id));
        // A departed sheep's baseline outlives its rows here unless dropped
        // too: without this, a later id reused by an unrelated sheep would
        // inherit a stranger's counter and difference its first honest
        // reading against it, breaking the exact guarantee
        // `Self::cpu_history`'s doc makes about a later id inheriting
        // nothing.
        self.cpu_last.retain(|id, _| self.flock.contains_key(id));
        self.flock_cpu.push_back(sum);
        if self.flock_cpu.len() > HISTORY {
            self.flock_cpu.pop_front();
        }
        self.flock_cpu.make_contiguous();
    }

    /// One sheep's CPU-percent samples, oldest first, newest last.
    ///
    /// Empty for a sheep with no history yet, and for one that has left the
    /// flock: [`Self::record_samples`] drops its entry entirely.
    #[must_use]
    pub fn cpu_history(&self, id: u32) -> &[f32] {
        self.cpu_history
            .get(&id)
            .map_or(&[][..], |history| history.as_slices().0)
    }

    /// `id`'s newest differenced CPU sample: [`Self::cpu_history`]'s last
    /// entry, the same number its sparkline's last cell draws.
    ///
    /// `None` in two cases, both honest gaps rather than a claimed zero:
    /// before a first difference exists (one poll after launch, on
    /// [`Self::cpu_history`]'s own terms), and when the current snapshot's
    /// `cpu_ms` is itself `None` (the sheep is not running, or the peer
    /// daemon predates the field). The second check matters because
    /// [`Self::record_samples`] still appends a zero to the history buffer
    /// in that case, to keep the sparkline's window from sliding; reading
    /// that zero back as a figure would report "0.0%" for a sheep whose CPU
    /// was never sampled, the same false claim `ProcessInfo::cpu_percent`'s
    /// own `None` exists to refuse.
    ///
    /// Every CPU figure lookout draws reads through here rather than
    /// `ProcessInfo::cpu_percent`, the shepherd's own mean over a window
    /// that resets independently of this pane's polls: reading both would
    /// put two different numbers under one label.
    #[must_use]
    pub fn cpu_now(&self, id: u32) -> Option<f32> {
        self.flock.get(&id)?.info.cpu_ms?;
        self.cpu_history(id).last().copied()
    }

    /// One sheep's RSS samples in bytes, oldest first, newest last.
    ///
    /// Empty for a sheep with no history yet and for one that has left the
    /// flock, on [`Self::cpu_history`]'s terms.
    #[must_use]
    pub fn rss_history(&self, id: u32) -> &[u64] {
        self.rss_history
            .get(&id)
            .map_or(&[][..], |series| series.as_slices().0)
    }

    /// The whole flock's summed CPU-percent samples, oldest first, newest
    /// last, same depth as [`Self::cpu_history`].
    pub fn flock_cpu_history(&self) -> &[f32] {
        self.flock_cpu.as_slices().0
    }

    /// The ceiling every row's CPU sparkline scales against: the busiest
    /// sample any sheep has recorded in the retained window.
    ///
    /// One ceiling shared by every row is what makes the column comparable
    /// down the table. Per-row peaks make an idle sheep and a busy one both
    /// fill their own cells; a fixed 100% of a core makes an ordinary flock,
    /// where nothing is above two percent, draw a screen of flat lines.
    ///
    /// Floored at [`CPU_CEILING_FLOOR`] so a flock that is genuinely doing
    /// nothing stays flat instead of having its rounding noise stretched
    /// into a shape. Below that floor there is nothing to see and saying so
    /// is the honest answer.
    #[must_use]
    pub fn cpu_ceiling(&self) -> f32 {
        self.cpu_history
            .values()
            .flat_map(|series| series.iter().copied())
            .fold(CPU_CEILING_FLOOR, f32::max)
    }

    /// Takes an armed prompt off the screen once its target is gone, rather
    /// than leaving a question about nothing. An action already in flight
    /// keeps its line.
    fn forget_missing_target(&mut self) {
        let gone = self.action.as_ref().is_some_and(|action| {
            action.stage == Stage::Armed && !self.target_present(&action.target)
        });
        if gone {
            self.action = None;
        }
    }

    /// Takes an armed prompt off the screen when the link stops being live.
    ///
    /// On a frozen dashboard it would never expire either, since `now` stops
    /// advancing and the expiry check rides it. An action already sent keeps
    /// its line: `run_connected` answers it with an `Err` before its loop ends.
    fn disarm_on_link_change(&mut self) {
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.stage == Stage::Armed)
        {
            self.action = None;
        }
    }

    /// The text keymap's router: the filter box while the settings screen is
    /// closed, [`Self::on_settings_text_key`]'s editor while it is open. The
    /// two never both own [`InputMode::Text`].
    fn on_text_key(&mut self, key: KeyPress) -> Effect {
        // Six now, and the split is still total: the config pane, the
        // settings screen, the bleats pane, the secrets pane and the sheep
        // pane cannot coexist with each other (`e`, `s` and `S` reach the
        // dashboard only from the dashboard, and `b`/`↵` only from there
        // too), and none of them
        // coexist with the dashboard's own filter box, which `Msg::Settings`'s
        // own arm closed the window on.
        if self.config_pane().is_some() {
            return self.on_pane_text_key(key);
        }
        if self.settings().is_some() {
            return self.on_settings_text_key(key);
        }
        if self.bleats_pane().is_some() {
            return self.on_bleats_text_key(key);
        }
        if matches!(self.body, Body::Secrets(_)) {
            return self.on_secrets_text_key(key);
        }
        if self.sheep_pane().is_some() {
            return self.on_sheep_feed_text_key(key);
        }
        self.on_filter_text_key(key)
    }

    /// The filter box's keymap.
    ///
    /// Ctrl-C still quits: in raw mode it is a key event, not a signal. Does
    /// not clear [`Self::notice`], unlike normal mode, since a notice can be
    /// raised with no keypress involved; the status bar hides it under the box
    /// and shows it again when the box closes.
    fn on_filter_text_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::TextChar(typed) => {
                let mut query = self.filter.clone();
                query.push(typed);
                self.set_filter(query)
            }
            KeyPress::TextBackspace => {
                let mut query = self.filter.clone();
                query.pop();
                self.set_filter(query)
            }
            KeyPress::TextApply => {
                self.mode = InputMode::Normal;
                Effect::None
            }
            KeyPress::TextAbandon => {
                self.mode = InputMode::Normal;
                self.set_filter(String::new())
            }
            _ => Effect::None,
        }
    }

    /// The settings editor's own text keymap, in force while a
    /// [`Pending::Typing`] owns [`InputMode::Text`]. The buffer is never
    /// trimmed.
    ///
    /// `TextApply` arms rather than writes: an empty buffer becomes
    /// [`SettingEdit::Unset`], anything else [`SettingEdit::Set`], and the next
    /// `Enter` sends it. `TextAbandon` leaves the screen open.
    fn on_settings_text_key(&mut self, key: KeyPress) -> Effect {
        let now = self.now;
        let Some(settings) = self.settings_mut() else {
            return Effect::None;
        };
        match key {
            KeyPress::Quit => return Effect::Quit,
            KeyPress::TextChar(typed) => {
                if let Some(Pending::Typing { buffer, .. }) = settings.pending.as_mut() {
                    buffer.push(typed);
                }
            }
            KeyPress::TextBackspace => {
                if let Some(Pending::Typing { buffer, .. }) = settings.pending.as_mut() {
                    buffer.pop();
                }
            }
            KeyPress::TextApply => {
                if let Some(Pending::Typing { field, buffer }) = settings.pending.take() {
                    let edit = if buffer.is_empty() {
                        SettingEdit::Unset { field }
                    } else {
                        SettingEdit::Set {
                            field,
                            value: buffer,
                        }
                    };
                    let text = confirm_text_for_edit(&edit);
                    settings.pending = Some(Pending::Armed {
                        edit,
                        text,
                        at: now,
                    });
                }
                self.mode = InputMode::Normal;
            }
            KeyPress::TextAbandon => {
                settings.pending = None;
                self.mode = InputMode::Normal;
            }
            _ => {}
        }
        Effect::None
    }

    /// The rows the table draws, in `(name, instance, id)` order: the whole
    /// flock, or whatever the filter leaves of it.
    ///
    /// Under [`Grouping::Flat`] that is a "Flock" section and a "Dogs"
    /// section. Under [`Grouping::ByFold`] it is one [`RowKey::Fold`] header
    /// per fold, a "no fold" section and a "Dogs" section, built by
    /// [`Self::push_fold_rows`].
    ///
    /// A [`RowKey::Group`] header comes immediately before its own
    /// [`RowKey::Sheep`] entries, and a [`RowKey::Section`] header only when
    /// its side has a row to introduce. The sort key is total on purpose: the
    /// table repolls every two seconds, and a partial one would let two
    /// instances swap places under the cursor. Every cursor move reads this
    /// sequence and nothing else.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<RowKey> {
        let needle = self.filter.to_lowercase();
        let mut visible: Vec<RowEntry<'_>> = self
            .flock
            .iter()
            .filter(|(_, row)| needle.is_empty() || row.info.name.to_lowercase().contains(&needle))
            .map(|(id, row)| {
                (
                    row.info.name.as_str(),
                    row.info.instance,
                    *id,
                    row.info.dog.is_some(),
                )
            })
            .collect();
        visible.sort_unstable_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let (dogs, sheep): (Vec<_>, Vec<_>) = visible.into_iter().partition(|entry| entry.3);

        let mut out = Vec::new();
        match self.grouping {
            Grouping::Flat => {
                if !sheep.is_empty() {
                    out.push(RowKey::Section("Flock"));
                    self.push_grouped_rows(&sheep, &mut out);
                }
            }
            Grouping::ByFold => self.push_fold_rows(&sheep, &mut out),
        }
        if !dogs.is_empty() {
            out.push(RowKey::Section("Dogs"));
            self.push_grouped_rows(&dogs, &mut out);
        }
        out
    }

    /// The `ByFold` half of [`Self::visible_rows`]: `sheep` gathered under
    /// its fold, each fold's own name-order header pushed before
    /// [`Self::push_fold_group_rows`] lays down its members, so a
    /// multi-instance app inside a fold keeps its own [`RowKey::Group`] row
    /// rather than flattening to one row per instance.
    ///
    /// Sheep carrying no fold land under [`RowKey::Section`]`("no fold")`
    /// instead of a [`RowKey::Fold`]: `SelectorSpec::Fold` cannot name "no
    /// fold" on the wire, so a header rather than an action target is the
    /// honest answer.
    ///
    /// A fold in [`Self::collapsed_folds`] still gets its own
    /// [`RowKey::Fold`] header; only [`Self::push_fold_group_rows`]'s call
    /// is skipped, so `z` hides the members and nothing else.
    fn push_fold_rows(&self, sheep: &[RowEntry<'_>], out: &mut Vec<RowKey>) {
        let mut folds: BTreeMap<String, Vec<RowEntry<'_>>> = BTreeMap::new();
        let mut unfoldered: Vec<RowEntry<'_>> = Vec::new();
        for entry in sheep {
            match self
                .flock
                .get(&entry.2)
                .and_then(|row| row.info.fold.clone())
            {
                Some(fold) => folds.entry(fold).or_default().push(*entry),
                None => unfoldered.push(*entry),
            }
        }
        for (fold, members) in &folds {
            out.push(RowKey::Fold(fold.clone()));
            if !self.collapsed_folds.contains(fold) {
                self.push_fold_group_rows(members, out);
            }
        }
        if !unfoldered.is_empty() {
            out.push(RowKey::Section("no fold"));
            self.push_fold_group_rows(&unfoldered, out);
        }
    }

    /// [`Self::push_grouped_rows`]'s fold-scoped twin: a grouped app
    /// collapses to its own [`RowKey::Group`] header alone, with no
    /// [`RowKey::Sheep`] row following it, so a fold never nests three
    /// levels deep (fold, app, instance). An app with no group still gets
    /// its ordinary sheep row, exactly as the flat path would draw it.
    fn push_fold_group_rows(&self, entries: &[RowEntry<'_>], out: &mut Vec<RowKey>) {
        for run in name_runs(entries) {
            let name = run[0].0;
            if self.is_grouped(name) {
                out.push(RowKey::Group(name.to_string()));
            } else {
                out.extend(run.iter().map(|entry| RowKey::Sheep(entry.2)));
            }
        }
    }

    /// Appends `entries`' rows to `out`, splicing a [`RowKey::Group`] header
    /// before a grouped app's instances.
    fn push_grouped_rows(&self, entries: &[RowEntry<'_>], out: &mut Vec<RowKey>) {
        for run in name_runs(entries) {
            let name = run[0].0;
            if self.is_grouped(name) {
                out.push(RowKey::Group(name.to_string()));
            }
            out.extend(run.iter().map(|entry| RowKey::Sheep(entry.2)));
        }
    }

    /// Whether `row` names a dog. A header and a group row are neither.
    #[cfg(test)]
    fn is_dog_row(&self, row: &RowKey) -> bool {
        match row {
            RowKey::Sheep(id) => self.flock.get(id).is_some_and(|r| r.info.dog.is_some()),
            RowKey::Group(_) | RowKey::Section(_) | RowKey::Fold(_) => false,
        }
    }

    fn visible_len(&self) -> usize {
        self.visible_rows().len()
    }

    /// Puts the selection back on a real row after the flock changed, and
    /// reports whether it moved.
    ///
    /// `previous_index` is where the selection sat before the change, read
    /// while the old map was still in place. A surviving key is left alone; a
    /// lost one falls to whatever now occupies that position, clamped to the
    /// last row rather than to row 0.
    fn reseat(&mut self, previous_index: Option<usize>) -> bool {
        // `selected_index`, not `flock.contains_key`: a selection the filter
        // hides is not seated, however present its id is. Must come before the
        // emptiness check below, which would otherwise return early for a query
        // that matches no sheep.
        if self.selected_index().is_some() {
            return false;
        }
        let before = self.selected.clone();
        if self.visible_rows().is_empty() {
            self.selected = None;
            return before != self.selected;
        }
        self.select_at(previous_index.unwrap_or(0), 1);
        before != self.selected
    }

    /// Moves the selection by `delta` rows and reports whether it moved.
    /// Clamped rather than wrapping.
    fn select_by(&mut self, delta: isize) -> Effect {
        let Some(index) = self.selected_index() else {
            return Effect::None;
        };
        let next = index.saturating_add_signed(delta);
        let direction = if delta < 0 { -1 } else { 1 };
        self.select_at(next, direction)
    }

    /// Selects the row at `index`, clamped to the flock, and reports whether
    /// that changed anything.
    ///
    /// `direction` is which way to search past a [`RowKey::Section`] header,
    /// and a header at row 0 searches forward whatever it says.
    ///
    /// `Effect::None` when nothing changed: [`Effect::RefreshSelected`] reads
    /// two files and asks the shepherd for lambs, and a held `k` at the top
    /// must not do that once per keypress.
    fn select_at(&mut self, index: usize, direction: isize) -> Effect {
        let visible = self.visible_rows();
        if visible.is_empty() {
            return Effect::None;
        }
        let mut index = index.min(visible.len() - 1);
        if matches!(visible[index], RowKey::Section(_)) {
            index = if direction < 0 && index > 0 {
                index - 1
            } else {
                index + 1
            };
        }
        let next = visible[index].clone();
        if Some(&next) == self.selected.as_ref() {
            return Effect::None;
        }
        self.selected = Some(next);
        // A frozen dashboard re-reading live log files would put content on
        // screen newer than the banner over it. The cursor still moves, and the
        // detail pane re-renders from the frozen listing.
        if matches!(self.link, Link::Lost { .. }) {
            return Effect::None;
        }
        Effect::RefreshSelected
    }

    /// Every sheep the table's rows are drawn from, in name-then-id order: the
    /// whole flock, or whatever the filter leaves of it.
    ///
    /// A flat sheep list, not [`Self::visible_rows`]'s [`RowKey`] sequence: the
    /// title bar counts this, and a group header is not a sheep.
    #[must_use]
    pub fn rows(&self) -> Vec<&Row> {
        let needle = self.filter.to_lowercase();
        let mut visible: Vec<&Row> = self
            .flock
            .values()
            .filter(|row| needle.is_empty() || row.info.name.to_lowercase().contains(&needle))
            .collect();
        visible.sort_unstable_by(|a, b| {
            (a.info.name.as_str(), a.info.id).cmp(&(b.info.name.as_str(), b.info.id))
        });
        visible
    }

    /// Every sheep the shepherd last reported, in id order, whatever the filter
    /// hides.
    ///
    /// The host strip sums this rather than [`Self::rows`], so a name filter
    /// cannot narrow what `flock cpu`/`flock mem` add up to while the label
    /// still says `flock`.
    #[must_use]
    pub fn all_rows(&self) -> Vec<&Row> {
        self.flock.values().collect()
    }

    /// Replaces the filter and puts the selection back on a visible sheep: a
    /// keystroke that narrows the query can hide the selected one.
    fn set_filter(&mut self, query: String) -> Effect {
        if self.filter == query {
            return Effect::None;
        }
        let previous = self.selected_index();
        self.filter = query;
        if self.reseat(previous) && !matches!(self.link, Link::Lost { .. }) {
            // The cursor moved, so the feed and the lambs are about to describe
            // a different sheep.
            return Effect::RefreshSelected;
        }
        Effect::None
    }

    /// The filter as typed, empty when there is none.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Which keymap is currently in force.
    #[must_use]
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// How many sheep the shepherd last reported, whatever the filter hides.
    #[must_use]
    pub fn flock_len(&self) -> usize {
        self.flock.len()
    }

    /// The selected row, or `None` for an empty flock.
    #[must_use]
    pub fn selected(&self) -> Option<RowKey> {
        self.selected.clone()
    }

    /// Which row of [`Self::visible_rows`] the selection sits on, derived every
    /// call rather than stored.
    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        let key = self.selected.clone()?;
        self.visible_rows().iter().position(|row| *row == key)
    }

    /// The selected sheep's row, which the detail pane and the feed read.
    /// `None` for a [`RowKey::Group`] selection as well as for none at all: a
    /// group has no single sheep to describe.
    #[must_use]
    pub fn selected_row(&self) -> Option<&Row> {
        match &self.selected {
            Some(RowKey::Sheep(id)) => self.flock.get(id),
            _ => None,
        }
    }

    /// Scrolls the bleats pane back by `amount`, then holds the offset at the
    /// last value that changes the frame.
    ///
    /// The ceiling has to live here rather than on the pane: it depends on
    /// the surviving lines and their wrapped heights, which the pane cannot
    /// see. Without it `scroll_up` saturating-adds forever and `j` stops
    /// appearing to work, since the render clamps while the stored value
    /// keeps climbing.
    fn scroll_bleats_back(&mut self, amount: usize) {
        let ceiling = self.bleats_pane().map_or(0, |pane| {
            super::view::bleats_full::max_scroll_offset(self, pane)
        });
        if let Some(pane) = self.bleats_pane_mut() {
            pane.scroll_up(amount);
            pane.clamp_scroll(ceiling);
        }
    }

    /// The row whose log files the feed should read: the bleats pane's
    /// pinned sheep while that pane is open, the sheep pane's own pinned
    /// sheep while its embedded feed is open, and the selection otherwise.
    ///
    /// The three are not the same and the difference is operator-visible.
    /// Both panes pin one sheep for their lifetime, but `Msg::Snapshot`
    /// reseats the selection whatever screen is showing, so a pinned sheep
    /// leaving the flock moves the selection to another one. Reading the
    /// selection here would then draw that other sheep's lines under a
    /// title still naming the pinned sheep, which is one sheep's output
    /// presented as another's.
    ///
    /// `None` once the pinned sheep is gone, so the pane shows its own
    /// "no longer in the flock" title over nothing rather than over somebody
    /// else's log.
    #[must_use]
    pub fn feed_row(&self) -> Option<&Row> {
        match self.bleats_pane() {
            Some(pane) => match pane.sheep() {
                RowKey::Sheep(id) => self.flock.get(id),
                _ => None,
            },
            None => match self.sheep_pane() {
                Some(pane) => match pane.feed_sheep() {
                    RowKey::Sheep(id) => self.flock.get(id),
                    _ => None,
                },
                None => self.selected_row(),
            },
        }
    }

    /// The selected row's app name: a sheep's own, or a group row's.
    ///
    /// Unlike [`Self::selected_row`] this answers for a group too: a config
    /// pane is about the stored spec every instance of an app shares, which
    /// a group row names exactly, while the detail pane and feed describe
    /// one process and have nothing to show for a group.
    #[must_use]
    pub fn selected_name(&self) -> Option<String> {
        match &self.selected {
            Some(RowKey::Group(name)) => Some(name.clone()),
            Some(RowKey::Sheep(id)) => self.flock.get(id).map(|row| row.info.name.clone()),
            // A fold names no single app config to fetch: `e` on a fold row
            // has nothing to open, the same answer a group gives the
            // detail pane and feed.
            Some(RowKey::Fold(_)) => None,
            Some(RowKey::Section(_)) => unreachable!("a header is never selectable"),
            None => None,
        }
    }

    /// One sheep by id, whatever the filter hides: the lookup a
    /// [`RowKey::Sheep`] row's rendering needs.
    #[must_use]
    pub fn row(&self, id: u32) -> Option<&Row> {
        self.flock.get(&id)
    }

    /// Every instance of `name`, sorted by slot: the members a
    /// [`RowKey::Group`] row summarises.
    #[must_use]
    pub fn group_members(&self, name: &str) -> Vec<&Row> {
        let mut members: Vec<&Row> = self
            .flock
            .values()
            .filter(|row| row.info.name == name)
            .collect();
        members.sort_by_key(|row| row.info.instance.unwrap_or(u32::MAX));
        members
    }

    /// Whether `name`'s instances draw under a [`RowKey::Group`] header: more
    /// than one instance of the name, every one of them reporting a slot.
    ///
    /// Read over the whole flock rather than the filtered sequence, which a
    /// name query keeps whole either way.
    #[must_use]
    pub fn is_grouped(&self, name: &str) -> bool {
        let members = self.group_members(name);
        members.len() > 1 && members.iter().all(|row| row.info.instance.is_some())
    }

    /// `name`'s rolled-up numbers. [`GroupTotals`] gives the rule each field
    /// follows.
    #[must_use]
    pub fn group_totals(&self, name: &str) -> GroupTotals {
        self.totals_for(self.group_members(name))
    }

    /// Every instance whose `fold` is `fold`: the members a [`RowKey::Fold`]
    /// row summarises.
    #[must_use]
    pub fn fold_members(&self, fold: &str) -> Vec<&Row> {
        self.flock
            .values()
            .filter(|row| row.info.fold.as_deref() == Some(fold))
            .collect()
    }

    /// `fold`'s rolled-up numbers, the same rule [`Self::group_totals`]
    /// applies but over every instance in the fold rather than one app's own.
    #[must_use]
    pub fn fold_totals(&self, fold: &str) -> GroupTotals {
        self.totals_for(self.fold_members(fold))
    }

    /// `fold`'s STATUS text: [`Self::group_status_text`]'s own rule, applied
    /// over [`Self::fold_members`] instead of [`Self::group_members`].
    #[must_use]
    pub fn fold_status_text(&self, fold: &str) -> String {
        Self::status_text_for(&self.fold_members(fold))
    }

    /// Whether `fold`'s members are hidden by [`KeyPress::Collapse`].
    ///
    /// Read by the fold header's name cell, which carries the disclosure
    /// triangle: without it a collapsed fold and a fold whose members all
    /// left the flock render identically, and the design's rule 3 asks that
    /// the frame read with every colour stripped.
    #[must_use]
    pub fn is_fold_collapsed(&self, fold: &str) -> bool {
        self.collapsed_folds.contains(fold)
    }

    /// `fold`'s status when every member agrees on one.
    /// [`Self::group_uniform_status`]'s own rule, by fold rather than by
    /// name.
    #[must_use]
    pub fn fold_uniform_status(&self, fold: &str) -> Option<ProcStatus> {
        Self::uniform_status_for(&self.fold_members(fold))
    }

    /// The shared rollup [`Self::group_totals`] and [`Self::fold_totals`]
    /// both compute, over whichever members each selects.
    fn totals_for(&self, members: Vec<&Row>) -> GroupTotals {
        GroupTotals {
            count: members.len(),
            restarts: members.iter().map(|row| row.info.restarts).sum(),
            // `Self::cpu_now`, not `row.info.cpu_percent`: this rollup feeds
            // the same CPU cell a standalone row draws, and must answer the
            // same question the row and the flock figure do.
            cpu: members
                .iter()
                .filter_map(|row| self.cpu_now(row.info.id))
                .fold(None, |acc, cpu| Some(acc.unwrap_or(0.0) + cpu)),
            memory: members
                .iter()
                .filter_map(|row| row.info.memory_bytes)
                .fold(None, |acc, mem| Some(acc.unwrap_or(0) + mem)),
            uptime_ms: members
                .iter()
                .filter_map(|row| self.uptime_ms(row.info.id))
                .min(),
        }
    }

    /// `name`'s STATUS cell: the shared status word when every instance agrees,
    /// else a count per state, as `output::rows::group_status` does for
    /// `shep flock`.
    ///
    /// Reads `ProcStatus` directly, never [`Row::reported`]: a dog is never
    /// stocked to several instances, so a group has no handshake to report.
    #[must_use]
    pub fn group_status_text(&self, name: &str) -> String {
        Self::status_text_for(&self.group_members(name))
    }

    /// The status sentence [`Self::group_status_text`] and
    /// [`Self::fold_status_text`] both build, over whichever members each
    /// selects. One word when they agree, otherwise a count per status.
    ///
    /// Shares its shape with [`Self::totals_for`] deliberately: these are the
    /// same rollup question asked of two different member sets, and a second
    /// copy of the walk is how the fourth one gets written.
    fn status_text_for(members: &[&Row]) -> String {
        let Some(first) = members.first().map(|row| row.info.status) else {
            return String::new();
        };
        if members.iter().all(|row| row.info.status == first) {
            return first.to_string();
        }
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in members {
            *counts.entry(row.info.status.to_string()).or_default() += 1;
        }
        counts
            .into_iter()
            .map(|(status, n)| format!("{n} {status}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `name`'s status when every instance agrees on one, which the STATUS
    /// colouring and the detail pane's status word key off. A mixed group's
    /// plain count text wears no colour.
    #[must_use]
    pub fn group_uniform_status(&self, name: &str) -> Option<ProcStatus> {
        Self::uniform_status_for(&self.group_members(name))
    }

    /// The one status every member agrees on, or `None` when they differ.
    /// [`Self::group_uniform_status`] and [`Self::fold_uniform_status`] both
    /// key their colouring off this.
    fn uniform_status_for(members: &[&Row]) -> Option<ProcStatus> {
        let first = members.first()?.info.status;
        members
            .iter()
            .all(|row| row.info.status == first)
            .then_some(first)
    }

    /// The link state, as the status bar reports it.
    #[must_use]
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// The clock the view reads: the last [`Msg::Tick`]'s instant, or the
    /// one the link froze at.
    pub(crate) fn now(&self) -> Instant {
        self.now
    }

    /// How long the link has been [`Link::Lost`], as of the last
    /// [`Msg::Tick`]. Zero while it is up.
    #[must_use]
    pub fn frozen_for(&self) -> Duration {
        self.frozen_for
    }

    /// The palette every cell of data renders through.
    ///
    /// [`Palette::frozen`] once the link is [`Link::Lost`], so the table and
    /// the host strip go to one muted ink and no cell can be read as live.
    /// The chrome keeps [`Self::palette`]: the band still names the mode in
    /// bark and the status bar still paints its keys.
    #[must_use]
    pub fn data_palette(&self) -> Palette {
        if matches!(self.link, Link::Lost { .. }) {
            self.palette.frozen()
        } else {
            self.palette
        }
    }

    /// The current notice, if the last message left one.
    #[must_use]
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// The resolved palette.
    #[must_use]
    pub fn palette(&self) -> Palette {
        self.palette
    }

    /// Whether actions are permitted.
    #[must_use]
    pub fn control(&self) -> Control {
        self.control
    }

    /// The `$SHEP_HOME` this lookout watches.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    /// The last host reading, or `None` if there has not been one.
    #[must_use]
    pub fn host(&self) -> Option<super::source::HostSample> {
        self.host
    }

    /// Whether [`Self::host`] is `None` because the platform cannot be read,
    /// rather than because no heartbeat has fired yet. The strip says a
    /// different sentence for each: an operator shown the wrong one waits for
    /// numbers that are never coming.
    #[must_use]
    pub fn host_unsupported(&self) -> bool {
        self.host_unsupported
    }

    /// The selected sheep's most recent output, as of the last refresh.
    #[must_use]
    pub fn feed(&self) -> &super::tail::Tail {
        &self.feed
    }

    /// One sheep's uptime as of this dashboard's own clock, in milliseconds.
    ///
    /// A running sheep's uptime advances between polls, from the anchor its row
    /// carries. One that is not running does not: its `uptime_ms` is a fact
    /// about how long it ran. Nothing advances once the link is [`Link::Lost`].
    #[must_use]
    pub fn uptime_ms(&self, id: u32) -> Option<u64> {
        let row = self.flock.get(&id)?;
        if !matches!(row.info.status, ProcStatus::Online | ProcStatus::Starting) {
            return Some(row.info.uptime_ms);
        }
        let elapsed = self.now.saturating_duration_since(row.anchor);
        Some(row.info.uptime_ms.saturating_add(millis(elapsed)))
    }

    /// The lamb reading for sheep `id`, with its age in milliseconds.
    ///
    /// `None` when there is no reading, or when the one there is was taken for
    /// a different sheep. The age stops when the dashboard freezes.
    #[must_use]
    pub fn lambs_for(&self, id: u32) -> Option<(&LambWalk, u64)> {
        let reading = self.lambs.as_ref().filter(|reading| reading.id == id)?;
        Some((
            &reading.walk,
            millis(self.now.saturating_duration_since(reading.at)),
        ))
    }

    /// The action in progress, for the status bar.
    #[must_use]
    pub fn action(&self) -> Option<ActionState<'_>> {
        let action = self.action.as_ref()?;
        Some(ActionState {
            verb: action.verb,
            target: &action.target,
            name: &action.name,
            count: action.count,
            sent: action.stage == Stage::Sent,
        })
    }

    /// What the body between the title band and the status bar is showing.
    ///
    /// `view::draw` matches on this directly rather than calling
    /// [`Self::settings`] and [`Self::config_pane`] in sequence, which is
    /// the two-branch `if let` chain a new pane would otherwise have to
    /// insert itself into. Eight more panes are planned; each becomes a new
    /// `Body` arm instead.
    #[must_use]
    pub(crate) fn body(&self) -> &Body {
        &self.body
    }

    /// How the flock table currently gathers its rows, toggled by
    /// [`KeyPress::FoldView`].
    ///
    /// `view::mod`'s draw loop reads this to choose between the flat column
    /// set and the fold view's own.
    #[must_use]
    pub fn grouping(&self) -> Grouping {
        self.grouping
    }

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
    fn settings_mut(&mut self) -> Option<&mut Settings> {
        match &mut self.body {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// Tells every scrollable screen how tall the body is. Called by the
    /// event loop before each draw, so a screen's cursor never lands on a
    /// row that was not rendered.
    pub fn note_body_rows(&mut self, rows: u16) {
        if let Some(settings) = self.settings_mut() {
            settings.set_rows(usize::from(rows));
        }
        if let Some(pane) = self.config_pane_mut() {
            // One less than the settings screen gets: the pane spends its
            // first line on a title naming the sheep, which
            // `view::pane::pane_lines` draws before any row.
            let body = usize::from(rows.saturating_sub(1));
            pane.set_rows(body);
            if let Some(list) = pane.list_mut() {
                list.set_rows(body);
            }
        }
        if let Some(pane) = self.bleats_pane_mut() {
            // Every row `view::bleats_full::lines` spends before the first
            // feed line: the title band always, and the filter row whenever
            // a chip is set.
            //
            // Both, not just the title. A page jump larger than the body it
            // scrolls skips lines outright rather than merely overshooting:
            // with a chip showing, a jump of `rows - 1` over a body of
            // `rows - 2` leaves one line between consecutive pages that
            // `ctrl-d` alone never renders. Paging down and back up is
            // symmetric either way, which is why nothing noticed.
            let chrome = 1 + usize::from(!pane.filters().is_empty());
            pane.set_rows(usize::from(rows).saturating_sub(chrome));
        }
    }

    /// Tells the bleats pane how many columns its own area draws into, so a
    /// wrapped line's row cost can be measured against something.
    ///
    /// Its own method rather than a second parameter on
    /// [`Self::note_body_rows`]: the settings screen and the config pane
    /// never need a column count, and every existing caller of that method
    /// — production and test alike — would otherwise have to invent one it
    /// has no use for. Called by the event loop right alongside
    /// [`Self::note_body_rows`], from the same `Rect` both figures come
    /// from.
    pub fn note_body_width(&mut self, width: u16) {
        if let Some(pane) = self.bleats_pane_mut() {
            pane.set_width(width);
        }
    }

    /// The open config pane, or `None` while nothing is being edited.
    #[must_use]
    pub fn config_pane(&self) -> Option<&ConfigPane> {
        match &self.body {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// [`Self::config_pane`]'s mutable twin, for the pane keymap and the
    /// handlers that settle a write or adopt a re-read in place.
    fn config_pane_mut(&mut self) -> Option<&mut ConfigPane> {
        match &mut self.body {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// The open bleats pane, or `None` on any other screen.
    #[must_use]
    pub fn bleats_pane(&self) -> Option<&BleatsPane> {
        match &self.body {
            Body::Bleats(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// [`Self::bleats_pane`]'s mutable twin, for `Escape`'s chip-by-chip
    /// backout. No key sets a filter axis yet; whichever task wires one
    /// needs this too.
    fn bleats_pane_mut(&mut self) -> Option<&mut BleatsPane> {
        match &mut self.body {
            Body::Bleats(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// The open secrets pane, or `None` on any other screen, for
    /// `on_secrets_key`'s handlers.
    fn secrets_pane_mut(&mut self) -> Option<&mut SecretsPane> {
        match &mut self.body {
            Body::Secrets(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Sheep(_) => None,
        }
    }

    /// Takes any revealed value off the screen, on any screen: every
    /// trigger calls this rather than reaching for [`SecretsPane::hide`]
    /// through a pane it first has to find.
    fn hide_revealed(&mut self) {
        if let Some(pane) = self.secrets_pane_mut() {
            pane.hide();
        }
    }

    /// Whether `[secrets] allow_read` lets this pane show a value.
    ///
    /// Read off the model the last [`Effect::LoadSecrets`] built, so the
    /// answer is the one `shep.toml` gave when the rows were gathered and
    /// the pane never opens that file itself. Fails closed everywhere it
    /// cannot be answered: a missing key, an unreadable file
    /// (`super::secrets::model`) and no open pane all read as `false`.
    pub(crate) fn reveal_gate_open(&self) -> bool {
        matches!(&self.body, Body::Secrets(pane) if pane.model.allow_read)
    }

    /// `v`'s answer: a read of the selected row's stored value, or a refusal
    /// naming the gate and the file.
    ///
    /// The value is not on screen when this returns. [`Self::on_revealed`]
    /// puts it there once the read lands.
    ///
    /// A visibility check on `pane.selected` here, because `move_by` cannot
    /// keep it inside the visible set when that set is empty: every group
    /// folded away and no operator row standing leaves `selected` naming a
    /// hidden row, and nothing else writes it back. Refused silently,
    /// the same answer every other `v` press against a pane that is not
    /// open gives.
    fn reveal_selected(&mut self) -> Effect {
        if !self.reveal_gate_open() {
            self.notice = Some(Notice {
                text: crate::commands::secret::HOW_TO_ALLOW_READ.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        if !pane.visible_row_indices().contains(&pane.selected) {
            return Effect::None;
        }
        let (Some(row), Some(environment)) = (
            pane.model.rows.get(pane.selected).cloned(),
            pane.environment().map(str::to_string),
        ) else {
            return Effect::None;
        };
        // Through `hide`, so a value already on screen goes now rather than
        // sitting there under a read that answers for another key.
        pane.hide();
        pane.pending_reveal = Some(row.key.clone());
        Effect::RevealSecret {
            store: pane.model.store.clone(),
            provider_cache: pane.model.provider_cache.clone(),
            row,
            environment,
        }
    }

    /// `y`'s answer: the already-revealed value, on its way to
    /// [`Effect::CopyToClipboard`], or the `allow_read` refusal.
    ///
    /// A reveal by another route, so it takes [`Self::reveal_gate_open`]'s
    /// own gate rather than a second one, and it copies what
    /// [`SecretsPane::reveal`] already holds on screen rather than reading
    /// the store afresh: a fresh read would let `y` show a value the
    /// operator never asked [`KeyPress::Reveal`] to put on screen, past the
    /// same gate a reveal takes.
    ///
    /// Silent, not the `allow_read` refusal, when the gate is open but
    /// nothing is revealed: the gate is not what is missing there, the same
    /// silence [`Self::reveal_selected`] falls back to for an unrelated
    /// selection.
    fn copy_revealed(&mut self) -> Effect {
        if !self.reveal_gate_open() {
            self.notice = Some(Notice {
                text: crate::commands::secret::HOW_TO_ALLOW_READ.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(value) = self
            .secrets_pane_mut()
            .and_then(|pane| pane.reveal.as_ref())
            .map(|reveal| reveal.value.clone())
        else {
            return Effect::None;
        };
        self.notice = Some(Notice {
            text: COPY_SENT_NOTICE.to_string(),
            grave: false,
        });
        Effect::CopyToClipboard(ClipboardValue(value))
    }

    /// An [`Effect::RevealSecret`] has landed.
    ///
    /// Drawn only when the reveal is still the one that was asked for: the
    /// gate can have shut under a fresh model, the tab can have moved, and
    /// every clear trigger drops the pending key. A value that reached the
    /// screen past any of those would be a value nobody asked for, which
    /// for a shut gate is the failure the gate exists to stop.
    fn on_revealed(&mut self, key: &str, environment: &str, value: Option<RevealedValue>) {
        let gate_open = self.reveal_gate_open();
        let until = self.now + REVEAL_HOLDS;
        let Some(pane) = self.secrets_pane_mut() else {
            return;
        };
        if pane.pending_reveal.as_deref() != Some(key) || pane.environment() != Some(environment) {
            return;
        }
        pane.pending_reveal = None;
        let Some(RevealedValue(value)) = value.filter(|_| gate_open) else {
            return;
        };
        pane.reveal = Some(Reveal {
            key: key.to_string(),
            value,
            until,
        });
    }

    /// [`Self::bleats_pane_mut`], exposed past this module so a fixture can
    /// stack filters onto a pane it opened without walking `o` and `m`
    /// through their cycles or typing into the match box.
    #[cfg(test)]
    pub(crate) fn bleats_pane_mut_for_tests(&mut self) -> Option<&mut BleatsPane> {
        self.bleats_pane_mut()
    }

    /// The open sheep pane, or `None` on any other screen.
    #[must_use]
    pub fn sheep_pane(&self) -> Option<&SheepPane> {
        match &self.body {
            Body::Sheep(pane) => Some(pane.as_ref()),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_) => None,
        }
    }

    /// The open sheep pane's own pinned sheep, or `None` once it has left
    /// the flock.
    ///
    /// Reads [`SheepPane::sheep`], never [`Self::selected`]: the same
    /// reasoning [`Self::feed_row`]'s own doc gives. `Msg::Snapshot` reseats
    /// the selection whatever screen is showing, so a pane pinned to a
    /// sheep that then leaves the flock would have this read a neighbour's
    /// row while the pane still names the first, one sheep's facts
    /// presented as another's. The identity band draws this instead of
    /// `App::selected_row`, and any figure it shows (a CPU reading among
    /// them) resolves through the row this returns, not the selection.
    #[must_use]
    pub fn sheep_pane_row(&self) -> Option<&Row> {
        match self.sheep_pane()?.sheep() {
            RowKey::Sheep(id) => self.flock.get(id),
            RowKey::Group(_) | RowKey::Fold(_) | RowKey::Section(_) => None,
        }
    }

    /// [`Self::sheep_pane`]'s mutable twin, for `J`/`K` and for adopting a
    /// `Request::SheepConfig` reply in place.
    fn sheep_pane_mut(&mut self) -> Option<&mut SheepPane> {
        match &mut self.body {
            Body::Sheep(pane) => Some(pane.as_mut()),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_) => None,
        }
    }

    /// The embedded feed inside the open sheep pane, or `None` while the
    /// pane itself is closed.
    ///
    /// [`Self::sheep_pane_mut`] and [`SheepPane::feed_mut`] composed once,
    /// for `on_sheep_pane_key`'s own filter-axis arms, which would otherwise
    /// repeat the two-step `and_then` at every one of them.
    fn sheep_feed_mut(&mut self) -> Option<&mut BleatsPane> {
        self.sheep_pane_mut().map(SheepPane::feed_mut)
    }

    /// The apply offer over the open pane, or `None`.
    #[must_use]
    pub fn pane_menu(&self) -> Option<PaneMenu> {
        self.pane_menu
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

    /// Overrides the control gate a fixture built with. Every shipped fixture
    /// hard-codes [`Control::ReadOnly`].
    #[cfg(test)]
    pub(crate) fn set_control_for_tests(&mut self, control: Control) {
        self.control = control;
    }

    /// Sets the filter directly, bypassing [`Self::set_filter`]'s reseat.
    #[cfg(test)]
    pub(crate) fn set_filter_for_tests(&mut self, query: &str) {
        self.filter = query.to_string();
    }

    /// Points the cursor at `key` without simulating keypresses.
    #[cfg(test)]
    fn select(&mut self, key: RowKey) {
        self.selected = Some(key);
    }

    /// [`Self::select`]'s [`RowKey::Fold`] case, `pub(crate)` so a test in
    /// another module (`view::detail`'s own, in particular) can select a
    /// fold header without reaching into `App`'s private fields.
    #[cfg(test)]
    pub(crate) fn select_fold_for_tests(&mut self, name: &str) {
        self.select(RowKey::Fold(name.to_string()));
    }
}

/// The prefix every action's notice shares: the verb, and the target. A single
/// sheep takes the `(id N)` form; a group names the app, having no one id.
fn target_prefix(verb: ActionVerb, target: &RowKey, name: &str) -> String {
    match target {
        RowKey::Sheep(id) => format!("{} {name} (id {id})", verb.label()),
        RowKey::Group(_) => format!("{} all instances of {name}", verb.label()),
        RowKey::Fold(_) => format!("{} all sheep in fold {name}", verb.label()),
        RowKey::Section(_) => unreachable!("a header is never an action target"),
    }
}

/// Saturating `Duration` to milliseconds. Saturates for clippy's
/// `cast_possible_truncation`, not for a lookout left open 580 million years.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// What the bar says once the shepherd has answered. `Response::Reloading` is
/// an acceptance, not a result: the swaps arrive later on the bus.
const fn outcome(verb: ActionVerb) -> &'static str {
    match verb {
        ActionVerb::Stop => "the shepherd stopped it",
        ActionVerb::Restart => "the shepherd restarted it",
        ActionVerb::Reload => "accepted, the swaps report themselves as they happen",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::edits::EditKey;
    use super::*;
    use crate::lookout::pane::ListRow;
    use crate::lookout::secrets::{SecretRow, Source};
    use shep_core::protocol::{ProcessEventKind, RpcError, RpcErrorCode};

    use super::super::view::fixtures;
    use crate::commands::settings::ScalarView;

    fn sheep(id: u32, name: &str, status: ProcStatus) -> ProcessInfo {
        ProcessInfo::builder(id, name, status)
            .pid(Some(1000 + id))
            .uptime_ms(60_000)
            .build()
    }

    /// One snapshot row for `id`, reporting `cpu_ms` CPU-milliseconds.
    fn row_with_cpu_ms(id: u32, cpu_ms: u64) -> ProcessInfo {
        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online)
            .cpu_ms(Some(cpu_ms))
            .build()
    }

    /// A row naming its own pid, for the respawn case: one sheep id outlives
    /// the process under it, and `cpu_ms` counts whichever tree the shepherd
    /// watches now.
    fn row_with_pid_and_cpu_ms(id: u32, pid: u32, cpu_ms: u64) -> ProcessInfo {
        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online)
            .pid(Some(pid))
            .cpu_ms(Some(cpu_ms))
            .build()
    }

    /// The same row with no CPU reading, which is what a stopped sheep sends.
    fn row_without_cpu(id: u32) -> ProcessInfo {
        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Stopped).build()
    }

    /// One snapshot row for `id`, reporting `rss` bytes of resident memory.
    fn row_with_rss(id: u32, rss: u64) -> ProcessInfo {
        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online)
            .memory_bytes(Some(rss))
            .build()
    }

    /// A dashboard with an empty flock, for tests that only exercise the
    /// snapshot's history bookkeeping.
    fn fixture() -> App {
        App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        )
    }

    impl App {
        /// Drives `Msg::Snapshot` the way the poll does, two seconds after
        /// the last one. The gap is load-bearing: a differenced sample over
        /// a zero window has no honest value.
        fn on_snapshot(&mut self, rows: Vec<ProcessInfo>) {
            self.now += Duration::from_secs(2);
            let at = self.now;
            self.update(Msg::Snapshot { rows, at });
        }
    }

    /// The first reading has nothing behind it to difference, so it records a
    /// baseline and appends nothing. A zero would claim an idle sample that
    /// was never measured.
    #[test]
    fn the_first_reading_records_a_baseline_and_no_sample() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        assert!(app.cpu_history(1).is_empty());
    }

    /// 2000 CPU-milliseconds across a two-second poll is one core.
    #[test]
    fn two_readings_difference_into_one_sample() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        assert_eq!(app.cpu_history(1), &[100.0]);
    }

    /// The 15s baseline is what this whole change exists to stop mattering. A
    /// one-second burst reads once and then reads zero, rather than decaying
    /// across the next seven polls.
    #[test]
    fn a_burst_does_not_smear_across_later_polls() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 1_000)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 1_000)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 1_000)]);
        assert_eq!(app.cpu_history(1), &[50.0, 0.0, 0.0]);
    }

    /// A sheep with no reading appends a zero rather than a gap: the chart is
    /// one cell per sample, and a skipped sample would slide the whole window
    /// and make an old spike look recent. The stored reading goes with it, so
    /// the next live reading is not differenced across the stop.
    #[test]
    fn an_unsampled_sheep_appends_a_zero_and_forgets_its_baseline() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        app.on_snapshot(vec![row_without_cpu(1)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 9_000)]);
        assert_eq!(app.cpu_history(1), &[100.0, 0.0]);
    }

    /// `App::cpu_now` is the source every CPU figure lookout draws reads
    /// through, and it must agree with the sparkline beside it: `None`
    /// while one poll has nothing differenced yet, then the same newest
    /// sample [`App::cpu_history`] holds once a second poll has something
    /// to difference against.
    #[test]
    fn cpu_now_reads_none_after_one_poll_and_matches_cpu_history_after_two() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        assert_eq!(app.cpu_now(1), None, "one poll has nothing to difference");
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        assert_eq!(app.cpu_now(1), Some(100.0));
        assert_eq!(
            app.cpu_history(1).last().copied(),
            app.cpu_now(1),
            "the figure and the sparkline's newest cell must be the same number"
        );
    }

    /// `App::record_samples` still appends a zero to the history buffer for
    /// a sheep with no current reading, so the sparkline's window does not
    /// slide. `App::cpu_now` must not read that buffered zero back as a
    /// figure: a sheep whose `cpu_ms` is `None` this poll has nothing
    /// measured, and `0.0%` would claim otherwise.
    #[test]
    fn cpu_now_reads_none_for_a_sheep_with_no_current_reading() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        app.on_snapshot(vec![row_without_cpu(1)]);
        assert_eq!(
            app.cpu_history(1),
            &[100.0, 0.0],
            "sanity: the buffer still holds the appended zero"
        );
        assert_eq!(app.cpu_now(1), None);
    }

    /// A departed sheep's baseline must not survive to be inherited by an
    /// unrelated sheep that later reuses its id. Without
    /// [`App::record_samples`]'s `cpu_last.retain`, the third poll below
    /// would difference the new sheep's tiny counter against the departed
    /// sheep's much larger one and manufacture a sample, instead of
    /// recording an honest baseline and appending nothing.
    #[test]
    fn a_departed_sheeps_baseline_is_not_inherited_by_a_reused_id() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 9_000)]);
        // Sheep 1 leaves the flock entirely.
        app.on_snapshot(vec![]);
        // An unrelated sheep reuses id 1, with its own counter starting low.
        app.on_snapshot(vec![row_with_cpu_ms(1, 12)]);
        assert!(
            app.cpu_history(1).is_empty(),
            "the reused id's first reading should record a baseline and \
             append nothing, on `Self::cpu_history`'s own terms for a first \
             reading: {:?}",
            app.cpu_history(1)
        );
    }

    /// A respawn gives a new tree whose counter starts below the old one's.
    /// Clamped to zero, the same rule the daemon applies, and it costs one
    /// dropped sample rather than a negative spike.
    /// A respawn keeps the id and takes a new pid, so differencing across it
    /// would subtract a dead process's counter from a live one's.
    ///
    /// The counter rising across the boundary is the case `saturating_sub`
    /// cannot save: it reads as a real delta and underreports the new
    /// process by exactly what the old one had spent. A new process is a
    /// first reading, so it records a baseline and appends nothing.
    #[test]
    fn a_respawn_under_the_same_id_starts_a_new_baseline() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_pid_and_cpu_ms(1, 100, 50)]);
        app.on_snapshot(vec![row_with_pid_and_cpu_ms(1, 100, 2_050)]);
        assert_eq!(app.cpu_history(1), &[100.0], "the live process differences");
        app.on_snapshot(vec![row_with_pid_and_cpu_ms(1, 200, 3_000)]);
        assert_eq!(
            app.cpu_history(1),
            &[100.0],
            "the new pid appends nothing rather than differencing 3000 against 2050"
        );
    }

    #[test]
    fn a_counter_that_went_backwards_reads_zero() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 9_000)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 12)]);
        assert_eq!(app.cpu_history(1), &[0.0]);
    }

    /// RSS is sampled at an instant, so it is buffered as it arrives with no
    /// differencing at all.
    #[test]
    fn rss_is_buffered_as_read() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_rss(1, 1_024)]);
        app.on_snapshot(vec![row_with_rss(1, 2_048)]);
        assert_eq!(app.rss_history(1), &[1_024, 2_048]);
    }

    /// Same depth and same drop-on-leave rule as the CPU buffer.
    #[test]
    fn a_sheep_that_leaves_takes_its_rss_history_too() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_rss(1, 1_024), row_with_rss(2, 512)]);
        app.on_snapshot(vec![row_with_rss(1, 1_024)]);
        assert!(app.rss_history(2).is_empty());
    }

    #[test]
    fn the_buffer_holds_at_most_a_hundred_and_forty_samples() {
        // Each poll's counter climbs by a distinct step, so each differenced
        // percent is distinct too; a wrong-end eviction or a reversed order
        // fails this, not just a wrong length.
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0)]);
        let mut counter: u64 = 0;
        for i in 1..=200u64 {
            counter += i * 20;
            app.on_snapshot(vec![row_with_cpu_ms(1, counter)]);
        }
        let history = app.cpu_history(1);
        assert_eq!(history.len(), 140);
        assert_eq!(history.first(), Some(&61.0), "oldest survivor");
        assert_eq!(history.last(), Some(&200.0), "newest sample");
    }

    #[test]
    fn a_sheep_that_leaves_the_flock_takes_its_history_with_it() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0), row_with_cpu_ms(2, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000)]);
        assert!(
            app.cpu_history(2).is_empty(),
            "a deleted sheep leaves no history behind"
        );
    }

    /// The first poll only records a baseline for each sheep and contributes
    /// nothing to the sum, so the series starts with a zero.
    #[test]
    fn the_flock_series_is_the_sum_of_the_snapshot() {
        let mut app = fixture();
        app.on_snapshot(vec![row_with_cpu_ms(1, 0), row_with_cpu_ms(2, 0)]);
        app.on_snapshot(vec![row_with_cpu_ms(1, 2_000), row_with_cpu_ms(2, 1_000)]);
        assert_eq!(app.flock_cpu_history(), &[0.0, 150.0]);
    }

    fn started() -> (App, Instant) {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(1, "web", ProcStatus::Online),
                sheep(2, "api", ProcStatus::Errored),
                sheep(3, "worker", ProcStatus::Online),
            ],
            at: t0,
        });
        (app, t0)
    }

    /// `started()`'s three sheep with the gate open and the cursor mid-list, on
    /// `web` at id 1.
    ///
    /// The table reads by name, so the ids disagree with the display order:
    /// `api` 2, `web` 1, `worker` 3. Mid-list, because a cursor clamped at
    /// either end would pass the tests that assert a stray `j` did not move it.
    fn allowed() -> App {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::Allowed,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(1, "web", ProcStatus::Online),
                sheep(2, "api", ProcStatus::Online),
                sheep(3, "worker", ProcStatus::Online),
            ],
            at: t0,
        });
        app.update(Msg::Tick { now: t0 });
        app.update(Msg::Key(KeyPress::SelectDown));
        app
    }

    /// `allowed()`'s shape with three instances of one app: `web` at slots 0, 1
    /// and 2, ids 1 through 3. Nothing is selected; each test selects itself.
    fn allowed_with_instances() -> App {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::Allowed,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: instanced_rows(),
            at: t0,
        });
        app
    }

    /// `web`'s three instances, at slots 0, 1 and 2 and ids 1 through 3.
    fn instanced_rows() -> Vec<ProcessInfo> {
        (0..3)
            .map(|slot| {
                ProcessInfo::builder(slot + 1, "web", ProcStatus::Online)
                    .instance(Some(slot))
                    .build()
            })
            .collect()
    }

    /// The status bar's own rendered text.
    fn status_line_text(app: &App) -> String {
        super::super::view::fixtures::rendered(&super::super::view::status::status_line(app, 200))
    }

    /// Two online sheep, `alpha` (id 1) and `bravo` (id 2), named so their
    /// alphabetical table order agrees with their ids: `alpha` is selected
    /// by [`App::reseat`]'s own default the moment the snapshot lands, and
    /// stepping down from it reaches `bravo` in one move.
    fn fixture_with_two_sheep() -> App {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(1, "alpha", ProcStatus::Online),
                sheep(2, "bravo", ProcStatus::Online),
            ],
            at: t0,
        });
        app
    }

    /// One dog and nothing else, so `App::reseat`'s own header-skip selects
    /// it the moment the snapshot lands: the only row that is not a
    /// `Section` header is the dog.
    fn fixture_with_a_dog_selected() -> App {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(90, "otel", ProcStatus::Online)
                    .dog(Some(DogSource::BuiltIn))
                    .build(),
            ],
            at: t0,
        });
        app
    }

    /// `↵` opens the pane on the selected sheep and asks for its config in
    /// the same step, since the pane's left column has nothing to draw
    /// without it.
    #[test]
    fn enter_opens_the_sheep_pane_and_asks_for_its_config() {
        let mut app = fixture_with_two_sheep();
        let effect = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::Sheep(_)));
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig {
                name: "alpha".to_string()
            })
        );
    }

    /// An armed prompt owns `↵`. Opening a pane out from under a question
    /// the operator has not answered would answer it for them.
    #[test]
    fn enter_confirms_an_armed_action_rather_than_opening_the_pane() {
        let mut app = allowed();
        let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// A dog row has no charts to draw and its config is a TOML section
    /// rather than a `SheepConfigView`, so `↵` does nothing there. `e`
    /// still opens the dog config pane it opens today.
    #[test]
    fn enter_on_a_dog_row_opens_nothing() {
        let mut app = fixture_with_a_dog_selected();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// `e` inside the sheep pane opens the editor, not a refill of the pane
    /// that asked. Both send `Request::SheepConfig`, so the reply has to
    /// say which one it is for.
    #[test]
    fn e_inside_the_sheep_pane_opens_the_editor() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "alpha".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert!(matches!(app.body(), Body::ConfigPane(_)));
    }

    /// Ahead of the mutation check below: if `on_sheep_config` branched on
    /// the current body instead of `ConfigFor`, this reply would refill the
    /// sheep pane it found on screen rather than open the editor `e` asked
    /// for, since the pane is still `Body::Sheep` while the reply is in
    /// flight.
    #[test]
    fn escape_closes_the_sheep_pane() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// `on_sheep_pane_key`'s own `SelectUp`/`SelectDown`/`SelectFirst`/
    /// `SelectLast` arms, exercised through `App::update` rather than by
    /// calling `SheepPane::move_by` directly: this pins the routing itself,
    /// the surface `pane_sheep.rs`'s own unit tests cannot reach.
    #[test]
    fn j_k_g_and_capital_g_scroll_the_sheep_panes_column() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "alpha".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        let len = crate::lookout::view::sheep::column_len(app.sheep_pane().unwrap().config());

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(app.sheep_pane().unwrap().view().cursor(), 1);
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(app.sheep_pane().unwrap().view().cursor(), 0);

        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.sheep_pane().unwrap().view().cursor(), len - 1);
        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        assert_eq!(app.sheep_pane().unwrap().view().cursor(), 0);
    }

    /// `J` walks the flock without leaving the pane, and asks for the new
    /// sheep's config.
    #[test]
    fn step_down_moves_to_the_next_sheep() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let effect = app.update(Msg::Key(KeyPress::StepDown));
        let Body::Sheep(pane) = app.body() else {
            panic!("still in the sheep pane")
        };
        assert_eq!(pane.sheep(), &RowKey::Sheep(2));
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig {
                name: "bravo".to_string()
            })
        );
    }

    /// `fixture_with_two_sheep`, with a tail already landed for `alpha`: two
    /// lines, one of them containing `boom`, so the embedded feed's filter
    /// and promotion tests below have something to narrow and a second
    /// sheep to step onto.
    fn fixture_with_feed() -> App {
        let mut app = fixture_with_two_sheep();
        app.update(Msg::Bleats {
            tail: super::super::tail::Tail {
                lines: vec![
                    super::super::tail::TailLine {
                        stream: Stream::Out,
                        text: "boom detected".to_string(),
                    },
                    super::super::tail::TailLine {
                        stream: Stream::Out,
                        text: "all quiet".to_string(),
                    },
                ],
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 0,
                note: None,
            },
        });
        app
    }

    /// Types `text` into the embedded feed's match box and applies it,
    /// through the same keys an operator presses (`FilterStart`, one
    /// `TextChar` per byte, `TextApply`) rather than reaching into
    /// `SheepPane` directly: this is what `on_sheep_pane_key`'s own filter
    /// arms are for, and a fixture that skipped them would not exercise the
    /// routing this task adds.
    fn apply_match(app: &mut App, text: &str) {
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        for ch in text.chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(ch)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
    }

    /// The embedded feed's surviving lines, filtered the way its own
    /// `Filters` would narrow them, for a test to inspect without reaching
    /// into `view::sheep::draw` for a rendered row.
    fn feed_rows(app: &App) -> Vec<String> {
        let Body::Sheep(pane) = app.body() else {
            panic!("the sheep pane is not open")
        };
        pane.feed()
            .visible(&app.feed().lines)
            .into_iter()
            .map(|line| line.text.clone())
            .collect()
    }

    /// The pane's own filters, not a second set. The header advertises them,
    /// so they have to work here and not only in the full-screen pane.
    #[test]
    fn a_filter_applied_in_the_sheep_pane_narrows_its_feed() {
        let mut app = fixture_with_feed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        apply_match(&mut app, "boom");
        assert!(feed_rows(&app).iter().all(|row| row.contains("boom")));
    }

    /// `b` hands the same pane the whole screen, carrying its filters.
    #[test]
    fn b_promotes_the_feed_to_full_screen_with_its_filters() {
        let mut app = fixture_with_feed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        apply_match(&mut app, "boom");
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let Body::Bleats(pane) = app.body() else {
            panic!("full screen")
        };
        assert_eq!(pane.match_filter(), Some("boom"));
    }

    /// Promotion clamps the offset the embedded feed never clamps itself.
    ///
    /// `N` walks the stored value up one line at a time and the embedded
    /// feed draws no scrollback, so nothing bounds it there. Carried across
    /// unclamped, the full screen renders the oldest survivor while the
    /// stored value sits past it, and `j` does nothing visible until it has
    /// been pressed back down through the excess.
    #[test]
    fn promotion_clamps_an_offset_the_embedded_feed_left_out_of_range() {
        let mut app = fixture_with_feed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        if let Some(feed) = app.sheep_feed_mut() {
            feed.scroll_up(9_999);
        }
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let Body::Bleats(pane) = app.body() else {
            panic!("full screen")
        };
        let ceiling = super::super::view::bleats_full::max_scroll_offset(&app, pane);
        assert!(
            pane.scroll_offset() <= ceiling,
            "offset {} should have been clamped to {ceiling}",
            pane.scroll_offset()
        );
    }

    /// Stepping to another sheep re-scopes the feed. A feed left on the
    /// previous sheep under a new title is worse than an empty one.
    #[test]
    fn stepping_re_scopes_the_feed() {
        let mut app = fixture_with_feed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::StepDown));
        let Body::Sheep(pane) = app.body() else {
            panic!("still in the sheep pane")
        };
        assert_eq!(pane.feed_sheep(), &RowKey::Sheep(2));
    }

    /// `allowed()`'s cursor is parked on `web`, id 1; `↵` pins the pane to
    /// it.
    fn allowed_in_the_sheep_pane() -> App {
        let mut app = allowed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app
    }

    /// `x` arms against the pane's own pinned sheep (`web`, id 1), the same
    /// target the dashboard's own `arm` would reach for the same cursor
    /// position. The two agree here because nothing has moved the
    /// selection out from under the pane yet.
    #[test]
    fn x_arms_a_confirm_against_the_panes_pinned_sheep() {
        let mut app = allowed_in_the_sheep_pane();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop))),
            Effect::None
        );
        let armed = app.action().expect("armed");
        assert_eq!(armed.verb, ActionVerb::Stop);
        assert_eq!(armed.target, &RowKey::Sheep(1));
        assert_eq!(armed.name, "web");
        assert!(!armed.sent, "nothing has gone out");
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "arming does not close the pane"
        );
    }

    /// `↵` confirms the armed action from inside the pane, the same send
    /// the dashboard's own `confirm` produces.
    #[test]
    fn confirm_inside_the_pane_sends_the_armed_action() {
        let mut app = allowed_in_the_sheep_pane();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter sends the armed action");
        };
        assert_eq!(
            sent,
            Sent::Action {
                verb: ActionVerb::Restart,
                target: RowKey::Sheep(1),
                name: "web".to_string(),
            }
        );
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "confirming does not close the pane"
        );
    }

    /// Every key but `↵` and `q` cancels an action armed from inside the
    /// pane, the same rule the dashboard's own armed check applies, needed
    /// in the pane's own copy, since `on_key` routes here ahead of that
    /// check.
    #[test]
    fn any_other_key_cancels_an_action_armed_inside_the_pane() {
        for key in [
            KeyPress::Escape,
            KeyPress::StepDown,
            KeyPress::StepUp,
            KeyPress::Edit,
        ] {
            let mut app = allowed_in_the_sheep_pane();
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_some(), "armed before {key:?}");
            assert_eq!(
                app.update(Msg::Key(key)),
                Effect::None,
                "{key:?} sent something"
            );
            assert!(app.action().is_none(), "{key:?} did not cancel");
            assert!(
                matches!(app.body(), Body::Sheep(_)),
                "{key:?} must not also close the pane"
            );
        }
    }

    /// `--read-only` refuses the same way it refuses the dashboard's own
    /// `x`/`R`/`L`, with the same sentence.
    #[test]
    fn x_refuses_under_read_only_from_inside_the_pane() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.action().is_none());
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("read-only: from --read-only or lookout.allow_control")
        );
    }

    /// The pinned sheep can leave the flock entirely while the pane stays
    /// open on it (nothing but `Escape` closes it): arming then must refuse
    /// rather than target whoever replaced it.
    #[test]
    fn arming_refuses_once_the_pinned_sheep_has_left_the_flock() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::Sheep(_)), "pinned to alpha");
        app.set_control_for_tests(Control::Allowed);
        app.update(Msg::Snapshot {
            rows: vec![sheep(2, "bravo", ProcStatus::Online)],
            at: Instant::now(),
        });
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(
            app.action().is_none(),
            "refused rather than arming against bravo"
        );
        assert!(app.notice().is_some_and(Notice::is_grave));
    }

    /// `e` from inside the sheep pane targets the pane's own pinned sheep
    /// (`alpha`, id 1), not the dashboard's selection: nothing has moved
    /// the selection out from under the pane yet, so the two agree here,
    /// but only [`Self::sheep_pane_row`] is asked.
    #[test]
    fn e_asks_for_the_panes_own_pinned_sheeps_config() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::Sheep(_)), "pinned to alpha");
        let request = wire(app.update(Msg::Key(KeyPress::Edit)));
        assert_eq!(
            request,
            Request::SheepConfig {
                name: "alpha".to_string()
            }
        );
    }

    /// The regression the reviewer reproduced: pane pinned to `alpha`,
    /// `Msg::Snapshot` reseats the dashboard's selection onto `bravo` once
    /// `alpha` leaves the flock, and `e` must refuse rather than open
    /// `bravo`'s config under a pane still titled `alpha`.
    #[test]
    fn e_refuses_once_the_pinned_sheep_has_left_the_flock() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::Sheep(_)), "pinned to alpha");
        app.update(Msg::Snapshot {
            rows: vec![sheep(2, "bravo", ProcStatus::Online)],
            at: Instant::now(),
        });
        assert_eq!(app.update(Msg::Key(KeyPress::Edit)), Effect::None);
        assert!(app.notice().is_some_and(Notice::is_grave));
    }

    /// `arm` and `arm_sheep_pane` share their refusal ladder through
    /// `confirm_refusal`, but nothing before this test exercised
    /// `arm_sheep_pane`'s own copy of the "one already in flight" branch: a
    /// hand-copied ladder that dropped it silently would still pass every
    /// other sheep-pane test, since none of them arm twice.
    #[test]
    fn a_second_arm_from_inside_the_sheep_pane_refuses_while_one_is_in_flight() {
        let mut app = allowed_in_the_sheep_pane();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent), "in flight");
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("one action is already in flight")
        );
        let action = app.action().expect("the first one is untouched");
        assert_eq!(action.verb, ActionVerb::Stop);
        assert!(action.sent);
    }

    /// `on_key`'s own armed-keypress prelude clears `self.notice` before its
    /// `match`; `on_sheep_pane_key` copied the prelude but not that line, so
    /// a refusal raised inside the pane never cleared, kept overriding the
    /// pane's own key hints in `status_line`, and survived `close_pane`
    /// back to the flock table.
    #[test]
    fn a_refusal_inside_the_sheep_pane_clears_on_the_next_keypress() {
        let mut app = allowed_in_the_sheep_pane();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert!(app.notice().is_some(), "setup: the refusal is raised");
        app.update(Msg::Key(KeyPress::SelectDown));
        assert!(
            app.notice().is_none(),
            "the refusal outlived a keypress that was not the one that \
             raised it"
        );
    }

    /// `↵` confirms an armed action, correctly: a question awaiting an
    /// answer keeps `↵`. But once sent (`Stage::Sent`), it is in flight and
    /// no longer asking anything, and a regression that let `Stage::Sent` keep
    /// swallowing `↵` (rather than falling through, here, to the dashboard's
    /// own `Confirm` handler, which opens the pane) would pass
    /// `a_second_confirm_does_not_resend_an_action_already_in_flight` above
    /// just as easily, since that test only checks nothing resends.
    #[test]
    fn a_confirm_at_stage_sent_falls_through_to_opening_the_sheep_pane() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent), "in flight");
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "the second Enter opened the pane rather than being swallowed"
        );
    }

    /// The reviewer's own reachable state: an action sent (`Stage::Sent`, in
    /// flight), then a filter typed down to zero rows clears the selection
    /// (`reseat`'s own empty-flock-view branch), then an action key. Before
    /// this fix, `confirm_refusal` checked "one already in flight" ahead of
    /// `arm`'s own "nothing selected", so this exact sequence told the
    /// operator the wrong thing (the in-flight action, not the empty
    /// selection the keypress actually asked about).
    #[test]
    fn arm_with_nothing_selected_refuses_that_and_not_the_in_flight_action() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent), "in flight");

        app.update(Msg::Key(KeyPress::FilterStart));
        for letter in ['z', 'z', 'z'] {
            app.update(Msg::Key(KeyPress::TextChar(letter)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        assert_eq!(app.rows().len(), 0, "the query matches nothing");
        assert!(app.selected_row().is_none(), "reseat cleared the selection");

        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("no sheep is selected"),
            "the keypress asked whether it had a target, and it did not"
        );
        let action = app.action().expect("the first one is still in flight");
        assert_eq!(action.verb, ActionVerb::Stop);
        assert!(action.sent, "untouched by the refused second arm");
    }

    #[test]
    fn a_multi_instance_app_shows_a_group_row_above_its_slots() {
        let app = allowed_with_instances();
        assert_eq!(
            app.visible_rows().len(),
            5,
            "the flock header, three slots and the group row above them"
        );
        assert_eq!(app.visible_rows()[0], RowKey::Section("Flock"));
        assert!(matches!(app.visible_rows()[1], RowKey::Group(ref n) if n == "web"));
    }

    /// Sheep gather under their fold, unfoldered ones under a header that
    /// names the situation, and dogs keep their own band because a dog
    /// cannot carry a fold at all.
    #[test]
    fn by_fold_groups_sheep_under_their_fold() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
                fixtures::sheep_in_fold(3, "batch", None),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        let rows = app.visible_rows();
        assert!(
            rows.iter()
                .any(|r| matches!(r, RowKey::Fold(name) if name == "edge"))
        );
        assert!(rows.iter().any(|r| matches!(r, RowKey::Section("no fold"))));
    }

    /// `F` toggles rather than opening, so pressing it twice is where it
    /// began.
    #[test]
    fn f_toggles_back_to_the_flat_list() {
        let mut app = fixtures::app_with(
            vec![fixtures::sheep_in_fold(1, "api", Some("edge"))],
            fixtures::plain(),
        );
        let flat = app.visible_rows();
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        assert_ne!(app.visible_rows(), flat);
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        assert_eq!(app.visible_rows(), flat);
    }

    /// A selected instance of a grouped app has no [`RowKey::Sheep`] row of
    /// its own once `F` collapses it under a [`RowKey::Group`] header: the
    /// selection must reseat onto something visible rather than sit on a row
    /// `visible_rows()` no longer draws.
    #[test]
    fn f_reseats_a_selection_that_folds_away() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Sheep(1));
        assert!(
            app.selected_index().is_some(),
            "sanity: seated before the toggle"
        );

        let _ = app.update(Msg::Key(KeyPress::FoldView));

        assert!(
            app.selected().is_some(),
            "F must not orphan the selection: {:?}",
            app.visible_rows()
        );
        assert!(app.selected_index().is_some());
    }

    /// Two levels, never three. A three-instance app inside a fold is one
    /// member row keeping its own rollup, or `edge ×4` stops meaning
    /// anything fixed.
    #[test]
    fn an_app_inside_a_fold_stays_one_row() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::instance_in_fold(1, "web", 0, Some("edge")),
                fixtures::instance_in_fold(2, "web", 1, Some("edge")),
                fixtures::instance_in_fold(3, "web", 2, Some("edge")),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        let rows = app.visible_rows();
        let sheep = rows
            .iter()
            .filter(|r| matches!(r, RowKey::Sheep(_)))
            .count();
        assert_eq!(sheep, 0, "instances stay behind their app's row: {rows:?}");
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, RowKey::Group(_)))
                .count(),
            1
        );
    }

    #[test]
    fn a_flock_with_a_dog_draws_a_section_header_before_each_kind() {
        let app = fixtures::app_with_a_dog();
        let rows = app.visible_rows();
        assert_eq!(rows.first(), Some(&RowKey::Section("Flock")), "{rows:?}");
        let dogs = rows
            .iter()
            .position(|row| *row == RowKey::Section("Dogs"))
            .unwrap_or_else(|| panic!("no dogs header: {rows:?}"));
        // Every sheep sorts above the header and every dog below it.
        assert!(
            rows[..dogs].iter().all(|row| !app.is_dog_row(row)),
            "{rows:?}"
        );
        assert!(
            rows[dogs + 1..].iter().all(|row| app.is_dog_row(row)),
            "{rows:?}"
        );
    }

    #[test]
    fn a_flock_with_no_dog_draws_no_dogs_header() {
        let app = started().0;
        let rows = app.visible_rows();
        assert!(!rows.contains(&RowKey::Section("Dogs")), "{rows:?}");
    }

    #[test]
    fn a_dog_only_flock_draws_no_flock_header_and_selects_past_the_dogs_one() {
        let mut app = fixtures::app_with_a_dog();
        // A filter that leaves only the dog, so the sheep side is empty.
        app.set_filter("otel".to_string());
        let rows = app.visible_rows();
        assert!(!rows.contains(&RowKey::Section("Flock")), "{rows:?}");
        assert_eq!(rows.first(), Some(&RowKey::Section("Dogs")), "{rows:?}");

        // The only header is row 0, which has nowhere to search backward.
        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        assert!(
            !matches!(app.selected(), Some(RowKey::Section(_))),
            "{:?}",
            app.selected()
        );
    }

    #[test]
    fn moving_down_steps_over_a_section_header() {
        let mut app = fixtures::app_with_a_dog();
        app.select_at(1, 1);
        let before = app.selected();
        // Walk the whole list; a header must never become the selection.
        for _ in 0..app.visible_rows().len() + 2 {
            let _ = app.update(Msg::Key(KeyPress::SelectDown));
            assert!(
                !matches!(app.selected(), Some(RowKey::Section(_))),
                "landed on a header from {before:?}"
            );
        }
    }

    #[test]
    fn moving_up_steps_over_a_section_header() {
        let mut app = fixtures::app_with_a_dog();
        app.select_at(app.visible_rows().len() - 1, -1);
        let before = app.selected();
        // Walk the whole list; a header must never become the selection.
        for _ in 0..app.visible_rows().len() + 2 {
            let _ = app.update(Msg::Key(KeyPress::SelectUp));
            assert!(
                !matches!(app.selected(), Some(RowKey::Section(_))),
                "landed on a header from {before:?}"
            );
        }
        // The walk has to actually cross the `Dogs` header going up, not
        // just avoid landing on it: it should reach the first row, `api`
        // sorting ahead of `web`.
        assert_eq!(app.selected(), Some(RowKey::Sheep(2)), "{before:?}");
    }

    #[test]
    fn an_action_on_a_group_row_targets_the_whole_app_by_name() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter sends");
        };
        assert_eq!(
            sent.request(),
            Request::Stop {
                selector: SelectorSpec::Name("web".to_string())
            }
        );
    }

    #[test]
    fn a_group_confirm_states_how_many_processes_it_reaches() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let prompt = status_line_text(&app);
        assert!(prompt.contains('3'), "names the blast radius: {prompt}");
    }

    /// The same rule group_totals uses, whose own doc calls uptime the
    /// minimum. A second rollup rule would make two headers disagree about
    /// the same numbers.
    #[test]
    fn a_fold_rolls_up_like_a_group_does() {
        let app = fixtures::app_with(
            vec![
                fixtures::sheep_with(1, "api", Some("edge"), 120_000, Some(100 << 20), 2),
                fixtures::sheep_with(2, "cdn", Some("edge"), 30_000, Some(150 << 20), 5),
            ],
            fixtures::plain(),
        );
        let totals = app.fold_totals("edge");
        assert_eq!(totals.count, 2);
        assert_eq!(totals.restarts, 7);
        assert_eq!(totals.memory, Some(250 << 20));
        assert_eq!(
            totals.uptime_ms,
            Some(30_000),
            "the minimum, not the first or the longest"
        );
    }

    /// A fold restart that half refused says so, and names the apps.
    ///
    /// `refused` only arrives on a multi-app walk, which a fold action is
    /// and a single-app action is not. Unreported, the operator reads
    /// "restarted" over a fold where some apps did not.
    #[test]
    fn a_partly_refused_fold_restart_names_what_refused_it() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        app.set_control_for_tests(Control::Allowed);
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select(RowKey::Fold("edge".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let _ = app.update(Msg::Key(KeyPress::Confirm));

        let _ = app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Restart,
                target: RowKey::Fold("edge".to_string()),
                name: "edge".to_string(),
            },
            result: Ok(Response::Restarted {
                accepted: vec![ProcessInfo::builder(1, "api", ProcStatus::Online).build()],
                refused: vec![SheepRefusal::new("cdn", "its Flockfile moved")],
            }),
        });

        let notice = app.notice().expect("a reply always leaves a notice");
        assert!(
            notice.text.contains("cdn"),
            "names the app: {}",
            notice.text
        );
        assert!(
            notice.text.contains("its Flockfile moved"),
            "and the shepherd's reason: {}",
            notice.text
        );
        assert!(
            notice.grave,
            "a half-done fold action is not a success sentence"
        );
    }

    #[test]
    fn a_fold_confirm_states_how_many_it_reaches() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        app.set_control_for_tests(Control::Allowed);
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select(RowKey::Fold("edge".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let text = status_line_text(&app);
        assert!(
            text.contains('2'),
            "the confirm must name the count: {text}"
        );
        assert!(text.contains("edge"), "and the fold: {text}");

        // The half the confirm cannot check. `Sent::Action`'s `RowKey::Fold`
        // arm is the only thing turning a fold header into a fold selector,
        // and nothing else asserts it: change it to `SelectorSpec::Name` and
        // the confirm still reads "2 sheep in fold edge", Enter still sends,
        // and the shepherd matches no app. A fold-wide stop that silently
        // stops nothing.
        let request = wire(app.update(Msg::Key(KeyPress::Confirm)));
        let Request::Stop { selector } = request else {
            panic!("expected Stop, got {request:?}");
        };
        assert_eq!(selector, SelectorSpec::Fold("edge".to_string()));
    }

    /// `z` hides a fold's members and leaves its header, so a big flock can
    /// be read a fold at a time.
    #[test]
    fn z_collapses_a_fold_and_keeps_its_header() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select_fold_for_tests("edge");
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        let rows = app.visible_rows();
        assert!(
            rows.iter()
                .any(|r| matches!(r, RowKey::Fold(n) if n == "edge"))
        );
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, RowKey::Sheep(_)))
                .count(),
            0
        );
    }

    /// Pressed a second time on the same fold, `z` shows its members again.
    #[test]
    fn z_again_expands_a_collapsed_fold() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select_fold_for_tests("edge");
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        let rows = app.visible_rows();
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, RowKey::Sheep(_)))
                .count(),
            2,
            "both members are back: {rows:?}"
        );
    }

    /// `z` on anything other than a fold header is a no-op, even inside the
    /// fold view: a sheep row does not vanish because the wrong key was
    /// pressed near it.
    #[test]
    fn z_on_a_sheep_row_does_nothing() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select(RowKey::Sheep(1));
        let before = app.visible_rows();
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        assert_eq!(
            app.visible_rows(),
            before,
            "the selection is a sheep row, not a fold header"
        );
    }

    /// The no-fold header is a header, not a fold. There is no
    /// `SelectorSpec` that names "everything with no fold", so an action there
    /// would have to enumerate ids behind the operator's back.
    #[test]
    fn the_no_fold_header_is_not_selectable() {
        let mut app = fixtures::app_with(
            vec![fixtures::sheep_in_fold(1, "batch", None)],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        assert!(
            !matches!(app.selected(), Some(RowKey::Section(_))),
            "selection steps past a header, got {:?}",
            app.selected()
        );
    }

    #[test]
    fn selection_survives_a_poll_on_both_row_kinds() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Snapshot {
            rows: instanced_rows(),
            at: Instant::now(),
        });
        assert_eq!(app.selected(), Some(RowKey::Group("web".to_string())));
    }

    #[test]
    fn arming_a_group_action_refuses_when_read_only() {
        let mut app = allowed_with_instances();
        app.set_control_for_tests(Control::ReadOnly);
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.action().is_none());
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("read-only: from --read-only or lookout.allow_control")
        );
    }

    #[test]
    fn arming_a_group_action_refuses_while_the_link_is_not_live() {
        for link in [
            Msg::Retrying { attempt: 2 },
            Msg::Frozen {
                at_local: "2026-08-16 09:00:00".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            },
        ] {
            let mut app = allowed_with_instances();
            app.select(RowKey::Group("web".to_string()));
            app.update(link);
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_none());
            assert!(app.notice().is_some_and(Notice::is_grave));
        }
    }

    #[test]
    fn arming_a_group_action_refuses_while_one_is_already_in_flight() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("one action is already in flight")
        );
        let action = app.action().expect("the first one is untouched");
        assert_eq!(action.verb, ActionVerb::Stop);
        assert!(action.sent);
    }

    #[test]
    fn an_action_key_arms_a_confirm_and_sends_nothing() {
        let mut app = allowed();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop))),
            Effect::None
        );
        let armed = app.action().expect("armed");
        assert_eq!(armed.verb, ActionVerb::Stop);
        assert_eq!(armed.target, &RowKey::Sheep(1));
        assert_eq!(armed.name, "web");
        assert!(!armed.sent, "nothing has gone out");
    }

    #[test]
    fn only_enter_confirms_and_every_other_key_cancels() {
        for key in [
            KeyPress::SelectDown,
            KeyPress::SelectUp,
            KeyPress::SelectFirst,
            KeyPress::Refresh,
            KeyPress::Escape,
            KeyPress::FilterStart,
            KeyPress::Action(ActionVerb::Stop),
            KeyPress::Action(ActionVerb::Restart),
        ] {
            let mut app = allowed();
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_some(), "armed before {key:?}");
            assert_eq!(
                app.update(Msg::Key(key)),
                Effect::None,
                "{key:?} sent something"
            );
            assert!(app.action().is_none(), "{key:?} did not cancel");
        }

        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(
            matches!(app.update(Msg::Key(KeyPress::Confirm)), Effect::Send(_)),
            "and Enter is the one key that sends"
        );
    }

    /// `allowed()` parks the selection mid-list, so a `j` genuinely could move
    /// it.
    #[test]
    fn a_cancelling_key_is_consumed_and_does_not_also_move_the_selection() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let before = app.selected();
        let effect = app.update(Msg::Key(KeyPress::SelectDown));
        assert!(app.action().is_none(), "the stray j cancelled the confirm");
        assert_eq!(app.selected(), before, "and did not also move the cursor");
        assert_eq!(effect, Effect::None, "nor ask for a feed read or a walk");
    }

    /// The snapshot renames the armed sheep out of the filter while another
    /// enters it, so id 2 stays in `self.flock` while leaving `visible_rows()`
    /// and the cursor moves to id 9. Deleting a neighbour would not separate
    /// the two: `reseat` leaves a surviving id alone.
    #[test]
    fn the_confirm_is_pinned_to_the_id_it_was_armed_on() {
        let mut app = allowed();
        app.set_filter("api".to_string());
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(2, "gateway", ProcStatus::Online),
                sheep(9, "api-new", ProcStatus::Online),
            ],
            at: Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(9)),
            "sanity: the cursor followed the filter off the armed id"
        );
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter sends");
        };
        assert_eq!(
            sent,
            Sent::Action {
                verb: ActionVerb::Stop,
                target: RowKey::Sheep(2),
                name: "api".to_string()
            }
        );
    }

    #[test]
    fn a_confirm_whose_sheep_left_the_flock_refuses_instead_of_sending() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Event(BusEvent::Process {
            event: ProcessEventKind::Delete,
            info: sheep(1, "web", ProcStatus::Stopped),
            manually: true,
            at_ms: 0,
        }));
        assert!(app.action().is_none());
        // Nothing is armed any more, so `Confirm` falls to its other
        // meaning: it opens the sheep pane on whichever row the reseat
        // above moved the selection to, rather than sending the disarmed
        // `Sent::Action` the stale confirm would have.
        assert!(matches!(
            app.update(Msg::Key(KeyPress::Confirm)),
            Effect::Send(Sent::SheepConfig { .. })
        ));
    }

    /// Driven by `Msg::Tick`, so there is no sleep here.
    #[test]
    fn a_confirm_expires_after_ten_seconds_of_ticks() {
        let mut app = allowed();
        let t0 = Instant::now();
        app.update(Msg::Tick { now: t0 });
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(9),
        });
        assert!(app.action().is_some(), "nine seconds is still armed");
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(10),
        });
        assert!(app.action().is_none(), "ten is not");
    }

    /// The pane asks for its own refreshes rather than changing the link's
    /// interval, which is fixed for a connection's lifetime.
    #[test]
    fn a_tick_while_the_pane_is_open_asks_for_a_poll() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let now = Instant::now();
        assert_eq!(app.update(Msg::Tick { now }), Effect::RefreshFeed);
    }

    /// And does not on the dashboard, or every lookout would poll twice as
    /// often for nothing.
    #[test]
    fn a_tick_on_the_dashboard_asks_for_nothing() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let now = Instant::now();
        assert_eq!(app.update(Msg::Tick { now }), Effect::None);
    }

    #[test]
    fn every_action_key_refuses_while_the_link_is_not_live() {
        for link in [
            Msg::Retrying { attempt: 2 },
            Msg::Frozen {
                at_local: "2026-08-16 09:00:00".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            },
        ] {
            let mut app = allowed();
            app.update(link);
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_none());
            assert!(app.notice().is_some_and(Notice::is_grave));
        }
    }

    #[test]
    fn a_second_action_refuses_while_one_is_in_flight() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("one action is already in flight")
        );
        let action = app.action().expect("the first one is untouched");
        assert_eq!(action.verb, ActionVerb::Stop);
        assert!(action.sent);
    }

    #[test]
    fn an_in_flight_line_survives_a_keypress() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::SelectDown));
        assert!(
            app.action().is_some_and(|action| action.sent),
            "the keypress moved the cursor and left the in-flight state alone"
        );
    }

    #[test]
    fn quit_still_quits_while_a_confirm_is_armed() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::Quit);
    }

    /// Outside an armed confirm, `Enter` opens the sheep pane
    /// ([`enter_opens_the_sheep_pane_and_asks_for_its_config`] pins the
    /// whole of that); the point pinned here is narrower and unchanged by
    /// that: a second `Enter` over an action already sent does not re-send
    /// it, the armed-confirm guard having already let it through once.
    #[test]
    fn a_second_confirm_does_not_resend_an_action_already_in_flight() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent), "in flight");
        assert!(
            !matches!(
                app.update(Msg::Key(KeyPress::Confirm)),
                Effect::Send(Sent::Action { .. })
            ),
            "a second Enter does not re-send the action"
        );
    }

    #[test]
    fn a_request_that_could_not_be_sent_says_so_and_clears_the_state() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter sends");
        };
        app.update(Msg::Unsent { sent });
        assert!(app.action().is_none());
        assert!(app.notice().is_some_and(Notice::is_grave));
    }

    #[test]
    fn a_link_that_stops_being_live_takes_an_armed_prompt_down() {
        for link in [
            Msg::Retrying { attempt: 2 },
            Msg::Frozen {
                at_local: "2026-08-16 09:00:00".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            },
        ] {
            let mut app = allowed();
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_some(), "armed while live");
            app.update(link);
            assert!(app.action().is_none(), "and gone once the link is not");
            // Nothing is armed any more, so `Enter` falls to its other
            // meaning (opening the sheep pane) rather than to the confirm
            // this prompt no longer has a question for.
            assert!(!matches!(
                app.update(Msg::Key(KeyPress::Confirm)),
                Effect::Send(Sent::Action { .. })
            ));
        }
    }

    #[test]
    fn a_snapshot_replaces_the_flock_wholesale() {
        let (mut app, t0) = started();
        app.update(Msg::Event(BusEvent::Process {
            event: ProcessEventKind::Start,
            info: sheep(9, "ghost", ProcStatus::Starting),
            manually: true,
            at_ms: 0,
        }));
        assert_eq!(app.rows().len(), 4, "the bus event upserted");

        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "web", ProcStatus::Online)],
            at: t0,
        });
        assert_eq!(app.rows().len(), 1);
        assert!(app.rows().iter().all(|row| row.info.id == 1));
    }

    #[test]
    fn a_snapshot_that_shrinks_the_flock_pulls_the_selection_back() {
        let (mut app, t0) = started();
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.selected_index(), Some(3), "past the flock header");

        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "web", ProcStatus::Online)],
            at: t0,
        });
        assert_eq!(
            app.selected_index(),
            Some(1),
            "the selection came back with the flock, past the header"
        );

        app.update(Msg::Snapshot {
            rows: vec![],
            at: t0,
        });
        assert_eq!(app.selected_index(), None, "an empty flock selects nothing");
    }

    #[test]
    fn the_selection_follows_the_sheep_and_not_the_row_number() {
        let (mut app, t0) = started();
        app.update(Msg::Key(KeyPress::SelectDown));
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(3)),
            "the third row, worker"
        );

        // Sheep 1 goes away. `worker` is now row 1 rather than row 2, where
        // an index cursor would be pointing at `api`.
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(2, "api", ProcStatus::Errored),
                sheep(3, "worker", ProcStatus::Online),
            ],
            at: t0,
        });
        assert_eq!(app.selected(), Some(RowKey::Sheep(3)), "still worker");
        assert_eq!(
            app.selected_index(),
            Some(2),
            "which is now row 2, past the header"
        );
    }

    #[test]
    fn a_deleted_selection_falls_to_the_row_that_took_its_place() {
        let (mut app, t0) = started();
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(1)),
            "web, at index 1 by name"
        );

        // web dies; api and worker remain. Index 1 is now worker.
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(2, "api", ProcStatus::Online),
                sheep(3, "worker", ProcStatus::Online),
            ],
            at: t0,
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(3)),
            "the row that took index 1"
        );

        // The last row dying clamps rather than leaving the cursor past the end.
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.selected(), Some(RowKey::Sheep(3)));
        app.update(Msg::Snapshot {
            rows: vec![sheep(2, "api", ProcStatus::Online)],
            at: t0,
        });
        assert_eq!(app.selected(), Some(RowKey::Sheep(2)));

        app.update(Msg::Snapshot {
            rows: vec![],
            at: t0,
        });
        assert_eq!(app.selected(), None);
        assert_eq!(app.selected_index(), None);
    }

    #[test]
    fn a_selection_that_moves_refreshes_the_feed_and_one_that_cannot_does_not() {
        let (mut app, _) = started();
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::RefreshSelected
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectFirst)),
            Effect::RefreshSelected
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectUp)),
            Effect::None,
            "already at the top: nothing moved, so nothing is re-read"
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectLast)),
            Effect::RefreshSelected
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::None,
            "already at the bottom"
        );
    }

    #[test]
    fn moving_the_selection_asks_for_lambs() {
        let (mut app, _t0) = started();
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::RefreshSelected
        );
    }

    /// `ListFlock` declines the lamb walk: a full machine enumeration every two
    /// seconds, times every open lookout.
    #[test]
    fn a_snapshot_refreshes_the_feed_and_does_not_ask_for_lambs() {
        let (mut app, t0) = started();
        assert_eq!(
            app.update(Msg::Snapshot {
                rows: vec![sheep(1, "web", ProcStatus::Online)],
                at: t0,
            }),
            Effect::RefreshFeed
        );
    }

    #[test]
    fn nothing_is_requested_while_the_link_is_lost() {
        let (mut app, _t0) = started();
        app.update(Msg::Frozen {
            at_local: "2026-08-16 09:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
    }

    #[test]
    fn a_snapshot_refreshes_the_feed_unless_the_link_is_frozen() {
        let (mut app, t0) = started();
        assert_eq!(
            app.update(Msg::Snapshot {
                rows: vec![sheep(1, "web", ProcStatus::Online)],
                at: t0
            }),
            Effect::RefreshFeed
        );
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.update(Msg::Snapshot {
                rows: vec![sheep(1, "web", ProcStatus::Online)],
                at: t0
            }),
            Effect::None,
            "a frozen dashboard does not re-read anything"
        );
    }

    /// The cursor still moves: re-rendering the detail pane from the frozen
    /// listing is data already on the frame. Touching the disk is not.
    #[test]
    fn a_frozen_dashboard_moves_the_cursor_without_touching_a_file() {
        let (mut app, _) = started();
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });

        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::None,
            "no file is read once the link is lost"
        );
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(1)),
            "but the cursor moved anyway"
        );
        assert_eq!(app.update(Msg::Key(KeyPress::SelectLast)), Effect::None);
        assert_eq!(app.selected(), Some(RowKey::Sheep(3)));
    }

    #[test]
    fn a_drop_and_a_lag_both_ask_for_an_immediate_poll() {
        let (mut app, _) = started();
        assert_eq!(
            app.update(Msg::Event(BusEvent::Dropped { count: 12 })),
            Effect::PollNow
        );
        assert_eq!(app.update(Msg::BusLagged { count: 3 }), Effect::PollNow);
        assert_eq!(
            app.update(Msg::Event(BusEvent::Process {
                event: ProcessEventKind::Online,
                info: sheep(1, "web", ProcStatus::Online),
                manually: false,
                at_ms: 0,
            })),
            Effect::None,
            "an ordinary event needs no repair"
        );
    }

    #[test]
    fn a_shepherd_side_drop_and_a_local_lag_read_differently() {
        let (mut app, _) = started();
        app.update(Msg::Event(BusEvent::Dropped { count: 12 }));
        let shepherd_side = app.notice().expect("a drop leaves a notice").to_string();
        app.update(Msg::BusLagged { count: 3 });
        let local = app.notice().expect("a lag leaves a notice").to_string();

        assert!(shepherd_side.contains("the shepherd dropped"));
        assert!(local.contains("lookout fell behind"));
        assert_ne!(shepherd_side, local);
    }

    #[test]
    fn a_running_sheeps_uptime_advances_with_the_heartbeat() {
        let (mut app, t0) = started();
        assert_eq!(app.uptime_ms(app.rows()[0].info.id), Some(60_000));
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(5),
        });
        assert_eq!(app.uptime_ms(1), Some(65_000));
    }

    #[test]
    fn a_frozen_dashboard_stops_the_uptime_clock() {
        let (mut app, t0) = started();
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(5),
        });
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let at_freeze = app.uptime_ms(1);
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(400),
        });
        assert_eq!(
            app.uptime_ms(1),
            at_freeze,
            "the clock stopped with the link"
        );
        assert_eq!(at_freeze, Some(65_000));
    }

    #[test]
    fn a_stopped_sheeps_uptime_does_not_advance() {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "web", ProcStatus::Stopped)],
            at: t0,
        });
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(30),
        });
        assert_eq!(app.uptime_ms(1), Some(60_000));
    }

    #[test]
    fn every_action_key_refuses_while_the_gate_is_closed() {
        for verb in [ActionVerb::Stop, ActionVerb::Restart, ActionVerb::Reload] {
            let (mut app, _t0) = started();
            app.update(Msg::Key(KeyPress::Action(verb)));
            assert!(
                app.action().is_none(),
                "{verb:?} armed behind a closed gate"
            );
            assert_eq!(
                app.notice().map(ToString::to_string).as_deref(),
                Some("read-only: from --read-only or lookout.allow_control"),
                "{verb:?}"
            );
        }
    }

    /// A `DaemonShutdown` is a notice here, where in `bleats` it precedes a
    /// clean exit.
    #[test]
    fn nothing_but_a_keypress_quits() {
        let (mut app, _) = started();
        for msg in [
            Msg::Event(BusEvent::DaemonShutdown),
            Msg::Event(BusEvent::Dropped { count: 1 }),
            Msg::BusLagged { count: 1 },
            Msg::Retrying { attempt: 5 },
            Msg::Frozen {
                at_local: "2026-08-14 14:32:07".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            },
        ] {
            assert_ne!(app.update(msg), Effect::Quit);
        }
        assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::Quit);
    }

    /// `Effect::None`, not `RefreshFeed`: clearing a filter only widens the
    /// visible set, so the selection stays seated and `reseat` is a no-op.
    #[test]
    fn esc_clears_the_filter_instead_of_quitting_while_one_is_set() {
        let mut app = filtered("web");
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        assert_eq!(app.filter(), "");
        assert_eq!(app.rows().len(), 4);
    }

    #[test]
    fn esc_still_quits_with_no_filter_set() {
        let (mut app, _t0) = started();
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::Quit);
    }

    #[test]
    fn the_table_narrows_while_the_query_is_still_being_typed() {
        let (mut app, _t0) = started();
        app.update(Msg::Key(KeyPress::FilterStart));
        assert_eq!(app.mode(), InputMode::Text);
        for letter in ['w', 'e', 'b'] {
            app.update(Msg::Key(KeyPress::TextChar(letter)));
        }
        assert_eq!(app.rows().len(), 1, "narrowed before Enter");
        app.update(Msg::Key(KeyPress::TextApply));
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(
            app.rows().len(),
            1,
            "and applying changed nothing but the mode"
        );
    }

    #[test]
    fn backspace_widens_the_table_back_out() {
        let (mut app, _t0) = started();
        app.update(Msg::Key(KeyPress::FilterStart));
        app.update(Msg::Key(KeyPress::TextChar('w')));
        app.update(Msg::Key(KeyPress::TextChar('z')));
        assert_eq!(app.rows().len(), 0);
        app.update(Msg::Key(KeyPress::TextBackspace));
        assert_eq!(
            app.rows().len(),
            2,
            "wz became w, which matches web and worker"
        );
    }

    #[test]
    fn esc_while_editing_clears_the_filter_and_leaves_the_box() {
        let (mut app, _t0) = started();
        app.update(Msg::Key(KeyPress::FilterStart));
        app.update(Msg::Key(KeyPress::TextChar('w')));
        app.update(Msg::Key(KeyPress::TextAbandon));
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(app.filter(), "");
        assert_eq!(app.rows().len(), 3);
    }

    #[test]
    fn opening_the_filter_takes_a_notice_off_the_bar() {
        let (mut app, _t0) = started();
        app.update(Msg::Event(BusEvent::Dropped { count: 3 }));
        assert!(app.notice().is_some());
        app.update(Msg::Key(KeyPress::FilterStart));
        assert!(app.notice().is_none(), "the box is what the bar shows now");
    }

    #[test]
    fn a_notice_raised_while_typing_is_deferred_and_not_destroyed() {
        let (mut app, _t0) = started();
        app.update(Msg::Key(KeyPress::FilterStart));
        app.update(Msg::Key(KeyPress::TextChar('w')));
        app.update(Msg::Event(BusEvent::DaemonShutdown));
        app.update(Msg::Key(KeyPress::TextChar('e')));
        assert!(
            app.notice().is_some(),
            "typing did not wipe the shepherd's announcement"
        );
        assert_eq!(app.filter(), "we", "and the box kept the query");
    }

    #[test]
    fn the_link_state_walks_live_to_retrying_to_lost_and_back() {
        let (mut app, t0) = started();
        assert_eq!(app.link(), &Link::Live);

        app.update(Msg::Retrying { attempt: 1 });
        assert_eq!(app.link(), &Link::Retrying { attempt: 1 });

        app.update(Msg::Relinked);
        assert_eq!(app.link(), &Link::Live);

        app.update(Msg::Retrying { attempt: 5 });
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.link(),
            &Link::Lost {
                at_local: "2026-08-14 14:32:07".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            }
        );

        // A late snapshot must not unfreeze it.
        app.update(Msg::Snapshot {
            rows: vec![],
            at: t0,
        });
        assert!(matches!(app.link(), Link::Lost { .. }));
    }

    #[test]
    fn the_selection_clamps_at_both_ends() {
        let (mut app, _) = started();
        for _ in 0..10 {
            app.update(Msg::Key(KeyPress::SelectUp));
        }
        assert_eq!(
            app.selected_index(),
            Some(1),
            "up past the first row stays on it, below the header"
        );
        for _ in 0..10 {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
        assert_eq!(
            app.selected_index(),
            Some(3),
            "down past the last row stays on it"
        );
        app.update(Msg::Key(KeyPress::SelectFirst));
        assert_eq!(app.selected_index(), Some(1));
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.selected_index(), Some(3));
    }

    #[test]
    fn refresh_polls_while_live_and_says_why_it_cannot_once_frozen() {
        let (mut app, _) = started();
        assert_eq!(app.update(Msg::Key(KeyPress::Refresh)), Effect::PollNow);

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::None,
            "there is no link task left to ask"
        );
        let notice = app.notice().expect("a refusal is a notice").to_string();
        assert!(notice.contains("the shepherd is gone"));
        assert!(notice.contains("nothing left to ask"));
    }

    #[test]
    fn the_next_keypress_clears_the_notice() {
        let (mut app, _) = started();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.notice().is_some());
        app.update(Msg::Key(KeyPress::SelectDown));
        assert!(app.notice().is_none());
    }

    /// The strip reads this machine, which lookout can still see after the
    /// shepherd dies, so it is the one pane that could keep ticking under a
    /// banner saying the values are frozen.
    #[test]
    fn a_frozen_dashboard_ignores_a_host_sample() {
        let (mut app, _) = started();
        app.update(Msg::Host {
            sample: Some(super::super::source::HostSample {
                load: (2.31, 4.10, 3.88),
                cores: Some(10),
                memory_total_bytes: 32 << 30,
                memory_used_bytes: 12 << 30,
                uptime_seconds: 600,
            }),
        });
        assert!(app.host().is_some(), "a live dashboard takes the sample");

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let frozen = app.host();
        assert_eq!(app.update(Msg::Host { sample: None }), Effect::None);
        assert_eq!(app.host(), frozen, "the last values stay, unchanged");
        assert!(
            !app.host_unsupported(),
            "and a refused sample changes no flag"
        );
    }

    #[test]
    fn applying_a_tail_does_not_ask_for_another_one() {
        let (mut app, _) = started();
        assert_eq!(
            app.update(Msg::Bleats {
                tail: super::super::tail::Tail::default()
            }),
            Effect::None
        );
    }

    /// `run_ui`'s coalesced read is armed before the freeze, so a read can
    /// still be in flight when `Msg::Frozen` lands.
    #[test]
    fn a_frozen_dashboard_ignores_a_bleats_tail_in_flight_at_the_freeze() {
        let (mut app, _) = started();
        let live_tail = super::super::tail::Tail {
            lines: vec![super::super::tail::TailLine {
                stream: super::super::tail::Stream::Out,
                text: "read before the freeze".to_string(),
            }],
            ..Default::default()
        };
        app.update(Msg::Bleats {
            tail: live_tail.clone(),
        });
        assert_eq!(app.feed(), &live_tail, "a live dashboard takes the tail");

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });

        let in_flight_tail = super::super::tail::Tail {
            lines: vec![super::super::tail::TailLine {
                stream: super::super::tail::Stream::Out,
                text: "read after the freeze".to_string(),
            }],
            ..Default::default()
        };
        assert_eq!(
            app.update(Msg::Bleats {
                tail: in_flight_tail
            }),
            Effect::None
        );
        assert_eq!(
            app.feed(),
            &live_tail,
            "the tail read after the freeze must not reach the rendered frame"
        );
    }

    /// A dashboard whose filter is set without any keymap involved.
    ///
    /// Four sheep, two of which contain `web`: `api-web` at id 1 and
    /// `web-worker` at id 4, with `cron` and `queue` between them. The table
    /// sorts by name, so the gap is what makes `j` stepping over a hidden row
    /// falsifiable.
    fn filtered(query: &str) -> App {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(1, "api-web", ProcStatus::Online),
                sheep(2, "cron", ProcStatus::Online),
                sheep(3, "queue", ProcStatus::Online),
                sheep(4, "web-worker", ProcStatus::Online),
            ],
            at: t0,
        });
        app.set_filter(query.to_string());
        app
    }

    /// The fixture separates the two answers: by id it is `web` 0, `api` 1,
    /// `web` 2; by name then id it is `api` 1, `web` 0, `web` 2. The `(name,
    /// id)` tiebreak itself is not falsifiable here, since the rows arrive in
    /// id order; what this catches is the sort going missing entirely.
    #[test]
    fn the_table_draws_by_name_then_by_id() {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(0, "web", ProcStatus::Online),
                sheep(1, "api", ProcStatus::Online),
                sheep(2, "web", ProcStatus::Online),
            ],
            at: t0,
        });

        let drawn: Vec<(&str, u32)> = app
            .rows()
            .iter()
            .map(|row| (row.info.name.as_str(), row.info.id))
            .collect();
        assert_eq!(drawn, vec![("api", 1), ("web", 0), ("web", 2)]);
    }

    #[test]
    fn a_filter_narrows_the_rows_and_leaves_the_real_size_readable() {
        let app = filtered("web");
        assert_eq!(app.rows().len(), 2, "api-web and web-worker");
        assert_eq!(app.flock_len(), 4, "the flock did not get smaller");
    }

    /// `ProcessSelector`'s `Name` compares with `==`, so borrowing the CLI's
    /// selector grammar would match nothing while `web-worker` is being typed.
    #[test]
    fn the_filter_matches_a_substring_and_not_a_whole_name() {
        assert_eq!(filtered("wor").rows().len(), 1, "web-worker, by its middle");
        assert_eq!(filtered("w").rows().len(), 2, "api-web, by its own middle");
    }

    #[test]
    fn the_filter_ignores_case_in_both_directions() {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "WebEdge", ProcStatus::Online)],
            at: t0,
        });
        app.set_filter("webedge".to_string());
        assert_eq!(
            app.rows().len(),
            1,
            "a lowercase query against a mixed name"
        );
        app.set_filter("WEBEDGE".to_string());
        assert_eq!(app.rows().len(), 1, "and an uppercase one");
    }

    #[test]
    fn j_and_k_step_only_over_visible_rows() {
        let mut app = filtered("web");
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(1)),
            "api-web, the first visible sheep"
        );
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "web-worker, skipping the hidden cron and queue"
        );
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "clamped at the last visible row"
        );
        app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(app.selected(), Some(RowKey::Sheep(1)));
    }

    #[test]
    fn select_last_lands_on_the_last_visible_row() {
        let mut app = filtered("web");
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "web-worker, not queue at id 3"
        );
    }

    #[test]
    fn a_filter_that_hides_the_selection_clamps_to_the_nearest_visible_row() {
        let mut app = filtered("");
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "web-worker, position 3 of 4"
        );
        app.set_filter("web".to_string());
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "position 3 clamps to the last visible row, which is web-worker"
        );
    }

    #[test]
    fn nothing_visible_means_nothing_selected() {
        let app = filtered("zzz");
        assert_eq!(app.rows().len(), 0);
        assert_eq!(app.selected(), None);
        assert!(app.selected_row().is_none());
        assert_eq!(app.flock_len(), 4, "the flock is still four sheep");
    }

    #[test]
    fn a_filter_survives_the_two_second_snapshot() {
        let mut app = filtered("web");
        let t1 = Instant::now();
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(1, "api-web", ProcStatus::Online),
                sheep(2, "cron", ProcStatus::Online),
                sheep(3, "queue", ProcStatus::Online),
                sheep(4, "web-worker", ProcStatus::Online),
            ],
            at: t1,
        });
        assert_eq!(app.filter(), "web", "the snapshot did not clear it");
        assert_eq!(app.rows().len(), 2, "and did not widen the table");
        assert_eq!(app.flock_len(), 4);
    }

    #[test]
    fn an_empty_query_is_the_same_as_no_filter() {
        let mut app = filtered("zzz");
        app.set_filter(String::new());
        assert_eq!(app.rows().len(), 4);
        assert_eq!(app.selected(), Some(RowKey::Sheep(1)), "seated again");
    }

    /// `None` means the reply did not walk, `Some(vec![])` means it walked and
    /// found nothing.
    #[test]
    fn a_lamb_reply_records_which_of_the_three_states_it_saw() {
        let (mut app, t0) = started();
        let walked = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .lambs(Some(vec![Lamb::new(48_220, "node")]))
            .build();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![walked])),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::Walked(lambs), _)) if lambs.len() == 1));

        let empty = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .lambs(Some(Vec::new()))
            .build();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![empty])),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::Walked(lambs), _)) if lambs.is_empty()));

        let unwalked = ProcessInfo::builder(1, "web", ProcStatus::Stopped).build();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![unwalked])),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::NotWalked, _))));
        let _ = t0;
    }

    #[test]
    fn a_reading_for_another_sheep_reads_as_not_read_yet() {
        let (mut app, _t0) = started();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![
                ProcessInfo::builder(1, "web", ProcStatus::Online)
                    .lambs(Some(vec![Lamb::new(48_220, "node")]))
                    .build(),
            ])),
        });
        assert!(app.lambs_for(1).is_some());
        assert!(app.lambs_for(2).is_none(), "not this sheep's reading");
    }

    #[test]
    fn a_failed_lamb_fetch_says_so_in_the_pane_and_raises_no_notice() {
        let (mut app, _t0) = started();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Err(RequestError::Closed),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::Failed, _))));
        assert!(app.notice().is_none(), "no notice for a decoration");
    }

    #[test]
    fn an_unrecognised_lamb_reply_is_a_failure_and_not_an_empty_walk() {
        let (mut app, _t0) = started();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Pong),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::Failed, _))));
    }

    /// The fetch is armed before the freeze can land, so a reply can still be
    /// in flight when `Msg::Frozen` arrives.
    #[test]
    fn a_lamb_reply_after_a_freeze_is_refused() {
        let (mut app, _t0) = started();
        app.update(Msg::Frozen {
            at_local: "2026-08-16 09:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![
                ProcessInfo::builder(1, "web", ProcStatus::Online)
                    .lambs(Some(vec![Lamb::new(48_220, "node")]))
                    .build(),
            ])),
        });
        assert!(
            app.lambs_for(1).is_none(),
            "the frozen frame learned nothing"
        );
    }

    #[test]
    fn an_accepted_stop_upserts_the_rows_the_shepherd_returned() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Stop,
                target: RowKey::Sheep(2),
                name: "api".to_string(),
            },
            result: Ok(Response::Stopped(vec![sheep(
                2,
                "api",
                ProcStatus::Stopped,
            )])),
        });
        assert_eq!(
            app.rows()
                .iter()
                .find(|row| row.info.id == 2)
                .map(|row| row.info.status),
            Some(ProcStatus::Stopped),
            "the table shows what the shepherd said, without waiting for a poll"
        );
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("stop api (id 2): the shepherd stopped it")
        );
        assert!(app.action().is_none(), "the in-flight state cleared");
    }

    /// `Response::Reloading` is an acceptance; the swaps arrive afterwards on
    /// the bus, which the table consumes.
    #[test]
    fn a_reload_reply_does_not_claim_the_swap_finished() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Reload,
                target: RowKey::Sheep(2),
                name: "api".to_string(),
            },
            result: Ok(Response::Reloading {
                accepted: vec![sheep(2, "api", ProcStatus::Online)],
                refused: Vec::new(),
            }),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert_eq!(
            said,
            "reload api (id 2): accepted, the swaps report themselves as they happen"
        );
        assert!(!said.contains("reloaded"), "got {said:?}");
    }

    /// `RequestError`'s full `Display` would put a Rust identifier on screen.
    #[test]
    fn a_daemon_refusal_reaches_the_bar_in_the_daemons_own_words() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Restart,
                target: RowKey::Sheep(2),
                name: "api".to_string(),
            },
            result: Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::NotFound,
                message: "selector matched no registered sheep".to_string(),
                daemon_version: None,
            })),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert_eq!(
            said,
            "restart api (id 2): selector matched no registered sheep"
        );
        assert!(!said.contains("NotFound"), "no Rust identifiers: {said:?}");
        assert!(app.notice().is_some_and(Notice::is_grave));
    }

    #[test]
    fn a_connection_that_died_mid_request_says_so_under_the_same_prefix() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Stop,
                target: RowKey::Sheep(2),
                name: "api".to_string(),
            },
            result: Err(RequestError::Closed),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.starts_with("stop api (id 2): "), "got {said:?}");
        assert!(said.contains(&RequestError::Closed.to_string()));
    }

    /// The second case is the sharper one: the right shape for the wrong verb.
    /// A `Stopped` answering a `Restart` carries rows and would upsert happily.
    #[test]
    fn an_unrecognised_reply_says_so_rather_than_reading_as_success() {
        for reply in [
            Response::Pong,
            Response::Stopped(vec![sheep(2, "api", ProcStatus::Stopped)]),
        ] {
            let mut app = allowed();
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
            app.update(Msg::Key(KeyPress::Confirm));
            app.update(Msg::Replied {
                sent: Sent::Action {
                    verb: ActionVerb::Restart,
                    target: RowKey::Sheep(2),
                    name: "api".to_string(),
                },
                result: Ok(reply),
            });
            assert_eq!(
                app.notice().map(ToString::to_string).as_deref(),
                Some(
                    "restart api (id 2): the shepherd answered something this lookout does not understand"
                )
            );
            assert!(app.notice().is_some_and(Notice::is_grave));
        }
    }

    #[test]
    fn s_asks_for_the_file_before_the_screen_opens() {
        let mut app = fixtures::full_app();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Settings)),
            Effect::LoadSettings
        );
        assert!(
            app.settings().is_none(),
            "nothing opens until the read lands"
        );
    }

    #[test]
    fn the_screen_opens_when_the_read_lands() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(app.settings().is_some());
    }

    #[test]
    fn a_read_that_failed_says_so_and_leaves_the_dashboard_up() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Err("no such file".into()),
        });
        assert!(app.settings().is_none());
        let notice = app.notice().expect("a failed read has to say so");
        assert!(notice.is_grave());
        assert!(notice.to_string().contains("no such file"));
    }

    #[test]
    fn s_closes_the_screen_again() {
        let mut app = fixtures::app_in_settings();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        assert!(app.settings().is_none());
    }

    /// From the dashboard with no filter `Esc` quits; from here it must not.
    #[test]
    fn escape_closes_the_screen_and_never_quits() {
        let mut app = fixtures::app_in_settings();
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        assert!(app.settings().is_none());
    }

    /// A model whose only interesting field is `environments`, in the order
    /// given: enough for every test below that only cares about the tab
    /// row, not what is on it.
    fn model_with_environments(envs: &[&str]) -> SecretsModel {
        SecretsModel {
            environments: envs.iter().map(|env| (*env).to_string()).collect(),
            ..SecretsModel::default()
        }
    }

    #[test]
    fn capital_s_opens_the_pane_and_pressing_it_again_closes_it() {
        let mut app = fixtures::full_app();

        let effect = app.update(Msg::Key(KeyPress::Secrets));

        assert!(matches!(effect, Effect::LoadSecrets));
        assert!(matches!(app.body(), Body::Secrets(_)), "pane is open");

        let effect = app.update(Msg::Key(KeyPress::Secrets));

        assert!(matches!(effect, Effect::None));
        assert!(matches!(app.body(), Body::FlockTable), "pane is closed");
    }

    #[test]
    fn escape_closes_the_secrets_pane_too() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));

        let _ = app.update(Msg::Key(KeyPress::Escape));

        assert!(matches!(app.body(), Body::FlockTable));
    }

    #[test]
    fn the_tab_moves_and_stops_at_both_ends() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(model_with_environments(&[
                "dev", "staging", "prod",
            ]))),
        });

        let _ = app.update(Msg::Key(KeyPress::TabPrev));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 0, "the first tab does not wrap");

        for _ in 0..6 {
            let _ = app.update(Msg::Key(KeyPress::TabNext));
        }
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 2, "the last of three does not wrap");
    }

    #[test]
    fn a_tab_move_reloads_because_in_force_is_per_environment() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(model_with_environments(&[
                "dev", "staging", "prod",
            ]))),
        });

        let effect = app.update(Msg::Key(KeyPress::TabNext));

        assert!(
            matches!(effect, Effect::LoadSecrets),
            "every row's IN FORCE, VALUE and byte length belong to one \
             environment, so the tab cannot move without rebuilding them"
        );
    }

    /// Unsetting the last key in an environment drops it from the union
    /// `secrets::model` recomputes on every load, so a tab sitting on the
    /// rightmost entry can be left pointing past the end of a shorter
    /// list. This pins `tab` staying in range and `environment()` still
    /// naming a real entry once that happens.
    #[test]
    fn a_shrinking_environment_list_leaves_the_tab_somewhere_valid() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(model_with_environments(&[
                "dev", "staging", "prod",
            ]))),
        });
        for _ in 0..2 {
            let _ = app.update(Msg::Key(KeyPress::TabNext));
        }
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 2, "sitting on the rightmost tab, `prod`");

        let _ = app.update(Msg::Secrets {
            environment: "prod".into(),
            result: Ok(Box::new(model_with_environments(&["dev", "staging"]))),
        });

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.tab < pane.model.environments.len(),
            "tab {} is past the end of {:?}",
            pane.tab,
            pane.model.environments
        );
        assert_eq!(
            pane.environment(),
            Some("staging"),
            "a subsequent load asks for a real environment, not a dangling one"
        );
    }

    #[test]
    fn z_collapses_a_namespace_group_and_leaves_its_header() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        // The only row, so it is `pane.selected`'s default (0) with no
        // navigation key needed.
        let row = SecretRow {
            key: "vercel/API_TOKEN".to_string(),
            source: Source::Namespace("vercel".to_string()),
            in_force: None,
            set_in: Vec::new(),
            byte_len: None,
            readers: Vec::new(),
        };
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![row],
                ..SecretsModel::default()
            })),
        });

        let _ = app.update(Msg::Key(KeyPress::Collapse));

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(pane.collapsed.contains("vercel"), "the group is collapsed");

        let _ = app.update(Msg::Key(KeyPress::Collapse));

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.collapsed.is_empty(),
            "and pressing it again undoes that"
        );
    }

    /// A row for [`SecretsModel::rows`] carrying no value: every field
    /// besides `key` and `source` is irrelevant to where the cursor lands.
    fn plain_row(key: &str, source: Source) -> SecretRow {
        SecretRow {
            key: key.to_string(),
            source,
            in_force: None,
            set_in: Vec::new(),
            byte_len: None,
            readers: Vec::new(),
        }
    }

    /// Three operator rows and nothing else: the `+ new key` affordance has
    /// no namespace group to sit in front of, so it is the true last thing
    /// on screen, and a plain `j`/`G` from the last real row reaches it,
    /// the fix this pane's own reachability bug needed. `k` off the
    /// affordance lands back on that real row.
    #[test]
    fn j_k_g_and_shift_g_move_the_selection_over_the_pane_s_rows() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let rows = ["FIRST", "SECOND", "THIRD"]
            .into_iter()
            .map(|key| plain_row(key, Source::Operator))
            .collect();
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows,
                ..SecretsModel::default()
            })),
        });

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.selected, 1, "j moves one row down");

        for _ in 0..5 {
            let _ = app.update(Msg::Key(KeyPress::SelectDown));
        }
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "clamped at the new-key affordance, one past THIRD, not wrapping"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(
            pane.selected, 2,
            "k leaves the affordance upward, back onto THIRD"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.selected, 0, "g jumps to the first row");

        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "G reaches the affordance directly: nothing follows the operator group here"
        );
    }

    /// A namespace group following the one operator row: `j` from that row
    /// reaches the affordance in a single press rather than needing a second
    /// `G` nothing on screen ever hinted at, and a further `j` carries on
    /// into the namespace group beyond it.
    #[test]
    fn j_from_the_last_operator_row_reaches_the_new_key_row() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                ],
                ..SecretsModel::default()
            })),
        });

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "one `j` from the only operator row reaches the affordance"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(
            pane.model.rows[pane.selected].key, "vercel/A",
            "a further `j` carries on into the namespace group past it"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.selected_is_new_key_row(),
            "`k` off the namespace row lands back on the affordance"
        );
    }

    /// `Enter` pressed before the first `Msg::Secrets` load lands: `Secrets`
    /// opens against a placeholder model with no rows, where `selected` (0)
    /// and `model.rows.len()` (also 0) coincide, so `selected_is_new_key_row`
    /// reads true over data that never resolved. Deterministic and needs no
    /// async round trip: `KeyPress::Secrets` sets the placeholder
    /// synchronously, and this drives `Confirm` before any `Msg::Secrets`
    /// ever arrives.
    #[test]
    fn confirm_before_the_first_load_lands_does_not_open_the_new_key_input() {
        let mut app = fixtures::full_app();
        app.set_control_for_tests(Control::Allowed);
        let _ = app.update(Msg::Key(KeyPress::Secrets));

        let effect = app.update(Msg::Key(KeyPress::Confirm));

        assert_eq!(effect, Effect::None);
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.typing.is_none(),
            "the placeholder model must not open the value input"
        );
    }

    /// A cursor that stepped over `model.rows` itself, one index at a
    /// time, would land inside a namespace `z` just folded away: the two
    /// hidden rows between `FIRST` and `LAST` are exactly wide enough that
    /// a blind `+1` cannot reach `LAST` by accident. Only a `move_by` that
    /// walks [`SecretsPane::visible_row_indices`] does.
    #[test]
    fn selecting_down_skips_a_namespace_z_has_collapsed() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/B", Source::Namespace("vercel".to_string())),
                    plain_row("LAST", Source::Operator),
                ],
                ..SecretsModel::default()
            })),
        });

        // Select a member of the group before folding it: `Collapse` acts
        // on the selected row's own source.
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        // Back to `FIRST`, a row `visible_row_indices` never hid, so the
        // step below tests `move_by`'s ordinary case rather than its
        // hidden-selection fallback.
        let _ = app.update(Msg::Key(KeyPress::SelectFirst));

        let _ = app.update(Msg::Key(KeyPress::SelectDown));

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(
            pane.model.rows[pane.selected].key, "LAST",
            "the two collapsed rows in between are not a landing row"
        );
    }

    /// Collapsing the very group `selected` sits in must not leave it
    /// naming a hidden row: `view::secrets::draw` skips a row its source
    /// is collapsed, so an untouched `selected` there would mark nothing
    /// on screen at all. Asserted through [`SecretsPane::is_collapsed`],
    /// the same predicate the view calls before drawing a gutter marker,
    /// rather than a raw index, so this fails the way the view would fail
    /// rather than the way an internal counter would.
    #[test]
    fn collapsing_the_selected_group_lands_on_a_still_visible_row() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/B", Source::Namespace("vercel".to_string())),
                    plain_row("LAST", Source::Operator),
                ],
                ..SecretsModel::default()
            })),
        });

        // Land on a member of the group before folding it.
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Collapse));

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        let row = &pane.model.rows[pane.selected];
        assert!(
            !pane.is_collapsed(&row.source),
            "selected still names a row the fold it sat in just hid"
        );
    }

    /// `move_by` cannot land `selected` inside the visible set when that
    /// set is empty (every row here belongs to the one namespace being
    /// folded), so `selected` is left naming a hidden row. `v` has to
    /// check for itself rather than trust the invariant `move_by` cannot
    /// keep.
    #[test]
    fn reveal_over_an_empty_visible_set_does_nothing() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                environments: vec!["dev".to_string()],
                rows: vec![
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/B", Source::Namespace("vercel".to_string())),
                ],
                allow_read: true,
                ..SecretsModel::default()
            })),
        });
        let _ = app.update(Msg::Key(KeyPress::Collapse));

        let effect = app.update(Msg::Key(KeyPress::Reveal));

        assert_eq!(
            effect,
            Effect::None,
            "nothing on screen names a row to read"
        );
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert!(
            pane.visible_row_indices().is_empty(),
            "the fold hid everything"
        );
        assert_eq!(
            pane.pending_reveal, None,
            "no read was asked for, so nothing is pending one"
        );
    }

    /// `collapsed` outlives a reload; row order does not. This pins the
    /// `Msg::Secrets` clamp against exactly that gap: the row count clamp
    /// alone would leave `selected` at the same numeric index, which the
    /// fresh model happens to give to a member of the still-folded
    /// `vercel` namespace, hiding it just as surely as a `Collapse` this
    /// reload never asked for.
    #[test]
    fn reloading_cannot_leave_selected_on_a_row_a_standing_fold_hides() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("vercel/A", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/B", Source::Namespace("vercel".to_string())),
                    plain_row("LAST", Source::Operator),
                ],
                ..SecretsModel::default()
            })),
        });
        // Fold `vercel` while sitting on one of its rows: the `Collapse`
        // fix carries `selected` forward to `LAST`, index 3.
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Collapse));

        // A reload whose row order gives index 3 to a `vercel` row rather
        // than to `LAST`. `collapsed` is untouched by this message, so
        // `vercel` is still folded.
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![
                    plain_row("FIRST", Source::Operator),
                    plain_row("SECOND", Source::Operator),
                    plain_row("vercel/C", Source::Namespace("vercel".to_string())),
                    plain_row("vercel/D", Source::Namespace("vercel".to_string())),
                ],
                ..SecretsModel::default()
            })),
        });

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        let row = &pane.model.rows[pane.selected];
        assert!(
            !pane.is_collapsed(&row.source),
            "a reload left selected on a row its own standing fold hides"
        );
    }

    /// The clear itself is pinned by `a_reveal_clears_on_every_one_of_its_
    /// triggers_that_exists_yet`, which reveals a value before every one
    /// of the ten triggers including this one. Nothing here reveals
    /// anything, so this test pins only the effect `r` returns.
    #[test]
    fn r_requests_a_reload() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::new(SecretsModel {
                rows: vec![plain_row("KEY", Source::Operator)],
                ..SecretsModel::default()
            })),
        });

        let effect = app.update(Msg::Key(KeyPress::Refresh));

        assert_eq!(
            effect,
            Effect::LoadSecrets,
            "`r` re-reads the store, the provider cache and the roll"
        );
    }

    #[test]
    fn a_late_model_for_a_closed_pane_does_not_reopen_it() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));
        let _ = app.update(Msg::Key(KeyPress::Escape));

        let _ = app.update(Msg::Secrets {
            environment: "dev".into(),
            result: Ok(Box::default()),
        });

        assert!(
            matches!(app.body(), Body::FlockTable),
            "a reply that outlived its pane must be dropped"
        );
    }

    /// The mutation this pins: `unwrap_or(0)` alone would make the tab
    /// index always 0, and it would still pass every test above, because
    /// `dev` is index 0 in each of their environment lists. `prod` here is
    /// not, and is also not first alphabetically, so a fixture that quietly
    /// switched to always-0 or always-sorted-first both fail this one.
    #[test]
    fn the_first_load_lands_on_the_requested_environments_tab() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));

        let _ = app.update(Msg::Secrets {
            environment: "prod".into(),
            result: Ok(Box::new(model_with_environments(&["all", "dev", "prod"]))),
        });

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 2, "prod is the third tab, not the first");
    }

    /// Falls back to 0 rather than panicking or leaving the previous tab in
    /// place, when the requested environment is not in the fresh model
    /// (an operator who deleted it between the request and the reply).
    #[test]
    fn a_first_load_for_a_since_removed_environment_falls_back_to_tab_zero() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::Secrets));

        let _ = app.update(Msg::Secrets {
            environment: "gone".into(),
            result: Ok(Box::new(model_with_environments(&["all", "dev"]))),
        });

        let Body::Secrets(pane) = app.body() else {
            panic!("pane is open");
        };
        assert_eq!(pane.tab, 0);
    }

    /// Exact-string, so restoring a derived `Debug` fails this test rather
    /// than silently reopening the leak (IR-41).
    #[test]
    fn the_pane_debug_never_prints_a_revealed_value() {
        let pane = SecretsPane {
            model: Box::default(),
            tab: 0,
            selected: 0,
            collapsed: HashSet::new(),
            reveal: Some(Reveal {
                key: "K".into(),
                value: "hunter2".into(),
                until: Instant::now(),
            }),
            pending_reveal: None,
            armed: None,
            typing: Some(Typing {
                what: TypingWhat::ValueFor("K".into()),
                buffer: "hunter2".into(),
            }),
        };

        let printed = format!("{pane:?}");

        assert_eq!(
            printed,
            "SecretsPane { rows: 0, tab: 0, selected: 0, collapsed: 0, \
             revealing: true, pending_reveal: None, armed: None, typing: true }"
        );
        assert!(!format!("{:?}", pane.reveal).contains("hunter2"));
        assert!(!format!("{:?}", pane.typing).contains("hunter2"));
    }

    /// The value on screen, or `None`. Reads the pane rather than the
    /// rendered frame: these tests are about when a value is held, and the
    /// drawing of it has its own tests in `view::secrets`.
    fn reveal_of(app: &App) -> Option<&Reveal> {
        match app.body() {
            Body::Secrets(pane) => pane.reveal.as_ref(),
            _ => None,
        }
    }

    /// The pane's own open input, or `None`.
    fn typing_of(app: &App) -> Option<&Typing> {
        match app.body() {
            Body::Secrets(pane) => pane.typing.as_ref(),
            _ => None,
        }
    }

    /// The status bar's current line, rendered, or `None`.
    fn notice_of(app: &App) -> Option<String> {
        app.notice().map(ToString::to_string)
    }

    /// The key an armed delete names, or `None`.
    fn armed_of(app: &App) -> Option<String> {
        match app.body() {
            Body::Secrets(pane) => pane.armed.as_ref().map(|a| a.key.clone()),
            _ => None,
        }
    }

    /// Walks [`fixtures::app_with_secrets`]'s cursor onto `SET_EVERYWHERE`,
    /// whose only slot is `all` while the tab names `production`.
    ///
    /// # Panics
    /// If the cursor did not land on a row taking its value from `all`.
    #[track_caller]
    fn select_the_all_slot_row(app: &mut App) {
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is not open");
        };
        let row = &pane.model.rows[pane.selected];
        assert_eq!(row.key, "SET_EVERYWHERE");
        assert_eq!(row.in_force.as_deref(), Some("all"));
        assert_eq!(pane.environment(), Some("production"));
    }

    /// How many rows the table currently holds, for the test proving a
    /// failed write redraws nothing.
    fn row_count(app: &App) -> usize {
        match app.body() {
            Body::Secrets(pane) => pane.model.rows.len(),
            _ => 0,
        }
    }

    /// The environment the pane's own tab currently names.
    ///
    /// # Panics
    /// If the pane is not open.
    #[track_caller]
    fn second_tab_of(app: &App) -> String {
        let Body::Secrets(pane) = app.body() else {
            panic!("pane is not open");
        };
        pane.model.environments[pane.tab].clone()
    }

    /// `G`: the pane's own cursor scheme already lands on the trailing
    /// `+ new key` row, so this is `SelectLast` rather than a second way to
    /// reach it.
    fn select_new_key_row(app: &mut App) {
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
    }

    #[test]
    fn enter_opens_the_value_input_seeded_empty() {
        let mut app = fixtures::app_with_secrets_and_control();

        let _ = app.update(Msg::Key(KeyPress::Confirm));

        let typing = typing_of(&app).expect("the input is open");
        assert_eq!(typing.what, TypingWhat::ValueFor("DB_PASSWORD".into()));
        assert_eq!(
            typing.buffer, "",
            "seeding it with the stored value would put a secret on screen \
             that `v` and its gate exist to control"
        );
    }

    #[test]
    fn a_write_refuses_without_the_control_gate() {
        let mut app = fixtures::app_with_secrets_read_only();

        let _ = app.update(Msg::Key(KeyPress::Confirm));

        assert!(typing_of(&app).is_none());
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("read-only")),
            "the existing refusal, not a second one"
        );
    }

    #[test]
    fn a_value_over_the_cap_is_refused_at_the_input_not_at_the_file() {
        let mut app = fixtures::app_typing_a_value();
        for _ in 0..=shep_core::secrets::MAX_VALUE_BYTES {
            let _ = app.update(Msg::Key(KeyPress::TextChar('x')));
        }

        let effect = app.update(Msg::Key(KeyPress::TextApply));

        assert!(matches!(effect, Effect::None), "nothing reached the file");
        assert!(notice_of(&app).is_some_and(|n| n.contains("4096")));
    }

    #[test]
    fn a_key_outside_the_grammar_is_refused_with_the_grammar() {
        let mut app = fixtures::app_typing_a_new_key();
        for c in ".bad".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }

        let effect = app.update(Msg::Key(KeyPress::TextApply));

        assert!(matches!(effect, Effect::None));
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("not starting with a dot")),
            "the refusal states the rule, not just that it failed"
        );
    }

    #[test]
    fn a_value_lands_in_the_tabs_own_environment() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::TabNext));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for c in "s3cret".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }

        let effect = app.update(Msg::Key(KeyPress::TextApply));

        let Effect::WriteSecret(edit, _) = effect else {
            panic!("expected a write, got {effect:?}");
        };
        assert_eq!(edit.environment, second_tab_of(&app));
        assert_eq!(edit.value.as_deref(), Some("s3cret"));
    }

    #[test]
    fn a_successful_write_takes_a_revealed_value_off_the_screen() {
        let mut app = fixtures::app_revealing_with_control();

        let _ = app.update(Msg::SecretWritten { result: Ok(true) });

        assert!(
            reveal_of(&app).is_none(),
            "the value on screen belonged to what the store held before the write"
        );
    }

    #[test]
    fn a_failed_write_says_why_and_leaves_the_table_alone() {
        let mut app = fixtures::app_with_secrets_and_control();
        let before = row_count(&app);

        let _ = app.update(Msg::SecretWritten {
            result: Err("permission denied (os error 13)".to_string()),
        });

        assert!(
            notice_of(&app).is_some_and(|n| n.contains("permission denied")),
            "the operator gets the reason, not a silent no-op"
        );
        assert_eq!(row_count(&app), before, "and nothing is redrawn as changed");
    }

    #[test]
    fn the_new_key_row_opens_a_name_input_and_then_a_value_input() {
        let mut app = fixtures::app_with_secrets_and_control();
        select_new_key_row(&mut app);

        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(
            typing_of(&app).map(|t| t.what.clone()),
            Some(TypingWhat::NewKey)
        );

        for c in "NEW_KEY".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let effect = app.update(Msg::Key(KeyPress::TextApply));

        assert!(
            matches!(effect, Effect::None),
            "naming a key writes nothing on its own"
        );
        assert_eq!(
            typing_of(&app).map(|t| t.what.clone()),
            Some(TypingWhat::ValueFor("NEW_KEY".into())),
            "the name input hands straight over to the value input"
        );
    }

    #[test]
    fn a_provider_row_refuses_a_write() {
        let mut app = fixtures::app_with_a_pushed_secret_selected_and_control();

        let _ = app.update(Msg::Key(KeyPress::Confirm));

        assert!(typing_of(&app).is_none());
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("pushed by a dog")),
            "and it says why rather than doing nothing"
        );
    }

    #[test]
    fn enter_sets_when_nothing_is_armed_and_confirms_when_something_is() {
        let mut idle = fixtures::app_with_secrets_and_control();

        let _ = idle.update(Msg::Key(KeyPress::Confirm));

        assert!(
            typing_of(&idle).is_some(),
            "unarmed Enter opens the value input"
        );

        let mut armed = fixtures::app_with_secrets_and_control();
        let _ = armed.update(Msg::Key(KeyPress::SecretDelete));

        let effect = armed.update(Msg::Key(KeyPress::Confirm));

        assert!(
            typing_of(&armed).is_none(),
            "armed Enter must not also open the input"
        );
        let Effect::WriteSecret(edit, _) = effect else {
            panic!("expected the delete, got {effect:?}");
        };
        assert_eq!(edit.key, "DB_PASSWORD");
        assert_eq!(edit.value, None, "None is what removes the slot");
    }

    #[test]
    fn escape_disarms_before_it_closes_the_pane() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        let _ = app.update(Msg::Key(KeyPress::Escape));

        assert!(
            matches!(app.body(), Body::Secrets(_)),
            "the pane stays open"
        );
        assert!(armed_of(&app).is_none(), "and the arm is gone");

        let _ = app.update(Msg::Key(KeyPress::Escape));

        assert!(
            matches!(app.body(), Body::FlockTable),
            "a second Escape closes it"
        );
    }

    #[test]
    fn moving_the_selection_disarms() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        let _ = app.update(Msg::Key(KeyPress::SelectDown));

        assert!(
            armed_of(&app).is_none(),
            "an arm must not follow the cursor onto another key"
        );
    }

    /// The armed prompt says "enter confirms, any other key cancels", so
    /// every key but the confirm and the quit has to cancel. `v`, `y` and
    /// `z` are the three that used to leave the arm standing under the
    /// sentence promising they would not.
    #[test]
    fn any_key_but_the_confirm_and_the_quit_disarms() {
        for key in [
            KeyPress::Reveal,
            KeyPress::Copy,
            KeyPress::Collapse,
            KeyPress::Refresh,
            KeyPress::Help,
            KeyPress::Settings,
            KeyPress::Bleats,
        ] {
            let mut app = fixtures::app_armed_to_delete_a_secret();

            let _ = app.update(Msg::Key(key));

            assert!(
                armed_of(&app).is_none(),
                "{key:?} left the delete armed while the bar promised it cancelled"
            );
        }
    }

    #[test]
    fn quit_still_quits_while_a_delete_is_armed() {
        let mut app = fixtures::app_armed_to_delete_a_secret();

        let effect = app.update(Msg::Key(KeyPress::Quit));

        assert!(matches!(effect, Effect::Quit));
    }

    #[test]
    fn moving_the_tab_disarms() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        let _ = app.update(Msg::Key(KeyPress::TabNext));

        assert!(
            armed_of(&app).is_none(),
            "an arm must not follow a tab move onto another environment"
        );
    }

    #[test]
    fn a_reload_disarms() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));
        assert!(armed_of(&app).is_some(), "the delete armed");

        let _ = app.update(Msg::Secrets {
            environment: "all".to_string(),
            result: Ok(Box::default()),
        });

        assert!(
            armed_of(&app).is_none(),
            "a fresh read describes the store as it is now, not the arm"
        );
    }

    /// An armed delete is the fourth armed thing in this module the tick
    /// expires, mirroring the config pane's own `armed_at` at
    /// `app.rs:1999-2005`.
    #[test]
    fn an_armed_delete_expires_like_every_other_armed_thing() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));
        assert!(armed_of(&app).is_some(), "the delete armed");

        let later = Instant::now() + CONFIRM_EXPIRY;
        let _ = app.update(Msg::Tick { now: later });

        assert!(armed_of(&app).is_none(), "it did not expire");
    }

    #[test]
    fn an_armed_delete_survives_a_tick_just_before_the_deadline() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SecretDelete));
        assert!(armed_of(&app).is_some(), "the delete armed");

        let almost = Instant::now() + CONFIRM_EXPIRY - Duration::from_secs(1);
        let _ = app.update(Msg::Tick { now: almost });

        assert!(armed_of(&app).is_some(), "not yet ten seconds");
    }

    #[test]
    fn a_delete_refuses_without_the_control_gate() {
        let mut app = fixtures::app_with_secrets_read_only();

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        assert!(armed_of(&app).is_none());
        assert!(notice_of(&app).is_some_and(|n| n.contains("read-only")));
    }

    #[test]
    fn a_provider_row_refuses_a_delete() {
        let mut app = fixtures::app_with_a_pushed_secret_selected_and_control();

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        assert!(armed_of(&app).is_none());
        assert!(notice_of(&app).is_some_and(|n| n.contains("pushed by a dog")));
    }

    /// The row's value comes from the `all` slot, so an unset against the
    /// tab's own environment would remove nothing and report success, and an
    /// unset against `all` would change every environment at once.
    #[test]
    fn a_value_that_comes_from_the_all_slot_refuses_a_delete_on_a_named_tab() {
        let mut app = fixtures::app_with_secrets_on_a_named_tab_and_control();
        select_the_all_slot_row(&mut app);

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        assert!(armed_of(&app).is_none(), "it must refuse before it arms");
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("`all` slot")
                && n.contains("every environment")
                && n.contains("`all` tab")),
            "the notice says where the value lives, what removing it costs, \
             and how to do it deliberately"
        );
    }

    #[test]
    fn the_all_tab_still_deletes_an_all_slot() {
        let mut app = fixtures::app_with_secrets_and_control();

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));
        let effect = app.update(Msg::Key(KeyPress::Confirm));

        let Effect::WriteSecret(edit, _) = effect else {
            panic!("expected a write, got {effect:?}");
        };
        assert_eq!(edit.key, "DB_PASSWORD");
        assert_eq!(edit.environment, "all");
        assert!(edit.value.is_none(), "a delete sends no value");
    }

    #[test]
    fn an_unset_that_removed_nothing_says_so_rather_than_reporting_success() {
        let mut app = fixtures::app_with_secrets_and_control();

        let effect = app.update(Msg::SecretWritten { result: Ok(false) });

        assert!(
            matches!(effect, Effect::None),
            "nothing changed, so nothing is re-read"
        );
        assert!(
            notice_of(&app).is_some_and(|n| n.contains("nothing to remove")),
            "a delete that removed nothing must not read as a delete that worked"
        );
    }

    #[test]
    fn a_write_that_changed_the_store_re_reads_it() {
        let mut app = fixtures::app_with_secrets_and_control();

        let effect = app.update(Msg::SecretWritten { result: Ok(true) });

        assert!(matches!(effect, Effect::LoadSecrets));
        assert!(notice_of(&app).is_none(), "and says nothing about it");
    }

    #[test]
    fn d_does_not_arm_the_new_key_row() {
        let mut app = fixtures::app_with_secrets_and_control();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert!(
            matches!(app.body(), Body::Secrets(pane) if pane.selected_is_new_key_row()),
            "sanity: `G` landed on the affordance"
        );

        let _ = app.update(Msg::Key(KeyPress::SecretDelete));

        assert!(
            armed_of(&app).is_none(),
            "there is no value there to delete"
        );
    }

    #[test]
    fn a_secret_edit_debug_prints_a_length_and_never_the_value() {
        let edit = SecretEdit {
            key: "DB_PASSWORD".into(),
            environment: "production".into(),
            value: Some("hunter2".into()),
        };

        assert_eq!(
            format!("{edit:?}"),
            "SecretEdit { key: \"DB_PASSWORD\", environment: \"production\", \
             value: Some(<7 bytes>) }"
        );
    }

    /// Exact-string, so restoring a derived `Debug` fails this test rather
    /// than silently reopening the leak (IR-41). [`Msg`]'s own `Debug` is
    /// derived, so the redaction has to live in the field's type.
    #[test]
    fn the_msg_debug_never_prints_a_revealed_value() {
        let landed = Msg::Revealed {
            key: "K".to_string(),
            environment: "production".to_string(),
            value: Some(RevealedValue("hunter2".to_string())),
        };

        let printed = format!("{landed:?}");

        assert_eq!(
            printed,
            "Revealed { key: \"K\", environment: \"production\", \
             value: Some(RevealedValue(<7 bytes>)) }"
        );
    }

    /// Exact-string, [`the_msg_debug_never_prints_a_revealed_value`]'s own
    /// reason: [`Effect`]'s own `Debug` is derived, so the redaction has to
    /// live in [`ClipboardValue`] itself (IR-41).
    #[test]
    fn the_effect_debug_never_prints_a_copied_value() {
        let effect = Effect::CopyToClipboard(ClipboardValue("hunter2".to_string()));

        assert_eq!(
            format!("{effect:?}"),
            "CopyToClipboard(ClipboardValue(<7 bytes>))"
        );
    }

    /// The gate is why the round trip needs a guard at all: a value drawn
    /// after `allow_read` went false is the failure the gate exists to
    /// stop, and nothing hides a reveal when a fresh model arrives.
    #[test]
    fn a_reveal_that_lands_after_the_gate_closed_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
        let answer = fixtures::ask_to_reveal(&mut app);
        let mut shut = fixtures::secrets_model(dir.path(), false);
        shut.environments = vec!["all".to_string(), "production".to_string()];
        let _ = app.update(Msg::Secrets {
            environment: "production".to_string(),
            result: Ok(Box::new(shut)),
        });

        let _ = app.update(answer);

        assert!(reveal_of(&app).is_none(), "the gate shut while it was read");
    }

    /// A selection move or a tab move, each hiding through
    /// [`SecretsPane::hide`]: the pane is still on screen, still pending
    /// nothing, and a late answer has to find that out rather than land on
    /// a row or a tab the operator has moved past.
    #[test]
    fn a_reveal_that_lands_after_its_reason_went_away_shows_nothing() {
        for (name, press) in [
            ("selection", KeyPress::SelectDown),
            ("tab", KeyPress::TabNext),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
            let answer = fixtures::ask_to_reveal(&mut app);
            let _ = app.update(Msg::Key(press));

            let _ = app.update(answer);

            assert!(
                reveal_of(&app).is_none(),
                "{name} moved on and the answer put the value back"
            );
        }
    }

    /// `close` and `escape` do not leave `SecretsPane` in place the way a
    /// selection or a tab move does: they replace `self.body` with
    /// `Body::FlockTable` outright, so a late answer landing there has
    /// nowhere to write and would show nothing whether or not the guard
    /// works. Reopening the pane before delivering it puts a real
    /// `SecretsPane` back on screen, one with no pending reveal of its
    /// own, so the guard actually has something to refuse.
    #[test]
    fn a_reveal_that_lands_after_the_pane_closed_and_reopened_shows_nothing() {
        for (name, press) in [("close", KeyPress::Secrets), ("escape", KeyPress::Escape)] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
            let answer = fixtures::ask_to_reveal(&mut app);
            let _ = app.update(Msg::Key(press));

            let _ = app.update(Msg::Key(KeyPress::Secrets));
            let _ = app.update(Msg::Secrets {
                environment: "production".to_string(),
                result: Ok(Box::new(fixtures::secrets_model(dir.path(), true))),
            });
            let _ = app.update(answer);

            assert!(
                reveal_of(&app).is_none(),
                "{name} reopened a pane the stale answer names no pending read for"
            );
        }
    }

    /// A tab change reloads, so the answer can arrive against a pane whose
    /// rows are a different environment's: the echo, not the key alone,
    /// says whether it is still the answer that was asked for.
    #[test]
    fn a_reveal_that_lands_for_another_environment_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
        let Msg::Revealed { key, value, .. } = fixtures::ask_to_reveal(&mut app) else {
            panic!("a reveal answers with a value");
        };

        let _ = app.update(Msg::Revealed {
            key,
            environment: "ci".to_string(),
            value,
        });

        assert!(reveal_of(&app).is_none(), "that is another tab's value");
    }

    #[test]
    fn v_reveals_only_when_allow_read_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let mut shut = fixtures::app_with_secrets_and_reads(dir.path(), false);

        let effect = shut.update(Msg::Key(KeyPress::Reveal));

        assert_eq!(effect, Effect::None, "a shut gate does not read the store");
        assert!(reveal_of(&shut).is_none(), "the gate is shut");
        assert!(
            shut.notice()
                .is_some_and(|notice| notice.to_string().contains("allow_read")),
            "and it says which gate and where"
        );

        let mut open = fixtures::app_with_secrets_and_reads(dir.path(), true);
        let answer = fixtures::ask_to_reveal(&mut open);

        let _ = open.update(answer);

        assert_eq!(
            reveal_of(&open).map(|reveal| reveal.key.as_str()),
            Some("DB_PASSWORD")
        );
        assert_eq!(
            reveal_of(&open).map(|reveal| reveal.value.as_str()),
            Some(fixtures::REVEALED_VALUE),
            "the value comes off the store, not out of the model"
        );
    }

    #[test]
    fn copying_says_it_was_sent_rather_than_that_it_arrived() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());

        let _ = app.update(Msg::Key(KeyPress::Copy));

        let notice = notice_of(&app).expect("a notice");
        assert!(notice.contains("sent to the terminal"), "got {notice:?}");
        assert!(
            !notice.contains("copied"),
            "OSC 52 is write-only and many terminals refuse it, so claiming \
             success is a claim nothing can check: {notice:?}"
        );
    }

    #[test]
    fn copy_needs_a_revealed_value_rather_than_reading_the_store_behind_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), false);

        let _ = app.update(Msg::Key(KeyPress::Copy));

        assert!(
            notice_of(&app).is_some_and(|n| n.contains("allow_read")),
            "copy is a reveal by another route and takes the same gate"
        );
    }

    #[test]
    fn copy_carries_the_revealed_value_to_the_effect() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());

        let Effect::CopyToClipboard(value) = app.update(Msg::Key(KeyPress::Copy)) else {
            panic!("an open gate over a revealed value copies it");
        };

        assert_eq!(value.0, fixtures::REVEALED_VALUE);
    }

    #[test]
    fn a_reveal_clears_on_every_one_of_its_triggers_that_exists_yet() {
        for (name, press) in [
            ("k", KeyPress::SelectUp),
            ("j", KeyPress::SelectDown),
            ("g", KeyPress::SelectFirst),
            ("G", KeyPress::SelectLast),
            ("shift-tab", KeyPress::TabPrev),
            ("tab", KeyPress::TabNext),
            ("escape", KeyPress::Escape),
            ("close", KeyPress::Secrets),
            ("refresh", KeyPress::Refresh),
            ("quit", KeyPress::Quit),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixtures::app_revealing(dir.path());

            let _ = app.update(Msg::Key(press));

            assert!(reveal_of(&app).is_none(), "{name} left the value on screen");
        }

        let dir = tempfile::tempdir().unwrap();
        let mut timed = fixtures::app_revealing(dir.path());
        let start = timed.now();

        let _ = timed.update(Msg::Tick {
            now: start + REVEAL_HOLDS,
        });

        assert!(
            reveal_of(&timed).is_none(),
            "the tenth second is the last one, so the value is gone by it"
        );
    }

    /// Stops a clear-on-every-tick implementation passing the test above for
    /// the wrong reason.
    #[test]
    fn a_reveal_survives_the_tick_before_it_expires() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());
        let start = app.now();

        let _ = app.update(Msg::Tick {
            now: start + REVEAL_HOLDS - Duration::from_millis(1),
        });

        assert!(
            reveal_of(&app).is_some(),
            "clearing early makes the countdown a lie"
        );
    }

    /// The expiry rides the tick's own clock, not `self.now`, which stops
    /// advancing on a dead link. A value that outlived a link failure would
    /// sit on screen until the operator pressed something.
    #[test]
    fn a_frozen_link_does_not_hold_a_value_on_screen() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());
        let start = app.now();
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });

        let _ = app.update(Msg::Tick {
            now: start + REVEAL_HOLDS,
        });

        assert!(reveal_of(&app).is_none());
    }

    #[test]
    fn a_key_with_no_value_in_this_tab_reveals_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("secrets.json");
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
        // The same key, set only in an environment this tab is not showing:
        // `secrets::get` would find a value under `ci` and must not be asked
        // for one.
        let _ = app.update(Msg::Secrets {
            environment: "production".to_string(),
            result: Ok(Box::new(SecretsModel {
                environments: vec!["all".to_string(), "production".to_string()],
                rows: vec![SecretRow {
                    key: "DB_PASSWORD".to_string(),
                    source: Source::Operator,
                    in_force: None,
                    set_in: vec!["ci".to_string()],
                    byte_len: None,
                    readers: Vec::new(),
                }],
                allow_read: true,
                store,
                ..SecretsModel::default()
            })),
        });

        let answer = fixtures::ask_to_reveal(&mut app);
        let _ = app.update(answer);

        assert!(
            reveal_of(&app).is_none(),
            "nothing resolves here, so there is nothing to show"
        );
    }

    #[test]
    fn the_flock_cursor_and_the_filter_survive_the_swap() {
        let mut app = fixtures::full_app();
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        for c in "web".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let selected = app.selected();
        let filter = app.filter().to_string();

        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        let _ = app.update(Msg::Key(KeyPress::Settings));

        assert_eq!(app.selected(), selected);
        assert_eq!(app.filter(), filter);
    }

    #[test]
    fn the_settings_cursor_starts_at_the_first_row_on_every_open() {
        let mut app = fixtures::app_in_settings();
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });

        let first = app.settings().unwrap().rows()[0];
        assert_eq!(app.settings().unwrap().cursor(), Some(first));
    }

    #[test]
    fn the_cursor_moves_through_the_scalars_and_into_the_dogs() {
        let mut app = fixtures::app_in_settings();
        let rows = app.settings().unwrap().rows();
        for _ in 0..rows.len() - 1 {
            let _ = app.update(Msg::Key(KeyPress::SelectDown));
        }
        assert_eq!(
            app.settings().unwrap().cursor(),
            Some(*rows.last().unwrap())
        );
        // and it stops rather than wrapping
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.settings().unwrap().cursor(),
            Some(*rows.last().unwrap())
        );
    }

    #[test]
    fn an_action_key_from_the_dashboard_is_unreachable_while_the_screen_is_up() {
        let mut app = fixtures::app_in_settings_with_control();
        // `x` is the stop key on the dashboard. In here it is not an action.
        let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.action().is_none(), "no sheep confirm can arm from here");
    }

    /// Every key sequence that ends in a write, not one key: a gate guarding
    /// `space` alone leaves the free-text editor's route reaching
    /// `WriteSetting` on a read-only lookout.
    ///
    /// The refusal is checked per keypress rather than at the end, because
    /// `on_settings_key` clears `self.notice` on every key.
    #[test]
    fn no_key_route_writes_the_config_while_the_gate_is_closed() {
        use KeyPress::{Confirm, Cycle, SelectDown, TextApply, TextChar};

        // `Settings::rows` puts the six scalars first, then the fixture's two
        // dogs: two `SelectDown`s reach `socket`, six the first dog row.
        let routes: &[(&str, &[KeyPress])] = &[
            ("space on a cycled scalar", &[Cycle, Confirm]),
            (
                "the socket editor",
                &[
                    SelectDown,
                    SelectDown,
                    Confirm,
                    TextChar('/'),
                    TextChar('x'),
                    TextApply,
                    Confirm,
                ],
            ),
            (
                "the max_cron_sleep editor",
                &[
                    SelectDown,
                    SelectDown,
                    SelectDown,
                    Confirm,
                    TextChar('9'),
                    TextChar('s'),
                    TextApply,
                    Confirm,
                ],
            ),
            (
                "the whistle gate",
                &[
                    SelectDown, SelectDown, SelectDown, SelectDown, Cycle, Confirm,
                ],
            ),
            (
                "space on a dog row",
                &[
                    SelectDown, SelectDown, SelectDown, SelectDown, SelectDown, SelectDown, Cycle,
                    Confirm,
                ],
            ),
        ];

        for (what, keys) in routes {
            let mut app = fixtures::app_in_settings(); // Control::ReadOnly
            let mut refused = false;
            for key in *keys {
                let effect = app.update(Msg::Key(*key));
                assert!(
                    !matches!(
                        effect,
                        Effect::WriteSetting { .. } | Effect::WriteDog { .. }
                    ),
                    "{what}: a read-only lookout reached {effect:?}"
                );
                refused |= app.notice().is_some_and(Notice::is_grave);
            }
            assert!(refused, "{what}: the refusal has to say why");
            assert!(
                app.settings().unwrap().pending().is_none(),
                "{what}: nothing is left armed"
            );
            assert!(
                app.settings().unwrap().typing().is_none(),
                "{what}: no editor is left open"
            );
        }
    }

    /// The half that keeps the closed-gate test honest: a gate that refused
    /// everything would pass it and be useless.
    #[test]
    fn every_one_of_those_routes_writes_once_the_gate_is_open() {
        use KeyPress::{Confirm, Cycle, SelectDown, TextApply, TextChar};

        let routes: &[(&str, &[KeyPress])] = &[
            ("space on a cycled scalar", &[Cycle, Confirm]),
            (
                "the socket editor",
                &[
                    SelectDown,
                    SelectDown,
                    Confirm,
                    TextChar('/'),
                    TextChar('x'),
                    TextApply,
                    Confirm,
                ],
            ),
            (
                "space on a dog row",
                &[
                    SelectDown, SelectDown, SelectDown, SelectDown, SelectDown, SelectDown, Cycle,
                    Confirm,
                ],
            ),
        ];

        for (what, keys) in routes {
            let mut app = fixtures::app_in_settings_with_control();
            let mut wrote = false;
            for key in *keys {
                let effect = app.update(Msg::Key(*key));
                wrote |= matches!(
                    effect,
                    Effect::WriteSetting { .. } | Effect::WriteDog { .. }
                );
            }
            assert!(wrote, "{what}: an open gate has to reach the write");
        }
    }

    #[test]
    fn a_read_only_lookout_opens_the_screen_and_refuses_the_edit_key() {
        let mut app = fixtures::app_in_settings(); // Control::ReadOnly
        assert!(app.settings().is_some(), "reading shep.toml is not gated");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let notice = app.notice().expect("the refusal has to say why");
        assert!(notice.is_grave());
    }

    #[test]
    fn space_arms_a_candidate_without_changing_the_row() {
        let mut app = fixtures::app_in_settings_with_control();
        let before = app.settings().unwrap().snapshot().log_level.value.clone();

        assert_eq!(app.update(Msg::Key(KeyPress::Cycle)), Effect::None);

        assert_eq!(
            app.settings().unwrap().snapshot().log_level.value,
            before,
            "arming is a question, so the row still shows what the file says"
        );
        assert!(app.settings().unwrap().pending().is_some());
    }

    /// Six log levels and one cycle key: without re-arming, the fourth needs a
    /// cancel in between.
    #[test]
    fn space_advances_the_candidate_rather_than_needing_a_cancel() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let first = app.settings().unwrap().pending().unwrap().text.to_string();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let second = app.settings().unwrap().pending().unwrap().text.to_string();
        assert_ne!(first, second);
    }

    #[test]
    fn the_daemon_confirm_names_both_layers_lookout_cannot_see() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("shep daemon reload"), "got: {text}");
        assert!(text.contains("SHEP_LOG_LEVEL"), "got: {text}");
        assert!(text.contains("--log-level"), "got: {text}");
    }

    #[test]
    fn the_whistle_confirm_names_a_whistle_restart_and_not_a_reload() {
        let mut app = fixtures::app_in_settings_on(SettingField::AllowControl);
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("shep whistle restarted"), "got: {text}");
        assert!(
            !text.contains("daemon reload"),
            "a whistle key needs no reload: {text}"
        );
    }

    #[test]
    fn the_style_confirm_promises_nothing_beyond_the_next_command() {
        let mut app = fixtures::app_in_settings_on(SettingField::StyleLevel);
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("the next command reads it"), "got: {text}");
    }

    /// `style::resolve` is flag over env over config, so with `$SHEP_STYLE` or
    /// `--style` in play the write lands and nothing changes.
    #[test]
    fn a_shadowed_style_confirm_names_the_layer_that_keeps_winning() {
        for (source, layer) in [
            (StyleSource::Env, "$SHEP_STYLE"),
            (StyleSource::Flag, "--style"),
        ] {
            let mut app = fixtures::app_in_settings_with_shadowed_style(source);
            let _ = app.update(Msg::Key(KeyPress::Cycle));
            let text = app.settings().unwrap().pending().unwrap().text.to_string();
            assert!(text.contains(layer), "{source} must name itself: {text}");
            assert!(
                text.contains("keeps winning"),
                "{source} must say what it does: {text}"
            );
            assert!(
                !text.contains("the next command reads it"),
                "{source}: the next command reads {source}, not the file: {text}"
            );
        }
    }

    /// With `$SHEP_STYLE=bare` over a file saying `full`, cycling the resolved
    /// value would propose `full`: a no-op write, reported as a change.
    #[test]
    fn the_style_cycle_starts_from_the_file_and_not_the_level_in_force() {
        let mut app = fixtures::app_in_settings_with_shadowed_style(StyleSource::Env);
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(
            text.contains("set style level to plain"),
            "the file says full, so one step is plain: {text}"
        );
    }

    #[test]
    fn enter_sends_the_armed_edit() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let effect = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(
            effect,
            Effect::WriteSetting {
                edit: SettingEdit::Set {
                    field: SettingField::LogLevel,
                    ..
                },
                ..
            }
        ));
        assert!(app.settings().unwrap().pending().unwrap().sent);
    }

    #[test]
    fn a_written_edit_updates_the_row_and_its_source() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        let SettingEdit::Set {
            value: candidate, ..
        } = edit.clone()
        else {
            panic!("cycling only ever arms Set");
        };

        let effect = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Ok(()),
        });
        assert_eq!(
            effect,
            Effect::LoadSettings,
            "a landed write re-reads rather than hand-folding the row"
        );
        assert!(app.settings().unwrap().pending().is_none());

        // The re-read, which `run_ui` drives through `load_settings`.
        let mut updated = fixtures::settings_snapshot();
        updated.log_level = ScalarView {
            value: candidate,
            source: StyleSource::Config,
        };
        let _ = app.update(Msg::Settings {
            result: Ok(updated.clone()),
        });

        assert_eq!(app.settings().unwrap().snapshot(), &updated);
    }

    #[test]
    fn an_unset_write_returns_the_row_to_the_default() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..8 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        assert!(matches!(
            edit,
            SettingEdit::Unset {
                field: SettingField::MaxCronSleep
            }
        ));

        let effect = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Ok(()),
        });
        assert_eq!(effect, Effect::LoadSettings);

        let mut updated = fixtures::settings_snapshot();
        updated.max_cron_sleep = ScalarView {
            value: "30s".to_string(),
            source: StyleSource::Default,
        };
        let _ = app.update(Msg::Settings {
            result: Ok(updated.clone()),
        });

        assert_eq!(app.settings().unwrap().snapshot(), &updated);
    }

    /// Pins `Msg::Settings`'s `opening` check.
    #[test]
    fn the_cursor_survives_a_landed_writes_reload() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let before = app.settings().unwrap().cursor();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        let _ = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Ok(()),
        });
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert_eq!(app.settings().unwrap().cursor(), before);
    }

    #[test]
    fn the_cursor_survives_a_refresh() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let before = app.settings().unwrap().cursor();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::LoadSettings
        );
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert_eq!(app.settings().unwrap().cursor(), before);
    }

    #[test]
    fn a_refused_write_says_why_and_leaves_the_row_alone() {
        let mut app = fixtures::app_in_settings_with_control();
        let before = app.settings().unwrap().snapshot().log_level.clone();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        let _ = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Err("max_cron_sleep is 500ms, below the 1s floor".into()),
        });

        assert_eq!(app.settings().unwrap().snapshot().log_level, before);
        let notice = app.notice().unwrap();
        assert!(notice.is_grave());
        assert!(notice.to_string().contains("below the 1s floor"));
    }

    /// Two writes can be in flight at once: `Pending::Sent` eats no key, so
    /// `space` arms a second edit over the first and `Enter` sends it. The
    /// first write's answer must not resolve the second, which is the edit
    /// the screen is actually showing a prompt for.
    #[test]
    fn a_superseded_reply_does_not_resolve_the_edit_that_replaced_it() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting {
            edit: first,
            ticket: first_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the first edit");
        };

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting {
            edit: second,
            ticket: second_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the second edit too");
        };
        assert_ne!(first_ticket, second_ticket, "two writes are two tickets");

        let effect = app.update(Msg::SettingWritten {
            edit: first,
            ticket: first_ticket,
            result: Ok(()),
        });
        assert_eq!(
            effect,
            Effect::None,
            "a re-read here rebuilds the screen and throws the live edit away"
        );
        assert!(
            app.settings().unwrap().pending().is_some_and(|p| p.sent),
            "the second edit is still in flight and still says so"
        );

        let effect = app.update(Msg::SettingWritten {
            edit: second,
            ticket: second_ticket,
            result: Ok(()),
        });
        assert_eq!(effect, Effect::LoadSettings, "its own reply re-reads");
        assert!(app.settings().unwrap().pending().is_none());
    }

    /// The worst of the two: a refusal reopens the editor for a free-text
    /// field, so a superseded one used to replace a live `Pending::Sent`
    /// with a text editor. The live write's own answer then cleared it and
    /// left `InputMode::Text` behind with nothing to type into, which is
    /// the state `a_refused_settings_write_landing_over_a_dog_pane_does_not_arm_text_mode`
    /// exists to keep out by the other door.
    #[test]
    fn a_superseded_refusal_does_not_reopen_the_editor_over_a_live_edit() {
        let mut app = fixtures::app_in_settings_on(SettingField::Socket);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting {
            edit: socket,
            ticket: socket_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the socket edit");
        };

        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting {
            edit: level,
            ticket: level_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the log level edit too");
        };

        let _ = app.update(Msg::SettingWritten {
            edit: socket,
            ticket: socket_ticket,
            result: Err("the socket path is too long".into()),
        });
        assert_eq!(
            app.mode(),
            InputMode::Normal,
            "no editor was opened, so no text mode is owed one"
        );
        assert!(app.settings().unwrap().typing().is_none());
        assert!(
            app.settings().unwrap().pending().is_some_and(|p| p.sent),
            "the log level edit is still in flight"
        );
        assert!(
            app.notice().unwrap().to_string().contains("too long"),
            "the refusal is still reported"
        );

        let _ = app.update(Msg::SettingWritten {
            edit: level,
            ticket: level_ticket,
            result: Ok(()),
        });
        assert_eq!(app.mode(), InputMode::Normal);
        assert!(app.settings().unwrap().pending().is_none());
    }

    /// A dog's ticket has to survive two hops: the file half answers as
    /// `Msg::DogWritten`, which raises `Sent::Dog`, and only the shepherd's
    /// answer to that clears the prompt. An edit armed in between owns the
    /// prompt by then.
    #[test]
    fn a_superseded_dog_reply_does_not_resolve_the_edit_that_replaced_it() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteDog { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter must send the file half");
        };
        let Effect::Send(dog) = app.update(Msg::DogWritten {
            edit,
            ticket,
            result: Ok(DogSource::BuiltIn),
        }) else {
            panic!("a landed file half must raise the daemon half");
        };

        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting { ticket: scalar, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the scalar edit");
        };
        assert_ne!(ticket, scalar, "the toggle and the scalar are two writes");

        let info = ProcessInfo::builder(50, "metrics", ProcStatus::Online)
            .pid(Some(50_000))
            .dog(Some(DogSource::BuiltIn))
            .build();
        let effect = app.update(Msg::Replied {
            sent: dog,
            result: Ok(Response::DogStarted(info)),
        });
        assert_eq!(effect, Effect::None, "the live edit survives a re-read");
        assert!(
            app.settings().unwrap().pending().is_some_and(|p| p.sent),
            "the scalar edit is still in flight and still says so"
        );
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("enable metrics: the shepherd started it"),
            "the toggle still reports what the shepherd did"
        );
    }

    /// The door a refuse-to-arm guard could not close: `Escape` leaves the
    /// screen without cancelling the write, and reopening it builds a fresh
    /// `Settings` with nothing pending. The abandoned write's answer must
    /// still not touch whatever the reopened screen has armed since.
    #[test]
    fn a_reply_for_a_write_the_screen_walked_away_from_touches_nothing() {
        let mut app = fixtures::app_in_settings_on(SettingField::Socket);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting {
            edit: socket,
            ticket: socket_ticket,
            ..
        } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the socket edit");
        };

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.settings().is_none(), "Escape leaves the screen");
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteSetting { ticket: level, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send the reopened screen's edit");
        };
        assert_ne!(socket_ticket, level, "a reopened screen mints its own");

        let _ = app.update(Msg::SettingWritten {
            edit: socket,
            ticket: socket_ticket,
            result: Err("the socket path is too long".into()),
        });
        assert_eq!(app.mode(), InputMode::Normal);
        assert!(app.settings().unwrap().typing().is_none());
        assert!(
            app.settings().unwrap().pending().is_some_and(|p| p.sent),
            "the reopened screen's own edit is untouched"
        );
    }

    /// The divergence from the sheep confirm, which `disarm_on_link_change`
    /// clears: a settings edit is local file I/O.
    #[test]
    fn a_lost_link_leaves_a_scalar_confirm_armed() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".into(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert!(
            app.settings().unwrap().pending().is_some(),
            "a scalar never leaves the machine, so a dead shepherd is irrelevant to it"
        );
    }

    /// Off the raw tick rather than `self.now`, which stops advancing once the
    /// link is lost.
    #[test]
    fn a_settings_confirm_expires_on_a_frozen_dashboard() {
        let (mut app, start) = fixtures::app_in_settings_at();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".into(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let _ = app.update(Msg::Tick {
            now: start + CONFIRM_EXPIRY,
        });
        assert!(app.settings().unwrap().pending().is_none());
    }

    #[test]
    fn escape_cancels_the_confirm_before_it_closes_the_screen() {
        let mut app = fixtures::app_in_settings_with_control();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.settings().unwrap().pending().is_none());
        assert!(
            app.settings().is_some(),
            "the first Esc cancels, it does not close"
        );
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.settings().is_none());
    }

    /// `s` raises `Effect::LoadSettings` while `body` is still `Body::FlockTable`,
    /// so `x` reaches `arm()`. Once the read lands, `on_settings_key` no-ops
    /// `Confirm`, so nothing could resolve the armed action.
    #[test]
    fn opening_the_screen_clears_an_action_armed_while_the_read_was_in_flight() {
        let mut app = fixtures::allowed_app();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(
            app.action().is_some(),
            "the arm must still succeed before the read lands"
        );
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(
            app.action().is_none(),
            "no armed action may survive the screen opening"
        );
    }

    /// `on_key` checks the text mode ahead of its settings branch, so a box
    /// left open would eat every key the settings keymap owns. The query itself
    /// is kept.
    #[test]
    fn opening_the_screen_closes_a_filter_box_left_open_while_the_read_was_in_flight() {
        let mut app = fixtures::allowed_app();
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        let _ = app.update(Msg::Key(KeyPress::TextChar('w')));
        let _ = app.update(Msg::Key(KeyPress::TextChar('e')));
        assert_eq!(
            app.mode(),
            InputMode::Text,
            "the box is open before the read lands"
        );
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(app.settings().is_some(), "the screen opened");
        assert_eq!(
            app.mode(),
            InputMode::Normal,
            "the box must not survive the screen opening"
        );
        assert_eq!(app.filter(), "we", "the typed query is kept, not discarded");
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

        let row = &app.settings().unwrap().snapshot().style_level;
        assert_eq!(
            row.source,
            StyleSource::Flag,
            "the flag beats the file rather than being dropped by it"
        );
        assert_eq!(row.value, "plain");
    }

    #[test]
    fn enter_on_a_text_row_opens_the_editor_seeded_with_the_current_value() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let (field, buffer) = app.settings().unwrap().typing().expect("the editor opens");
        assert_eq!(*field, SettingField::MaxCronSleep);
        assert_eq!(buffer, "30s");
    }

    #[test]
    fn typing_then_enter_arms_rather_than_writing() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..3 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for c in "45s".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        assert_eq!(app.update(Msg::Key(KeyPress::TextApply)), Effect::None);
        let prompt = app.settings().unwrap().pending().unwrap();
        assert!(
            !prompt.sent,
            "the editor arms; a second Enter is what sends"
        );
        assert!(prompt.text.contains("45s"), "got: {}", prompt.text);
        assert!(
            prompt.text.contains("SHEP_MAX_CRON_SLEEP"),
            "got: {}",
            prompt.text
        );
    }

    #[test]
    fn an_empty_editor_arms_an_unset() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..8 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.starts_with("unset max_cron_sleep?"), "got: {text}");
    }

    #[test]
    fn the_socket_confirm_rules_out_the_reload_it_would_otherwise_imply() {
        let mut app = fixtures::app_in_settings_on(SettingField::Socket);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("stopped and started"), "got: {text}");
        assert!(text.contains("a reload will not move it"), "got: {text}");
    }

    /// A refusal is discovered under the lock, so it lands after the confirm,
    /// and the typed text has to survive it.
    #[test]
    fn a_refused_write_reopens_the_editor_with_the_text_intact() {
        let mut app = fixtures::app_in_settings_on(SettingField::MaxCronSleep);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..3 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for c in "500ms".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(c)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let Effect::WriteSetting { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm))
        else {
            panic!("Enter must send");
        };
        let _ = app.update(Msg::SettingWritten {
            edit,
            ticket,
            result: Err("max_cron_sleep is 500ms, below the 1s floor".into()),
        });

        let (_, buffer) = app
            .settings()
            .unwrap()
            .typing()
            .expect("the editor reopens");
        assert_eq!(buffer, "500ms");
        assert_eq!(
            app.mode(),
            InputMode::Text,
            "a reopened editor owns the keyboard, or the text is unreachable"
        );
        assert!(
            app.notice()
                .unwrap()
                .to_string()
                .contains("below the 1s floor")
        );
    }

    #[test]
    fn escape_abandons_the_editor_and_keeps_the_screen_open() {
        let mut app = fixtures::app_in_settings_on(SettingField::Socket);
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextAbandon));
        assert!(app.settings().unwrap().typing().is_none());
        assert!(app.settings().is_some());
    }

    #[test]
    fn a_closed_scalar_has_no_editor() {
        let mut app = fixtures::app_in_settings_with_control(); // on log_level
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            app.settings().unwrap().typing().is_none(),
            "log_level is a cycle, not a text field"
        );
    }

    #[test]
    fn movement_cancels_an_armed_candidate_rather_than_also_moving() {
        let mut app = fixtures::app_in_settings_with_control(); // cursor on log_level
        let before = app.settings().unwrap().cursor();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.settings().unwrap().pending().is_some(),
            "space must arm before this test means anything"
        );
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert!(
            app.settings().unwrap().pending().is_none(),
            "the armed candidate must not survive the movement key"
        );
        assert_eq!(
            app.settings().unwrap().cursor(),
            before,
            "the cursor must not also move on the same keypress"
        );
    }

    #[test]
    fn refresh_cancels_an_armed_candidate_rather_than_silently_dropping_it() {
        let mut app = fixtures::app_in_settings_with_control(); // cursor on log_level
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.settings().unwrap().pending().is_some(),
            "space must arm before this test means anything"
        );
        let effect = app.update(Msg::Key(KeyPress::Refresh));
        assert_eq!(
            effect,
            Effect::None,
            "a cancel must not also raise a reload"
        );
        assert!(
            app.settings().unwrap().pending().is_none(),
            "the armed candidate must not survive `r`"
        );
    }

    #[test]
    fn arming_a_dog_names_the_live_apply_and_not_a_reload() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("it starts now, no reload"), "got: {text}");
    }

    #[test]
    fn disabling_says_it_deregisters() {
        let mut app = fixtures::app_in_settings_on_enabled_dog("otel");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("deregistered"), "got: {text}");
    }

    /// One message still yields one effect.
    #[test]
    fn a_written_dog_toggle_raises_the_daemon_half() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteDog { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter must send the file half first");
        };
        let effect = app.update(Msg::DogWritten {
            edit,
            ticket,
            result: Ok(DogSource::BuiltIn),
        });
        assert!(matches!(
            effect,
            Effect::Send(Sent::Dog { enable: true, .. })
        ));
    }

    #[test]
    fn a_refused_file_half_never_reaches_the_shepherd() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteDog { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter must send the file half first");
        };
        let effect = app.update(Msg::DogWritten {
            edit,
            ticket,
            result: Err("permission denied".into()),
        });
        assert_eq!(
            effect,
            Effect::None,
            "a failed write must not ask the shepherd"
        );
        assert!(app.notice().unwrap().is_grave());
    }

    /// The scalars never leave the machine; a dog's second half does.
    #[test]
    fn a_dog_toggle_refuses_while_the_link_is_gone() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".into(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let effect = app.update(Msg::Key(KeyPress::Cycle));
        assert_eq!(effect, Effect::None);
        assert!(app.settings().unwrap().pending().is_none(), "nothing arms");
        assert!(app.notice().unwrap().is_grave());
    }

    #[test]
    fn a_scalar_still_edits_while_the_link_is_gone() {
        let mut app = fixtures::app_in_settings_with_control(); // on log_level
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".into(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.settings().unwrap().pending().is_some(),
            "a scalar is local file I/O and needs no shepherd"
        );
    }

    /// Drives a dog toggle to `Effect::Send`: arm, confirm the file half, then
    /// land `Msg::DogWritten` so the daemon half goes out.
    fn armed_and_sent_dog(name: &str) -> (App, Sent) {
        let mut app = fixtures::app_in_settings_on_dog(name);
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteDog { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter must send the file half first");
        };
        let Effect::Send(sent) = app.update(Msg::DogWritten {
            edit,
            ticket,
            result: Ok(DogSource::BuiltIn),
        }) else {
            panic!("a landed write must raise the daemon half");
        };
        (app, sent)
    }

    /// `EnableDog` answers `Response::DogStarted`. The sentence names what the
    /// shepherd did rather than a bare "done".
    #[test]
    fn a_landed_enable_names_what_the_shepherd_did() {
        let (mut app, sent) = armed_and_sent_dog("metrics");
        assert!(
            app.settings().unwrap().pending().is_some(),
            "the sent line stays up until the reply lands"
        );
        let info = ProcessInfo::builder(50, "metrics", ProcStatus::Online)
            .pid(Some(50_000))
            .dog(Some(DogSource::BuiltIn))
            .build();
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::DogStarted(info)),
        });
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("enable metrics: the shepherd started it")
        );
        assert!(!app.notice().unwrap().is_grave());
        assert!(
            app.settings().unwrap().pending().is_none(),
            "the sent line clears once the reply lands"
        );
    }

    /// `DisableDog` answers `Response::Deleted`, the reply `Delete` gives. The
    /// sentence names the deregistration, since the confirm is gone by now.
    #[test]
    fn a_landed_disable_names_the_deregistration() {
        let (mut app, sent) = armed_and_sent_dog("otel");
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::Deleted(vec![50])),
        });
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("disable otel: the shepherd stopped and deregistered it")
        );
        assert!(!app.notice().unwrap().is_grave());
    }

    /// Whether the settings screen's own dogs list still says `name` is
    /// enabled. Reads the snapshot, not the file: the two can disagree.
    #[track_caller]
    fn dog_enabled_in_view(app: &App, name: &str) -> bool {
        app.settings()
            .expect("the settings screen is open")
            .snapshot()
            .dogs
            .iter()
            .find(|dog| dog.name == name)
            .expect("the fixture carries this dog")
            .enabled
    }

    /// The file half lands first, so `DogView.enabled` is stale by the time
    /// this reply arrives while running keeps updating off the poll. Without
    /// the re-read a landed `enable metrics` reads `metrics | no | online`.
    #[test]
    fn a_landed_toggle_re_reads_the_file_in_both_directions() {
        for (name, enable) in [("metrics", true), ("otel", false)] {
            let (mut app, sent) = armed_and_sent_dog(name);
            assert_eq!(
                dog_enabled_in_view(&app, name),
                !enable,
                "{name}: the fixture starts on the other bit"
            );
            let reply = if enable {
                Response::DogStarted(
                    ProcessInfo::builder(50, name, ProcStatus::Online)
                        .pid(Some(50_000))
                        .dog(Some(DogSource::BuiltIn))
                        .build(),
                )
            } else {
                Response::Deleted(vec![50])
            };

            let effect = app.update(Msg::Replied {
                sent,
                result: Ok(reply),
            });

            assert_eq!(
                effect,
                Effect::LoadSettings,
                "{name}: a landed toggle has to re-read the file it changed"
            );
            assert_eq!(
                dog_enabled_in_view(&app, name),
                !enable,
                "{name}: nothing is folded into the row by hand -- the re-read is the repair"
            );

            // The re-read landing, with the bit the write put in the file.
            let mut fresh = app.settings().unwrap().snapshot().clone();
            for dog in &mut fresh.dogs {
                if dog.name == name {
                    dog.enabled = enable;
                }
            }
            app.update(Msg::Settings { result: Ok(fresh) });

            assert_eq!(
                dog_enabled_in_view(&app, name),
                enable,
                "{name}: and the row agrees with the file once it lands"
            );
        }
    }

    /// `metrics` is armed as an `enable`, so `Response::Deleted` is the right
    /// shape for the wrong verb and `Response::Pong` is a reply this binary has
    /// never heard of.
    #[test]
    fn an_unrecognised_dog_reply_says_so_rather_than_reading_as_success() {
        for reply in [Response::Pong, Response::Deleted(vec![1])] {
            let (mut app, sent) = armed_and_sent_dog("metrics");
            app.update(Msg::Replied {
                sent,
                result: Ok(reply),
            });
            assert_eq!(
                app.notice().map(ToString::to_string).as_deref(),
                Some(
                    "enable metrics: the shepherd answered something this lookout does not understand"
                )
            );
            assert!(app.notice().unwrap().is_grave());
        }
    }

    #[test]
    fn a_dog_reply_that_failed_to_send_says_so_under_the_same_prefix() {
        let (mut app, sent) = armed_and_sent_dog("metrics");
        app.update(Msg::Replied {
            sent,
            result: Err(RequestError::Closed),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.starts_with("enable metrics: "), "got {said:?}");
        assert!(said.contains(&RequestError::Closed.to_string()));
    }

    /// `e` raises the read, not the open: the pane shows the shepherd's own
    /// answer or it shows nothing, the same rule `s` and `Msg::Settings`
    /// already follow for the settings screen.
    #[test]
    fn e_asks_for_the_selected_sheeps_config_and_the_reply_opens_the_pane() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let effect = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig {
                name: "web".to_string()
            })
        );
        assert!(app.config_pane().is_none(), "nothing opens on the keypress");

        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        let pane = app.config_pane().expect("the reply opens the pane");
        assert_eq!(pane.target().name(), "web");
        assert_eq!(pane.fields().len(), 41);
    }

    /// `s` then `e` fire two reads; if the settings one lands first it opens
    /// the settings screen, and the config-pane reply that follows replaces
    /// it (`Body` cannot hold both at once). `Escape` from the config pane
    /// must land on the dashboard, not resurrect the settings screen it
    /// walked past on the way in — see the doc note on [`Body`] and on
    /// [`App::close_pane`].
    #[test]
    fn escape_from_a_config_pane_that_outraced_a_settings_read_lands_on_the_dashboard() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(app.settings().is_some(), "the settings reply lands first");
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert!(
            app.config_pane().is_some(),
            "the config pane reply replaces the settings screen"
        );
        assert!(app.settings().is_none());
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            matches!(app.body(), Body::FlockTable),
            "esc from the pane goes to the dashboard, not back to settings"
        );
        assert!(app.settings().is_none());
    }

    /// The sibling above pins the order where the settings read wins. This
    /// is the other one, and it is the order that used to corrupt state:
    /// the pane's own reply lands first, and the settings read arrives with
    /// the operator two actions past caring about it. `Msg::Settings` wrote
    /// `body` unconditionally, so the reply replaced the pane, reset the
    /// cursor as if opening, and forced `InputMode::Normal` while
    /// `config_target` and `pane_menu` went on describing a pane that was
    /// no longer on screen.
    #[test]
    fn a_settings_read_landing_after_a_config_pane_leaves_the_pane_up() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Settings));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert!(app.config_pane().is_some(), "the pane reply lands first");

        let _ = app.update(Msg::Settings {
            result: Ok(fixtures::settings_snapshot()),
        });
        assert!(
            app.config_pane().is_some(),
            "the stale settings reply leaves the pane alone"
        );
        assert!(app.settings().is_none());
    }

    /// `typed_text_of` answers for the two free-text settings fields, and
    /// its `Some` used to arm `InputMode::Text` whether or not the editor
    /// it types into was still there. Opening a dog section replaces the
    /// settings screen, so a refusal landing afterwards armed a text mode
    /// over a pane, and `on_key` then sent every later keystroke to a text
    /// handler owning nothing.
    #[test]
    fn a_refused_settings_write_landing_over_a_dog_pane_does_not_arm_text_mode() {
        let mut app = fixtures::app_in_dog_pane();
        assert!(app.config_pane().is_some(), "the pane is open");

        let _ = app.update(Msg::SettingWritten {
            edit: SettingEdit::Set {
                field: SettingField::MaxCronSleep,
                value: "500ms".to_string(),
            },
            // Any ticket: there is no settings screen to hold one.
            ticket: 0,
            result: Err("max_cron_sleep is 500ms, below the 1s floor".to_string()),
        });

        assert!(app.config_pane().is_some(), "the pane survives the reply");
        assert_ne!(
            app.mode(),
            InputMode::Text,
            "there is no settings editor for the keystrokes to reach"
        );
    }

    #[test]
    fn e_with_nothing_selected_asks_for_nothing() {
        let mut app = fixtures::app_with(Vec::new(), fixtures::plain());
        assert_eq!(app.update(Msg::Key(KeyPress::Edit)), Effect::None);
        assert!(app.config_pane().is_none());
    }

    /// The pane opens on whatever was selected and pins it: full screen
    /// leaves no table to change a selection with.
    /// The feed follows the pinned sheep, not the selection.
    ///
    /// The pane pins one sheep for its lifetime, but `Msg::Snapshot` reseats
    /// the selection whatever screen is showing. So a pinned sheep leaving
    /// the flock moved the selection to another one, and the next refresh
    /// read that sheep's log files while the title still named the pinned
    /// one: one sheep's output under another sheep's heading.
    ///
    /// Asserts the row the feed reads, which is the thing that was wrong.
    /// Asserting the rendered title would have passed throughout, because
    /// the title was always right.
    #[test]
    fn the_feed_follows_the_pinned_sheep_when_the_selection_moves() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(9, "web", ProcStatus::Online).build(),
                ProcessInfo::builder(4, "billing", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        app.select(RowKey::Sheep(9));
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        assert_eq!(
            app.feed_row().map(|row| row.info.id),
            Some(9),
            "the pane opened on 9"
        );

        // 9 leaves the flock. The reseat moves the selection to 4.
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(4, "billing", ProcStatus::Online).build()],
            at: Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "the selection did move, which is the setup for the bug"
        );
        assert_eq!(
            app.feed_row().map(|row| row.info.id),
            None,
            "and the feed reads nothing rather than billing's log"
        );
    }

    /// The same regression as [`the_feed_follows_the_pinned_sheep_when_the_selection_moves`],
    /// through the sheep pane's own embedded feed rather than the
    /// full-screen one: `Confirm` pins the pane to sheep 9, `feed_row`
    /// answers 9 while it is open, sheep 9 then leaves the flock and the
    /// reseat moves the selection to 4, and `feed_row` must still answer
    /// `None` (sheep 9 is gone) rather than 4 (the selection's own new row).
    /// Before this task, `feed_row` had no `Body::Sheep` branch at all and
    /// fell through to `self.selected_row()` unconditionally, so this would
    /// have read billing's log under a pane still naming `web`.
    #[test]
    fn the_feed_row_follows_the_sheep_panes_own_pinned_sheep_when_the_selection_moves() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(9, "web", ProcStatus::Online).build(),
                ProcessInfo::builder(4, "billing", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        app.select(RowKey::Sheep(9));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(
            app.feed_row().map(|row| row.info.id),
            Some(9),
            "the pane opened on 9"
        );

        // 9 leaves the flock. The reseat moves the selection to 4.
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(4, "billing", ProcStatus::Online).build()],
            at: Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "the selection did move, which is the setup for the bug"
        );
        assert_eq!(
            app.feed_row().map(|row| row.info.id),
            None,
            "and the feed reads nothing rather than billing's log"
        );
    }

    #[test]
    fn b_opens_the_pane_on_the_selected_sheep() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let pane = app.bleats_pane().expect("the pane is open");
        assert!(matches!(pane.sheep(), RowKey::Sheep(id) if *id == 9));
    }

    /// `close_pane` always lands on the dashboard, never on whatever screen
    /// preceded the pane. Same rule the config pane follows.
    #[test]
    fn esc_from_the_bleats_pane_lands_on_the_dashboard() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// `Escape` with a chip set drops the chip and leaves the pane open; only
    /// the next one closes it. Both halves in one test, because the guard in
    /// `on_bleats_key` is invisible to a test that never sets a chip: with a
    /// freshly opened pane `drop_newest_chip` returns `false` either way, so
    /// removing the guard entirely still passes every other `esc` test in this
    /// file. Measured, not assumed.
    #[test]
    fn esc_drops_a_chip_before_it_closes_the_pane() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut()
            .expect("the pane is open")
            .set_min_level(Some(super::super::level::Level::Warn));

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            matches!(app.body(), Body::Bleats(_)),
            "the first Escape spends the chip and keeps the pane"
        );
        assert!(
            app.bleats_pane()
                .expect("still open")
                .filters()
                .min_level
                .is_none(),
            "the chip it spent was the level one"
        );

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            matches!(app.body(), Body::FlockTable),
            "with no chips left the next Escape closes"
        );
    }

    /// `o` cycles the stream axis through its three states and back. Three
    /// presses return to where it began, which is what makes it a cycle
    /// rather than a toggle that strands the operator on `err`.
    #[test]
    fn o_cycles_the_stream_axis_and_returns_to_both() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        assert!(app.bleats_pane().expect("open").filters().stream.is_none());
        let _ = app.update(Msg::Key(KeyPress::StreamCycle));
        let first = app.bleats_pane().expect("open").filters().stream;
        assert!(first.is_some(), "one press sets an axis");
        let _ = app.update(Msg::Key(KeyPress::StreamCycle));
        let _ = app.update(Msg::Key(KeyPress::StreamCycle));
        assert!(
            app.bleats_pane().expect("open").filters().stream.is_none(),
            "three presses land back on both"
        );
    }

    /// `m` raises the minimum level and eventually clears it. The unset state
    /// has to be reachable by key, or an operator who sets a minimum can never
    /// see unlevelled output again without closing the pane.
    #[test]
    fn m_cycles_the_level_axis_back_to_unset() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let mut seen_some = false;
        for _ in 0..8 {
            let _ = app.update(Msg::Key(KeyPress::LevelCycle));
            if app
                .bleats_pane()
                .expect("open")
                .filters()
                .min_level
                .is_some()
            {
                seen_some = true;
            }
        }
        assert!(seen_some, "the cycle passes through a set minimum");
        // Whatever the cycle length, it must return to unset within one lap.
        let mut cleared = false;
        for _ in 0..8 {
            let _ = app.update(Msg::Key(KeyPress::LevelCycle));
            if app
                .bleats_pane()
                .expect("open")
                .filters()
                .min_level
                .is_none()
            {
                cleared = true;
                break;
            }
        }
        assert!(cleared, "the cycle returns to unset");
    }

    /// Scrolling back stops the follow, or the next refresh undoes the
    /// operator's keypress.
    #[test]
    fn scrolling_back_stops_following() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        assert!(app.bleats_pane().expect("open").following());
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert!(
            !app.bleats_pane().expect("open").following(),
            "one line back is enough to mean the operator took over"
        );
    }

    /// `G` is the way back to the live tail, so it restores following as well
    /// as jumping.
    #[test]
    fn g_returns_to_the_end_and_resumes_following() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let pane = app.bleats_pane().expect("open");
        assert!(pane.following(), "G resumes the follow");
        assert_eq!(pane.scroll_offset(), 0, "and lands on the newest line");
    }

    /// A filter that hides most of the window must not leave the offset
    /// pointing past the end of what survives.
    #[test]
    fn a_narrowing_filter_clamps_the_scroll_offset() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        let _ = app.update(Msg::Key(KeyPress::PageUp));
        let _ = app.update(Msg::Key(KeyPress::PageUp));

        // One survivor, far fewer than the offset two pages back. Without
        // the clamp the skip underflows: it panics in debug and wraps in
        // release, and a wrapped skip yields no feed lines at all.
        app.bleats_pane_mut_for_tests()
            .expect("open")
            .set_match("line-119".to_string());
        let text =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 160, 40));

        // Asserts a rendered FEED line, tagged `out`, not merely that the
        // text appears: the filter row echoes the matcher back as its own
        // `match line-119` chip, so `contains("line-119")` passes even when
        // every feed line was skipped away. `!text.is_empty()` was weaker
        // still, and passed in release with the clamp removed because the
        // title and the chip alone kept the buffer non-empty.
        assert!(
            text.contains("out  line-119"),
            "the one surviving line is drawn in the body: {text}"
        );
    }

    /// `SelectUp`/`SelectDown` actually move the window, not just the
    /// `following` flag: pins which lines are on screen before and after,
    /// so a wrong direction or an off-by-one is caught rather than passing
    /// on the presence of any text at all.
    #[test]
    fn select_up_and_down_move_which_lines_are_on_screen() {
        let mut app = fixtures::bleats_pane_with_lines(20);
        app.note_body_rows(6); // 1 title row + 5 body rows
        let before =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        assert!(
            before.contains("line-19"),
            "starts pinned to the tail: {before}"
        );
        assert!(
            !before.contains("line-14"),
            "one line older than the window: {before}"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let after = fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        assert!(
            after.contains("line-14") && !after.contains("line-19"),
            "one line back drops the newest line and reveals the one above the old window: {after}"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let restored =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        assert_eq!(
            restored, before,
            "one line forward undoes the one line back"
        );
    }

    /// `ctrl-u`/`ctrl-d` move by a body height rather than a line, and the
    /// body height is what `note_body_rows` last reported, not a hardcoded
    /// guess.
    /// Two pages back leave no line unseen, with a filter chip on screen.
    ///
    /// The chip costs a row, so the body is two rows shorter than the pane,
    /// not one. A jump sized to the pane rather than the body skips a line
    /// between consecutive pages: it belongs to neither window and `ctrl-u`
    /// alone never renders it. Paging back down is symmetric either way,
    /// which is why the round trip looked fine and the gap did not.
    ///
    /// Asserts every line across the two windows, not the offset: an
    /// operator reading history by paging silently misses the gap, so a
    /// test that only watched the number move would too.
    /// Wrapped paging leaves no line unseen either, over a feed whose older
    /// lines are far longer than its newest.
    ///
    /// Measured: with the page sized from the tail, three steps into the
    /// wrapped stretch showed `old-32..old-35` where the step before showed
    /// `new-4..new-15`, skipping eight lines no screen ever drew, and the
    /// next step skipped eight more. Sized from the current window instead,
    /// the same walk is contiguous.
    ///
    /// That is the off-by-one page size one level harder: sized from the
    /// wrong place the jump was one row too many, here it is most of a
    /// screen. Both were invisible for the same reason, that paging back
    /// cancels the error out.
    ///
    /// `note_body_width` matters as much as `note_body_rows` here. The
    /// wrap-aware path is skipped entirely while the pane's width is `0`,
    /// which is what a test that never reports one leaves it as, so a test
    /// missing that call passes against a page size that was never wrapped.
    ///
    /// Walks in one direction and asserts every line across the windows,
    /// because paging back is symmetric and would hide the gap.
    /// Over-scrolling does not make `j` stop working.
    ///
    /// `scroll_up` saturating-adds and the clamp lived only in the render, so
    /// the stored offset climbed past anything that changes the frame.
    /// Measured before the ceiling: a 20-line feed with a 5-row body left the
    /// offset at 39 after 40 `k` presses, where 15 was the most that did
    /// anything, and the operator then pressed `j` 25 times before the window
    /// moved. `G` and `f` escape that; `j` is the reflex and it did nothing.
    /// Holding `N` past the oldest match does not deaden `n` either.
    ///
    /// `k` and `ctrl-u` route through the clamp; `N` called `match_prev`
    /// directly and climbed past it. The existing `n`/`N` test presses `N`
    /// once, so it never reached the ceiling.
    #[test]
    fn over_stepping_back_through_matches_does_not_deaden_the_step_forward() {
        let mut app = fixtures::bleats_pane_with_lines(20);
        app.note_body_rows(6);
        app.note_body_width(80);
        app.bleats_pane_mut_for_tests()
            .expect("open")
            .set_match("line".to_string());

        for _ in 0..40 {
            let _ = app.update(Msg::Key(KeyPress::MatchPrev));
        }
        let parked =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        let _ = app.update(Msg::Key(KeyPress::MatchNext));
        let after_one =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        assert_ne!(parked, after_one, "one step forward has to move the window");
    }

    /// A frozen lookout with the pane open stops re-reading the log files.
    ///
    /// `Msg::Bleats` throws the tail away while the link is down, so every
    /// read was work done and discarded once a second. `Msg::Snapshot` and
    /// `select_at` already guard on the same thing.
    #[test]
    fn a_frozen_lookout_does_not_re_read_the_pane_s_log() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let now = Instant::now();
        assert_eq!(app.update(Msg::Tick { now }), Effect::RefreshFeed);

        let _ = app.update(Msg::Frozen {
            at_local: "2026-09-08 09:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.update(Msg::Tick { now }),
            Effect::None,
            "a read whose result Msg::Bleats discards is not worth doing"
        );
    }

    #[test]
    fn over_scrolling_back_does_not_deaden_the_scroll_forward() {
        let mut app = fixtures::bleats_pane_with_lines(20);
        app.note_body_rows(6);
        app.note_body_width(80);

        for _ in 0..40 {
            let _ = app.update(Msg::Key(KeyPress::SelectUp));
        }
        let parked =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let after_one =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        assert_ne!(
            parked, after_one,
            "one press forward has to move the window, however far back the \
             operator scrolled"
        );
    }

    #[test]
    fn wrapped_pages_leave_no_line_unseen() {
        let mut app = fixtures::bleats_pane_with_mixed_line_lengths();
        let _ = app.update(Msg::Key(KeyPress::WrapToggle));
        app.note_body_rows(13);
        app.note_body_width(60);

        let mut seen = String::new();
        for _ in 0..12 {
            seen.push_str(&fixtures::render_all(
                &super::super::view::bleats_full::draw_lines(&app, 60, 13),
            ));
            let _ = app.update(Msg::Key(KeyPress::PageUp));
        }
        for n in 30..40 {
            assert!(
                seen.contains(&format!("old-{n} ")),
                "old-{n} fell between two wrapped pages"
            );
        }
    }

    /// The same, walking `ctrl-d` instead. Its own test because its own
    /// arithmetic: a single shared page size drops lines in one direction
    /// whichever way it is measured.
    ///
    /// This one was missing when the wrapped-page fix landed, and that is
    /// exactly why the fix was half a fix. The sibling test above presses
    /// only `PageUp`, so a `PageDown` that skipped eight lines a step passed
    /// the whole suite.
    #[test]
    fn wrapped_pages_down_leave_no_line_unseen() {
        /// The `old-N`/`new-N` ids the pane is drawing, in order.
        fn shown(app: &App) -> Vec<String> {
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(app, 60, 13))
                .split_whitespace()
                .filter(|word| word.starts_with("old-") || word.starts_with("new-"))
                .map(str::to_string)
                .collect()
        }

        let mut app = fixtures::bleats_pane_with_mixed_line_lengths();
        let _ = app.update(Msg::Key(KeyPress::WrapToggle));
        app.note_body_rows(13);
        app.note_body_width(60);

        // Into the wrapped stretch, then forward one page at a time.
        for _ in 0..6 {
            let _ = app.update(Msg::Key(KeyPress::PageUp));
        }

        // Consecutive windows, not eventual coverage. A long enough walk
        // sees every line whatever the step size, so the gap only shows in
        // the join between one window and the next.
        for step in 0..5 {
            let before = shown(&app);
            let _ = app.update(Msg::Key(KeyPress::PageDown));
            let after = shown(&app);
            let (Some(last), Some(first)) = (before.last(), after.first()) else {
                continue;
            };
            // The feed is `old-0..old-39` then `new-0..new-39`, so this is
            // each id's position in it.
            let position = |id: &str| -> usize {
                let (prefix, n) = id.split_once('-').expect("id-N");
                let n: usize = n.parse().expect("a number");
                if prefix == "old" { n } else { 40 + n }
            };
            assert!(
                position(first) <= position(last) + 1,
                "step {step} jumped from {last} to {first}, leaving a gap: \
                 {before:?} then {after:?}"
            );
            // Two-sided. The check above catches a page that steps over
            // lines; this one catches a page that barely steps at all, which
            // `page_amount_down` returning a constant 1 would do while
            // satisfying the first assertion trivially.
            assert!(
                position(first) + 1 >= position(before[0].as_str()) + before.len(),
                "step {step} moved by almost nothing, so it is not a page: \
                 {before:?} then {after:?}"
            );
        }
    }

    #[test]
    fn consecutive_pages_leave_no_line_unseen_while_a_chip_is_showing() {
        let mut app = fixtures::bleats_pane_with_lines(40);
        app.bleats_pane_mut_for_tests()
            .expect("open")
            .set_match("line".to_string());
        app.note_body_rows(6); // 1 title + 1 filter row + 4 body rows

        let first = fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        let _ = app.update(Msg::Key(KeyPress::PageUp));
        let second =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        let _ = app.update(Msg::Key(KeyPress::PageUp));
        let third = fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));

        let seen = format!("{first}{second}{third}");
        // The three windows are contiguous, so every line from the oldest
        // one drawn through the newest must appear in one of them.
        for n in 28..=39 {
            assert!(
                seen.contains(&format!("line-{n}")),
                "line-{n} fell between two pages: {seen}"
            );
        }
    }

    #[test]
    fn page_up_and_down_move_by_a_body_height() {
        let mut app = fixtures::bleats_pane_with_lines(20);
        app.note_body_rows(6); // 1 title row + 5 body rows
        let before =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        assert!(
            before.contains("line-19"),
            "starts pinned to the tail: {before}"
        );

        let _ = app.update(Msg::Key(KeyPress::PageUp));
        let after = fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        assert!(
            after.contains("line-10") && !after.contains("line-15"),
            "a page is 5 body rows, so one page up lands on 10..=14, not 1 row back: {after}"
        );
        assert!(
            !app.bleats_pane().expect("open").following(),
            "ctrl-u is backward movement too"
        );

        let _ = app.update(Msg::Key(KeyPress::PageDown));
        let restored =
            fixtures::render_all(&super::super::view::bleats_full::draw_lines(&app, 80, 6));
        assert_eq!(restored, before, "one page down undoes one page up");
    }

    /// `f` toggles following explicitly, and turning it back on snaps to the
    /// tail rather than leaving the view wherever it was scrolled to.
    #[test]
    fn f_toggles_following_and_resuming_snaps_to_the_tail() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert!(!app.bleats_pane().expect("open").following());

        let _ = app.update(Msg::Key(KeyPress::FollowToggle));
        let pane = app.bleats_pane().expect("open");
        assert!(pane.following(), "f turned it back on");
        assert_eq!(pane.scroll_offset(), 0, "and snapped to the tail");

        let _ = app.update(Msg::Key(KeyPress::FollowToggle));
        assert!(
            !app.bleats_pane().expect("open").following(),
            "f is a toggle, not a one-way switch"
        );
    }

    /// `n` with no match axis set does nothing, rather than quietly becoming
    /// a line-movement key.
    /// `n` and `N` step between matches, in opposite directions, and both
    /// stop the follow.
    ///
    /// The no-matcher case below covers only the inert branch, so a swapped
    /// direction, a wrong step, or a `following` regression would all have
    /// shipped unseen.
    /// A wrapped line's height is measured in display columns, not `char`s.
    ///
    /// Sixty full-width characters occupy 120 columns, so at this width they
    /// wrap to about twice the rows sixty ASCII characters would. Every other
    /// wrap test here is ASCII, where the two counts agree and a regression
    /// to `chars().count()` would pass unnoticed. This repo has fixed that
    /// same bug on two other branches.
    #[test]
    fn a_wide_character_line_wraps_by_columns_not_char_count() {
        let mut wide = fixtures::bleats_pane_with_a_wide_line();
        let _ = wide.update(Msg::Key(KeyPress::WrapToggle));
        wide.note_body_rows(30);
        wide.note_body_width(40);
        let wide_rows = super::super::view::bleats_full::draw_lines(&wide, 40, 30).len();

        // 60 double-width characters are 120 display columns. The body is
        // 40 wide less the 5-column stream tag, so 35, and 120 columns need
        // 4 rows. Counting `char`s instead gives 60 over 35, which is 2.
        // The title takes one more row, so 5 total by columns and 3 by
        // `char`s: the assertion separates the two.
        assert!(
            wide_rows >= 5,
            "wrapped by columns that is 4 body rows plus a title; by \
             `char`s it would be 2. Got {wide_rows}"
        );
    }

    #[test]
    fn n_and_shift_n_step_between_matches_in_opposite_directions() {
        let mut app = fixtures::bleats_pane_with_lines(40);
        app.note_body_rows(6);
        app.bleats_pane_mut_for_tests()
            .expect("open")
            .set_match("line".to_string());
        assert!(app.bleats_pane().expect("open").following());

        let _ = app.update(Msg::Key(KeyPress::MatchPrev));
        let back = app.bleats_pane().expect("open").scroll_offset();
        assert!(back > 0, "N steps toward older matches");
        assert!(
            !app.bleats_pane().expect("open").following(),
            "stepping back is backward movement, so the follow stops"
        );

        let _ = app.update(Msg::Key(KeyPress::MatchNext));
        assert!(
            app.bleats_pane().expect("open").scroll_offset() < back,
            "n steps the other way"
        );
    }

    #[test]
    fn n_without_a_matcher_does_nothing() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        let before = app.bleats_pane().expect("open").scroll_offset();
        let _ = app.update(Msg::Key(KeyPress::MatchNext));
        assert_eq!(app.bleats_pane().expect("open").scroll_offset(), before);
        assert!(app.bleats_pane().expect("open").following());
    }

    /// Wrapping a long line makes it occupy more rows than one, which is the
    /// whole point, and the pane must still draw inside its area.
    #[test]
    fn a_wrapped_line_occupies_more_rows_and_stays_in_the_area() {
        let mut app = fixtures::bleats_pane_with_long_line();
        let unwrapped = fixtures::draw_lines(&app, 80, 20).len();
        let _ = app.update(Msg::Key(KeyPress::WrapToggle));
        let wrapped = fixtures::draw_lines(&app, 80, 20);
        assert!(wrapped.len() <= 20, "never draws past its own height");
        assert!(
            wrapped.iter().filter(|line| !line.spans.is_empty()).count() >= unwrapped,
            "wrapping uses at least as many rows as not wrapping"
        );
    }

    /// The brief's own test above (`a_wrapped_line_occupies_more_rows_and_stays_in_the_area`)
    /// only asserts `>=`, which a `w` that did nothing at all would still
    /// satisfy: the unwrapped and "wrapped" renders would be identical, and
    /// identical passes `>=` too. This pins the number changing, not merely
    /// never shrinking.
    #[test]
    fn toggling_wrap_actually_changes_how_many_rows_a_long_line_draws() {
        let mut app = fixtures::bleats_pane_with_long_line();
        let unwrapped = fixtures::draw_lines(&app, 80, 20).len();
        let _ = app.update(Msg::Key(KeyPress::WrapToggle));
        let wrapped = fixtures::draw_lines(&app, 80, 20).len();
        assert!(
            wrapped > unwrapped,
            "wrap must add rows for a line too long for one, not just permit them: \
             {unwrapped} unwrapped rows, {wrapped} wrapped: got the same window"
        );
    }

    /// `/` opens the match input rather than doing nothing, which is what it
    /// did when this pane first shipped.
    #[test]
    fn slash_opens_the_match_input_in_the_bleats_pane() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        assert_eq!(app.mode(), InputMode::Text, "typing goes to the pane");
    }

    /// Typing into the match box narrows live, the same as the flock
    /// table's own `/` box: the operator sees the filter row react to every
    /// keystroke rather than only after `Enter`.
    #[test]
    fn typing_in_the_match_box_narrows_live() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        let _ = app.update(Msg::Key(KeyPress::TextChar('p')));
        let _ = app.update(Msg::Key(KeyPress::TextChar('o')));
        assert_eq!(
            app.bleats_pane()
                .expect("open")
                .filters()
                .matcher
                .as_deref(),
            Some("po")
        );
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(
            app.bleats_pane()
                .expect("open")
                .filters()
                .matcher
                .as_deref(),
            Some("po"),
            "TextApply keeps what was already applied live"
        );
    }

    /// `TextAbandon` restores the match axis to what it held before the box
    /// opened, discarding whatever was typed since, rather than clearing it
    /// outright the way the flock table's own filter box does.
    #[test]
    fn abandoning_the_match_box_restores_the_axis_it_had_before() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut()
            .expect("open")
            .set_match("pool".to_string());

        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        let _ = app.update(Msg::Key(KeyPress::TextChar('x')));
        assert_eq!(
            app.bleats_pane()
                .expect("open")
                .filters()
                .matcher
                .as_deref(),
            Some("poolx"),
            "typing narrowed live"
        );
        let _ = app.update(Msg::Key(KeyPress::TextAbandon));
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(
            app.bleats_pane()
                .expect("open")
                .filters()
                .matcher
                .as_deref(),
            Some("pool"),
            "abandon restores what the axis held before the edit"
        );
    }

    /// `o` and `m` are global bindings, so the dashboard sees them too; with
    /// no bleats pane open there is no stream or level axis to cycle, and
    /// the reducer says so explicitly rather than falling through to a
    /// wildcard, the same way it already does for `KeyPress::Bleats` here.
    #[test]
    fn stream_and_level_cycle_are_inert_on_the_dashboard() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        assert_eq!(app.update(Msg::Key(KeyPress::StreamCycle)), Effect::None);
        assert_eq!(app.update(Msg::Key(KeyPress::LevelCycle)), Effect::None);
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// `b` with nothing selected asks for nothing, the way `e` does.
    #[test]
    fn b_with_nothing_selected_opens_no_pane() {
        let mut app = fixtures::app_with(Vec::new(), fixtures::plain());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        assert!(app.bleats_pane().is_none());
    }

    /// A group has no single sheep, so `selected_row` answers `None` for
    /// one. Every instance behind it shares one stored spec, and the group
    /// row is what a multi-instance app shows by default.
    #[test]
    fn e_on_a_group_row_asks_by_the_apps_name() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(1, "web", ProcStatus::Online)
                    .instance(Some(0))
                    .build(),
                ProcessInfo::builder(2, "web", ProcStatus::Online)
                    .instance(Some(1))
                    .build(),
            ],
            fixtures::plain(),
        );
        app.update(Msg::Key(KeyPress::SelectFirst));
        assert!(
            app.selected_row().is_none(),
            "the selection is the group row, not one instance"
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::Edit)),
            Effect::Send(Sent::SheepConfig {
                name: "web".to_string()
            })
        );
    }

    #[test]
    fn escape_closes_the_pane_and_does_not_quit() {
        let mut app = fixtures::app_in_sheep_pane();
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        assert!(app.config_pane().is_none());
    }

    /// The wire carries no env value for any key, Flockfile or store, so
    /// every one renders the same way. SheepConfigView::new clears the map.
    #[test]
    fn every_env_value_renders_as_set_and_never_as_itself() {
        let app = fixtures::app_in_sheep_pane_with_env(&[("NODE_ENV", "production")]);
        let rows = fixtures::config_pane_env_rows_for_tests(
            app.config_pane().expect("the pane is open"),
            app.pane_menu().as_ref(),
        );
        assert!(rows.iter().any(|row| row.contains("NODE_ENV")), "{rows:?}");
        assert!(
            rows.iter().all(|row| !row.contains("production")),
            "an env value reached the pane: {rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("(set)")), "{rows:?}");
    }

    #[test]
    fn one_cursor_walks_from_the_last_field_into_the_env_keys() {
        let mut app = fixtures::app_in_sheep_pane_with_env(&[("NODE_ENV", "x")]);
        app.update(Msg::Key(KeyPress::SelectLast));
        assert!(matches!(
            app.config_pane().unwrap().rows().last(),
            Some(PaneRow::AddEnv)
        ));
    }

    #[test]
    fn setting_an_env_key_files_an_edit_rather_than_sending() {
        let mut app = fixtures::app_in_sheep_pane_with_env(&[("NODE_ENV", "x")]);
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_env_key(&mut app, "NODE_ENV");
        app.update(Msg::Key(KeyPress::Confirm));
        for typed in "staging".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let effect = app.update(Msg::Key(KeyPress::TextApply));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
    }

    /// An env edit and a config field of the same name are two entries,
    /// which is the whole reason `EditKey` has two arms.
    #[test]
    fn an_env_edit_does_not_collide_with_the_env_config_field() {
        let mut app = fixtures::app_in_sheep_pane_with_env(&[("env", "x")]);
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_env_key(&mut app, "env");
        fixtures::type_into_the_open_editor(&mut app, "y");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert!(
            app.config_pane()
                .unwrap()
                .edits()
                .get(&EditKey::Env("env".to_owned()))
                .is_some()
        );
    }

    #[test]
    fn the_add_a_key_row_opens_an_editor() {
        let mut app = fixtures::app_in_sheep_pane_with_env(&[]);
        app.set_control_for_tests(Control::Allowed);
        app.update(Msg::Key(KeyPress::SelectLast));
        app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(app.mode(), InputMode::Text);
    }

    /// The behaviour change this branch exists for: `e` used to close the
    /// pane, and now it does the field's own edit instead.
    #[test]
    fn e_no_longer_closes_the_pane() {
        let mut app = fixtures::app_in_sheep_pane();
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert!(
            app.config_pane().is_some(),
            "e edits now; it does not close"
        );
    }

    #[test]
    fn e_opens_the_editor_on_a_typed_field_the_same_as_enter() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(app.mode(), InputMode::Text, "e opens the text editor");
        assert_eq!(
            app.config_pane().unwrap().typing().map(|t| t.key.as_str()),
            Some("cwd")
        );
    }

    /// `e` opens the env editor exactly as `Enter` does, and does not close
    /// the pane, the same as it does for any other field row: an env row
    /// walks the same cursor and answers to the same key.
    #[test]
    fn e_opens_the_env_editor_the_same_as_enter_and_does_not_close_the_pane() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert!(
            app.config_pane().is_some(),
            "e must not close the pane from an env row"
        );
        assert_eq!(app.mode(), InputMode::Text, "e opens the editor here too");
    }

    #[test]
    fn escape_closes_a_pane_with_nothing_parked() {
        let mut app = fixtures::app_in_sheep_pane_with_nothing_parked();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.config_pane().is_none(),
            "no menu when nothing is parked"
        );
        assert!(app.pane_menu().is_none());
    }

    #[test]
    fn escape_on_a_parked_pane_offers_the_menu_and_escape_again_leaves() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.config_pane().is_some(),
            "the pane stays up behind the menu"
        );
        assert!(app.pane_menu().is_some(), "the menu is open");

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none(), "escape twice leaves");
        assert!(app.pane_menu().is_none());
    }

    #[test]
    fn the_menu_counts_the_parked_fields_once() {
        let mut app = fixtures::app_in_sheep_pane_with_two_parked_fields();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(app.pane_menu().expect("the menu is open").parked(), 2);
        assert_eq!(
            app.config_pane().expect("a pane").parked_count(),
            2,
            "the pane and the menu agree"
        );
    }

    #[test]
    fn the_menu_reads_which_reload_this_sheep_would_get() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(
            app.pane_menu().expect("the menu is open").reload(),
            ReloadKind::Overlap,
            "the fixture sets no readiness probe"
        );
    }

    #[test]
    fn the_menu_never_opens_while_the_gate_is_closed() {
        let mut app = fixtures::app_in_sheep_pane();
        assert!(app.config_pane().expect("a pane").parked_count() > 0);
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.pane_menu().is_none(), "read-only can apply nothing");
        assert!(app.config_pane().is_none());
    }

    #[test]
    fn l_from_the_menu_reloads_the_sheep_and_leaves() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let request = wire(app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload))));
        assert!(
            matches!(request, Request::Reload { .. }),
            "expected Reload, got {request:?}"
        );
        assert!(app.config_pane().is_none());
        assert!(app.pane_menu().is_none());
    }

    #[test]
    fn r_from_the_menu_restarts_the_sheep_and_leaves() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let request = wire(app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart))));
        assert!(
            matches!(request, Request::Restart { .. }),
            "expected Restart, got {request:?}"
        );
        assert!(app.config_pane().is_none());
    }

    /// `TextAbandon` drops the env editor and leaves the pane exactly as
    /// `Escape` leaves the field editor: open, on the same row, nothing
    /// filed. `Escape` from there walks the menu then the pane, the same
    /// as it does for any other row.
    #[test]
    fn abandoning_the_env_editor_leaves_the_pane_then_escape_walks_the_menu_then_the_pane() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(app.mode(), InputMode::Text);
        let _ = app.update(Msg::Key(KeyPress::TextAbandon));
        assert_eq!(app.mode(), InputMode::Normal);
        assert!(app.config_pane().is_some(), "the pane stays open");
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert!(app.pane_menu().is_none());
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.pane_menu().is_some());
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
    }

    /// Help is dismissed before the menu is offered, so `h` then `esc`
    /// still puts the operator back on the field list.
    #[test]
    fn escape_dismisses_help_before_it_offers_the_menu() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Help));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(!app.config_pane().unwrap().help_open());
        assert!(app.pane_menu().is_none());
    }

    #[test]
    fn h_toggles_help_and_escape_dismisses_it_before_closing_the_pane() {
        let mut app = fixtures::app_in_sheep_pane();
        pane_to(&mut app, "max_memory");
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.config_pane().unwrap().help_open());
        // Escape dismisses help first; the pane is still open.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(!app.config_pane().unwrap().help_open());
        assert!(
            app.config_pane().is_some(),
            "the first escape only closes help"
        );
        // A second `h` toggles it back open, and pressing it again closes it.
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.config_pane().unwrap().help_open());
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(!app.config_pane().unwrap().help_open());
        // Escape with help already closed closes the pane, same as ever.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
    }

    /// `h` on an env row has no field to show help for, so `top_line`
    /// draws nothing for it even though the flag it toggles is the same
    /// one a field row uses: `h` is bound once, on the whole field list,
    /// not per row.
    #[test]
    fn h_toggles_help_on_an_env_row_but_nothing_draws_it() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        assert_eq!(app.update(Msg::Key(KeyPress::Help)), Effect::None);
        assert!(app.config_pane().unwrap().help_open());
    }

    #[test]
    fn the_pane_owns_the_keyboard_while_it_is_open() {
        let mut app = fixtures::app_in_sheep_pane();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop))),
            Effect::None
        );
        assert!(
            app.action().is_none(),
            "no action arms from inside the pane"
        );
        assert_eq!(app.update(Msg::Key(KeyPress::Settings)), Effect::None);
        assert!(
            app.settings().is_none(),
            "`s` does not open a second screen"
        );
        assert!(app.config_pane().is_some());
    }

    #[test]
    fn the_movement_keys_walk_the_fields() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::SelectDown));
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(app.config_pane().unwrap().view().cursor(), 2);
        app.update(Msg::Key(KeyPress::SelectLast));
        // `process`, the group a fresh pane opens on, has ten fields, then
        // the fixture's two env keys and `+ add a key`: thirteen rows,
        // index 12.
        assert_eq!(app.config_pane().unwrap().view().cursor(), 12);
        app.update(Msg::Key(KeyPress::SelectFirst));
        assert_eq!(app.config_pane().unwrap().view().cursor(), 0);
    }

    #[test]
    fn r_re_reads_the_same_sheep_and_the_cursor_survives_it() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::SelectLast));
        let sent = Sent::SheepConfig {
            name: "web".to_string(),
        };
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::Send(sent.clone())
        );
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        // `process`, the group a fresh pane opens on, has ten fields, then
        // the fixture's two env keys and `+ add a key`: thirteen rows,
        // index 12.
        assert_eq!(app.config_pane().unwrap().view().cursor(), 12);
    }

    #[test]
    fn a_refused_config_read_says_why_and_leaves_the_pane_alone() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::NotFound,
                message: "no sheep named web".to_string(),
                daemon_version: None,
            })),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.contains("no sheep named web"), "got {said:?}");
        assert!(app.notice().unwrap().is_grave());
        assert!(app.config_pane().is_some(), "the pane stays as it was");
    }

    /// Silence looks exactly like a key that is not bound.
    #[test]
    fn a_config_read_that_was_never_sent_says_so() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Unsent {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.contains("web"), "got {said:?}");
        assert!(app.notice().unwrap().is_grave());
    }

    /// It spends its first line on a title naming the sheep, so a viewport
    /// told the full body height would scroll one row late.
    #[test]
    fn the_pane_gets_the_body_height_minus_its_own_title() {
        let mut app = fixtures::app_in_sheep_pane();
        app.note_body_rows(6);
        app.update(Msg::Key(KeyPress::SelectLast));
        // `process`, the group a fresh pane opens on, has ten fields, then
        // the fixture's two env keys and `+ add a key`: thirteen rows.
        assert_eq!(app.config_pane().unwrap().view().offset(), 13 - 5);
    }

    /// Walks the pane's cursor onto `key`. The pane is a public type with
    /// no public "go to this field" key, so the cursor is driven the way
    /// an operator drives it. A thin wrapper: `view::fixtures::select_field`
    /// is this exact walk, and this module had its own copy before the tab
    /// row gave a field's group somewhere to switch to first.
    fn pane_to(app: &mut App, key: &str) {
        fixtures::select_field(app, key);
    }

    /// Lands a `Request::SheepConfig` reply for `web` carrying `env_keys`,
    /// the way the event loop lands one after a write or an `r`.
    fn refresh_config(app: &mut App, env_keys: &[&str]) {
        let mut config = shep_core::config::AppConfig {
            name: "web".to_string(),
            ..Default::default()
        };
        for key in env_keys {
            config.env.insert((*key).to_string(), "x".to_string());
        }
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                shep_core::protocol::SheepConfigView::new(config, Vec::new(), Vec::new()),
            ))),
        });
    }

    /// The request an effect would put on the wire, or a panic naming what
    /// came back instead. The seam this test module cares about: the
    /// reducer's own `Sent` is an echo tag, and `Sent::request` is what the
    /// link task actually sends.
    fn wire(effect: Effect) -> Request {
        match effect {
            Effect::Send(sent) => sent.request(),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    /// Every request a closed pane's batch would put on the wire, in the
    /// order it sends them.
    fn wire_all(effect: Effect) -> Vec<Request> {
        match effect {
            Effect::SendAll(batch) => batch.iter().map(Sent::request).collect(),
            other => panic!("expected a batch, got {other:?}"),
        }
    }

    /// The `Sent` values a closed pane's batch carries.
    fn wire_batch(effect: Effect) -> Vec<Sent> {
        match effect {
            Effect::SendAll(batch) => batch,
            other => panic!("expected a batch, got {other:?}"),
        }
    }

    /// The one request a closed pane's batch carries, or a panic naming
    /// how many it carried instead.
    fn one_wire(effect: Effect) -> Request {
        let mut requests = wire_all(effect);
        assert_eq!(requests.len(), 1, "{requests:?}");
        requests.remove(0)
    }

    /// One field from each of the groups the pane draws, since the
    /// daemon's routing classification is per field. `rpc.rs`'s
    /// `a_field_edit_is_reported_as_an_operator_override` asserts the
    /// other half, where the marker is actually built.
    #[test]
    fn one_edit_reaches_the_wire_as_a_single_field_override() {
        for key in [
            "autorestart", // control
            "watch",       // process
            "merge_logs",  // inputs
        ] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, key);
            let _ = app.update(Msg::Key(KeyPress::Cycle));
            assert_eq!(
                app.config_pane().unwrap().edits().len(),
                1,
                "{key}: space files an edit and sends nothing"
            );
            let request = one_wire(app.update(Msg::Key(KeyPress::Escape)));
            let Request::SetSheepField {
                name,
                key: sent,
                value,
            } = request
            else {
                panic!("{key}: expected SetSheepField, got {request:?}");
            };
            assert_eq!(name, "web", "{key}");
            assert_eq!(sent, key, "{key}");
            assert!(value.is_boolean(), "{key}: {value}");
        }
    }

    /// `cwd` and `max_restarts` between them cover text and integer, which
    /// file differently, and an integer sent as a string is refused by the
    /// daemon rather than set.
    #[test]
    fn a_typed_field_reaches_the_wire_as_the_value_that_was_typed() {
        for (key, typed, want) in [
            ("cwd", "/srv/web", serde_json::json!("/srv/web")),
            ("max_restarts", "40", serde_json::json!(40)),
        ] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, key);
            let _ = app.update(Msg::Key(KeyPress::Confirm));
            assert_eq!(app.mode(), InputMode::Text, "{key}: the editor opens");
            for _ in 0..40 {
                let _ = app.update(Msg::Key(KeyPress::TextBackspace));
            }
            for typed in typed.chars() {
                let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
            }
            let _ = app.update(Msg::Key(KeyPress::TextApply));
            assert_eq!(app.mode(), InputMode::Normal, "{key}: the editor closes");
            let request = one_wire(app.update(Msg::Key(KeyPress::Escape)));
            let Request::SetSheepField {
                key: sent, value, ..
            } = request
            else {
                panic!("{key}: expected SetSheepField, got {request:?}");
            };
            assert_eq!(sent, key, "{key}");
            assert_eq!(value, want, "{key}");
        }
    }

    /// Routing through `ApplyConfig` would not work: no `ResetDepth`
    /// names a single key.
    #[test]
    fn the_env_rows_file_then_set_one_key_and_remove_another() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        // `+ add a key`, under the two keys the fixture's sheep has.
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(app.mode(), InputMode::Text, "enter opens the env editor");
        for typed in "API_TOKEN=hunter2".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        assert_eq!(
            app.update(Msg::Key(KeyPress::TextApply)),
            Effect::None,
            "applying the editor files; it does not send"
        );
        assert_eq!(app.mode(), InputMode::Normal);

        // An existing key with an empty buffer removes it. The cursor is
        // still on `+ add a key`, since applying an edit does not move it;
        // two steps up reaches `DB_HOST`, the fixture's first env key.
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::Env(0)));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::TextApply));

        // Both leave together, on the `Escape` that closes the pane.
        let requests = wire_all(app.update(Msg::Key(KeyPress::Escape)));
        assert_eq!(
            requests,
            vec![
                Request::SetSheepEnv {
                    name: "web".to_owned(),
                    key: "API_TOKEN".to_owned(),
                    value: Some("hunter2".to_owned().into()),
                },
                Request::SetSheepEnv {
                    name: "web".to_owned(),
                    key: "DB_HOST".to_owned(),
                    value: None,
                },
            ]
        );
    }

    /// A removal shortens the list, so a cursor carried by index would name
    /// the next key down, and a reflexive second `Enter` would arm a write
    /// against a neighbour nobody chose. A key that is gone lands on
    /// `+ new`, the one row where `Enter` destroys nothing.
    #[test]
    fn the_env_cursor_is_carried_by_key_and_not_by_index_across_a_refresh() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        // `SelectLast` lands on `+ add a key`; one step up is `LOG_LEVEL`,
        // the fixture's second env key.
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(
            app.config_pane().unwrap().cursor_env_key_name(),
            Some("LOG_LEVEL")
        );
        // A refresh that leaves both keys in place keeps the cursor on its
        // own key rather than on row 1.
        refresh_config(&mut app, &["DB_HOST", "LOG_LEVEL"]);
        assert_eq!(
            app.config_pane().unwrap().cursor_env_key_name(),
            Some("LOG_LEVEL")
        );
        // A refresh that removed the key above it keeps it on its own key,
        // which is now row 0. Carrying the index would have moved it to
        // `+ add a key`; carrying nothing would have moved it to `DB_HOST`.
        refresh_config(&mut app, &["LOG_LEVEL"]);
        assert_eq!(
            app.config_pane().unwrap().cursor_env_key_name(),
            Some("LOG_LEVEL")
        );
        // A refresh that removed the cursor's own key lands on `+ add a
        // key`, never on whatever took its place.
        refresh_config(&mut app, &["OTHER"]);
        assert_eq!(app.config_pane().unwrap().cursor_env_key_name(), None);
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
    }

    /// A reply for a write from an earlier pane can land while a new one
    /// is open and being typed into. On the env screen the discarded
    /// buffer is a secret the operator cannot read back.
    #[test]
    fn a_reply_landing_mid_edit_leaves_the_buffer_and_the_keyboard_alone() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let mut batch = wire_batch(app.update(Msg::Key(KeyPress::Escape)));
        let sent = batch.remove(0);
        // The pane is reopened and the operator starts typing while the
        // first write is still out.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for typed in "/srv".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        assert_eq!(app.mode(), InputMode::Text);
        let _ = app.update(Msg::Unsent { sent });
        assert_eq!(app.mode(), InputMode::Text, "the keyboard is not stranded");
        let typing = app
            .config_pane()
            .unwrap()
            .typing()
            .expect("the buffer survives the reply");
        assert_eq!(typing.key, "cwd");
        assert!(typing.buffer.ends_with("/srv"), "{}", typing.buffer);
    }

    /// A landed write asks for a re-read, and the re-read rebuilds the
    /// whole `ConfigPane`, editor included.
    #[test]
    fn a_refresh_that_drops_an_open_editor_puts_the_keyboard_back() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(app.mode(), InputMode::Text);
        refresh_config(&mut app, &["DB_HOST", "LOG_LEVEL"]);
        assert_eq!(app.mode(), InputMode::Normal);
        assert!(app.config_pane().unwrap().typing().is_none());
    }

    /// The whole shape of the pane in one test: a keystroke files, and
    /// nothing reaches the shepherd for it.
    #[test]
    fn cycling_a_bool_files_an_edit_and_sends_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "autorestart");
        let effect = app.update(Msg::Key(KeyPress::Cycle));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
    }

    #[test]
    fn undo_drops_the_edit_it_filed() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        app.update(Msg::Key(KeyPress::Undo));
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    #[test]
    fn cycling_a_bool_back_to_its_stored_value_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.config_pane().unwrap().edits().is_empty(),
            "a round trip back to the stored value is not an edit"
        );
    }

    /// `d` on an overridden field files the schema's own `default` rather
    /// than [`Value::Null`]: `max_restarts` is a plain `u32`, not an
    /// `Option<u32>`, and the shepherd's deserializer refuses `null` for
    /// one of those. The Flockfile schema's default for `max_restarts` is
    /// `16`.
    #[test]
    fn d_restores_an_overridden_field_to_its_schema_default() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        assert!(
            app.config_pane().unwrap().is_overridden("max_restarts"),
            "the fixture overrides max_restarts"
        );
        pane_to(&mut app, "max_restarts");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert_eq!(filed_value(&app, "max_restarts"), serde_json::json!(16));

        let request = one_wire(app.update(Msg::Key(KeyPress::Escape)));
        let Request::SetSheepField { key, value, .. } = request else {
            panic!("expected SetSheepField, got {request:?}");
        };
        assert_eq!(key, "max_restarts");
        assert_eq!(value, serde_json::json!(16));
    }

    /// A field the operator has not overridden is already showing its
    /// default, so `d` files nothing: an edit that changes nothing would
    /// still be counted by the title band.
    #[test]
    fn d_on_a_field_already_at_its_default_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        assert!(
            !app.config_pane().unwrap().is_overridden("autorestart"),
            "the fixture does not override autorestart"
        );
        pane_to(&mut app, "autorestart");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    /// A field flipped this session, even one the shepherd never
    /// overrode, is no longer showing its default: `d` restores the
    /// schema default rather than leaving the flipped value in place. For
    /// a field the shepherd never overrode, the schema default and what
    /// the shepherd already holds are the same value, so restoring it
    /// exactly cancels the flip: the fresh edit drops rather than being
    /// replaced by a second one.
    #[test]
    fn d_after_cycling_a_fresh_value_restores_the_default_anyway() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        assert!(
            !app.config_pane().unwrap().is_overridden("autostart"),
            "the fixture does not override autostart"
        );
        pane_to(&mut app, "autostart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert_ne!(
            filed_value(&app, "autostart"),
            serde_json::Value::Null,
            "the flip files the opposite of the stored value"
        );

        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(
            app.config_pane().unwrap().edits().is_empty(),
            "the schema default for autostart is the fixture's own stored value, \
             so restoring it cancels the flip rather than filing a second edit"
        );
    }

    /// A field that is already `(unset)` on the shepherd's own side has
    /// nowhere further to fall: cancelling a fresh, unsent edit to it
    /// files nothing rather than a `Null` edit that would just repeat what
    /// the shepherd already has.
    #[test]
    fn d_after_typing_an_unset_field_leaves_nothing_filed() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        fixtures::type_into_the_open_editor(&mut app, "/srv/api");
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);

        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(
            app.config_pane().unwrap().edits().is_empty(),
            "cwd was already (unset), so cancelling the typed edit leaves nothing to send"
        );
    }

    /// The lock wins over the control gate here too: `d` refuses a
    /// Structural field with the same sentence `space` and `Enter` give it.
    #[test]
    fn d_refuses_a_locked_field_with_its_lock_sentence() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "instances");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        let notice = app.notice().expect("a locked row answers").to_string();
        assert!(notice.contains("`shep stock`"), "{notice}");
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    /// A read-only pane refuses `d` the same way it refuses `space` and
    /// `Enter`: on the keystroke that would file the edit, not on a later
    /// close that would try to send it.
    #[test]
    fn d_refuses_when_the_pane_is_read_only() {
        let mut app = fixtures::app_in_sheep_pane();
        pane_to(&mut app, "autorestart");
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert_eq!(
            app.notice().map(ToString::to_string),
            Some(READ_ONLY_REFUSAL.to_string())
        );
    }

    /// `d` means nothing on an env row: unsetting a key entirely is a
    /// different act from restoring a default, and the spec does not ask
    /// for it.
    #[test]
    fn d_on_an_env_row_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        let effect = app.update(Msg::Key(KeyPress::Remove));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().unwrap().edits().is_empty());
    }

    #[test]
    fn escape_sends_every_filed_edit_at_once() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(sent) = effect else {
            panic!("wanted a batch, got {effect:?}");
        };
        assert_eq!(sent.len(), 2);
        assert!(
            app.config_pane().is_none(),
            "the pane closes on the same key"
        );
    }

    #[test]
    fn escape_with_nothing_filed_closes_and_sends_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().is_none());
    }

    /// Read-only refuses the first keypress, not the close. Building five
    /// edits and losing them all at `esc` wastes the operator's time.
    #[test]
    fn read_only_refuses_the_first_edit_and_files_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::ReadOnly);
        fixtures::select_field(&mut app, "autorestart");
        app.update(Msg::Key(KeyPress::Cycle));
        assert!(app.config_pane().unwrap().edits().is_empty());
        let notice = app
            .notice()
            .map(ToString::to_string)
            .expect("a refusal says so");
        assert!(notice.contains("read-only"), "{notice}");
    }

    /// A refusal now lands after the pane has gone, so its arm must not
    /// assume a pane is open.
    #[test]
    fn a_refused_write_notices_after_the_pane_has_closed() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let Effect::SendAll(mut sent) = app.update(Msg::Key(KeyPress::Escape)) else {
            panic!("wanted a batch");
        };
        let first = sent.remove(0);
        assert!(app.config_pane().is_none());
        app.update(Msg::Replied {
            sent: first,
            result: Err(fixtures::a_refusal()),
        });
        let notice = app
            .notice()
            .map(ToString::to_string)
            .expect("a refusal says so");
        assert!(
            notice.contains("cwd"),
            "the notice names the field: {notice}"
        );
    }

    /// Validation runs on entry, so the set is always sendable. An integer
    /// field mid-word holds the editor open rather than filing a bad value,
    /// which is what `apply_typing` already does today.
    #[test]
    fn a_value_that_does_not_parse_never_joins_the_set() {
        let mut app = fixtures::app_in_sheep_pane();
        app.set_control_for_tests(Control::Allowed);
        fixtures::select_field(&mut app, "max_restarts");
        app.update(Msg::Key(KeyPress::Confirm));
        for typed in "not a number".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert_eq!(app.mode(), InputMode::Text, "the editor stays open");
    }

    /// The set is the operator's and the values are the shepherd's.
    #[test]
    fn a_config_re_read_replaces_the_values_and_keeps_the_edits() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
            panic!("refresh asks the shepherd for the config again");
        };
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert_eq!(app.config_pane().unwrap().edits().len(), 2);
    }

    /// Two edits, one close, two requests, each naming its own key: a
    /// batch is not one write carrying a map, so neither entry can carry
    /// the other's value.
    #[test]
    fn a_batch_names_each_key_in_its_own_request() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        pane_to(&mut app, "watch");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let requests = wire_all(app.update(Msg::Key(KeyPress::Escape)));
        let named: Vec<String> = requests
            .iter()
            .map(|request| match request {
                Request::SetSheepField { key, .. } => key.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(named, vec!["autorestart".to_owned(), "watch".to_owned()]);
    }

    /// `instances` is `Lock::Refused`, since shep takes no config write for
    /// it at all. `liveness_probe` is `Lock::NoWidget`, since this pane
    /// simply has no editor for a nested object. `Lock` exists so one
    /// sentence never covers both.
    #[test]
    fn a_refused_field_and_one_with_no_widget_refuse_for_different_reasons() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "instances");
        assert_eq!(app.update(Msg::Key(KeyPress::Cycle)), Effect::None);
        assert!(app.config_pane().unwrap().edits().is_empty());
        let refused = app.notice().expect("a refusal is raised").to_string();
        assert!(refused.contains("instances"), "{refused}");
        assert!(
            !refused.contains("no editor in this pane"),
            "a field shep refuses is not a field this pane merely lacks a widget for: {refused}"
        );

        pane_to(&mut app, "liveness_probe");
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        assert!(app.config_pane().unwrap().edits().is_empty());
        let no_widget = app.notice().expect("a refusal is raised").to_string();
        assert!(no_widget.contains("liveness_probe"), "{no_widget}");
        assert!(
            no_widget.contains("Flockfile"),
            "a shape with no widget is still one a Flockfile writes: {no_widget}"
        );
        assert_ne!(refused, no_widget, "two facts, two sentences");
    }

    #[test]
    fn a_refused_field_names_the_verb_that_owns_it() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "instances");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let said = app.notice().expect("a refusal is raised").to_string();
        assert!(said.contains("`shep stock`"), "{said}");
    }

    /// The lock wins over the control gate: `--allow-control` does not
    /// unlock a Structural field.
    #[test]
    fn space_and_enter_refuse_a_locked_row_with_the_same_sentence() {
        for control in [Control::ReadOnly, Control::Allowed] {
            for key in ["instances", "liveness_probe"] {
                let mut app = fixtures::app_in_sheep_pane();
                app.set_control_for_tests(control);
                pane_to(&mut app, key);
                let _ = app.update(Msg::Key(KeyPress::Cycle));
                let cycled = app.notice().map(ToString::to_string);
                let _ = app.update(Msg::Key(KeyPress::Confirm));
                let confirmed = app.notice().map(ToString::to_string);
                assert_eq!(cycled, confirmed, "{control:?} {key}");
                assert!(
                    cycled.as_deref().is_some_and(|text| text.contains(key)),
                    "{control:?} {key}: {cycled:?}"
                );
                assert_ne!(
                    cycled.as_deref(),
                    Some(READ_ONLY_REFUSAL),
                    "the lock is the more specific fact: {control:?} {key}"
                );
            }
        }
    }

    #[test]
    fn a_read_only_pane_refuses_every_door_that_writes() {
        // One pair per door: `space` cycles, `Enter` opens the text
        // editor.
        for (key, press) in [("autorestart", KeyPress::Cycle), ("cwd", KeyPress::Confirm)] {
            let mut app = fixtures::app_in_sheep_pane();
            pane_to(&mut app, key);
            assert_eq!(app.update(Msg::Key(press)), Effect::None, "{key}");
            assert!(app.config_pane().unwrap().edits().is_empty(), "{key}");
            assert!(app.config_pane().unwrap().typing().is_none(), "{key}");
            assert!(app.config_pane().unwrap().env_typing().is_none(), "{key}");
            assert_eq!(app.mode(), InputMode::Normal, "{key}");
            assert_eq!(
                app.notice().map(ToString::to_string),
                Some(READ_ONLY_REFUSAL.to_string()),
                "{key}"
            );
        }
    }

    /// `Enter` on an env row is the fourth door: it opens the env editor
    /// exactly as `Enter` on a typed field does, so it is gated the same
    /// way.
    #[test]
    fn a_read_only_pane_refuses_enter_on_an_env_row() {
        let mut app = fixtures::app_in_sheep_pane();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert!(app.config_pane().unwrap().env_typing().is_none());
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(
            app.notice().map(ToString::to_string),
            Some(READ_ONLY_REFUSAL.to_string())
        );
    }

    /// Nothing is armed, so nothing eats a keystroke: the next key does
    /// its own job and the filed edit stays filed.
    #[test]
    fn a_key_after_an_edit_does_its_own_job_and_keeps_the_edit() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let before = app.config_pane().unwrap().view().cursor();
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
        assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
        assert_eq!(
            app.config_pane().unwrap().view().cursor(),
            before + 1,
            "the movement key moves"
        );
        assert_eq!(
            app.config_pane().unwrap().edits().len(),
            1,
            "and does not cancel the edit"
        );
    }

    /// A filed edit is not a question waiting for an answer, so no timer
    /// takes it away: an operator who walked off must not come back to
    /// work silently discarded.
    #[test]
    fn a_filed_edit_outlives_the_confirm_budget() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Tick {
            now: Instant::now() + CONFIRM_EXPIRY,
        });
        assert_eq!(app.config_pane().unwrap().edits().len(), 1);
    }

    /// `u` drops the newest edit and leaves the older one filed.
    #[test]
    fn u_undoes_the_newest_edit_only() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        pane_to(&mut app, "watch");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let _ = app.update(Msg::Key(KeyPress::Undo));
        let requests = wire_all(app.update(Msg::Key(KeyPress::Escape)));
        let named: Vec<String> = requests
            .iter()
            .map(|request| match request {
                Request::SetSheepField { key, .. } => key.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(named, vec!["autorestart".to_owned()]);
    }

    /// `pending` is the shepherd's answer, reported verbatim rather than
    /// re-derived from `apply_group`: the two disagree on `autostart`, and
    /// on a field whose config subset would not normalize.
    #[test]
    fn a_landed_write_re_reads_the_config_and_reports_what_the_shepherd_said() {
        for (pending, wanted) in [(false, "set to false"), (true, "shep reload")] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, "autorestart");
            let _ = app.update(Msg::Key(KeyPress::Cycle));
            let mut batch = wire_batch(app.update(Msg::Key(KeyPress::Escape)));
            let effect = app.update(Msg::Replied {
                sent: batch.remove(0),
                result: Ok(Response::SheepFieldSet {
                    name: "web".to_owned(),
                    key: "autorestart".to_owned(),
                    pending,
                }),
            });
            assert_eq!(
                effect,
                Effect::Send(Sent::SheepConfig {
                    name: "web".to_owned()
                }),
                "a landed write re-reads what the shepherd now holds"
            );
            let notice = app.notice().expect("the outcome is reported");
            assert!(!notice.is_grave(), "{notice:?}");
            assert!(notice.to_string().contains(wanted), "{notice:?}");
        }
    }

    /// Every refusal this door can meet is an `Err`, which is why
    /// `Response::SheepFieldSet` carries no `refused` field: two ways to
    /// say no is one a client forgets to check.
    #[test]
    fn a_refused_write_is_reported_and_does_not_re_read() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let mut batch = wire_batch(app.update(Msg::Key(KeyPress::Escape)));
        let effect = app.update(Msg::Replied {
            sent: batch.remove(0),
            result: Err(fixtures::a_refusal()),
        });
        assert_eq!(effect, Effect::None, "a refusal does not re-read");
        let notice = app.notice().expect("the refusal is reported");
        assert!(notice.is_grave());
        assert!(
            notice.to_string().contains("the store is locked"),
            "{notice:?}"
        );
    }

    /// Asserted on the whole `Effect`, since that is what a diagnostic
    /// would print. Nothing between here and the wire unwraps the value.
    #[test]
    fn a_write_effects_debug_names_no_value() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..40 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for typed in "/home/ada/secret-project".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let field = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(
            format!("{field:?}"),
            "SendAll([ApplyField { name: \"web\", ticket: 0, key: \"cwd\", \
             value: FieldValue(<string>), authority: WriteAuthority(()) }])"
        );

        let mut app = fixtures::app_in_sheep_pane_with_control();
        // `SelectLast` lands on `+ add a key`; two steps up is `DB_HOST`,
        // the fixture's first env key.
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::Env(0)));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for typed in "hunter2".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        // One `Escape`, not two: there is no sub-screen level to back out
        // of any more, so the write goes out on the same keypress that
        // closes the field list (or offers its menu).
        let env = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(
            format!("{env:?}"),
            "SendAll([SetEnv { name: \"web\", ticket: 0, key: \"DB_HOST\", \
             value: Some(EnvValue(<7 bytes>)), authority: WriteAuthority(()) }])"
        );
    }

    /// Two writes to the same field in one batch is unreachable, since
    /// the set holds one entry per key, but two closes of two panes are
    /// not: the counter is monotonic and never reused, so no two `Sent`
    /// values are ever equal.
    #[test]
    fn no_two_writes_share_a_ticket() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        pane_to(&mut app, "watch");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let first = wire_batch(app.update(Msg::Key(KeyPress::Escape)));
        assert_ne!(first[0], first[1], "two entries are two tickets");

        // The same lookout, a second pane. `app_in_sheep_pane_with_control`
        // parks a field, so the close above stopped on the apply menu and
        // this `Escape` is what leaves it.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_owned(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let second = wire_batch(app.update(Msg::Key(KeyPress::Escape)));
        assert_ne!(
            first[0], second[0],
            "a second close does not reuse the first's tickets"
        );
    }

    /// The pending set is the operator's and the values are the
    /// shepherd's: a re-read replaces one and keeps the other.
    #[test]
    fn a_refresh_replaces_the_values_and_keeps_the_edits() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        refresh_config(&mut app, &[]);
        assert_eq!(
            app.config_pane().unwrap().edits().len(),
            1,
            "the edit survives the rebuild"
        );
        let request = one_wire(app.update(Msg::Key(KeyPress::Escape)));
        let Request::SetSheepField { key, .. } = request else {
            panic!("{request:?}");
        };
        assert_eq!(key, "autorestart");
    }

    /// A menu nobody answered is a question nobody is still looking at, and
    /// `L` an hour later must not reload a sheep.
    #[test]
    fn the_apply_menu_expires_like_every_other_armed_thing() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.pane_menu().is_some(), "the menu opened");

        let later = Instant::now() + CONFIRM_EXPIRY;
        let _ = app.update(Msg::Tick { now: later });
        assert!(app.pane_menu().is_none(), "it did not expire");

        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        assert_eq!(effect, Effect::None, "a stale L reloads nothing");
    }

    #[test]
    fn the_apply_menu_refuses_on_a_dead_link_like_every_other_action() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Retrying { attempt: 3 });
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        assert_eq!(
            effect,
            Effect::None,
            "nothing goes to a shepherd that is gone"
        );
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.contains("attempt 3"), "{said}");
    }

    /// The pane-level test reaches `begin_typing` directly, so it passes
    /// over a dead key path. This one presses the key.
    #[test]
    fn e_opens_the_editor_on_a_suggested_field() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "kill_signal");
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            app.config_pane()
                .and_then(ConfigPane::typing)
                .map(|typing| typing.key.as_str()),
            Some("kill_signal")
        );
    }

    #[test]
    fn e_on_a_dog_row_opens_its_pane_instead_of_refusing() {
        let mut app = fixtures::app_with_a_dog_selected_and_control();
        let effect = app.update(Msg::Key(KeyPress::Edit));
        let Effect::LoadDogPane { name, adopted_path } = effect else {
            panic!("expected a dog pane, got {effect:?}");
        };
        assert_eq!(name, "otel");
        // The path comes off the row, not the settings screen.
        assert_eq!(adopted_path.as_deref(), Some(Path::new("/opt/otel")));
        assert!(app.notice().is_none(), "{:?}", app.notice());
    }

    #[test]
    fn e_on_a_built_in_dog_opens_a_pane_with_no_path() {
        let mut app = fixtures::app_with_a_built_in_dog_selected_and_control();
        let effect = app.update(Msg::Key(KeyPress::Edit));
        let Effect::LoadDogPane { adopted_path, .. } = effect else {
            panic!("expected a dog pane, got {effect:?}");
        };
        // A built-in dog is the shep binary's own argv branch, so there is no
        // adopted path to probe and the pane asks the running binary instead.
        assert_eq!(adopted_path, None);
    }

    /// `EngineStopped` is the one of this request's three refusals with no
    /// subject of its own: `rpc_error` renders it as `the supervisor
    /// engine has stopped`, full stop, naming neither the sheep nor the
    /// screen it came from.
    #[test]
    fn a_refusal_that_names_nothing_still_reaches_the_operator_named() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::Internal,
                message: "the supervisor engine has stopped".to_string(),
                daemon_version: None,
            })),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert_eq!(said, "web: the supervisor engine has stopped");
        assert!(app.notice().unwrap().is_grave());
    }

    #[test]
    fn a_config_reply_for_a_pane_nobody_wants_is_dropped() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let reply = || Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        };
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Key(KeyPress::Edit));
        app.update(reply());
        assert!(app.config_pane().is_some(), "the first answer opens it");

        app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());

        app.update(reply());
        assert!(
            app.config_pane().is_none(),
            "the second answer lands on a dashboard and is dropped"
        );
        assert!(app.notice().is_none(), "silently: nothing went wrong");
    }

    /// Closing must clear the target it is keyed on: left stale, it would
    /// either re-open on a stray reply or refuse the next open.
    #[test]
    fn e_still_opens_a_pane_after_one_was_closed() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert!(app.config_pane().is_some());
    }
    /// The path is what carries a probe to an adopted dog's own binary; a
    /// built-in dog answers in-process and has none.
    #[test]
    fn e_on_a_settings_dog_row_probes_that_dogs_own_binary() {
        let mut app = fixtures::app_in_settings_on_dog("otel");
        assert_eq!(
            app.update(Msg::Key(KeyPress::Edit)),
            Effect::LoadDogPane {
                name: "otel".to_string(),
                adopted_path: Some(std::path::PathBuf::from("/usr/local/bin/shep-otel")),
            }
        );
        assert!(
            app.config_pane().is_none(),
            "the pane opens on the answer, never on the keypress"
        );
        assert!(
            app.settings().is_some(),
            "and the screen stays up until it does"
        );
    }

    /// Those rows are the settings screen's own subject, and `space` and
    /// `Enter` already edit them.
    #[test]
    fn e_on_a_settings_scalar_row_does_nothing_at_all() {
        let mut app = fixtures::app_in_settings();
        assert_eq!(app.update(Msg::Key(KeyPress::Edit)), Effect::None);
        assert!(app.config_pane().is_none());
        assert!(app.notice().is_none(), "and says nothing about it");
    }

    #[test]
    fn a_dog_with_no_schema_gets_no_pane_and_is_told_where_to_edit() {
        let mut app = fixtures::app_in_settings_on_dog("otel");
        app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            app.update(Msg::DogPane {
                name: "otel".to_string(),
                adopted_path: None,
                result: Err("otel publishes no schema; edit dogs.toml with $EDITOR".to_string()),
            }),
            Effect::None
        );
        assert!(app.config_pane().is_none());
        assert!(
            app.settings().is_some(),
            "the screen the operator pressed `e` on is still there"
        );
        let notice = app.notice().expect("a refusal is reported").to_string();
        assert!(notice.contains("dogs.toml"), "{notice}");
        assert!(notice.contains("$EDITOR"), "{notice}");
    }

    /// The schema is the first of two halves. The pane cannot be drawn
    /// until the shepherd answers with the section, the half this binary
    /// has no copy of.
    #[test]
    fn a_schema_asks_for_the_section_and_the_section_opens_the_pane() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            app.update(Msg::DogPane {
                name: "metrics".to_string(),
                adopted_path: None,
                result: Ok(crate::dog::builtin_schema("metrics").expect("a built-in")),
            }),
            Effect::Send(Sent::DogSection {
                name: "metrics".to_string()
            })
        );
        assert!(app.config_pane().is_none(), "one half is not a pane");
        app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "metrics".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: "bind = \"0.0.0.0:9615\"\n".to_string().into(),
            }),
        });
        let pane = app.config_pane().expect("both halves are a pane");
        assert_eq!(pane.target().name(), "metrics");
        assert_eq!(pane.value("bind"), "0.0.0.0:9615");
        assert!(
            app.settings().is_none(),
            "the settings screen closes once there is something to look at"
        );
    }

    /// The same property `config_target` buys for a sheep.
    #[test]
    fn a_section_for_a_dog_nobody_is_waiting_for_is_dropped() {
        let mut app = fixtures::app_in_dog_pane();
        app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
        app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "bark".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: fixtures::dog_section().into(),
            }),
        });
        assert!(app.config_pane().is_none(), "a late reply re-opens nothing");
    }

    /// Its twin above proves the pane drops once nobody is waiting; this
    /// one proves the name is checked while somebody still is, the half a
    /// cleared target cannot cover.
    #[test]
    fn a_section_for_a_different_dog_does_not_replace_the_pane() {
        let mut app = fixtures::app_in_dog_pane();
        app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "metrics".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: "bind = \"0.0.0.0:9615\"\n".to_string().into(),
            }),
        });
        let pane = app.config_pane().expect("the bark pane is still open");
        assert_eq!(pane.target().name(), "bark");
        assert_eq!(
            pane.value("poll"),
            "60s",
            "and still holds bark's own section"
        );
    }

    /// A dog has no override store and no Flockfile, so `ApplyConfig` has
    /// nothing to mean here: the write goes out through `SetDogConfig`
    /// instead.
    #[test]
    fn a_dog_pane_write_carries_the_whole_edited_section() {
        let mut app = fixtures::app_in_dog_pane();
        let index = app
            .config_pane()
            .expect("the pane is open")
            .fields()
            .fields()
            .iter()
            .position(|field| field.key == "poll")
            .expect("poll is a bark field");
        app.update(Msg::Key(KeyPress::SelectFirst));
        for _ in 0..index {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
        app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..3 {
            app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for typed in "30s".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        let batch = wire_batch(app.update(Msg::Key(KeyPress::Escape)));
        let [Sent::SetDogSection { name, toml, .. }] = batch.as_slice() else {
            panic!("closing the pane sends the section: {batch:?}");
        };
        assert_eq!(name, "bark");
        assert!(
            toml.as_str().contains("poll = \"30s\""),
            "{}",
            toml.as_str()
        );
        assert!(
            toml.as_str().contains("# how often"),
            "a comment shep did not write survives: {}",
            toml.as_str()
        );
    }

    /// `Request::SetDogConfig` replaces the whole table, so a dog's batch
    /// of edits closes as one write rather than one per entry, the way a
    /// sheep's own batch closes as `writes.len()` requests.
    #[test]
    fn closing_a_dog_pane_sends_one_write_for_two_edits() {
        let mut app = fixtures::app_in_dog_pane_with_two_edits();
        let Effect::SendAll(sent) = app.update(Msg::Key(KeyPress::Escape)) else {
            panic!("wanted a batch");
        };
        assert_eq!(sent.len(), 1, "a dog takes one section write, not two");
        let [Sent::SetDogSection { name, toml, .. }] = sent.as_slice() else {
            panic!("closing the pane sends the section: {sent:?}");
        };
        assert_eq!(name, "bark");
        assert!(
            toml.as_str().contains("poll = \"45s\""),
            "{}",
            toml.as_str()
        );
        assert!(
            toml.as_str().contains("history_bytes = 8192"),
            "{}",
            toml.as_str()
        );
    }

    #[test]
    fn a_landed_dog_write_re_reads_the_section_and_promises_nothing_more() {
        let mut app = fixtures::app_in_dog_pane();
        assert_eq!(
            app.update(Msg::Replied {
                sent: Sent::DogSection {
                    name: "bark".to_string(),
                },
                result: Ok(Response::DogConfigSet {
                    name: "bark".to_string()
                }),
            }),
            Effect::None,
            "a reply routed by its own request, and this one is not the write"
        );
        let Effect::Send(Sent::DogSection { name }) = app.update(Msg::Replied {
            sent: Sent::SetDogSection {
                name: "bark".to_string(),
                ticket: 0,
                toml: fixtures::dog_section().into(),
                authority: WriteAuthority::granted(&app).expect("the fixture opens the gate"),
            },
            result: Ok(Response::DogConfigSet {
                name: "bark".to_string(),
            }),
        }) else {
            panic!("a landed write re-reads");
        };
        assert_eq!(name, "bark");
        let notice = app
            .notice()
            .expect("a landed write is reported")
            .to_string();
        assert!(notice.contains("bark is told"), "{notice}");
    }

    /// The schema was read at open and is parked on the app: a keystroke
    /// that re-reads a file must not respawn somebody else's process.
    #[test]
    fn r_in_a_dog_pane_re_reads_the_section_and_never_re_probes() {
        let mut app = fixtures::app_in_dog_pane();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::Send(Sent::DogSection {
                name: "bark".to_string()
            })
        );
    }

    /// A dog's env writes through `Request::SetSheepEnv`, which would name
    /// a sheep that does not exist. It refuses with `Lock::NoWidget`'s own
    /// sentence instead.
    #[test]
    fn enter_on_a_dogs_map_field_refuses_rather_than_opening_an_editor() {
        let mut app = fixtures::app_in_dog_pane();
        let index = app
            .config_pane()
            .expect("the pane is open")
            .fields()
            .fields()
            .iter()
            .position(|field| field.key == "sinks")
            .expect("sinks is a bark field");
        app.update(Msg::Key(KeyPress::SelectFirst));
        for _ in 0..index {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
        assert_eq!(app.update(Msg::Key(KeyPress::Confirm)), Effect::None);
        assert!(
            app.config_pane()
                .expect("still open")
                .env_typing()
                .is_none(),
            "a dog has no env editor"
        );
        let notice = app.notice().expect("a locked row answers").to_string();
        assert!(notice.contains("no editor in this pane"), "{notice}");
    }

    /// Through `Msg::Key`, not `ConfigPane::open_list`: `confirm_field`
    /// has its own gate listing which kinds `Enter` opens, and a test that
    /// called the pane method would pass over it.
    #[test]
    fn enter_on_an_array_row_opens_the_list_sub_screen() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let list = app
            .config_pane()
            .expect("the pane is open")
            .list()
            .expect("enter on an array row opens the sub-screen");
        assert_eq!(list.key(), "args");
        assert_eq!(list.elements(), ["--port", "8080"]);
    }

    /// Every key the sub-screen's own hint names, driven the way an
    /// operator drives them, and the array that lands on the wire at the
    /// end of it.
    #[test]
    fn the_list_sub_screen_edits_removes_and_reorders_one_array() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(app.mode(), InputMode::Text, "e opens the element editor");
        for _ in 0..4 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for typed in "9090".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        assert_eq!(
            app.update(Msg::Key(KeyPress::TextApply)),
            Effect::None,
            "applying the editor files; it does not send"
        );
        assert_eq!(
            filed_value(&app, "args"),
            serde_json::json!(["--port", "9090"]),
            "the edited element lands in the whole array"
        );

        let _ = app.update(Msg::Key(KeyPress::StepUp));
        assert_eq!(
            filed_value(&app, "args"),
            serde_json::json!(["9090", "--port"]),
            "K moves the element under the cursor up one place, over the \
             array the edit before it filed"
        );
        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(
            filed_value(&app, "args"),
            serde_json::json!(["9090"]),
            "d drops the element under the cursor"
        );

        // One field, one entry, however many keystrokes reached it, and
        // every one of them is in the array the wire carries.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let request = one_wire(app.update(Msg::Key(KeyPress::Escape)));
        let Request::SetSheepField { key, value, .. } = request else {
            panic!("expected SetSheepField, got {request:?}");
        };
        assert_eq!(key, "args");
        assert_eq!(value, serde_json::json!(["9090"]));
    }

    /// The value the open pane has filed for `key`.
    fn filed_value(app: &App, key: &str) -> serde_json::Value {
        let entry = app
            .config_pane()
            .expect("the pane is open")
            .edits()
            .get(&EditKey::Field(key.to_owned()))
            .unwrap_or_else(|| panic!("nothing filed for {key}"));
        match entry.edit() {
            PaneEdit::Set { value, .. } => value.as_value().clone(),
            other => panic!("expected a field set, got {other:?}"),
        }
    }

    /// `Escape` backs out of the sub-screen and leaves the pane up, the
    /// same one-level-at-a-time rule the env screen follows.
    #[test]
    fn escape_leaves_the_list_sub_screen_before_it_closes_the_pane() {
        let mut app = fixtures::app_in_sheep_pane_with_nothing_parked();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().unwrap().list().is_none());
        assert!(app.config_pane().is_some(), "the pane is still open");
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
    }

    /// `u` is on the sub-screen's own key hint, so it has to do something
    /// there. It drops the field's entry and the rows go back to the
    /// array the shepherd sent.
    #[test]
    fn u_undoes_a_list_edit_from_inside_the_sub_screen() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(filed_value(&app, "args"), serde_json::json!(["--port"]));

        let _ = app.update(Msg::Key(KeyPress::Undo));
        assert!(
            app.config_pane()
                .expect("the pane is open")
                .edits()
                .is_empty(),
            "u drops the entry the removal filed"
        );
        assert_eq!(
            app.config_pane()
                .expect("the pane is open")
                .list()
                .expect("the sub-screen is still up")
                .elements(),
            ["--port", "8080"],
            "the rows show the array that was restored"
        );
    }

    /// A write re-reads the whole config, so without the carry the
    /// sub-screen would shut on the operator's own keystroke.
    #[test]
    fn the_list_sub_screen_survives_the_refresh_a_write_triggers() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "args");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        refresh_config(&mut app, &[]);
        let list = app
            .config_pane()
            .expect("the pane is open")
            .list()
            .expect("the sub-screen rides across the refresh");
        assert_eq!(list.key(), "args");
        assert_eq!(
            list.cursor(),
            Some(ListRow::New),
            "a cursor past the end lands on the row where enter destroys nothing"
        );
    }
}
