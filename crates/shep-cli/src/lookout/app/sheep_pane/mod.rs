//! The sheep pane: one sheep's own screen, with its config, its feed and its
//! actions.
//!
//! Three modules, one per reason to change: this one opens the pane on a sheep,
//! steps it to the next one the flock table would show, and holds the accessors
//! every screen reads; [`keys`] answers the pane's own keymap and hands its feed
//! to the full screen; [`actions`] arms a verb and opens the editor against the
//! pane's own pinned sheep.

use super::*;

mod actions;
mod keys;

impl App {
    /// `Enter`'s own handler on the dashboard: opens the sheep pane on the
    /// selected sheep and asks for its config in the same step, since the
    /// pane's own left column has nothing to draw without it.
    ///
    /// [`Self::selected_row`], not [`Self::selected_name`]: a group row has no
    /// single sheep to open the pane on, and a dog runs no config the pane's
    /// own `SheepConfigView` can show. Both are silently refused.
    pub(super) fn open_sheep_pane(&mut self) -> Effect {
        let Some(row) = self.selected_row() else {
            return Effect::None;
        };
        if row.info.dog.is_some() {
            return Effect::None;
        }
        let sheep = self
            .selected()
            .expect("selected_row answered, so a selection exists");
        let name = row.info.name.clone();
        self.body = Body::Sheep(Box::new(SheepPane::new(sheep)));
        self.ask_for_sheep_config(name, ConfigFor::SheepPane)
    }

    /// `J`/`K` from inside the sheep pane: steps to the next or previous
    /// sheep the flock table would show, skipping a dog, a group header and
    /// a fold header (none of which is a sheep the pane can open on), and
    /// asks for the new sheep's config in the same step.
    ///
    /// Silent past either end of the list, and silent if the pane's own
    /// sheep has already left the flock: there is nothing to step from.
    pub(super) fn step_sheep_pane(&mut self, delta: isize) -> Effect {
        let Body::Sheep(pane) = &self.body else {
            return Effect::None;
        };
        let current = pane.sheep().clone();
        let sheep_rows: Vec<RowKey> = self
            .visible_rows()
            .into_iter()
            .filter(|key| match key {
                RowKey::Sheep(id) => self.flock.get(id).is_some_and(|row| row.info.dog.is_none()),
                RowKey::Group(_) | RowKey::Fold(_) | RowKey::Section(_) => false,
            })
            .collect();
        let Some(index) = sheep_rows.iter().position(|key| *key == current) else {
            return Effect::None;
        };
        let next_index = index
            .saturating_add_signed(delta)
            .min(sheep_rows.len().saturating_sub(1));
        let next = sheep_rows[next_index].clone();
        if next == current {
            return Effect::None;
        }
        let RowKey::Sheep(id) = &next else {
            unreachable!("the filter above admits only `RowKey::Sheep`")
        };
        let Some(name) = self.flock.get(id).map(|row| row.info.name.clone()) else {
            return Effect::None;
        };
        self.selected = Some(next.clone());
        if let Some(pane) = self.sheep_pane_mut() {
            pane.set_sheep(next);
        }
        self.ask_for_sheep_config(name, ConfigFor::SheepPane)
    }

    /// The open sheep pane, or `None` on any other screen:
    /// [`Body::sheep_pane`] over this app's body.
    #[must_use]
    pub fn sheep_pane(&self) -> Option<&SheepPane> {
        self.body.sheep_pane()
    }

    /// The open sheep pane's own pinned sheep, or `None` once it has left
    /// the flock.
    ///
    /// Reads [`SheepPane::sheep`], never [`Self::selected`]: the same
    /// reasoning [`Self::feed_row`]'s own doc gives. The identity band draws
    /// this instead of `App::selected_row`, and any figure it shows resolves
    /// through the row this returns.
    #[must_use]
    pub fn sheep_pane_row(&self) -> Option<&Row> {
        match self.sheep_pane()?.sheep() {
            RowKey::Sheep(id) => self.flock.get(id),
            RowKey::Group(_) | RowKey::Fold(_) | RowKey::Section(_) => None,
        }
    }

    /// [`Self::sheep_pane`]'s mutable twin, for `J`/`K` and for adopting a
    /// `Request::SheepConfig` reply in place.
    pub(super) fn sheep_pane_mut(&mut self) -> Option<&mut SheepPane> {
        self.body.sheep_pane_mut()
    }

    /// The embedded feed inside the open sheep pane, or `None` while the
    /// pane itself is closed.
    ///
    /// [`Self::sheep_pane_mut`] and [`SheepPane::feed_mut`] composed once,
    /// for `on_sheep_pane_key`'s own filter-axis arms, which would otherwise
    /// repeat the two-step `and_then` at every one of them.
    pub(super) fn sheep_feed_mut(&mut self) -> Option<&mut BleatsPane> {
        self.sheep_pane_mut().map(SheepPane::feed_mut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::view::fixtures;

    /// Two online sheep, `alpha` (id 1) and `bravo` (id 2), named so their
    /// alphabetical table order agrees with their ids: `alpha` is selected
    /// by [`App::reseat`]'s own default the moment the snapshot lands, and
    /// stepping down from it reaches `bravo` in one move.
    pub(super) fn fixture_with_two_sheep() -> App {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(1, "alpha", ProcStatus::Online),
                sheep(2, "bravo", ProcStatus::Online),
            ],
            at: t0,
        });
        app
    }

    /// One dog and nothing else, so `App::reseat`'s own header-skip selects
    /// it the moment the snapshot lands: the only row that is not a
    /// `Section` header is the dog.
    fn fixture_with_a_dog_selected() -> App {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(90, "otel", ProcStatus::Online)
                    .dog(Some(DogSource::BuiltIn { grants: None }))
                    .build(),
            ],
            at: t0,
        });
        app
    }

    /// `↵` opens the pane on the selected sheep and asks for its config in
    /// the same step, since the pane's left column has nothing to draw
    /// without it.
    #[test]
    fn enter_opens_the_sheep_pane_and_asks_for_its_config() {
        let mut app = fixture_with_two_sheep();
        let effect = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::Sheep(_)));
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig {
                name: "alpha".to_string()
            })
        );
    }

    /// An armed prompt owns `↵`. Opening a pane out from under a question
    /// the operator has not answered would answer it for them.
    #[test]
    fn enter_confirms_an_armed_action_rather_than_opening_the_pane() {
        let mut app = allowed();
        let _ = app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// A dog row has no charts to draw and its config is a TOML section
    /// rather than a `SheepConfigView`, so `↵` does nothing there. `e`
    /// still opens the dog config pane it opens today.
    #[test]
    fn enter_on_a_dog_row_opens_nothing() {
        let mut app = fixture_with_a_dog_selected();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// The routing `ConfigFor` exists for. `↵` asks for the pane's own
    /// config while the pane is still `Body::Sheep`, so a reply routed by
    /// what is on screen would land in the editor instead: the pane would
    /// sit there with an empty column forever, and an editing pane the
    /// operator never asked for would take the screen.
    #[test]
    fn a_reply_to_the_sheep_panes_own_read_fills_its_column_and_opens_no_editor() {
        let mut app = fixture_with_two_sheep();
        let effect = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig {
                name: "alpha".to_string()
            })
        );
        assert!(
            app.sheep_pane().unwrap().config().is_none(),
            "the column has nothing to draw until the reply lands"
        );

        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "alpha".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });

        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "the pane the read belongs to stays up"
        );
        assert!(
            app.config_pane().is_none(),
            "a read the pane asked for opens no editor"
        );
        let pane = app.sheep_pane().expect("still the sheep pane");
        let config = pane.config().expect("the reply filled the pane's column");
        assert_eq!(
            config.name, "web",
            "the pane holds the view the reply carried"
        );
        assert!(
            crate::lookout::view::sheep::column_len(pane.config()) > 1,
            "1 is the line the column draws while it has no config at all"
        );
    }

    /// `esc` backs out of the sheep pane to the dashboard.
    #[test]
    fn escape_closes_the_sheep_pane() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// `on_sheep_pane_key`'s own `SelectUp`/`SelectDown`/`SelectFirst`/
    /// `SelectLast` arms, exercised through `App::update` rather than by
    /// calling `SheepPane::move_by` directly: this pins the routing itself,
    /// the surface `pane_sheep.rs`'s own unit tests cannot reach.
    #[test]
    fn j_k_g_and_capital_g_scroll_the_sheep_panes_column() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "alpha".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        let len = crate::lookout::view::sheep::column_len(app.sheep_pane().unwrap().config());

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(app.sheep_pane().unwrap().view().cursor(), 1);
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(app.sheep_pane().unwrap().view().cursor(), 0);

        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.sheep_pane().unwrap().view().cursor(), len - 1);
        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        assert_eq!(app.sheep_pane().unwrap().view().cursor(), 0);
    }

    /// `J` walks the flock without leaving the pane, and asks for the new
    /// sheep's config.
    #[test]
    fn step_down_moves_to_the_next_sheep() {
        let mut app = fixture_with_two_sheep();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let effect = app.update(Msg::Key(KeyPress::StepDown));
        let Body::Sheep(pane) = app.body() else {
            panic!("still in the sheep pane")
        };
        assert_eq!(pane.sheep(), &RowKey::Sheep(2));
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig {
                name: "bravo".to_string()
            })
        );
    }

    /// `fixture_with_two_sheep`, with a tail already landed for `alpha`: two
    /// lines, one of them containing `boom`, so the embedded feed's filter
    /// and promotion tests below have something to narrow and a second
    /// sheep to step onto.
    pub(super) fn fixture_with_feed() -> App {
        let mut app = fixture_with_two_sheep();
        app.update(Msg::Bleats {
            tail: crate::lookout::tail::Tail {
                lines: vec![
                    crate::lookout::tail::TailLine::new(Stream::Out, "boom detected"),
                    crate::lookout::tail::TailLine::new(Stream::Out, "all quiet"),
                ],
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 0,
                note: None,
            },
        });
        app
    }

    /// Types `text` into the embedded feed's match box and applies it,
    /// through the same keys an operator presses (`FilterStart`, one
    /// `TextChar` per byte, `TextApply`) rather than reaching into
    /// `SheepPane` directly: this is what `on_sheep_pane_key`'s own filter
    /// arms are for, and a fixture that skipped them would not exercise the
    /// routing this task adds.
    pub(super) fn apply_match(app: &mut App, text: &str) {
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        for ch in text.chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(ch)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
    }

    /// Stepping to another sheep re-scopes the feed. A feed left on the
    /// previous sheep under a new title is worse than an empty one.
    #[test]
    fn stepping_re_scopes_the_feed() {
        let mut app = fixture_with_feed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::StepDown));
        let Body::Sheep(pane) = app.body() else {
            panic!("still in the sheep pane")
        };
        assert_eq!(pane.feed_sheep(), &RowKey::Sheep(2));
    }

    /// `↵` confirms an armed action, correctly: a question awaiting an
    /// answer keeps `↵`. But once sent (`Stage::Sent`), it is in flight and
    /// no longer asking anything, and a regression that let `Stage::Sent` keep
    /// swallowing `↵` (rather than falling through, here, to the dashboard's
    /// own `Confirm` handler, which opens the pane) would pass
    /// `a_second_confirm_does_not_resend_an_action_already_in_flight` above
    /// just as easily, since that test only checks nothing resends.
    #[test]
    fn a_confirm_at_stage_sent_falls_through_to_opening_the_sheep_pane() {
        let mut app = allowed();
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(app.action().is_some_and(|action| action.sent), "in flight");
        app.update(Msg::Key(KeyPress::Confirm));
        assert!(
            matches!(app.body(), Body::Sheep(_)),
            "the second Enter opened the pane rather than being swallowed"
        );
    }
}
