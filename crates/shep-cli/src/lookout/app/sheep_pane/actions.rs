//! What the sheep pane asks for and arms: the editor `e` opens and the verb
//! `x`/`R`/`L` arm, both against the pane's own pinned sheep.

use super::super::*;

impl App {
    /// `e`'s own handler from inside the sheep pane: targets the pane's own
    /// pinned sheep, never [`Self::selected_row`]/[`Self::selected_name`].
    ///
    /// The same reasoning [`Self::arm_sheep_pane`]'s own doc gives:
    /// `Msg::Snapshot` reseats the dashboard's selection whatever screen is
    /// showing. Refuses instead of substituting.
    pub(in crate::lookout::app) fn ask_for_sheep_pane_config(&mut self) -> Effect {
        let Some(row) = self.sheep_pane_row() else {
            self.notice = Some(Notice::grave(
                "that sheep is no longer in the flock".to_string(),
            ));
            return Effect::None;
        };
        self.ask_for_sheep_config(row.info.name.clone(), ConfigFor::Editor)
    }

    /// `x`/`R`/`L` from inside the sheep pane: arms a confirm against the
    /// pane's own pinned sheep, never [`Self::selected`].
    ///
    /// Arming against the selection here would be the same mistake
    /// [`Self::sheep_pane_row`]'s own doc explains. The ladder is
    /// [`Self::confirm_refusal`]'s own gate and link, then one action already
    /// in flight or held, same order [`Self::arm`] uses.
    pub(in crate::lookout::app) fn arm_sheep_pane(&mut self, verb: ActionVerb) -> Effect {
        if let Some(text) = self.confirm_refusal() {
            self.notice = Some(Notice::grave(text));
            return Effect::None;
        }
        // [`Self::arm`]'s own pair, for the reason given there: a verb the
        // close dialog is holding has not gone out yet, so `self.action` is
        // still empty. These two are the whole set of doors that arm from a
        // keypress.
        if self.action.is_some() || self.held.is_some() {
            self.notice = Some(Notice::grave("one action is already in flight".to_string()));
            return Effect::None;
        }
        let Some(row) = self.sheep_pane_row() else {
            self.notice = Some(Notice::grave(
                "that sheep is no longer in the flock".to_string(),
            ));
            return Effect::None;
        };
        self.action = Some(Action {
            verb,
            target: RowKey::Sheep(row.info.id),
            name: row.info.name.clone(),
            count: 1,
            at: self.now,
            stage: Stage::Armed,
        });
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::fixture_with_two_sheep;
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::view::fixtures;

    /// `e` inside the sheep pane opens the editor, not a refill of the pane
    /// that asked. Both send `Request::SheepConfig`, so the reply has to
    /// say which one it is for.
    #[test]
    fn e_inside_the_sheep_pane_opens_the_editor() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "alpha".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert!(matches!(app.body(), Body::ConfigPane(_)));
    }

    /// `allowed()`'s cursor is parked on `web`, id 1; `↵` pins the pane to
    /// it.
    fn allowed_in_the_sheep_pane() -> App {
        let mut app = allowed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app
    }

    /// `x` arms against the pane's own pinned sheep (`web`, id 1), the same
    /// target the dashboard's own `arm` would reach for the same cursor
    /// position. The two agree here because nothing has moved the
    /// selection out from under the pane yet.
    #[test]
    fn x_arms_a_confirm_against_the_panes_pinned_sheep() {
        let mut app = allowed_in_the_sheep_pane();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop))),
            Effect::None
        );
        let armed = app.action().expect("armed");
        assert_eq!(armed.verb, ActionVerb::Stop);
        assert_eq!(armed.target, &RowKey::Sheep(1));
        assert_eq!(armed.name, "web");
        assert!(!armed.sent, "nothing has gone out");
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "arming does not close the pane"
        );
    }

    /// `↵` confirms the armed action from inside the pane, the same send
    /// the dashboard's own `confirm` produces.
    #[test]
    fn confirm_inside_the_pane_sends_the_armed_action() {
        let mut app = allowed_in_the_sheep_pane();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter sends the armed action");
        };
        assert_eq!(
            sent,
            Sent::Action {
                verb: ActionVerb::Restart,
                target: RowKey::Sheep(1),
                name: "web".to_string(),
            }
        );
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "confirming does not close the pane"
        );
    }

    /// Every key but `↵` and `q` cancels an action armed from inside the
    /// pane, the rule [`App::on_armed_key`] applies on both screens. The pane
    /// needs its own call, since `on_key` routes here before making it.
    #[test]
    fn any_other_key_cancels_an_action_armed_inside_the_pane() {
        for key in [
            KeyPress::Escape,
            KeyPress::StepDown,
            KeyPress::StepUp,
            KeyPress::Edit,
        ] {
            let mut app = allowed_in_the_sheep_pane();
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
            assert!(app.action().is_some(), "armed before {key:?}");
            assert_eq!(
                app.update(Msg::Key(key)),
                Effect::None,
                "{key:?} sent something"
            );
            assert!(app.action().is_none(), "{key:?} did not cancel");
            assert!(
                matches!(app.body(), Body::Sheep(_)),
                "{key:?} must not also close the pane"
            );
        }
    }

    /// `--read-only` refuses the same way it refuses the dashboard's own
    /// `x`/`R`/`L`, with the same sentence.
    #[test]
    fn x_refuses_under_read_only_from_inside_the_pane() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(app.action().is_none());
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("read-only: from --read-only or lookout.allow_control")
        );
    }

    /// The pinned sheep can leave the flock entirely while the pane stays
    /// open on it (nothing but `Escape` closes it): arming then must refuse
    /// rather than target whoever replaced it.
    #[test]
    fn arming_refuses_once_the_pinned_sheep_has_left_the_flock() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::Sheep(_)), "pinned to alpha");
        app.set_control_for_tests(Control::Allowed);
        app.update(Msg::Snapshot {
            rows: vec![sheep(2, "bravo", ProcStatus::Online)],
            at: Instant::now(),
        });
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        assert!(
            app.action().is_none(),
            "refused rather than arming against bravo"
        );
        assert!(app.notice().is_some_and(Notice::is_grave));
    }

    /// `e` from inside the sheep pane targets the pane's own pinned sheep
    /// (`alpha`, id 1), not the dashboard's selection: nothing has moved
    /// the selection out from under the pane yet, so the two agree here,
    /// but only [`Self::sheep_pane_row`] is asked.
    #[test]
    fn e_asks_for_the_panes_own_pinned_sheeps_config() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::Sheep(_)), "pinned to alpha");
        let request = wire(app.update(Msg::Key(KeyPress::Edit)));
        assert_eq!(
            request,
            Request::SheepConfig {
                name: "alpha".to_string()
            }
        );
    }

    /// The regression the reviewer reproduced: pane pinned to `alpha`,
    /// `Msg::Snapshot` reseats the dashboard's selection onto `bravo` once
    /// `alpha` leaves the flock, and `e` must refuse rather than open
    /// `bravo`'s config under a pane still titled `alpha`.
    #[test]
    fn e_refuses_once_the_pinned_sheep_has_left_the_flock() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::Sheep(_)), "pinned to alpha");
        app.update(Msg::Snapshot {
            rows: vec![sheep(2, "bravo", ProcStatus::Online)],
            at: Instant::now(),
        });
        assert_eq!(app.update(Msg::Key(KeyPress::Edit)), Effect::None);
        assert!(app.notice().is_some_and(Notice::is_grave));
    }

    /// `arm` and `arm_sheep_pane` share their refusal ladder through
    /// `confirm_refusal`, but nothing before this test exercised
    /// `arm_sheep_pane`'s own copy of the "one already in flight" branch: a
    /// hand-copied ladder that dropped it silently would still pass every
    /// other sheep-pane test, since none of them arm twice.
    #[test]
    fn a_second_arm_from_inside_the_sheep_pane_refuses_while_one_is_in_flight() {
        let mut app = allowed_in_the_sheep_pane();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent), "in flight");
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("one action is already in flight")
        );
        let action = app.action().expect("the first one is untouched");
        assert_eq!(action.verb, ActionVerb::Stop);
        assert!(action.sent);
    }

    /// Both screens clear `self.notice` after [`App::on_armed_key`] and
    /// before their own keymap: without it a refusal raised inside the pane
    /// never cleared, kept overriding the pane's own key hints in
    /// `status_line`, and survived `close_pane` back to the flock table.
    #[test]
    fn a_refusal_inside_the_sheep_pane_clears_on_the_next_keypress() {
        let mut app = allowed_in_the_sheep_pane();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert!(app.notice().is_some(), "setup: the refusal is raised");
        app.update(Msg::Key(KeyPress::SelectDown));
        assert!(
            app.notice().is_none(),
            "the refusal outlived a keypress that was not the one that \
             raised it"
        );
    }

    /// The reviewer's own reachable state: an action sent (`Stage::Sent`, in
    /// flight), then a filter typed down to zero rows clears the selection
    /// (`reseat`'s own empty-flock-view branch), then an action key. Before
    /// this fix, `confirm_refusal` checked "one already in flight" ahead of
    /// `arm`'s own "nothing selected", so this exact sequence told the
    /// operator the wrong thing (the in-flight action, not the empty
    /// selection the keypress actually asked about).
    #[test]
    fn arm_with_nothing_selected_refuses_that_and_not_the_in_flight_action() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent), "in flight");

        app.update(Msg::Key(KeyPress::FilterStart));
        for letter in ['z', 'z', 'z'] {
            app.update(Msg::Key(KeyPress::TextChar(letter)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        assert_eq!(app.rows().len(), 0, "the query matches nothing");
        assert!(app.selected_row().is_none(), "reseat cleared the selection");

        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("no sheep is selected"),
            "the keypress asked whether it had a target, and it did not"
        );
        let action = app.action().expect("the first one is still in flight");
        assert_eq!(action.verb, ActionVerb::Stop);
        assert!(action.sent, "untouched by the refused second arm");
    }
}
