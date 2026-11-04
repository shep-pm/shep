//! What a dog's `shep.pane` pull left on its config pane.
//!
//! The status tab draws it. The pane is rebuilt on every re-read of the dog's
//! section, so this rides across the rebuild, with the open tab and the
//! cursor, beside the open tables.

use std::time::{Duration, Instant};

use shep_core::dogs::pane::{Pane, Section};

use super::status::{Layout, StatusView, Step, Tab, layout};

/// What a dog's manifest says about its pane: the names a pane is checked
/// against.
///
/// The actions and the refresh slices read these, since a pane may only name
/// an action the manifest declares and a section's `refresh_on` only means
/// something for an event kind the manifest lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredPane {
    /// The names of the manifest's `actions`.
    pub actions: Vec<String>,
    /// The kinds of the manifest's `events`.
    pub events: Vec<String>,
}

/// The least time between two pulls of one pane, so a burst of events costs
/// one pull at its start and one at its end.
const PULL_GAP: Duration = Duration::from_secs(1);

/// How the pull stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneState {
    /// Asked, and no answer has landed yet.
    Waiting,
    /// The dog's answer, read.
    Ready(Pane),
    /// No answer to hold, and a one-line reason why.
    Unavailable(String),
}

/// A dog's pane as the config pane holds it: what the manifest declares and
/// what the last pull came back with.
#[derive(Debug, Clone)]
pub struct DogStatus {
    declared: DeclaredPane,
    state: PaneState,
    /// Why the newest pull failed, while [`Self::state`] still holds an older
    /// good pane, so one slow answer does not blank it.
    failed: Option<String>,
    /// Whether a pull is out and has not landed.
    pulling: bool,
    /// When the newest pull went out.
    last_pull: Option<Instant>,
    /// Whether an event asked for a pull that has not gone out yet.
    pending: bool,
    /// The open tab and the status tab's cursor.
    view: StatusView,
}

impl DogStatus {
    /// A dog that declares a pane, before any pull went out.
    #[must_use]
    pub(in crate::lookout) fn new(declared: DeclaredPane) -> Self {
        Self {
            declared,
            state: PaneState::Waiting,
            failed: None,
            pulling: false,
            last_pull: None,
            pending: false,
            view: StatusView::default(),
        }
    }

    /// Takes the manifest's newest word on what the pane declares, leaving
    /// what was pulled alone.
    pub(in crate::lookout) fn redeclare(&mut self, declared: DeclaredPane) {
        self.declared = declared;
    }

    /// What the dog's manifest declares.
    #[must_use]
    pub fn declared(&self) -> &DeclaredPane {
        &self.declared
    }

    /// How the pull stands.
    #[must_use]
    pub fn state(&self) -> &PaneState {
        &self.state
    }

    /// Why the newest pull failed, when [`Self::state`] is still an older
    /// good pane. [`None`] otherwise: an [`PaneState::Unavailable`] carries
    /// its own reason.
    #[must_use]
    pub fn failed(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    /// Marks a pull as out at `now`, and says whether to send it. `false`
    /// while one is already waiting, so a second ask is not sent.
    #[must_use]
    pub(in crate::lookout) fn begin_pull(&mut self, now: Instant) -> bool {
        if core::mem::replace(&mut self.pulling, true) {
            return false;
        }
        self.last_pull = Some(now);
        self.pending = false;
        true
    }

    /// Whether the pane is showing a section that names `kind`, an event kind
    /// the manifest declares.
    #[must_use]
    pub(in crate::lookout) fn refreshes_on(&self, kind: &str) -> bool {
        self.declared.events.iter().any(|declared| declared == kind)
            && self.named_kinds().contains(&kind)
    }

    /// The kinds the held pane's sections name that the manifest does not
    /// declare, each once, in the order the sections first name them.
    #[must_use]
    pub(in crate::lookout) fn undeclared_kinds(&self) -> Vec<&str> {
        let mut kinds: Vec<&str> = Vec::new();
        for kind in self.named_kinds() {
            let declared = self.declared.events.iter().any(|event| event == kind);
            if !declared && !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        kinds
    }

    /// Every `refresh_on` kind of the held pane's sections.
    fn named_kinds(&self) -> Vec<&str> {
        let PaneState::Ready(pane) = &self.state else {
            return Vec::new();
        };
        pane.sections
            .iter()
            .flat_map(|section| match section {
                Section::Fields(fields) => fields.refresh_on.as_slice(),
                Section::Table(table) => table.refresh_on.as_slice(),
                _unknown => &[],
            })
            .map(String::as_str)
            .collect()
    }

    /// Marks a pull due, and says whether to send it now: when none is out
    /// and the last went out a second or more before `now`. Otherwise the
    /// pull waits for [`Self::send_pending`].
    #[must_use]
    pub(in crate::lookout) fn pull_due(&mut self, now: Instant) -> bool {
        self.pending = true;
        self.send_pending(now)
    }

    /// Marks a pull due without sending it: the next tick does.
    pub(in crate::lookout) fn mark_due(&mut self) {
        self.pending = true;
    }

    /// Sends the pull that was marked due, once none is out and a second has
    /// passed since the last.
    #[must_use]
    pub(in crate::lookout) fn send_pending(&mut self, now: Instant) -> bool {
        let rested = self
            .last_pull
            .is_none_or(|last| now.saturating_duration_since(last) >= PULL_GAP);
        self.pending && rested && self.begin_pull(now)
    }

    /// Takes a pull's answer: a read pane, or the reason it came to nothing.
    ///
    /// A failure after a good pane keeps that pane and notes the reason beside
    /// it; a failure with none held is [`PaneState::Unavailable`].
    pub(in crate::lookout) fn land(&mut self, answer: Result<Pane, String>) {
        self.pulling = false;
        let landed = answer.is_ok();
        match answer {
            Ok(pane) => {
                self.state = PaneState::Ready(pane);
                self.failed = None;
            }
            Err(reason) if matches!(self.state, PaneState::Ready(_)) => {
                self.failed = Some(reason);
            }
            Err(reason) => self.state = PaneState::Unavailable(reason),
        }
        if landed || !matches!(self.state, PaneState::Ready(_)) {
            self.view.settle(&self.layout());
        }
    }

    /// The open tab.
    #[must_use]
    pub fn tab(&self) -> Tab {
        self.view.tab()
    }

    /// Opens the tab that is not open.
    ///
    /// Leaving the status tab drops a pull that was waiting for the tick.
    pub(in crate::lookout) fn switch_tab(&mut self) {
        self.view.switch();
        self.pending = false;
    }

    /// The row the status tab's cursor is on, counted over every table's rows
    /// in order. [`None`] while the pane holds no row.
    #[must_use]
    pub fn cursor(&self) -> Option<usize> {
        self.view.cursor()
    }

    /// Moves the status tab's cursor, or scrolls the tab when the cursor is
    /// at an end of the rows or there are none.
    pub(in crate::lookout) fn step(&mut self, step: Step) {
        let layout = self.layout();
        self.view.step(step, &layout);
    }

    /// Tells the status tab how tall the pane's body is, the title included.
    ///
    /// The tab spends a line on its strip and one per footer line, and the
    /// rest is what the window shows.
    pub(in crate::lookout) fn set_rows(&mut self, rows: usize) {
        let footer = usize::from(self.failed.is_some())
            + usize::from(matches!(&self.state, PaneState::Ready(pane) if pane.skipped > 0));
        let room = if rows == 0 {
            0
        } else {
            rows.saturating_sub(2 + footer).max(1)
        };
        let layout = self.layout();
        self.view.set_room(room, &layout);
    }

    /// The first body line the status tab draws, when a key scrolled it away
    /// from the cursor. [`None`] while the window follows the cursor.
    #[must_use]
    pub fn scroll(&self) -> Option<usize> {
        self.view.scroll()
    }

    /// Where the lines of the pane's body fall. Empty unless the pane is
    /// ready.
    fn layout(&self) -> Layout {
        match &self.state {
            PaneState::Ready(pane) => layout(pane),
            PaneState::Waiting | PaneState::Unavailable(_) => Layout::default(),
        }
    }
}
