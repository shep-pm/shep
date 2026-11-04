//! What a dog's `shep.pane` pull left on its config pane.
//!
//! Held, not drawn: the status tab that reads it comes later. The pane is
//! rebuilt on every re-read of the dog's section, so this rides across the
//! rebuild beside the cursor and the open tables.

use shep_core::dogs::pane::Pane;

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
        }
    }

    /// Takes the manifest's newest word on what the pane declares, leaving
    /// what was pulled alone.
    pub(in crate::lookout) fn redeclare(&mut self, declared: DeclaredPane) {
        self.declared = declared;
    }

    /// What the dog's manifest declares.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the status tab draws the pull, and has not landed"
        )
    )]
    pub fn declared(&self) -> &DeclaredPane {
        &self.declared
    }

    /// How the pull stands.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the status tab draws the pull, and has not landed"
        )
    )]
    pub fn state(&self) -> &PaneState {
        &self.state
    }

    /// Why the newest pull failed, when [`Self::state`] is still an older
    /// good pane. [`None`] otherwise: an [`PaneState::Unavailable`] carries
    /// its own reason.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the status tab draws the pull, and has not landed"
        )
    )]
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
    }
}
