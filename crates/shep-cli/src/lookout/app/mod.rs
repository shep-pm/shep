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
use super::level::{Classifier, Level};
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

mod action;
mod bleats;
mod close_dialog;
mod config_pane;
mod config_pane_edits;
mod dog_pane;
mod keys;
mod msg;
mod rows;
mod samples;
mod secrets_keys;
mod secrets_pane;
mod secrets_write;
mod selection;
mod settings;
mod settings_dogs;
mod settings_keys;
mod sheep_pane;
#[cfg(test)]
mod testing;

pub use action::*;
pub use close_dialog::*;
pub(crate) use config_pane::*;
pub use msg::*;
pub use rows::*;
pub(crate) use samples::*;
pub(crate) use secrets_pane::*;
pub use settings::*;

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
    /// The close dialog over the open pane, or `None`.
    ///
    /// Raised by `Escape` on a pane carrying changes the running child has
    /// not taken, and it owns the keyboard while it is up. Cleared with the
    /// pane, so no dialog can outlive the edits it counted.
    close_dialog: Option<CloseDialog>,
    /// The verb the close dialog chose, waiting on its own writes to answer.
    ///
    /// Set by [`Self::answer_close`] alongside the batch that must land
    /// first, and cleared on the reply that finishes it or on
    /// [`CONFIRM_EXPIRY`], the same as the dialog it followed from.
    held: Option<HeldAction>,
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
    /// Whether the keymap overlay is up.
    ///
    /// Not an [`InputMode`]: `map_key` has exactly two modes and an overlay
    /// that swallows keys is a reducer concern rather than a keyboard-edge
    /// one. A third mode would also have to answer what a letter means in
    /// it, and the answer is nothing.
    keymap_open: bool,
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
            close_dialog: None,
            held: None,
            style: (StyleLevel::Full, StyleSource::Default),
            cpu_history: HashMap::new(),
            flock_cpu: VecDeque::new(),
            rss_history: HashMap::new(),
            cpu_last: HashMap::new(),
            grouping: Grouping::Flat,
            collapsed_folds: HashSet::new(),
            keymap_open: false,
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
                    let stale = self.close_dialog.as_ref().is_some_and(|dialog| {
                        now.saturating_duration_since(dialog.at()) >= CONFIRM_EXPIRY
                    });
                    if stale {
                        self.close_dialog = None;
                    }
                    // A reply that never comes cannot strand the verb: it
                    // rides the same clock as the dialog it followed from.
                    let stale_held = self.held.as_ref().is_some_and(|held| {
                        now.saturating_duration_since(held.at) >= CONFIRM_EXPIRY
                    });
                    if stale_held {
                        self.held = None;
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
                    name,
                    ticket,
                    key,
                    value,
                    ..
                } => {
                    let landed = result.is_ok();
                    let effect = self.on_field_applied(&name, &key, &value, result);
                    self.resolve_held_write(ticket, landed).unwrap_or(effect)
                }
                Sent::SetEnv {
                    name,
                    ticket,
                    key,
                    value,
                    ..
                } => {
                    let landed = result.is_ok();
                    let was_set = value.is_some();
                    let effect = self.on_env_set(&name, &key, was_set, result);
                    self.resolve_held_write(ticket, landed).unwrap_or(effect)
                }
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
                    // and `close_dialog` describing a screen that is no longer
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
                            // The overlay closes rather than the editor
                            // reopening beneath it. `on_key` checks text mode
                            // ahead of `keymap_open`, deliberately, so an `h`
                            // typed into a filter box stays a letter; the cost
                            // is that text mode restored from a MESSAGE would
                            // take the keyboard while the box is still drawn,
                            // and every key would reach a socket path the box
                            // hides. Reachable because `is_armed` does not
                            // cover `Pending::Sent`, so `h` with a write in
                            // flight opens the overlay instead of cancelling.
                            //
                            // Closing it is the lesser surprise: the operator
                            // asked for a key list, and what they get instead
                            // is their refused write, the grave notice saying
                            // why, and their own typed text back. `h` reopens
                            // the box.
                            self.keymap_open = false;
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

    /// The list sub-screen's own keymap, in force for as long as the pane
    /// holds one.
    ///
    /// `Escape` closes the sub-screen, not the pane, the same
    /// innermost-first rule the env screen follows. `Enter` or `e` opens
    /// the editor on the element under the cursor, or adds one on
    /// `+ new`. `d` removes, `K`/`J` move the element one place, and `h`
    /// opens the keymap overlay, same as everywhere else.
    ///
    /// A removal and a move file the whole array, since that is what the
    /// write carries. Nothing goes out here: the pane's own `Escape` is
    /// what writes the set.
    fn on_list_key(&mut self, key: KeyPress) -> Effect {
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        match key {
            // Unreachable, the same way and for the same reason as
            // `on_pane_key`'s own copy of this arm: the guard above already
            // returned, and this stays instead of a wildcard so a stray
            // `KeyPress` variant added later cannot fall through unnoticed.
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
            KeyPress::Help => {
                self.open_keymap();
            }
            KeyPress::Action(_)
            | KeyPress::Cycle
            | KeyPress::Settings
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete
            | KeyPress::Collapse
            // Bound only in the close dialog; with none up on a list
            // sub-screen, `c` is a stray key the same way an action key is.
            | KeyPress::Continue => {}
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

    /// One sheep by id, whatever the filter hides: the lookup a
    /// [`RowKey::Sheep`] row's rendering needs.
    #[must_use]
    pub fn row(&self, id: u32) -> Option<&Row> {
        self.flock.get(&id)
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

    /// Overrides the control gate a fixture built with. Every shipped fixture
    /// hard-codes [`Control::ReadOnly`].
    #[cfg(test)]
    pub(crate) fn set_control_for_tests(&mut self, control: Control) {
        self.control = control;
    }
}

/// Saturating `Duration` to milliseconds. Saturates for clippy's
/// `cast_possible_truncation`, not for a lookout left open 580 million years.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::pane::ListRow;
    use shep_core::protocol::ProcessEventKind;

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
        let request = one_wire(close_writing(&mut app));
        let Request::SetSheepField { key, value, .. } = request else {
            panic!("expected SetSheepField, got {request:?}");
        };
        assert_eq!(key, "args");
        assert_eq!(value, serde_json::json!(["9090"]));
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
