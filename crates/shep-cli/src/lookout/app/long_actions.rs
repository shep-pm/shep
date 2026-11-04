//! The pane actions a dog answered with an id, which the status bar shows
//! until the dog's `action.done` event names them.
//!
//! The items belong to the app, not to the pane: closing the pane or leaving
//! the dog keeps them. A link that dropped ends them as "result unknown",
//! since the event may have been lost with it.

use serde_json::Value;
use shep_client::dogs::{ActionDone, ActionId};

use super::*;
use crate::terminal_safe::sanitise;

/// How long a result stays on the bar after it arrives.
const RESULT_SHOWS: Duration = Duration::from_secs(5);

/// The most characters of a failure's error the bar shows.
const ERROR_WIDTH: usize = 80;

/// One action a dog is still working on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Running {
    dog: String,
    /// The action as the pane labelled it, cleaned for the terminal.
    label: String,
    id: ActionId,
    started: Instant,
}

/// What the status bar says about long actions.
#[derive(Debug, Default)]
pub(super) struct LongActions {
    /// In the order they started.
    running: Vec<Running>,
    /// The latest result line and the instant it was shown at.
    result: Option<(String, Instant)>,
}

impl LongActions {
    fn start(&mut self, dog: &str, label: &str, id: ActionId, now: Instant) {
        self.running.push(Running {
            dog: dog.to_owned(),
            label: sanitise(label).0,
            id,
            started: now,
        });
    }

    /// Ends the action `dog` published `payload` for, and shows how it went.
    /// `false` when the payload names no action started here.
    fn finish(&mut self, dog: &str, payload: &Value, now: Instant) -> bool {
        let Some((id, outcome)) = ActionDone::read(payload) else {
            return false;
        };
        let Some(at) = self
            .running
            .iter()
            .position(|item| item.dog == dog && item.id == id)
        else {
            return false;
        };
        let item = self.running.remove(at);
        let text = match outcome {
            Ok(_) => format!("{}: done", item.label),
            Err(error) => format!("{}: failed, {}", item.label, one_line(&error)),
        };
        self.result = Some((text, now));
        true
    }

    /// Ends every running action as unknown.
    fn lose_all(&mut self, now: Instant) {
        if self.running.is_empty() {
            return;
        }
        let text = self
            .running
            .drain(..)
            .map(|item| format!("{}: result unknown", item.label))
            .collect::<Vec<_>>()
            .join("   ");
        self.result = Some((text, now));
    }

    /// Retires a result that has been shown long enough.
    pub(super) fn tick(&mut self, now: Instant) {
        if self
            .result
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= RESULT_SHOWS)
        {
            self.result = None;
        }
    }

    /// The latest result, while it is still shown.
    pub(super) fn result(&self) -> Option<&str> {
        self.result.as_ref().map(|(text, _)| text.as_str())
    }

    /// The oldest running action with its time spent and a count of the
    /// others, or `None` when nothing is running.
    pub(super) fn running_line(&self, now: Instant) -> Option<String> {
        let first = self.running.first()?;
        let spent = now.saturating_duration_since(first.started).as_secs();
        let more = match self.running.len() - 1 {
            0 => String::new(),
            others => format!("  +{others} more"),
        };
        Some(format!(
            "{}: {} running  {spent}s{more}",
            first.dog, first.label
        ))
    }
}

/// The first line of `error`, cleaned of control characters and cut to
/// [`ERROR_WIDTH`] characters.
fn one_line(error: &str) -> String {
    let first = sanitise(error.lines().next().unwrap_or_default()).0;
    let first = first.trim();
    if first.chars().count() <= ERROR_WIDTH {
        return first.to_owned();
    }
    let kept: String = first.chars().take(ERROR_WIDTH - 1).collect();
    format!("{kept}\u{2026}")
}

impl App {
    /// A pane action's reply that carries an id: the action is running until
    /// the dog's `action.done` names it.
    pub(super) fn start_long_action(&mut self, dog: &str, label: &str, id: ActionId) {
        let now = self.now;
        self.long_actions.start(dog, label, id, now);
    }

    /// A dog's `action.done` event. `None` when it names no action started
    /// here, and otherwise the pull the finished action calls for.
    pub(super) fn on_long_action_done(&mut self, dog: &str, payload: &Value) -> Option<Effect> {
        let now = self.now;
        if !self.long_actions.finish(dog, payload, now) {
            return None;
        }
        Some(self.pull_after_action_of(dog))
    }

    /// The link dropped or lost events: every running action may have ended
    /// unheard.
    pub(super) fn lose_long_actions(&mut self) {
        let now = self.now;
        self.long_actions.lose_all(now);
    }

    /// The status bar's line for the latest long-action result, if shown.
    pub(crate) fn long_action_result(&self) -> Option<&str> {
        self.long_actions.result()
    }

    /// The status bar's line for the long actions still running, if any.
    pub(crate) fn long_actions_running(&self) -> Option<String> {
        self.long_actions.running_line(self.now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_is_cut_to_its_first_line_cleaned_and_capped() {
        assert_eq!(one_line("disk full\nat line 2"), "disk full");
        assert_eq!(
            one_line("\u{1b}[31mdisk\u{7} full\u{1b}[0m"),
            "[31mdisk full[0m"
        );
        let long = "x".repeat(200);
        let cut = one_line(&long);
        assert_eq!(cut.chars().count(), ERROR_WIDTH);
        assert!(cut.ends_with('\u{2026}'));
    }
}
