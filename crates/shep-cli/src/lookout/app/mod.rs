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
//!
//! [`App`] and [`Body`] are declared here; everything that reads or changes
//! them is a sibling module, one per screen or per subject, each with its own
//! tests:
//!
//! - [`msg`] and [`rows`] are the vocabulary: what arrives, what departs, and
//!   the small value types both sides name.
//! - [`update`] is the reducer, [`keys`] the fork every keypress goes through,
//!   and [`read`] what the renderer reads back.
//! - [`selection`] owns which rows are visible and where the cursor sits;
//!   [`samples`] the sparkline series; [`lambs`] the walk under one sheep.
//! - [`action`] is the one armed verb, and [`close_dialog`] the one a pane
//!   raises on its way out.
//! - [`feed_keys`] is what the bleats pane and the sheep pane's embedded feed
//!   answer the same way, and the armed-confirm prelude both their screens
//!   run.
//! - A screen apiece: [`settings`], [`secrets_pane`], [`config_pane`],
//!   [`sheep_pane`], [`bleats`], [`dog_pane`], and the sub-screens in
//!   [`pane_list`] and [`pane_dogs`].

use core::fmt;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use shep_client::RequestError;
use shep_core::config::{DogTable, LogLevel};
use shep_core::protocol::{
    BusEvent, DogSectionToml, DogSource, EnvValue, Lamb, ProcessEventKind, ProcessInfo, Request,
    Response, SelectorSpec, SheepConfigView, SheepRefusal,
};
use shep_core::secrets::ALL_ENVIRONMENTS;
use shep_core::status::ProcStatus;

use super::field::{FieldKind, FieldSet};
use super::level::{Classifier, Level};
use super::pane::{
    ConfigPane, DogRow, DogToggle, FieldValue, ListedDog, Lock, PaneEdit, PaneRow, PaneTarget,
    ReloadKind, SectionWrite, SheepDogEntry,
};
use super::pane_bleats::BleatsPane;
use super::pane_sheep::SheepPane;
use super::secrets::{SecretRow, SecretsModel, Source};
use super::tail::Stream;
use super::textbox::{TextBox, TextEdit};
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
#[cfg(test)]
mod disabled_tests;
mod dog_pane;
#[cfg(test)]
mod dog_pane_entries;
#[cfg(test)]
mod dog_pane_entry_add;
#[cfg(test)]
mod dog_pane_entry_removal;
mod feed_keys;
mod keys;
mod lambs;
mod msg;
mod over_host;
mod pane_dogs;
mod pane_list;
mod read;
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
mod update;

pub use action::*;
pub use close_dialog::*;
pub(crate) use config_pane::*;
pub use msg::*;
pub(crate) use over_host::NOT_OVER_HOST;
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

/// The body's own pane accessors, so a call site need not match on it.
///
/// A method on the body borrows only the body field, which is what lets a
/// caller still read `App::now` or `App::mode` while it holds a pane. Every
/// match below lists every variant, so a new variant fails to compile.
impl Body {
    /// The open config pane, or `None` on any other variant.
    #[must_use]
    pub(super) fn config_pane(&self) -> Option<&ConfigPane> {
        match self {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// [`Self::config_pane`]'s mutable twin.
    pub(super) fn config_pane_mut(&mut self) -> Option<&mut ConfigPane> {
        match self {
            Body::ConfigPane(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// The settings screen, or `None` on any other variant.
    #[must_use]
    pub(super) fn settings(&self) -> Option<&Settings> {
        match self {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// [`Self::settings`]'s mutable twin.
    pub(super) fn settings_mut(&mut self) -> Option<&mut Settings> {
        match self {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// The open sheep pane, or `None` on any other variant.
    #[must_use]
    pub(super) fn sheep_pane(&self) -> Option<&SheepPane> {
        match self {
            Body::Sheep(pane) => Some(pane.as_ref()),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_) => None,
        }
    }

    /// [`Self::sheep_pane`]'s mutable twin.
    pub(super) fn sheep_pane_mut(&mut self) -> Option<&mut SheepPane> {
        match self {
            Body::Sheep(pane) => Some(pane.as_mut()),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_) => None,
        }
    }

    /// The full-screen bleats pane, or `None` on any other variant.
    #[must_use]
    pub(super) fn bleats_pane(&self) -> Option<&BleatsPane> {
        match self {
            Body::Bleats(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// [`Self::bleats_pane`]'s mutable twin.
    pub(super) fn bleats_pane_mut(&mut self) -> Option<&mut BleatsPane> {
        match self {
            Body::Bleats(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// The open secrets pane, or `None` on any other variant.
    ///
    /// The mutable shape alone, as [`App::secrets_pane_mut`] has had it: no
    /// reader needs the secrets pane by shared reference, the view reaching
    /// it through [`App::body`].
    pub(super) fn secrets_pane_mut(&mut self) -> Option<&mut SecretsPane> {
        match self {
            Body::Secrets(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Sheep(_) => None,
        }
    }
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
    filter: TextBox,
    /// Which keymap [`super::input::map_key`] is called with. Normal until `/`
    /// opens the box; the reducer, not the keymap, owns this state.
    mode: InputMode,
    /// The next write ticket, shared by the config pane's batches and the
    /// settings screen's one edit. Monotonic and never reused, so a reply
    /// can only name the write it belongs to.
    next_write_ticket: u64,
    /// The newest [`Effect::ListSheepDogs`] asked. Only its own answer may
    /// list the dogs tab: closing the pane moves it on, so no answer
    /// outlives the pane that asked.
    sheep_dogs_ask: u64,
    link: Link,
    notice: Option<Notice>,
    palette: Palette,
    control: Control,
    /// The `$SHEP_HOME` this lookout watches, for the title line.
    home: String,
    /// The ssh destination this lookout watches in place of this machine's
    /// shepherd, or `None` for the local one. Set through
    /// [`Self::set_remote_host`].
    remote_host: Option<String>,
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
    /// The last reading of the selected sheep's log files on disk, keyed the
    /// same way and taken on the same cadence as [`Self::lambs`].
    log_size: Option<LogSize>,
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
    /// Whether `Escape` on a whole-table pane is waiting on its re-read.
    ///
    /// Set by [`Self::reread_before_closing`]. Cleared by its answer, by an
    /// unsent read, or by the pane closing. While set, the pane's next
    /// config answer decides the close instead of refreshing it.
    closing: bool,
    /// Whether a [`Sent::SheepConfig`] or [`Sent::DogSection`] read for the
    /// open pane is out and has not answered yet.
    ///
    /// Set by [`Self::reread_pane`], cleared by [`Self::on_sheep_config`],
    /// [`Self::on_dog_section`], or an unsent read. `r` and
    /// [`Self::reread_before_closing`] both route through `reread_pane`, and
    /// a reply carries nothing back to say which of two outstanding reads it
    /// answers, so this stops a second one going out while the first is
    /// still in flight: without it, a close's own re-read and a stray
    /// refresh could each answer the other's request.
    config_read_in_flight: bool,
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

/// Saturating `Duration` to milliseconds. Saturates for clippy's
/// `cast_possible_truncation`, not for a lookout left open 580 million years.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::view::fixtures;

    /// The names of the [`Body`] accessors that answer for `body`, in the
    /// order the type declares them.
    fn answered(body: &mut Body) -> Vec<&'static str> {
        let mut answered = Vec::new();
        if body.config_pane().is_some() {
            answered.push("config_pane");
        }
        if body.config_pane_mut().is_some() {
            answered.push("config_pane_mut");
        }
        if body.settings().is_some() {
            answered.push("settings");
        }
        if body.settings_mut().is_some() {
            answered.push("settings_mut");
        }
        if body.sheep_pane().is_some() {
            answered.push("sheep_pane");
        }
        if body.sheep_pane_mut().is_some() {
            answered.push("sheep_pane_mut");
        }
        if body.bleats_pane().is_some() {
            answered.push("bleats_pane");
        }
        if body.bleats_pane_mut().is_some() {
            answered.push("bleats_pane_mut");
        }
        if body.secrets_pane_mut().is_some() {
            answered.push("secrets_pane_mut");
        }
        answered
    }

    /// A dashboard with one sheep given the whole screen: `enter` on the
    /// selected row, which is the only way this variant is ever reached.
    fn app_in_sheep_pane() -> App {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        app.update(Msg::Key(KeyPress::Confirm));
        app
    }

    /// Every pane variant is reached by its own accessors and by no
    /// other's, and the dashboard by none. An accessor that answered for a
    /// variant it does not name would hand a caller a pane the operator
    /// never opened.
    #[test]
    fn each_body_accessor_answers_only_for_its_own_variant() {
        let none: Vec<&'static str> = Vec::new();
        assert_eq!(answered(&mut fixtures::full_app().body), none);
        assert_eq!(
            answered(&mut fixtures::app_in_settings().body),
            ["settings", "settings_mut"]
        );
        assert_eq!(
            answered(&mut fixtures::app_in_dog_pane().body),
            ["config_pane", "config_pane_mut"]
        );
        assert_eq!(
            answered(&mut fixtures::bleats_pane_with_filters().body),
            ["bleats_pane", "bleats_pane_mut"]
        );
        assert_eq!(
            answered(&mut fixtures::app_with_secrets().body),
            ["secrets_pane_mut"]
        );
        assert_eq!(
            answered(&mut app_in_sheep_pane().body),
            ["sheep_pane", "sheep_pane_mut"]
        );
    }

    /// Each key that would read or write this machine's `shep.toml`,
    /// `dogs.toml`, secret store or dog binaries, pressed on an app watching
    /// a host: the same notice, no effect, and the screen it would have
    /// opened stays shut.
    #[test]
    fn over_a_host_the_settings_secrets_and_dog_keys_raise_a_notice_and_no_effect() {
        use crate::lookout::app::testing::app_in_web;

        let cases: [(&str, App, KeyPress); 5] = [
            (
                "settings",
                fixtures::app_with_a_dog_selected_and_control(),
                KeyPress::Settings,
            ),
            (
                "secrets",
                fixtures::app_with_a_dog_selected_and_control(),
                KeyPress::Secrets,
            ),
            (
                "a dog's config pane",
                fixtures::app_with_a_dog_selected_and_control(),
                KeyPress::Edit,
            ),
            (
                "a sheep's dogs tab",
                app_in_web(Control::Allowed),
                KeyPress::Group(9),
            ),
            (
                "a dog toggle",
                fixtures::app_in_settings_on_dog("metrics"),
                KeyPress::Cycle,
            ),
        ];
        for (what, mut app, key) in cases {
            let before = core::mem::discriminant(app.body());
            app.set_remote_host("build-01");

            let effect = app.update(Msg::Key(key));

            assert_eq!(effect, Effect::None, "{what}");
            let notice = app.notice().unwrap_or_else(|| panic!("{what}: no notice"));
            assert_eq!(notice.to_string(), NOT_OVER_HOST, "{what}");
            assert_eq!(
                core::mem::discriminant(app.body()),
                before,
                "{what}: the body changed"
            );
        }
    }
}
