//! A dog's pane actions: the letter pressed on its status tab, the question a
//! destructive one asks first, and what the dog's reply leaves in the footer.

use super::dog_pane_pull::replied_body;
use super::*;
use crate::terminal_safe::sanitise;

impl App {
    /// A letter pressed on the status tab: runs the action it names, if any.
    ///
    /// A row action needs the cursor on a row of its own table. A read-only
    /// lookout refuses every action. A destructive one asks first and its
    /// yes sends it, see [`Self::on_pane_action_answer`].
    pub(super) fn on_pane_action_key(&mut self, letter: char) -> Effect {
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        let name = pane.target().name().to_owned();
        let Some(status) = pane.status() else {
            return Effect::None;
        };
        let Some(key) = status
            .keys()
            .into_iter()
            .find(|key| key.letter == Some(letter))
        else {
            // A letter no action took keeps the meaning it has everywhere else.
            return match letter {
                'r' => self.reread_pane(),
                'h' => self.open_keymap(),
                _unbound => Effect::None,
            };
        };
        let row = match &key.rows {
            None => None,
            Some(rows) => {
                let under_cursor = status.cursor().filter(|at| rows.contains(at));
                match under_cursor.and(status.row_key()) {
                    Some(row) => Some(row.to_owned()),
                    None => {
                        self.say_on_dog_pane(&name, "no row selected");
                        return Effect::None;
                    }
                }
            }
        };
        if self.authorize_write().is_none() {
            return Effect::None;
        }
        let question = key.action.destructive.then(|| {
            let label = sanitise(&key.action.label).0;
            match &row {
                Some(row) => format!("Run {label} on {}?", sanitise(row).0),
                None => format!("Run {label}?"),
            }
        });
        let sent = Sent::DogPaneAction {
            name,
            action: key.action.action,
            row,
        };
        let Some(question) = question else {
            return Effect::Send(sent);
        };
        let tag = self.mint_tag();
        self.pane_action = Some((tag, sent));
        self.open_modal(Modal::confirm(tag, question));
        Effect::None
    }

    /// The answer to the question `tag` asked, or `None` when `tag` is not a
    /// pane action's. A yes sends the action and any other answer drops it.
    pub(super) fn on_pane_action_answer(&mut self, tag: u64, yes: bool) -> Option<Effect> {
        let (asked, sent) = self.pane_action.take()?;
        if asked != tag {
            self.pane_action = Some((asked, sent));
            return None;
        }
        Some(if yes {
            Effect::Send(sent)
        } else {
            Effect::None
        })
    }

    /// An action's reply: a plain answer is shown in the footer and the pane
    /// is pulled again, and a refusal or a timeout is shown and nothing else.
    ///
    /// Dropped when the operator has left the dog's pane.
    pub(super) fn on_dog_pane_action_reply(
        &mut self,
        name: &str,
        action: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        let opening = self.dog_opening;
        let Some(status) = self
            .dog_pane_of(name, opening)
            .and_then(ConfigPane::status_mut)
        else {
            return Effect::None;
        };
        match replied_body(name, result) {
            Ok(body) => {
                let body = body.trim();
                status.say(
                    if body.is_empty() {
                        format!("{action} done")
                    } else {
                        body.to_owned()
                    }
                    .as_str(),
                );
                if status.begin_pull() {
                    Effect::Send(Sent::DogPane {
                        name: name.to_owned(),
                        opening,
                    })
                } else {
                    Effect::None
                }
            }
            Err(reason) => {
                status.say(&reason);
                Effect::None
            }
        }
    }

    /// Puts `text` in the footer of dog `name`'s open status tab, or in the
    /// status bar when that pane is not the one open.
    pub(super) fn say_on_dog_pane(&mut self, name: &str, text: &str) {
        let opening = self.dog_opening;
        match self
            .dog_pane_of(name, opening)
            .and_then(ConfigPane::status_mut)
        {
            Some(status) => status.say(text),
            None => self.notice = Some(Notice::grave(format!("{name}: {text}"))),
        }
    }

    /// Whether a dog's status tab is the open screen: the tab is up and no
    /// dialog or list sits over it.
    pub(super) fn on_status_tab(&self) -> bool {
        self.close_dialog.is_none()
            && self.config_pane().is_some_and(|pane| {
                pane.list().is_none()
                    && pane
                        .status()
                        .is_some_and(|status| status.tab() == crate::lookout::pane::Tab::Status)
            })
    }
}
