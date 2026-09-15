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
mod pane_list;
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
}
