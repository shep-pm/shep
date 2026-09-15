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
mod keys;
mod msg;
mod rows;
mod samples;
mod secrets_keys;
mod secrets_pane;
mod secrets_write;
mod settings;
mod settings_dogs;
mod settings_keys;
mod sheep_pane;
#[cfg(test)]
mod testing;

pub use action::*;
pub use close_dialog::*;
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
        self.body = Body::ConfigPane(pane);
        // The rebuilt pane carries no editor, so the keyboard must not
        // still think one is open.
        self.release_text_mode_if_unowned();
        // An overlay open when this reply landed would otherwise survive
        // the body it was drawn over, and every key from here goes to
        // `on_keymap_key` instead of the pane the operator asked for.
        self.keymap_open = false;
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
                // sub-screen or list sub-screen, so only its view and its
                // edits carry across.
                let carried = self.config_pane().map(|pane| pane.view().clone());
                let carried_edits = self
                    .config_pane()
                    .map(|pane| pane.edits().clone())
                    .unwrap_or_default();
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
                // The settings screen is what a dog pane opens over, and this
                // one assignment is what closes it: `Body` holds one
                // variant, so the pane replaces it once there is something
                // to look at.
                self.body = Body::ConfigPane(pane);
                self.release_text_mode_if_unowned();
                // Same as `Self::open_or_refresh_config_pane`: an overlay
                // open when this reply landed must not survive the body
                // change under it.
                self.keymap_open = false;
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
    ///
    /// `warning` rides the same sentence rather than a second notice: the
    /// write still landed, so this is one more clause about it, on the same
    /// terms `pending`'s own `", and waits for..."` clause already sets.
    fn on_field_applied(
        &mut self,
        name: &str,
        key: &str,
        value: &FieldValue,
        result: Result<Response, RequestError>,
    ) -> Effect {
        match result {
            Ok(Response::SheepFieldSet {
                pending, warning, ..
            }) => {
                let key_text = match value.safe_summary() {
                    Some(v) => format!("{key} set to {v}"),
                    None => format!("{key} is set"),
                };
                let mut text = if pending {
                    format!("{name}: {key_text}, and waits for `shep reload {name}`")
                } else {
                    format!("{name}: {key_text}")
                };
                if let Some(warning) = warning {
                    text = format!("{text}; {warning}");
                }
                self.notice = Some(Notice { text, grave: false });
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

    /// The config pane's own keymap, in force for as long as
    /// [`Self::config_pane`] is `Some`.
    ///
    /// Movement walks fields, `r` re-reads, `space` cycles the row under
    /// the cursor, `Enter` or `e` edits it, `u` undoes the newest edit,
    /// and `Escape` asks the close dialog's question if there is one to
    /// ask, else writes everything filed and leaves. `h` raises the keymap
    /// overlay; see `KeyPress::Help`'s arm below. Everything else is named
    /// rather than wildcarded, so a stray variant cannot fall silently into
    /// an arm that ignores it.
    ///
    /// Nothing is armed here and no key is eaten. A keystroke that edits
    /// files into the pane's own set and sends nothing, so a stray one
    /// costs an `u` rather than a write to a running sheep.
    fn on_pane_key(&mut self, key: KeyPress) -> Effect {
        self.notice = None;
        if self.close_dialog.is_some() {
            return self.on_close_dialog_key(key);
        }
        if self.config_pane().is_some_and(|pane| pane.list().is_some()) {
            return self.on_list_key(key);
        }
        if key == KeyPress::Quit {
            return Effect::Quit;
        }
        match key {
            // Unreachable: the guard above already returned. Kept rather
            // than folded into a silent group or replaced with a wildcard,
            // because every other arm here is named on purpose (a stray
            // `KeyPress` variant should not fall through unnoticed), and a
            // wildcard would defeat that for every variant, not just this
            // one. If the guard above is ever removed, this is what Quit
            // still does.
            KeyPress::Quit => return Effect::Quit,
            // Backs out one level at a time: the close dialog's own
            // question first, if there is one, else the pane. `Escape`
            // closes rather than cascading to a filter clear or a quit,
            // exactly as it does on the settings screen.
            //
            // The dialog is asked before anything is taken: `esc` used to
            // write first and ask second, which missed the very edit that
            // made this pane's `Escape` worth asking about. Now nothing
            // leaves the pane until the question is answered, one way or
            // another.
            //
            // One press, not two: the field's help draws unconditionally
            // now, so nothing waits behind a second `esc` for it.
            KeyPress::Escape => {
                if let Some(dialog) = self.close_offer() {
                    self.close_dialog = Some(dialog);
                    return Effect::None;
                }
                let writes = self.take_pane_writes();
                self.close_pane();
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
                self.open_keymap();
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
            | KeyPress::Collapse
            // Bound only in the close dialog; with none up, `c` is a stray
            // key the same way an action key is.
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
        self.close_dialog = None;
        self.config_target = None;
        self.config_for = None;
        self.dog_target = None;
        self.release_text_mode_if_unowned();
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

    /// How the flock table currently gathers its rows, toggled by
    /// [`KeyPress::FoldView`].
    ///
    /// `view::mod`'s draw loop reads this to choose between the flat column
    /// set and the fold view's own.
    #[must_use]
    pub fn grouping(&self) -> Grouping {
        self.grouping
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

/// Saturating `Duration` to milliseconds. Saturates for clippy's
/// `cast_possible_truncation`, not for a lookout left open 580 million years.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::lookout::app::testing::*;

    use super::super::edits::EditKey;
    use super::*;
    use crate::lookout::pane::ListRow;
    use shep_core::protocol::{ProcessEventKind, RpcError, RpcErrorCode};

    use super::super::view::fixtures;

    /// The status bar's own rendered text.
    fn status_line_text(app: &App) -> String {
        super::super::view::fixtures::rendered(&super::super::view::status::status_line(app, 200))
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
        assert_eq!(pane.fields().len(), 42);
    }

    /// `h` before the reply arrives raises the overlay over the dashboard;
    /// the reply then replaces `self.body` with the pane, which must close
    /// the overlay too, or every key past this point goes to
    /// `on_keymap_key` instead of the pane the operator asked for.
    #[test]
    fn a_config_reply_that_lands_with_the_overlay_up_closes_it() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open(), "the overlay did not open");

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
            "the reply must still open the pane"
        );
        assert!(
            !app.keymap_open(),
            "the overlay survived a body change under it"
        );
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
    /// `config_target` and `close_dialog` went on describing a pane that was
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
        let rows =
            fixtures::config_pane_env_rows_for_tests(app.config_pane().expect("the pane is open"));
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

    /// `esc`, and `c` right behind it if that raised the close dialog
    /// instead of writing outright.
    ///
    /// What every write-on-close test in this module wants now: every
    /// sheep fixture here parks `kill_signal` unconditionally
    /// (`sheep_config_view`'s own default), so a bare `esc` only asks. `c`
    /// is what actually gets the write onto the wire, the same as an
    /// operator continuing past the dialog would; harmless when nothing
    /// asked, since [`App::close_dialog`] is `None` and this returns
    /// `esc`'s own effect unchanged.
    fn close_writing(app: &mut App) -> Effect {
        let effect = app.update(Msg::Key(KeyPress::Escape));
        if app.close_dialog().is_some() {
            app.update(Msg::Key(KeyPress::Continue))
        } else {
            effect
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
            let request = one_wire(close_writing(&mut app));
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
            let request = one_wire(close_writing(&mut app));
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
        let requests = wire_all(close_writing(&mut app));
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
        let mut batch = wire_batch(close_writing(&mut app));
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

        let request = one_wire(close_writing(&mut app));
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
        let effect = close_writing(&mut app);
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
        let Effect::SendAll(mut sent) = close_writing(&mut app) else {
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
        let requests = wire_all(close_writing(&mut app));
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
        let requests = wire_all(close_writing(&mut app));
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
            let mut batch = wire_batch(close_writing(&mut app));
            let effect = app.update(Msg::Replied {
                sent: batch.remove(0),
                result: Ok(Response::SheepFieldSet {
                    name: "web".to_owned(),
                    key: "autorestart".to_owned(),
                    pending,
                    warning: None,
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

    /// A `cwd`/`script`/`out_file`/`err_file` warning rides the same
    /// notice as the write it came back on, not a second one: the write
    /// still landed, and `grave` stays `false` since this is advisory.
    #[test]
    fn a_path_warning_rides_the_same_notice_as_the_write() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        fixtures::type_into_the_open_editor(&mut app, "/does/not/exist");
        // `cwd` needs a respawn, so `esc` raises the close dialog rather
        // than writing on the keypress. `c` is the answer that writes and
        // leaves the sheep alone, which is the same batch this test always
        // read, reached through the question the dialog now asks first.
        app.update(Msg::Key(KeyPress::Escape));
        let mut batch = wire_batch(app.update(Msg::Key(KeyPress::Continue)));
        let effect = app.update(Msg::Replied {
            sent: batch.remove(0),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_owned(),
                key: "cwd".to_owned(),
                pending: true,
                warning: Some("/does/not/exist does not exist yet".to_owned()),
            }),
        });
        // The re-read still goes out. This is the only test that answers
        // with a warning at all, so discarding the effect here would let a
        // refactor gate the re-read on there not being one.
        assert!(
            matches!(effect, Effect::Send(Sent::SheepConfig { .. })),
            "{effect:?}"
        );
        let notice = app.notice().expect("the outcome is reported");
        assert!(!notice.is_grave(), "{notice:?}");
        let text = notice.to_string();
        assert!(text.contains("shep reload"), "{text}");
        assert!(text.contains("does not exist yet"), "{text}");
    }

    /// Every refusal this door can meet is an `Err`, which is why
    /// `Response::SheepFieldSet` carries no `refused` field: two ways to
    /// say no is one a client forgets to check.
    #[test]
    fn a_refused_write_is_reported_and_does_not_re_read() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let mut batch = wire_batch(close_writing(&mut app));
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
        let field = close_writing(&mut app);
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
        // of any more, so the write goes out once the close dialog it
        // raises (this fixture parks `kill_signal`) is answered.
        let env = close_writing(&mut app);
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
        let first = wire_batch(close_writing(&mut app));
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
        let second = wire_batch(close_writing(&mut app));
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
        let request = one_wire(close_writing(&mut app));
        let Request::SetSheepField { key, .. } = request else {
            panic!("{request:?}");
        };
        assert_eq!(key, "autorestart");
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

    /// Same property as `a_config_reply_that_lands_with_the_overlay_up_closes_it`,
    /// on `on_dog_section`'s own body-replacing reply rather than
    /// `open_or_refresh_config_pane`'s.
    #[test]
    fn a_dog_sections_reply_that_lands_with_the_overlay_up_closes_it() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::DogPane {
            name: "metrics".to_string(),
            adopted_path: None,
            result: Ok(crate::dog::builtin_schema("metrics").expect("a built-in")),
        });
        assert!(app.config_pane().is_none(), "one half is not a pane yet");
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open(), "the overlay did not open");

        app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "metrics".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: "bind = \"0.0.0.0:9615\"\n".to_string().into(),
            }),
        });
        assert!(
            app.config_pane().is_some(),
            "the reply must still open the pane"
        );
        assert!(
            !app.keymap_open(),
            "the overlay survived a body change under it"
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
        let batch = wire_batch(close_writing(&mut app));
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
        let request = one_wire(close_writing(&mut app));
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
