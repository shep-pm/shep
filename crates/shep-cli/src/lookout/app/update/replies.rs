//! The answers to a request this dashboard sent: what landed, what never
//! went out, and what a batch left unsent.

use super::*;

impl App {
    /// `Msg::Replied`'s arm: routed by what was sent, to the handler that
    /// asked for it.
    pub(super) fn on_replied(
        &mut self,
        sent: Sent,
        result: Result<Response, RequestError>,
    ) -> Effect {
        match sent {
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
            Sent::GrantDog { name, ticket, .. } => self.on_dog_grant_reply(name, ticket, result),
            Sent::SheepConfig { name } => self.on_sheep_config(&name, result),
            Sent::DogSection { name } => self.on_dog_section(&name, result),
            Sent::SetDogSection { name, ticket, .. } => {
                self.on_dog_section_set(&name, ticket, result)
            }
            Sent::SetSheepDogTable {
                name, dog, table, ..
            } => self.on_table_set(&name, &dog, table.is_none(), result),
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
        }
    }

    /// `Msg::Unsent`'s arm: a request the caller could not hand to the link
    /// task, reported by what was sent.
    pub(super) fn on_unsent(&mut self, sent: Sent) -> Effect {
        match sent {
            Sent::Action { verb, target, name } => {
                self.action = None;
                self.notice = Some(Notice::grave(
                    // No cause: `Full` is reachable while the shepherd is
                    // merely slow, so naming one would invent it.
                    format!("{}: it was not sent", target_prefix(verb, &target, &name)),
                ));
                Effect::None
            }
            // A dropped lamb fetch already reads as "not read yet".
            Sent::Lambs { .. } => Effect::None,
            // A config read nobody took, reported rather than
            // swallowed: silence here looks like a key that is not
            // bound. A close waiting on it ends, having written nothing.
            Sent::SheepConfig { name } => {
                self.config_read_in_flight = false;
                self.notice = Some(Notice::grave(format!(
                    "{name}: its config was not asked for{}",
                    self.end_close()
                )));
                Effect::None
            }
            // Both write arms name the field, and neither reaches for
            // the pane: a close sends the whole set and leaves, so
            // every one of these lands with no pane on screen. The
            // notice is the whole report.
            Sent::ApplyField { name, key, .. } => {
                self.notice = Some(Notice::grave(format!("{name}: {key} was not sent")));
                Effect::None
            }
            Sent::SetEnv { name, key, .. } => {
                self.notice = Some(Notice::grave(format!("{name}: env {key} was not sent")));
                Effect::None
            }
            // The dog twins of the two arms above: a read nobody took
            // is reported, and so is a write nobody took.
            Sent::DogSection { name } => {
                self.config_read_in_flight = false;
                // A settings row left mid-read would otherwise sit there
                // holding nothing: the tree was put back to `Reading` for a
                // re-read that never went out.
                if self
                    .settings()
                    .is_some_and(|settings| settings.is_reading_a_dog(&name))
                {
                    self.refuse_settings_dog(
                        &name,
                        format!("{name}: its config was not asked for"),
                    );
                }
                self.notice = Some(Notice::grave(format!(
                    "{name}: its config was not asked for{}",
                    self.end_close()
                )));
                Effect::None
            }
            Sent::SetDogSection { name, ticket, .. } => {
                // The write never left, so nothing is in flight: the prompt has
                // to say so rather than sit at "sent" waiting on a reply that
                // can no longer arrive.
                if let Some(settings) = self.settings_mut() {
                    settings.resolve(ticket);
                }
                self.notice = Some(Notice::grave(format!("{name}: its config was not sent")));
                Effect::None
            }
            Sent::SetSheepDogTable { name, dog, .. } => self.on_table_unsent(&name, &dog),
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
                self.notice = Some(Notice::grave(format!("{verb} {name}: it was not sent")));
                Effect::None
            }
            // The record is already in the file by the time this half goes
            // out, so the sentence says the shepherd was not told rather than
            // that nothing happened.
            Sent::GrantDog { name, ticket, .. } => {
                if let Some(settings) = self.settings_mut() {
                    settings.resolve(ticket);
                }
                self.notice = Some(Notice::grave(format!(
                    "{name}: its new grants were not sent, and the running dog keeps the old ones"
                )));
                Effect::None
            }
        }
    }

    /// `Msg::BatchSent`'s arm.
    ///
    /// Delegates to `Msg::Unsent`'s own match rather than repeating
    /// it: every arm there already reports the right notice for the
    /// `Sent` it carries, and returns `Effect::None`.
    pub(super) fn on_batch_sent(&mut self, unsent: Option<Sent>) -> Effect {
        match unsent {
            Some(sent) => self.update(Msg::Unsent { sent }),
            None => Effect::None,
        }
    }
}
