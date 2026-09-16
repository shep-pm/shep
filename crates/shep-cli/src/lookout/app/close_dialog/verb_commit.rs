use super::super::*;
use super::dialog_model::CloseDialog;

/// A verb the close dialog chose, waiting on the writes it must follow.
///
/// `tickets` names the writes still outstanding, by the same [`u64`]
/// [`Sent::ticket`] mints them with, rather than merely counting them: two
/// close-dialog answers can each have a batch in flight at once (a second
/// `R`/`L` is refused, but its writes still go, per
/// [`App::answer_close`]'s own doc), and a bare count cannot tell a reply
/// from the other session apart from one of this session's own. `landed`
/// is whether any ticket in this set was accepted. The action goes once
/// the set is empty, and only if something landed: a batch refused in
/// full leaves nothing for a respawn to apply.
///
/// `Debug` is derived (IR-41): a verb, a ticket set, a bool, a name, a
/// time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::lookout::app) struct HeldAction {
    verb: ActionVerb,
    name: String,
    tickets: HashSet<u64>,
    landed: bool,
    pub(in crate::lookout::app) at: Instant,
}

impl App {
    /// The question this pane's `Escape` asks, or [`None`] when it just
    /// writes and leaves.
    ///
    /// Silent behind a closed gate, where every key it offers would be
    /// refused; silent on a dog, which has no apply table to classify an
    /// edit with; and silent on a sheep that is not running, where nothing
    /// holds the old config and `R` would start it rather than replace it.
    pub(in crate::lookout::app) fn close_offer(&self) -> Option<CloseDialog> {
        if self.control == Control::ReadOnly {
            return None;
        }
        let pane = self.config_pane()?;
        let PaneTarget::Sheep { name } = pane.target() else {
            return None;
        };
        let (status, pid) = self.running_state(name)?;
        let unsent = pane.unsent_fields_needing_a_respawn();
        let parked = pane.parked_count();
        if unsent.is_empty() && parked == 0 {
            return None;
        }
        Some(CloseDialog::new(
            unsent, parked, pane, status, pid, self.now,
        ))
    }

    /// Writes the pane's filed set and closes both the dialog and the
    /// pane, for `R`, `L` and `c` alike.
    ///
    /// `c` (`verb` is [`None`]) just sends the batch. `R`/`L` hold the verb
    /// on [`Self::held`] until every write in the batch is answered, per
    /// "write, then act" in the design: sending it alongside the writes
    /// risks the shepherd answering the action before a write it depended
    /// on, which respawns into the config the pane was just fixing. A batch
    /// with nothing to wait for (parked fields only, nothing filed) sends
    /// the verb at once instead; there is nothing for it to outrun.
    ///
    /// The writes always go: they are the operator's own edits, and the
    /// transport already orders them safely against whatever else is
    /// outstanding. What refuses is only the verb, and only when one is
    /// already in flight, held or armed: [`Self::held`] already occupied,
    /// or [`Self::action`] already sent by an earlier confirm. Holding a
    /// second verb there would let the first batch's own replies fire it,
    /// same conflict [`Self::arm`] and [`Self::arm_sheep_pane`] refuse by
    /// the front door.
    pub(super) fn answer_close(&mut self, verb: Option<ActionVerb>) -> Effect {
        let name = self
            .close_dialog
            .as_ref()
            .map(|dialog| dialog.target_name().to_owned());
        let writes = self.take_pane_writes();
        self.close_pane();
        let (Some(verb), Some(name)) = (verb, name) else {
            return Effect::SendAll(writes);
        };
        if self.held.is_some() || self.action.is_some() {
            self.notice = Some(Notice {
                text: "one action is already in flight".to_string(),
                grave: true,
            });
            return Effect::SendAll(writes);
        }
        // Nothing goes to a shepherd that is gone. The menu this dialog
        // replaced refused here through `apply_parked`, and losing that
        // refusal alongside it would have sent a restart into a dead link
        // with nothing on screen saying it never left. The writes still
        // go, for the same reason a refused verb's writes do: they are the
        // operator's own work and the batch reports its own failure.
        if let Some(text) = self.link_refusal() {
            self.notice = Some(Notice { text, grave: true });
            return Effect::SendAll(writes);
        }
        if writes.is_empty() {
            return self.send_held_action(verb, name);
        }
        let tickets = writes.iter().filter_map(Sent::ticket).collect();
        self.held = Some(HeldAction {
            verb,
            name,
            tickets,
            landed: false,
            at: self.now,
        });
        Effect::SendAll(writes)
    }

    /// One pane write's answer, matched against the held batch by its own
    /// ticket rather than merely counted off it.
    ///
    /// `None` when nothing is held, or when this reply's ticket names no
    /// write the held batch went out with (the second half of
    /// [`Self::answer_close`]'s refusal: a second session's writes still
    /// go, unheld, so their replies must never be read as this session's
    /// own). Either way the caller's own effect stands unchanged. `Some`
    /// overrides it: a batch still waiting on other tickets yields
    /// [`Effect::None`], since the pane closed with the write and nothing
    /// downstream needs its own chained re-read; the last ticket yields
    /// the verb, sent through [`Self::send_held_action`], or a notice when
    /// every write in the batch was refused.
    pub(in crate::lookout::app) fn resolve_held_write(
        &mut self,
        ticket: u64,
        landed: bool,
    ) -> Option<Effect> {
        let still_waiting = {
            let held = self.held.as_mut()?;
            if !held.tickets.remove(&ticket) {
                return None;
            }
            held.landed |= landed;
            !held.tickets.is_empty()
        };
        if still_waiting {
            return Some(Effect::None);
        }
        let held = self.held.take().expect("checked Some above");
        if !held.landed {
            // The per-field handler already set a notice naming which write
            // and why: reuse it rather than replace it, so the one thing an
            // operator needs most (the reason) is not the thing this frame
            // clobbers to say the action did not go out.
            let reason = self.notice.as_ref().map_or_else(
                || format!("{}: every write was refused", held.name),
                |notice| notice.text.clone(),
            );
            self.notice = Some(Notice {
                text: format!("{reason}, so {} did not go out", held.verb.label()),
                grave: true,
            });
            return Some(Effect::None);
        }
        Some(self.send_held_action(held.verb, held.name))
    }

    /// Sends `verb` at `name`, the same lookup and bookkeeping
    /// [`Self::confirm`] uses once an action is already past its question:
    /// pinned as [`Stage::Sent`] so the in-flight gate still sees it, and
    /// silent (no send) with a notice if the sheep left the flock while its
    /// writes were in flight.
    fn send_held_action(&mut self, verb: ActionVerb, name: String) -> Effect {
        let Some((target, count)) = self.flock_target(&name) else {
            self.notice = Some(Notice {
                text: format!("{name}: it is no longer in the flock"),
                grave: true,
            });
            return Effect::None;
        };
        self.action = Some(Action {
            verb,
            target: target.clone(),
            name: name.clone(),
            count,
            at: self.now,
            stage: Stage::Sent,
        });
        Effect::Send(Sent::Action { verb, target, name })
    }

    /// The close dialog over the open pane, or `None`.
    #[must_use]
    pub fn close_dialog(&self) -> Option<&CloseDialog> {
        self.close_dialog.as_ref()
    }

    /// The verb a close dialog's `R` or `L` chose, waiting on its own
    /// writes to answer, or `None` once it has fired, been dropped or
    /// expired.
    #[cfg(test)]
    fn held_action(&self) -> Option<&HeldAction> {
        self.held.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::*;
    use super::*;

    use crate::lookout::edits::EditKey;
    use crate::lookout::view::fixtures;

    #[test]
    fn escape_on_a_parked_pane_asks_and_escape_again_keeps_the_pane() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.config_pane().is_some(),
            "the pane stays up behind the dialog"
        );
        assert!(app.close_dialog().is_some(), "the dialog is open");

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.config_pane().is_some(),
            "escape twice keeps the pane: the second esc answers the dialog, not the pane"
        );
        assert!(app.close_dialog().is_none());
    }

    #[test]
    fn the_dialog_counts_the_parked_fields_once() {
        let mut app = fixtures::app_in_sheep_pane_with_two_parked_fields();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(app.close_dialog().expect("the dialog is open").parked(), 2);
        assert_eq!(
            app.config_pane().expect("a pane").parked_count(),
            2,
            "the pane and the dialog agree"
        );
    }

    #[test]
    fn the_dialog_reads_which_reload_this_sheep_would_get() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert_eq!(
            app.close_dialog().expect("the dialog is open").reload(),
            ReloadKind::Overlap,
            "the fixture sets no readiness probe"
        );
    }

    #[test]
    fn the_dialog_never_opens_while_the_gate_is_closed() {
        let mut app = fixtures::app_in_sheep_pane();
        assert!(app.config_pane().expect("a pane").parked_count() > 0);
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none(), "read-only can apply nothing");
        assert!(app.config_pane().is_none());
    }

    /// Order is the whole point: a restart sent before the write lands
    /// respawns into the old config.
    #[test]
    fn r_sends_every_write_before_the_restart() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let Effect::SendAll(batch) = effect else {
            panic!("expected a batch, got {effect:?}");
        };
        assert_eq!(batch.len(), 2, "the two writes and no action yet");
        assert!(
            batch
                .iter()
                .all(|sent| !matches!(sent, Sent::Action { .. }))
        );
    }

    #[test]
    fn the_restart_goes_once_the_last_write_is_answered() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(batch) = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected a batch");
        };
        let first = app.update(Msg::Replied {
            sent: batch[0].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "cwd".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(matches!(first, Effect::None), "not yet: {first:?}");
        let second = app.update(Msg::Replied {
            sent: batch[1].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "max_memory".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(
                second,
                Effect::Send(Sent::Action {
                    verb: ActionVerb::Restart,
                    ..
                })
            ),
            "got {second:?}"
        );
    }

    /// A refused field alongside an accepted one still needs the restart the
    /// accepted one was waiting for.
    #[test]
    fn a_partly_refused_batch_still_restarts() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(batch) = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected a batch");
        };
        app.update(Msg::Replied {
            sent: batch[0].clone(),
            result: Err(fixtures::invalid_config()),
        });
        let last = app.update(Msg::Replied {
            sent: batch[1].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "max_memory".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(last, Effect::Send(Sent::Action { .. })),
            "got {last:?}"
        );
    }

    /// Bouncing a healthy process to apply nothing is the one outcome with
    /// a cost and no benefit.
    #[test]
    fn a_wholly_refused_batch_does_not_restart() {
        let mut app = fixtures::app_in_sheep_pane_with_one_edit();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(batch) = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected a batch");
        };
        let last = app.update(Msg::Replied {
            sent: batch[0].clone(),
            result: Err(fixtures::invalid_config()),
        });
        assert!(matches!(last, Effect::None), "got {last:?}");
        let notice = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(
            notice.contains("no such directory"),
            "the field's own refusal is not lost: {notice:?}"
        );
        assert!(
            notice.contains("restart did not go out"),
            "and it says the action did not go out: {notice:?}"
        );
    }

    /// `L` takes the same wait-for-the-writes path as `R`, and must reach
    /// the shepherd as its own verb rather than `Restart`'s: no test drove
    /// this key from inside the dialog before this task.
    #[test]
    fn l_sends_reload_and_not_restart_once_the_writes_land() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(batch) = app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)))
        else {
            panic!("expected a batch");
        };
        let first = app.update(Msg::Replied {
            sent: batch[0].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "cwd".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(matches!(first, Effect::None), "not yet: {first:?}");
        let second = app.update(Msg::Replied {
            sent: batch[1].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "max_memory".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(
                second,
                Effect::Send(Sent::Action {
                    verb: ActionVerb::Reload,
                    ..
                })
            ),
            "got {second:?}, expected Reload and not Restart"
        );
    }

    /// `c` never holds a verb, so its writes' own replies are unaffected by
    /// anything this task adds: whatever the per-field reply handler
    /// returns stands, and it is never `Sent::Action`.
    #[test]
    fn c_writes_and_sends_no_action() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Continue));
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
        assert!(app.config_pane().is_none(), "the pane closed");
        assert!(app.held_action().is_none(), "`c` never holds a verb");
        let Effect::SendAll(batch) = effect else {
            unreachable!()
        };
        let done = app.update(Msg::Replied {
            sent: batch[batch.len() - 1].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "max_memory".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            !matches!(done, Effect::Send(Sent::Action { .. })),
            "no action follows a c: {done:?}"
        );
    }

    /// `arm` refuses on the same pair `answer_close` does. A verb the
    /// dialog is holding has not gone out, so `self.action` is still empty
    /// and only `self.held` says the operator is mid-answer; a dashboard
    /// `R` armed past it would be overwritten by `send_held_action` on the
    /// reply that releases the held one.
    #[test]
    fn the_dashboard_refuses_a_verb_while_the_dialog_still_holds_one() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert!(app.held_action().is_some(), "the verb is held");

        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        assert!(matches!(effect, Effect::None), "got {effect:?}");
        assert!(app.action().is_none(), "nothing armed past the held verb");
        assert!(
            app.notice()
                .is_some_and(|n| n.to_string().contains("already in flight")),
            "got {:?}",
            app.notice()
        );
    }

    /// Nothing goes to a shepherd that is gone.
    ///
    /// `the_apply_menu_refuses_on_a_dead_link_like_every_other_action`
    /// pinned this for the menu this dialog replaced, and went with it.
    /// The refusal went too: `confirm_refusal` gates `arm` and
    /// `arm_sheep_pane`, and the dialog answers through `answer_close`,
    /// which reached neither. So `R` on a dead link sent a restart with
    /// nothing on screen saying it never left.
    ///
    /// The writes still go out, the same as when a verb is refused for
    /// being second: they are the operator's own work and the batch
    /// reports its own failure. What must not go is the action.
    #[test]
    fn the_dialog_refuses_a_verb_on_a_dead_link_like_every_other_action() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Retrying { attempt: 3 });
        app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        assert!(
            !matches!(effect, Effect::Send(Sent::Action { .. })),
            "nothing goes to a shepherd that is gone: {effect:?}"
        );
        assert!(app.held_action().is_none(), "and no verb waits to go later");
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.contains("attempt 3"), "{said}");
    }

    /// The sibling door. `arm_sheep_pane` keeps its own copy of the
    /// refusal ladder, so a held verb has to be refused there as well or
    /// the fix reaches one of the two doors that arm from a keypress.
    #[test]
    fn the_sheep_pane_refuses_a_verb_while_the_dialog_still_holds_one() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert!(app.held_action().is_some(), "the verb is held");

        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.sheep_pane().is_some(), "the sheep pane is open");
        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        assert!(matches!(effect, Effect::None), "got {effect:?}");
        assert!(app.action().is_none(), "nothing armed past the held verb");
        assert_eq!(
            app.notice().map(|n| n.to_string()).as_deref(),
            Some("one action is already in flight")
        );
    }

    /// A reply that never comes cannot strand the verb.
    #[test]
    fn a_held_verb_expires() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let later = app.now() + CONFIRM_EXPIRY;
        app.update(Msg::Tick { now: later });
        assert!(app.held_action().is_none(), "it expired");
    }

    /// `TextAbandon` drops the env editor and leaves the pane exactly as
    /// `Escape` leaves the field editor: open, on the same row, nothing
    /// filed. `Escape` from there asks the dialog's question, and a second
    /// `Escape` answers it by closing the dialog, not the pane.
    #[test]
    fn abandoning_the_env_editor_leaves_the_pane_then_escape_asks_the_dialog() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::AddEnv));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(app.mode(), InputMode::Text);
        let _ = app.update(Msg::Key(KeyPress::TextAbandon));
        assert_eq!(app.mode(), InputMode::Normal);
        assert!(app.config_pane().is_some(), "the pane stays open");
        assert!(app.config_pane().unwrap().edits().is_empty());
        assert!(app.close_dialog().is_none());
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_some());
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_some(), "the second esc keeps the pane");
        assert!(app.close_dialog().is_none());
    }

    /// Nothing holds the old config, so there is nothing to respawn, and `R`
    /// on a stopped sheep would start it.
    #[test]
    fn no_dialog_for_a_stopped_sheep() {
        let mut app = fixtures::app_in_sheep_pane_on_a_stopped_sheep();
        fixtures::file_edit(&mut app, "cwd", "/srv/app");
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.close_dialog().is_none(),
            "a stopped sheep is not asked about"
        );
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
    }

    #[test]
    fn no_dialog_for_a_dog() {
        let mut app = fixtures::app_in_dog_pane();
        fixtures::file_edit(&mut app, "poll", "45s");
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none());
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
    }

    #[test]
    fn read_only_is_never_asked() {
        let mut app = fixtures::app_in_sheep_pane_read_only();
        app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none());
        assert!(app.config_pane().is_none(), "read-only still closes");
    }

    #[test]
    fn esc_from_the_dialog_writes_nothing_and_keeps_the_pane() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none(), "the dialog closed");
        assert!(app.config_pane().is_some(), "the pane did not");
        assert!(matches!(effect, Effect::None), "got {effect:?}");
    }

    /// The edits survive it: `esc` is `keep editing`, not `discard`.
    #[test]
    fn esc_from_the_dialog_leaves_the_edits_filed() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        // Raise the dialog, dismiss it, raise it again. The fixture parks
        // nothing, so the third `esc` can only find a dialog if an edit
        // survived the second. That is weaker than the claim below, which
        // is why the set itself is read rather than inferred.
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.close_dialog().is_some(),
            "the third esc found edits still filed"
        );
        let edits = app.config_pane().expect("the pane is still open").edits();
        assert_eq!(edits.len(), 2, "both edits survived, not merely one");
        for key in ["cwd", "max_memory"] {
            assert!(
                edits.get(&EditKey::Field(key.to_owned())).is_some(),
                "{key} is still filed"
            );
        }
    }

    /// An expiry is an `esc`, never a `c`. A dialog nobody answered is not
    /// consent to write.
    #[test]
    fn the_dialog_expires_without_writing() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let later = app.now() + CONFIRM_EXPIRY;
        let effect = app.update(Msg::Tick { now: later });
        assert!(app.close_dialog().is_none(), "it expired");
        assert!(app.config_pane().is_some(), "the pane is still open");
        assert!(matches!(effect, Effect::None), "got {effect:?}");
    }

    /// A dialog nobody answered is not consent to write, and `L` an hour
    /// later must not reload a sheep.
    #[test]
    fn the_close_dialog_expires_like_every_other_armed_thing() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_some(), "the dialog opened");

        let later = Instant::now() + CONFIRM_EXPIRY;
        let _ = app.update(Msg::Tick { now: later });
        assert!(app.close_dialog().is_none(), "it did not expire");

        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        assert_eq!(effect, Effect::None, "a stale L reloads nothing");
    }
}
