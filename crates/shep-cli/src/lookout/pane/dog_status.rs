//! What a dog's `shep.pane` pull left on its config pane.
//!
//! The status tab draws it. The pane is rebuilt on every re-read of the dog's
//! section, so this rides across the rebuild, with the open tab and the
//! cursor, beside the open tables.

use shep_core::dogs::pane::Pane;

use super::status::{Layout, PaneKey, StatusView, Step, Tab, keyed, layout, row_at};

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
    /// The open tab and the status tab's cursor.
    view: StatusView,
    /// What the newest action key or action reply said, for the footer.
    said: Option<String>,
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
            view: StatusView::default(),
            said: None,
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

    /// Marks a pull as out, and says whether to send it. `false` while one is
    /// already waiting, so a second ask is not sent.
    #[must_use]
    pub(in crate::lookout) fn begin_pull(&mut self) -> bool {
        !core::mem::replace(&mut self.pulling, true)
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
    pub(in crate::lookout) fn switch_tab(&mut self) {
        self.view.switch();
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
        let footer = self.footer_len();
        let room = if rows == 0 {
            0
        } else {
            rows.saturating_sub(2 + footer).max(1)
        };
        let layout = self.layout();
        self.view.set_room(room, &layout);
    }

    /// The pane's actions with their letters, in declared order. Empty unless
    /// the pane is ready.
    pub(in crate::lookout) fn keys(&self) -> Vec<PaneKey> {
        match &self.state {
            PaneState::Ready(pane) => keyed(pane, &self.declared.actions),
            PaneState::Waiting | PaneState::Unavailable(_) => Vec::new(),
        }
    }

    /// The names of the pane's actions the manifest does not declare.
    pub(in crate::lookout) fn undeclared(&self) -> impl Iterator<Item = String> {
        self.keys()
            .into_iter()
            .map(|key| key.action.action)
            .filter(|name| !self.declared.actions.contains(name))
    }

    /// The key of the row the cursor is on. [`None`] while the pane holds no
    /// row.
    pub(in crate::lookout) fn row_key(&self) -> Option<&str> {
        let PaneState::Ready(pane) = &self.state else {
            return None;
        };
        row_at(pane, self.view.cursor()?).map(|row| row.key.as_str())
    }

    /// Sets the footer's last result, `text` with the terminal's controls
    /// removed.
    pub(in crate::lookout) fn say(&mut self, text: &str) {
        self.said = Some(crate::terminal_safe::sanitise(text).0);
    }

    /// The footer's last result: what the newest action key or action reply
    /// said.
    #[must_use]
    pub fn said(&self) -> Option<&str> {
        self.said.as_deref()
    }

    /// How many lines the status tab's footer holds.
    pub(in crate::lookout) fn footer_len(&self) -> usize {
        usize::from(self.failed.is_some())
            + usize::from(matches!(&self.state, PaneState::Ready(pane) if pane.skipped > 0))
            + self.undeclared().count()
            + usize::from(self.said.is_some())
            + usize::from(self.keys().iter().any(|key| key.letter.is_some()))
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
