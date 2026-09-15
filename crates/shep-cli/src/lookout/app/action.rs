//! One armed action at a time: what a verb targets, what refuses it, and when
//! the confirm expires.

use super::*;

/// What an action key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionVerb {
    /// `x`. Stops the sheep; it stays registered.
    Stop,
    /// `R`, on shift because `r` is refresh.
    Restart,
    /// `L`, on shift for symmetry with `R`.
    Reload,
}

impl ActionVerb {
    /// The word the prompt and every outcome sentence begin with.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Reload => "reload",
        }
    }
}

/// Whether an action has been sent yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    /// Armed, waiting for the operator's Enter. Nothing has gone out.
    Armed,
    /// Sent, waiting for the shepherd.
    Sent,
}

/// The one action this dashboard is in the middle of.
///
/// The target is captured at arm time and never re-read from the selection: a
/// snapshot can land between the keypress and the Enter.
///
/// One field on [`App`] rather than two `Option`s, so "armed" and "in flight"
/// cannot both be true.
#[derive(Debug, Clone)]
pub(super) struct Action {
    pub(super) verb: ActionVerb,
    pub(super) target: RowKey,
    pub(super) name: String,
    /// How many processes [`Self::target`] reaches, captured at arm time: 1 for
    /// a sheep, the group's own size for a [`RowKey::Group`], and the fold's
    /// membership for a [`RowKey::Fold`].
    pub(super) count: usize,
    /// When it was armed. Only an armed action expires.
    pub(super) at: Instant,
    pub(super) stage: Stage,
}

/// What the status bar needs to know about the action in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionState<'a> {
    /// Which verb.
    pub verb: ActionVerb,
    /// The pinned target.
    pub target: &'a RowKey,
    /// The pinned target's name, as it was when the key was pressed.
    pub name: &'a str,
    /// How many processes [`Self::target`] reaches.
    pub count: usize,
    /// False while it is a question, true once it has gone out.
    pub sent: bool,
}

/// How long an armed confirm waits for its Enter.
///
/// Ten seconds: a prompt left armed while the operator walks away is the same
/// fat finger by a slower route. Rides `Msg::Tick`, so it needs no timer.
pub const CONFIRM_EXPIRY: Duration = Duration::from_secs(10);

/// The sentence `r` and the action keys both give when the link is gone.
pub(super) const LINK_GONE: &str = "the shepherd is gone: nothing left to ask";

/// The redial sentence, for the status bar and for a refused action key.
///
/// Both render on one frame, so they must agree exactly.
pub(in crate::lookout) fn retrying_sentence(attempt: u32) -> String {
    format!("the shepherd stopped answering: reconnecting (attempt {attempt})")
}

/// The sentence every closed-gate refusal gives, dashboard and settings alike.
pub(super) const READ_ONLY_REFUSAL: &str = "read-only: from --read-only or lookout.allow_control";

/// The prefix every action's notice shares: the verb, and the target. A single
/// sheep takes the `(id N)` form; a group names the app, having no one id.
pub(super) fn target_prefix(verb: ActionVerb, target: &RowKey, name: &str) -> String {
    match target {
        RowKey::Sheep(id) => format!("{} {name} (id {id})", verb.label()),
        RowKey::Group(_) => format!("{} all instances of {name}", verb.label()),
        RowKey::Fold(_) => format!("{} all sheep in fold {name}", verb.label()),
        RowKey::Section(_) => unreachable!("a header is never an action target"),
    }
}

/// What the bar says once the shepherd has answered. `Response::Reloading` is
/// an acceptance, not a result: the swaps arrive later on the bus.
const fn outcome(verb: ActionVerb) -> &'static str {
    match verb {
        ActionVerb::Stop => "the shepherd stopped it",
        ActionVerb::Restart => "the shepherd restarted it",
        ActionVerb::Reload => "accepted, the swaps report themselves as they happen",
    }
}

impl App {
    /// One action's answer: the shepherd's rows upserted, and one sentence.
    /// Nothing provisional is invented; all three replies carry the rows.
    pub(super) fn on_action_reply(
        &mut self,
        verb: ActionVerb,
        target: RowKey,
        name: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        self.action = None;
        let prefix = target_prefix(verb, &target, name);
        // Each verb accepts its own reply and no other: a `Stopped` answering
        // a `Restart` carries rows and would upsert happily.
        let mut refusal: Option<String> = None;
        let rows = match result {
            Ok(Response::Stopped(rows)) if verb == ActionVerb::Stop => rows,
            // `SelectorSpec::Fold` names every app in a fold, so this is the
            // multi-app walk that fills `refused`. Dropping it would tell an
            // operator the whole fold restarted when some of it did not.
            Ok(Response::Restarted { accepted, refused }) if verb == ActionVerb::Restart => {
                refusal = refusal_sentence(&refused);
                accepted
            }
            Ok(Response::Reloading { accepted, refused }) if verb == ActionVerb::Reload => {
                refusal = refusal_sentence(&refused);
                accepted
            }
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{prefix}: the shepherd answered something this lookout does not understand"
                    ),
                    grave: true,
                });
                return Effect::None;
            }
            // The daemon's own message: `RequestError`'s `Display` would put a
            // Rust identifier on an operator's screen.
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: {}", err.message),
                    grave: true,
                });
                return Effect::None;
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: {other}"),
                    grave: true,
                });
                return Effect::None;
            }
        };
        let anchor = self.now;
        let was_empty = self.flock.is_empty();
        for info in rows {
            self.flock.insert(info.id, Row { info, anchor });
        }
        // A partial walk is not a success sentence. `grave` follows, so a
        // fold that half refused reads as a problem rather than as a done
        // thing an operator scrolls past.
        self.notice = Some(match &refusal {
            Some(sentence) => Notice {
                text: format!("{prefix}: {}, but {sentence}", outcome(verb)),
                grave: true,
            },
            None => Notice {
                text: format!("{prefix}: {}", outcome(verb)),
                grave: false,
            },
        });
        if was_empty && self.reseat(None) {
            return Effect::RefreshSelected;
        }
        Effect::None
    }

    /// The row key `name` reaches, and how many processes that is.
    ///
    /// A name rather than [`Self::selected`]: a pane is opened per name and
    /// survives the table underneath it changing. [`None`] when the flock
    /// has no such sheep left.
    pub(super) fn flock_target(&self, name: &str) -> Option<(RowKey, usize)> {
        let ids: Vec<u32> = self
            .flock
            .values()
            .filter(|row| row.info.name == name)
            .map(|row| row.info.id)
            .collect();
        match ids.as_slice() {
            [] => None,
            [id] => Some((RowKey::Sheep(*id), 1)),
            _ => Some((RowKey::Group(name.to_owned()), ids.len())),
        }
    }

    /// Why the shepherd cannot be sent to right now, if it cannot.
    ///
    /// Shared by the dashboard's action keys and the pane's apply menu: a
    /// dead link refuses the same way whichever door an operator used.
    pub(super) fn link_refusal(&self) -> Option<String> {
        match self.link {
            // Not `LINK_GONE`: the ladder is still running.
            Link::Retrying { attempt } => Some(retrying_sentence(attempt)),
            // The ladder is exhausted, so the shepherd really is gone.
            Link::Lost { .. } => Some(LINK_GONE.to_string()),
            _ => None,
        }
    }

    /// The refusal ladder shared by [`Self::arm`] and [`Self::arm_sheep_pane`]:
    /// the gate, the link. Neither caller's own target-specific refusal
    /// (nothing selected, one action already in flight, the pane's pinned
    /// sheep is gone) lives here, since the two callers order those three
    /// differently: `arm` asks "nothing selected" before "one already in
    /// flight", `arm_sheep_pane` cannot ask the first (the pane would not be
    /// open without a sheep) so only asks the second. Folding "in flight"
    /// in here once put it ahead of `arm`'s "nothing selected" for every
    /// caller, which is the bug this comment now exists to keep out.
    pub(super) fn confirm_refusal(&self) -> Option<String> {
        if self.control == Control::ReadOnly {
            Some(READ_ONLY_REFUSAL.to_string())
        } else {
            self.link_refusal()
        }
    }

    /// Arms a confirm, or refuses and says why.
    ///
    /// Every refusal happens here rather than at confirm time, so an operator
    /// never answers a question that was never going to be honoured. The
    /// ladder is [`Self::confirm_refusal`]'s own gate and link, then nothing
    /// selected, then one action already in flight, but this method checks
    /// nothing selected first, since a keypress with no target asked a
    /// question that was never about the in-flight action at all.
    pub(super) fn arm(&mut self, verb: ActionVerb) -> Effect {
        if let Some(text) = self.confirm_refusal() {
            self.notice = Some(Notice { text, grave: true });
            return Effect::None;
        }
        let Some(key) = self.selected.clone() else {
            self.notice = Some(Notice {
                text: "no sheep is selected".to_string(),
                grave: true,
            });
            return Effect::None;
        };
        // `self.held` as well as `self.action`, the same pair
        // `answer_close` refuses on and in the same sentence: a verb the
        // close dialog is holding until its writes land has not gone out
        // yet, so `self.action` is still empty, and arming a second one
        // here would have `send_held_action` overwrite it on the reply
        // that releases it. [`Self::arm_sheep_pane`] is the only other
        // door that arms from a keypress, and it refuses on the same pair.
        if self.action.is_some() || self.held.is_some() {
            self.notice = Some(Notice {
                text: "one action is already in flight".to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let (target, name, count) = match &key {
            RowKey::Sheep(id) => {
                let row = self
                    .flock
                    .get(id)
                    .expect("a selected sheep is in the flock");
                (RowKey::Sheep(*id), row.info.name.clone(), 1)
            }
            RowKey::Group(group_name) => {
                let count = self
                    .flock
                    .values()
                    .filter(|row| &row.info.name == group_name)
                    .count();
                (RowKey::Group(group_name.clone()), group_name.clone(), count)
            }
            RowKey::Fold(fold_name) => {
                let count = self
                    .flock
                    .values()
                    .filter(|row| row.info.fold.as_deref() == Some(fold_name.as_str()))
                    .count();
                (RowKey::Fold(fold_name.clone()), fold_name.clone(), count)
            }
            RowKey::Section(_) => unreachable!("a header is never selectable"),
        };
        self.action = Some(Action {
            verb,
            target,
            name,
            count,
            at: self.now,
            stage: Stage::Armed,
        });
        Effect::None
    }

    /// The operator's Enter. Sends, or refuses because the target left.
    pub(super) fn confirm(&mut self) -> Effect {
        let Some(action) = self.action.take() else {
            return Effect::None;
        };
        // The whole flock, not the visible set: a filter typed after arming
        // hides a sheep, it does not remove it.
        if !self.target_present(&action.target) {
            self.notice = Some(Notice {
                text: format!(
                    "{}: it is no longer in the flock",
                    target_prefix(action.verb, &action.target, &action.name)
                ),
                grave: true,
            });
            return Effect::None;
        }
        let sent = Sent::Action {
            verb: action.verb,
            target: action.target.clone(),
            name: action.name.clone(),
        };
        self.action = Some(Action {
            stage: Stage::Sent,
            ..action
        });
        Effect::Send(sent)
    }

    /// Whether `target` still has at least one process in the flock: a
    /// single sheep by id, or a group by whether any instance of its name
    /// remains.
    fn target_present(&self, target: &RowKey) -> bool {
        match target {
            RowKey::Sheep(id) => self.flock.contains_key(id),
            RowKey::Group(name) => self.flock.values().any(|row| &row.info.name == name),
            RowKey::Fold(name) => self
                .flock
                .values()
                .any(|row| row.info.fold.as_deref() == Some(name.as_str())),
            RowKey::Section(_) => unreachable!("a header is never an action target"),
        }
    }

    /// Takes an armed prompt off the screen once its target is gone, rather
    /// than leaving a question about nothing. An action already in flight
    /// keeps its line.
    pub(super) fn forget_missing_target(&mut self) {
        let gone = self.action.as_ref().is_some_and(|action| {
            action.stage == Stage::Armed && !self.target_present(&action.target)
        });
        if gone {
            self.action = None;
        }
    }

    /// Takes an armed prompt off the screen when the link stops being live.
    ///
    /// On a frozen dashboard it would never expire either, since `now` stops
    /// advancing and the expiry check rides it. An action already sent keeps
    /// its line: `run_connected` answers it with an `Err` before its loop ends.
    pub(super) fn disarm_on_link_change(&mut self) {
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.stage == Stage::Armed)
        {
            self.action = None;
        }
    }

    /// The action in progress, for the status bar.
    #[must_use]
    pub fn action(&self) -> Option<ActionState<'_>> {
        let action = self.action.as_ref()?;
        Some(ActionState {
            verb: action.verb,
            target: &action.target,
            name: &action.name,
            count: action.count,
            sent: action.stage == Stage::Sent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::view::fixtures;
    use shep_core::protocol::ProcessEventKind;
    use shep_core::protocol::RpcError;
    use shep_core::protocol::RpcErrorCode;

    #[test]
    fn arming_a_group_action_refuses_when_read_only() {
        let mut app = allowed_with_instances();
        app.set_control_for_tests(Control::ReadOnly);
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.action().is_none());
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("read-only: from --read-only or lookout.allow_control")
        );
    }

    #[test]
    fn arming_a_group_action_refuses_while_the_link_is_not_live() {
        for link in [
            Msg::Retrying { attempt: 2 },
            Msg::Frozen {
                at_local: "2026-08-16 09:00:00".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            },
        ] {
            let mut app = allowed_with_instances();
            app.select(RowKey::Group("web".to_string()));
            app.update(link);
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_none());
            assert!(app.notice().is_some_and(Notice::is_grave));
        }
    }

    #[test]
    fn arming_a_group_action_refuses_while_one_is_already_in_flight() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("one action is already in flight")
        );
        let action = app.action().expect("the first one is untouched");
        assert_eq!(action.verb, ActionVerb::Stop);
        assert!(action.sent);
    }

    #[test]
    fn an_action_key_arms_a_confirm_and_sends_nothing() {
        let mut app = allowed();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop))),
            Effect::None
        );
        let armed = app.action().expect("armed");
        assert_eq!(armed.verb, ActionVerb::Stop);
        assert_eq!(armed.target, &RowKey::Sheep(1));
        assert_eq!(armed.name, "web");
        assert!(!armed.sent, "nothing has gone out");
    }

    #[test]
    fn only_enter_confirms_and_every_other_key_cancels() {
        for key in [
            KeyPress::SelectDown,
            KeyPress::SelectUp,
            KeyPress::SelectFirst,
            KeyPress::Refresh,
            KeyPress::Escape,
            KeyPress::FilterStart,
            KeyPress::Action(ActionVerb::Stop),
            KeyPress::Action(ActionVerb::Restart),
        ] {
            let mut app = allowed();
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_some(), "armed before {key:?}");
            assert_eq!(
                app.update(Msg::Key(key)),
                Effect::None,
                "{key:?} sent something"
            );
            assert!(app.action().is_none(), "{key:?} did not cancel");
        }

        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(
            matches!(app.update(Msg::Key(KeyPress::Confirm)), Effect::Send(_)),
            "and Enter is the one key that sends"
        );
    }

    /// `allowed()` parks the selection mid-list, so a `j` genuinely could move
    /// it.
    #[test]
    fn a_cancelling_key_is_consumed_and_does_not_also_move_the_selection() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let before = app.selected();
        let effect = app.update(Msg::Key(KeyPress::SelectDown));
        assert!(app.action().is_none(), "the stray j cancelled the confirm");
        assert_eq!(app.selected(), before, "and did not also move the cursor");
        assert_eq!(effect, Effect::None, "nor ask for a feed read or a walk");
    }

    /// The snapshot renames the armed sheep out of the filter while another
    /// enters it, so id 2 stays in `self.flock` while leaving `visible_rows()`
    /// and the cursor moves to id 9. Deleting a neighbour would not separate
    /// the two: `reseat` leaves a surviving id alone.
    #[test]
    fn the_confirm_is_pinned_to_the_id_it_was_armed_on() {
        let mut app = allowed();
        app.set_filter("api".to_string());
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(2, "gateway", ProcStatus::Online),
                sheep(9, "api-new", ProcStatus::Online),
            ],
            at: Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(9)),
            "sanity: the cursor followed the filter off the armed id"
        );
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter sends");
        };
        assert_eq!(
            sent,
            Sent::Action {
                verb: ActionVerb::Stop,
                target: RowKey::Sheep(2),
                name: "api".to_string()
            }
        );
    }

    #[test]
    fn a_confirm_whose_sheep_left_the_flock_refuses_instead_of_sending() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Event(BusEvent::Process {
            event: ProcessEventKind::Delete,
            info: sheep(1, "web", ProcStatus::Stopped),
            manually: true,
            at_ms: 0,
        }));
        assert!(app.action().is_none());
        // Nothing is armed any more, so `Confirm` falls to its other
        // meaning: it opens the sheep pane on whichever row the reseat
        // above moved the selection to, rather than sending the disarmed
        // `Sent::Action` the stale confirm would have.
        assert!(matches!(
            app.update(Msg::Key(KeyPress::Confirm)),
            Effect::Send(Sent::SheepConfig { .. })
        ));
    }

    /// Driven by `Msg::Tick`, so there is no sleep here.
    #[test]
    fn a_confirm_expires_after_ten_seconds_of_ticks() {
        let mut app = allowed();
        let t0 = Instant::now();
        app.update(Msg::Tick { now: t0 });
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(9),
        });
        assert!(app.action().is_some(), "nine seconds is still armed");
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(10),
        });
        assert!(app.action().is_none(), "ten is not");
    }

    /// The pane asks for its own refreshes rather than changing the link's
    /// interval, which is fixed for a connection's lifetime.
    #[test]
    fn a_tick_while_the_pane_is_open_asks_for_a_poll() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let now = Instant::now();
        assert_eq!(app.update(Msg::Tick { now }), Effect::RefreshFeed);
    }

    /// And does not on the dashboard, or every lookout would poll twice as
    /// often for nothing.
    #[test]
    fn a_tick_on_the_dashboard_asks_for_nothing() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let now = Instant::now();
        assert_eq!(app.update(Msg::Tick { now }), Effect::None);
    }

    #[test]
    fn every_action_key_refuses_while_the_link_is_not_live() {
        for link in [
            Msg::Retrying { attempt: 2 },
            Msg::Frozen {
                at_local: "2026-08-16 09:00:00".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            },
        ] {
            let mut app = allowed();
            app.update(link);
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_none());
            assert!(app.notice().is_some_and(Notice::is_grave));
        }
    }

    #[test]
    fn a_second_action_refuses_while_one_is_in_flight() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("one action is already in flight")
        );
        let action = app.action().expect("the first one is untouched");
        assert_eq!(action.verb, ActionVerb::Stop);
        assert!(action.sent);
    }

    #[test]
    fn an_in_flight_line_survives_a_keypress() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::SelectDown));
        assert!(
            app.action().is_some_and(|action| action.sent),
            "the keypress moved the cursor and left the in-flight state alone"
        );
    }

    #[test]
    fn quit_still_quits_while_a_confirm_is_armed() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert_eq!(app.update(Msg::Key(KeyPress::Quit)), Effect::Quit);
    }

    /// Outside an armed confirm, `Enter` opens the sheep pane
    /// ([`enter_opens_the_sheep_pane_and_asks_for_its_config`] pins the
    /// whole of that); the point pinned here is narrower and unchanged by
    /// that: a second `Enter` over an action already sent does not re-send
    /// it, the armed-confirm guard having already let it through once.
    #[test]
    fn a_second_confirm_does_not_resend_an_action_already_in_flight() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent), "in flight");
        assert!(
            !matches!(
                app.update(Msg::Key(KeyPress::Confirm)),
                Effect::Send(Sent::Action { .. })
            ),
            "a second Enter does not re-send the action"
        );
    }

    #[test]
    fn a_request_that_could_not_be_sent_says_so_and_clears_the_state() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter sends");
        };
        app.update(Msg::Unsent { sent });
        assert!(app.action().is_none());
        assert!(app.notice().is_some_and(Notice::is_grave));
    }

    #[test]
    fn a_link_that_stops_being_live_takes_an_armed_prompt_down() {
        for link in [
            Msg::Retrying { attempt: 2 },
            Msg::Frozen {
                at_local: "2026-08-16 09:00:00".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            },
        ] {
            let mut app = allowed();
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_some(), "armed while live");
            app.update(link);
            assert!(app.action().is_none(), "and gone once the link is not");
            // Nothing is armed any more, so `Enter` falls to its other
            // meaning (opening the sheep pane) rather than to the confirm
            // this prompt no longer has a question for.
            assert!(!matches!(
                app.update(Msg::Key(KeyPress::Confirm)),
                Effect::Send(Sent::Action { .. })
            ));
        }
    }

    #[test]
    fn an_accepted_stop_upserts_the_rows_the_shepherd_returned() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Stop,
                target: RowKey::Sheep(2),
                name: "api".to_string(),
            },
            result: Ok(Response::Stopped(vec![sheep(
                2,
                "api",
                ProcStatus::Stopped,
            )])),
        });
        assert_eq!(
            app.rows()
                .iter()
                .find(|row| row.info.id == 2)
                .map(|row| row.info.status),
            Some(ProcStatus::Stopped),
            "the table shows what the shepherd said, without waiting for a poll"
        );
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("stop api (id 2): the shepherd stopped it")
        );
        assert!(app.action().is_none(), "the in-flight state cleared");
    }

    /// `Response::Reloading` is an acceptance; the swaps arrive afterwards on
    /// the bus, which the table consumes.
    #[test]
    fn a_reload_reply_does_not_claim_the_swap_finished() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Reload)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Reload,
                target: RowKey::Sheep(2),
                name: "api".to_string(),
            },
            result: Ok(Response::Reloading {
                accepted: vec![sheep(2, "api", ProcStatus::Online)],
                refused: Vec::new(),
            }),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert_eq!(
            said,
            "reload api (id 2): accepted, the swaps report themselves as they happen"
        );
        assert!(!said.contains("reloaded"), "got {said:?}");
    }

    /// `RequestError`'s full `Display` would put a Rust identifier on screen.
    #[test]
    fn a_daemon_refusal_reaches_the_bar_in_the_daemons_own_words() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Restart,
                target: RowKey::Sheep(2),
                name: "api".to_string(),
            },
            result: Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::NotFound,
                message: "selector matched no registered sheep".to_string(),
                daemon_version: None,
            })),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert_eq!(
            said,
            "restart api (id 2): selector matched no registered sheep"
        );
        assert!(!said.contains("NotFound"), "no Rust identifiers: {said:?}");
        assert!(app.notice().is_some_and(Notice::is_grave));
    }

    #[test]
    fn a_connection_that_died_mid_request_says_so_under_the_same_prefix() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Stop,
                target: RowKey::Sheep(2),
                name: "api".to_string(),
            },
            result: Err(RequestError::Closed),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.starts_with("stop api (id 2): "), "got {said:?}");
        assert!(said.contains(&RequestError::Closed.to_string()));
    }

    /// The second case is the sharper one: the right shape for the wrong verb.
    /// A `Stopped` answering a `Restart` carries rows and would upsert happily.
    #[test]
    fn an_unrecognised_reply_says_so_rather_than_reading_as_success() {
        for reply in [
            Response::Pong,
            Response::Stopped(vec![sheep(2, "api", ProcStatus::Stopped)]),
        ] {
            let mut app = allowed();
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
            app.update(Msg::Key(KeyPress::Confirm));
            app.update(Msg::Replied {
                sent: Sent::Action {
                    verb: ActionVerb::Restart,
                    target: RowKey::Sheep(2),
                    name: "api".to_string(),
                },
                result: Ok(reply),
            });
            assert_eq!(
                app.notice().map(ToString::to_string).as_deref(),
                Some(
                    "restart api (id 2): the shepherd answered something this lookout does not understand"
                )
            );
            assert!(app.notice().is_some_and(Notice::is_grave));
        }
    }
}
