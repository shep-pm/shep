//! The dialog a pane raises on the way out, and the verb it holds until every
//! write lands.

use super::*;

/// The question `esc` asks when a pane closes over changes the running
/// child has not taken.
///
/// Raised by [`App::close_offer`], answered by [`App::on_close_dialog_key`],
/// and expired by the same [`CONFIRM_EXPIRY`] every other prompt gets.
///
/// Everything is carried rather than recomputed: the unsent names are
/// taken from the pane when the dialog goes up, and `parked` is the
/// shepherd's own answer from the last fetch.
///
/// `Debug` is derived (IR-41): field names the operator is already
/// reading on screen, two counts, an instance count, two durations
/// rendered for display, a reload mode, a name, a status, a pid and a
/// time. No value of any field, and no env value, since the wire never
/// sends one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseDialog {
    unsent: Vec<String>,
    parked: usize,
    live: usize,
    reload: ReloadKind,
    instances: u32,
    kill_timeout: String,
    graceful_timeout: String,
    name: String,
    status: ProcStatus,
    pid: Option<u32>,
    at: Instant,
}

impl CloseDialog {
    /// One, over `unsent` filed edits and `parked` fields, reading
    /// everything else off `pane`: which reload it would get, its own
    /// `kill_timeout` and `graceful_timeout`, its name, and how many other
    /// filed edits (`live`) the running sheep already takes without one.
    ///
    /// `status` and `pid` are the one pair a [`ConfigPane`] cannot answer,
    /// since only the flock map carries them, and the heading names both:
    /// an operator answering a question that restarts a process should not
    /// have to read the dimmed pane behind the box to learn which one.
    #[must_use]
    pub(in crate::lookout) fn new(
        unsent: Vec<String>,
        parked: usize,
        pane: &ConfigPane,
        status: ProcStatus,
        pid: Option<u32>,
        at: Instant,
    ) -> Self {
        Self {
            unsent,
            parked,
            live: pane.live_edit_count(),
            reload: pane.reload_kind(),
            // `value` renders the pane's own map as JSON and `instances`
            // is a plain `u32` every `AppConfig` carries, so the parse
            // cannot fail for a sheep. The only target without the field
            // is a dog, and `close_offer` refuses a dog before it builds
            // one of these. A fallback that ever fired would understate
            // how many processes the reload row is describing, which is
            // the one number that row exists to give.
            instances: pane.value("instances").parse().unwrap_or(1),
            kill_timeout: pane.display_value("kill_timeout"),
            graceful_timeout: pane.display_value("graceful_timeout"),
            name: pane.target().name().to_owned(),
            status,
            pid,
            at,
        }
    }

    /// When it opened. A dialog that outlives `CONFIRM_EXPIRY` is dropped
    /// by the tick, so a later keypress cannot answer a question nobody is
    /// still looking at.
    #[must_use]
    pub const fn at(&self) -> Instant {
        self.at
    }

    /// How many filed edits a respawn is what applies. The heading's own
    /// number; [`Self::unsent_fields`] is the sentence underneath it.
    #[must_use]
    pub fn unsent(&self) -> usize {
        self.unsent.len()
    }

    /// Those edits' own field names, in the set's key order.
    #[must_use]
    pub fn unsent_fields(&self) -> &[String] {
        &self.unsent
    }

    /// How many fields the shepherd already parked, from the last fetch.
    #[must_use]
    pub const fn parked(&self) -> usize {
        self.parked
    }

    /// How many other filed edits the running sheep already takes without
    /// a respawn. What "everything else you changed is already live" draws
    /// on: zero when the whole filed set needs one.
    #[must_use]
    pub const fn live(&self) -> usize {
        self.live
    }

    /// Which reload this sheep would get, so `L`'s row can name its cost.
    #[must_use]
    pub const fn reload(&self) -> ReloadKind {
        self.reload
    }

    /// How many instances a reload or restart would reach.
    #[must_use]
    pub const fn instances(&self) -> u32 {
        self.instances
    }

    /// The sheep's own `kill_timeout`, resolved for display: what `R`'s row
    /// names as the stop's own grace before SIGKILL.
    #[must_use]
    pub fn kill_timeout(&self) -> &str {
        &self.kill_timeout
    }

    /// The sheep's own `graceful_timeout`, resolved the same way: the drain
    /// window a serial reload gets.
    #[must_use]
    pub fn graceful_timeout(&self) -> &str {
        &self.graceful_timeout
    }

    /// The sheep this dialog is asking about.
    ///
    /// Unread by this frame's own render: `close_dialog_lines` names no
    /// target, only what changed. [`App::answer_close`] reads it, since a
    /// held verb has to name the sheep after the dialog itself is gone.
    #[must_use]
    pub fn target_name(&self) -> &str {
        &self.name
    }

    /// What the flock reported this sheep doing when the dialog went up.
    #[must_use]
    pub const fn status(&self) -> ProcStatus {
        self.status
    }

    /// The OS pid this sheep runs under, when there is exactly one running
    /// instance to name. [`None`] for a sheep the shepherd runs several of,
    /// where no single pid is the answer.
    #[must_use]
    pub const fn pid(&self) -> Option<u32> {
        self.pid
    }
}

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
pub(super) struct HeldAction {
    verb: ActionVerb,
    name: String,
    tickets: HashSet<u64>,
    landed: bool,
    pub(super) at: Instant,
}

impl App {
    /// The question this pane's `Escape` asks, or [`None`] when it just
    /// writes and leaves.
    ///
    /// Silent behind a closed gate, where every key it offers would be
    /// refused; silent on a dog, which has no apply table to classify an
    /// edit with; and silent on a sheep that is not running, where nothing
    /// holds the old config and `R` would start it rather than replace it.
    pub(super) fn close_offer(&self) -> Option<CloseDialog> {
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

    /// Every instance of `name` the flock reports running.
    ///
    /// `Stopping` does not count: the drainee and its replacement hold the
    /// same slot ([`crate::lookout::pane::ReloadKind`]'s own reasoning), and
    /// a sheep with every instance stopping holds no config a respawn would
    /// replace.
    fn running_instances<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Row> {
        self.flock.values().filter(move |row| {
            row.info.name == name
                && matches!(
                    row.info.status,
                    ProcStatus::Online | ProcStatus::Starting | ProcStatus::WaitingRestart
                )
        })
    }

    /// What the close dialog's heading says about the sheep itself: the
    /// status of the first running instance, and its pid when it is the
    /// only one. Several instances name no single pid, so the heading
    /// names none.
    fn running_state(&self, name: &str) -> Option<(ProcStatus, Option<u32>)> {
        let mut running = self.running_instances(name);
        let first = running.next()?;
        let alone = running.next().is_none();
        Some((first.info.status, if alone { first.info.pid } else { None }))
    }

    /// The dialog's own keymap: `R` and `L` write and hold their verb until
    /// the writes are answered ([`Self::answer_close`]), `c` writes and
    /// holds nothing. `Escape` closes the dialog and not the pane, which is
    /// the difference from the menu this replaces: `esc` here means keep
    /// editing, so the filed set stays filed and nothing is written.
    pub(super) fn on_close_dialog_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::Action(verb @ (ActionVerb::Reload | ActionVerb::Restart)) => {
                self.answer_close(Some(verb))
            }
            KeyPress::Continue => self.answer_close(None),
            KeyPress::Escape => {
                self.close_dialog = None;
                Effect::None
            }
            // `Help` among them: the dialog owns the keyboard until it is
            // answered, and the keymap overlay is not an exception to that.
            KeyPress::Action(ActionVerb::Stop)
            | KeyPress::SelectUp
            | KeyPress::SelectDown
            | KeyPress::SelectFirst
            | KeyPress::SelectLast
            | KeyPress::Refresh
            | KeyPress::Confirm
            | KeyPress::Edit
            | KeyPress::Cycle
            | KeyPress::Help
            | KeyPress::Settings
            | KeyPress::FilterStart
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            | KeyPress::FoldView
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete
            | KeyPress::Collapse => Effect::None,
            KeyPress::StreamCycle
            | KeyPress::LevelCycle
            | KeyPress::PageDown
            | KeyPress::PageUp
            | KeyPress::FollowToggle
            | KeyPress::WrapToggle
            | KeyPress::MatchNext
            | KeyPress::MatchPrev
            | KeyPress::Bleats
            // `NextGroup`/`Group`/`Undo` belong to the config pane: no
            // other screen has groups to walk or a filed edit set to undo.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo => Effect::None,
        }
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
    fn answer_close(&mut self, verb: Option<ActionVerb>) -> Effect {
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
    pub(super) fn resolve_held_write(&mut self, ticket: u64, landed: bool) -> Option<Effect> {
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
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::edits::EditKey;
    use crate::lookout::view::fixtures;

    #[test]
    fn escape_closes_a_pane_with_nothing_parked() {
        let mut app = fixtures::app_in_sheep_pane_with_nothing_parked();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.config_pane().is_none(),
            "no dialog when nothing is parked"
        );
        assert!(app.close_dialog().is_none());
    }

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

    /// A parked field alone (nothing filed to write) has nothing for `R` to
    /// wait on, so the restart goes at once rather than holding for a batch
    /// that is empty.
    #[test]
    fn r_over_a_parked_field_alone_sends_the_restart_at_once() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert!(
            matches!(
                effect,
                Effect::Send(Sent::Action {
                    verb: ActionVerb::Restart,
                    ..
                })
            ),
            "got {effect:?}"
        );
        assert!(app.config_pane().is_none());
        assert!(app.close_dialog().is_none());
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

    /// Several instances name no one pid, so the heading names none. The
    /// `api` row alongside is the control: it proves the fixture really
    /// carries pids, without which the `web` assertion would pass on a
    /// flock that had none to offer.
    #[test]
    fn a_sheep_with_two_running_instances_offers_no_single_pid() {
        let mut app = allowed();
        let at = app.now();
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(1, "web", ProcStatus::Online),
                sheep(4, "web", ProcStatus::Online),
                sheep(2, "api", ProcStatus::Online),
            ],
            at,
        });
        assert_eq!(app.running_state("web"), Some((ProcStatus::Online, None)));
        assert_eq!(
            app.running_state("api"),
            Some((ProcStatus::Online, Some(1002))),
            "one instance still names its pid"
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

    /// The gap that let a second held verb through: two `R` answers in a
    /// row, with the first batch's replies still outstanding when the
    /// second is asked. The bug was `self.held` being overwritten by the
    /// second session, so the first session's own replies (an unrelated
    /// batch, and the wrong count) resolved the second session's verb
    /// early. Proof of the fix: the first write's reply alone must not
    /// finish anything (the held count is still the first batch's own two,
    /// not the second batch's one), and the second session's `R` never
    /// gets an action at all, in flight or otherwise.
    #[test]
    fn a_second_r_never_holds_over_the_first() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(first_batch) =
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected the first batch");
        };
        assert_eq!(first_batch.len(), 2);

        // Reopen the pane, file another edit, and answer the dialog again
        // while the first batch's replies are still outstanding.
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        fixtures::file_edit(&mut app, "cwd", "/srv/second");
        app.update(Msg::Key(KeyPress::Escape));
        let second_answer = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert!(
            matches!(second_answer, Effect::SendAll(ref batch) if batch.len() == 1),
            "the edit still goes out: {second_answer:?}"
        );
        assert!(
            app.notice()
                .is_some_and(|n| n.to_string().contains("already in flight")),
            "got {:?}",
            app.notice()
        );

        // The first write's own reply must not finish anything: the held
        // count is still the first batch's own two, not the second
        // batch's one that a bug would have overwritten it with.
        let first_reply = app.update(Msg::Replied {
            sent: first_batch[0].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "cwd".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(first_reply, Effect::None),
            "fired early: {first_reply:?}"
        );

        // The first batch's own second reply completes it, correctly: this
        // is the first session's own action, not the second's.
        let second_reply = app.update(Msg::Replied {
            sent: first_batch[1].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "max_memory".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(
                second_reply,
                Effect::Send(Sent::Action {
                    verb: ActionVerb::Restart,
                    ..
                })
            ),
            "got {second_reply:?}"
        );
    }

    /// The reopened door into the same bug: the refused session's own
    /// write still goes out (per `answer_close`'s own doc, edits are never
    /// held back), and its reply must not count toward the held session's
    /// batch just because it is the only thing held at the time. A bare
    /// counter cannot tell the two apart; a ticket can.
    #[test]
    fn a_refused_sessions_reply_does_not_count_toward_the_held_batch() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(first_batch) =
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected the first batch");
        };
        assert_eq!(first_batch.len(), 2, "session A holds on two writes");

        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        fixtures::file_edit(&mut app, "cwd", "/srv/second");
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(second_batch) =
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected session B's write to still go out");
        };
        assert_eq!(
            second_batch.len(),
            1,
            "session B holds on nothing, but writes"
        );

        // Session B's own reply lands first. It must be entirely inert:
        // not held, so it cannot bring session A's batch any closer to
        // done.
        let after_b = app.update(Msg::Replied {
            sent: second_batch[0].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "cwd".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            !matches!(after_b, Effect::Send(Sent::Action { .. })),
            "session B holds no verb to send: {after_b:?}"
        );

        // Only one of session A's own two writes has answered. Nothing
        // must have gone out yet, from either session.
        let after_a_first = app.update(Msg::Replied {
            sent: first_batch[0].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "cwd".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(after_a_first, Effect::None),
            "session A's own batch still has one outstanding: {after_a_first:?}"
        );

        // Session A's second and last write answers. Now, and only now,
        // its restart goes.
        let after_a_second = app.update(Msg::Replied {
            sent: first_batch[1].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "max_memory".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(
                after_a_second,
                Effect::Send(Sent::Action {
                    verb: ActionVerb::Restart,
                    ..
                })
            ),
            "got {after_a_second:?}"
        );
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

    /// `esc` leaves the config pane on the first press: the blurb draws
    /// unconditionally now, so nothing else waits for a second press.
    ///
    /// 89 columns, one under `panel_width`'s floor, is what makes the
    /// blurb the only thing drawing the cursor's own help at this width;
    /// at 160 the panel draws it instead and the assertion below would
    /// pass regardless.
    #[test]
    fn esc_leaves_the_pane_on_one_press_with_a_blurb_showing() {
        let mut app = fixtures::app_in_sheep_pane_with_nothing_parked();
        let pane = app.config_pane().expect("the pane is open");
        let Some(PaneRow::Field(index)) = pane.cursor() else {
            panic!("the cursor is not on a field");
        };
        // The longest word, not the whole help string. The blurb wraps to
        // BLURB_WRAP, and `render_all` joins rows with newlines, so a help
        // text over that budget is present on screen and absent from this
        // assertion.
        let anchor = pane.fields().fields()[index]
            .help
            .split_whitespace()
            .max_by_key(|word| word.len())
            .expect("the field help is empty")
            .to_owned();
        let lines = crate::lookout::view::pane::pane_lines(pane, fixtures::plain(), 89, 40);
        assert!(
            fixtures::render_all(&lines).contains(&anchor),
            "the blurb is not on screen before esc"
        );
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.config_pane().is_none(),
            "one esc did not leave the pane"
        );
    }

    /// The case the old menu missed: an edit made in this pane, on a sheep
    /// with nothing parked before it opened.
    #[test]
    fn esc_with_a_respawn_edit_asks_before_it_writes() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_some(), "the dialog is up");
        assert!(
            matches!(effect, Effect::None),
            "nothing is written until the dialog is answered, got {effect:?}"
        );
        assert!(app.config_pane().is_some(), "the pane is still open");
    }

    #[test]
    fn esc_with_only_live_edits_writes_and_closes_with_no_dialog() {
        // `app_in_sheep_pane` parks `kill_signal` unconditionally
        // (`sheep_config_view`'s own default), which alone would raise the
        // dialog regardless of what is filed. This test is about the
        // unsent half alone, so it needs a pane starting with nothing
        // parked.
        let mut app = fixtures::app_in_sheep_pane_with_nothing_parked();
        // `max_restarts` is `ApplyGroup::Live`.
        fixtures::file_edit(&mut app, "max_restarts", "9");
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none());
        assert!(app.config_pane().is_none(), "the pane closed");
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
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

    /// A `Stopping` sheep's drainee and its replacement hold the same
    /// instance slot, so it is excluded from `App::sheep_is_running`
    /// alongside `Stopped`: no live config for a respawn to replace, and
    /// `R` would race the reload already under way rather than restart
    /// anything.
    #[test]
    fn no_dialog_for_a_sheep_mid_drain() {
        let mut app = fixtures::app_in_sheep_pane_on_a_draining_sheep();
        fixtures::file_edit(&mut app, "cwd", "/srv/app");
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.close_dialog().is_none(),
            "a sheep mid-drain is not asked about"
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

    /// The parked half: no edit of the operator's own, fields the shepherd
    /// is already holding.
    #[test]
    fn esc_over_parked_fields_alone_still_asks() {
        // `app_in_sheep_pane` is read-only by default, and a closed gate
        // is a separate reason for no dialog (`read_only_is_never_asked`).
        // This test is about the parked half alone, so it needs the gate
        // open and nothing filed.
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        let dialog = app.close_dialog().expect("one field is parked");
        assert_eq!(dialog.unsent(), 0);
        assert_eq!(dialog.parked(), 1);
        assert!(matches!(effect, Effect::None), "got {effect:?}");
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
