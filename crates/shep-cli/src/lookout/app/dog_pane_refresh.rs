//! A dog pane's status tab asks for `shep.pane` again when the dog publishes
//! an event one of its sections names.

use super::*;

impl App {
    /// A dog published `kind`. Pulls the open dog's pane again when its status
    /// tab is showing and one of its sections names `kind`, at most once a
    /// second: a pull held back goes out on a later tick.
    pub(super) fn on_dog_event(&mut self, dog: &str, kind: &str) -> Effect {
        let now = self.now;
        let send = self
            .showing_status_of(dog)
            .is_some_and(|status| status.refreshes_on(kind) && status.pull_due(now));
        self.pull_effect(dog, send)
    }

    /// A repair re-read the flock, and a lost event may have been one a
    /// section names: marks the open status tab's pull due for the next tick.
    pub(super) fn mark_dog_pane_due(&mut self) {
        if let Some(dog) = self.open_dog_name()
            && let Some(status) = self.showing_status_of(&dog)
        {
            status.mark_due();
        }
    }

    /// The tick's turn at a pull held back.
    pub(super) fn send_pending_pull(&mut self, now: Instant) -> Effect {
        let Some(dog) = self.open_dog_name() else {
            return Effect::None;
        };
        let send = self
            .showing_status_of(&dog)
            .is_some_and(|status| status.send_pending(now));
        self.pull_effect(&dog, send)
    }

    /// The operator switched tabs: coming back to the status tab asks for a
    /// fresh pull.
    pub(super) fn pull_on_return(&mut self) -> Effect {
        let now = self.now;
        let Some(dog) = self.open_dog_name() else {
            return Effect::None;
        };
        let send = self
            .showing_status_of(&dog)
            .is_some_and(|status| status.pull_due(now));
        self.pull_effect(&dog, send)
    }

    fn pull_effect(&self, dog: &str, send: bool) -> Effect {
        if send {
            Effect::Send(Sent::DogPane {
                name: dog.to_owned(),
                opening: self.dog_opening,
            })
        } else {
            Effect::None
        }
    }

    /// The name of the dog whose config pane is open.
    fn open_dog_name(&self) -> Option<String> {
        match self.config_pane()?.target() {
            PaneTarget::Dog { name, .. } => Some(name.clone()),
            _other => None,
        }
    }

    /// Dog `dog`'s pull, when its pane is open on the status tab.
    fn showing_status_of(&mut self, dog: &str) -> Option<&mut DogStatus> {
        let pane = self.config_pane_mut()?;
        if !matches!(pane.target(), PaneTarget::Dog { name, .. } if name == dog) {
            return None;
        }
        pane.status_mut()
            .filter(|status| status.tab() == crate::lookout::pane::Tab::Status)
    }
}
