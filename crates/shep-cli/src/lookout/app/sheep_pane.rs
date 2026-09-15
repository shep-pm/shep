//! The sheep pane: one sheep's own screen, with its config, its feed and its actions.

use super::*;

impl App {
    /// `b`, from inside the sheep pane: hands the embedded feed's own state
    /// to `Body::Bleats` rather than [`BleatsPane::new`]ing a fresh one, so
    /// a filter narrowed in the pane's own column survives going full
    /// screen. A no-op on any other screen; `on_sheep_pane_key` only reaches
    /// this while [`Self::sheep_pane`] is `Some`, but the match on `body`
    /// stays defensive rather than assuming that.
    pub(super) fn promote_feed_to_full_screen(&mut self) -> Effect {
        if let Body::Sheep(pane) = &self.body {
            self.body = Body::Bleats(pane.feed().clone());
        }
        // The embedded feed never clamps its own offset: it draws no
        // scrollback, so `N` can walk the stored value past anything the
        // full screen can scroll back to. Clamping on arrival rather than
        // leaving it is what `scroll_bleats_back` already documents, in
        // those words: the render clamps while the stored value keeps
        // climbing, so `j` stops appearing to work until the operator has
        // pressed it as many times as `N` was pressed before.
        let ceiling = self.bleats_pane().map_or(0, |pane| {
            crate::lookout::view::bleats_full::max_scroll_offset(self, pane)
        });
        if let Some(pane) = self.bleats_pane_mut() {
            pane.clamp_scroll(ceiling);
        }
        Effect::None
    }

    /// `Enter`'s own handler on the dashboard: opens the sheep pane on the
    /// selected sheep and asks for its config in the same step, since the
    /// pane's own left column has nothing to draw without it.
    ///
    /// [`Self::selected_row`], not [`Self::selected_name`]: a group row has
    /// no single sheep to open the pane on, and a dog runs no config the
    /// pane's own `SheepConfigView` can show (its section is a TOML table,
    /// not a Flockfile's `AppConfig`). Both are silently refused, the same
    /// silence [`Self::ask_for_bleats`] falls back to for a group.
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

    /// The sheep pane's own keymap, in force while [`Self::sheep_pane`] is
    /// `Some`. `esc` closes it; `e` opens the config editor over it,
    /// routed by [`ConfigFor`] once the reply lands, since both send the
    /// same request; `J`/`K` step to the next or previous sheep without
    /// leaving the pane; `x`/`R`/`L` arm a confirm against the pane's own
    /// pinned sheep ([`Self::arm_sheep_pane`]), `↵` confirms it and any
    /// other key cancels it, the same as the dashboard's own armed check
    /// just below in [`Self::on_key`], needed here too, in its own copy,
    /// because `on_key` routes to this method ahead of that check, so an
    /// action armed from inside this pane never reaches it. `j`/`k` and
    /// `g`/`G` scroll the config/env column through its own `Viewport`.
    /// `/`, `o`, `m`, `f`, `w`, `n` and `N` belong to the embedded feed
    /// ([`Self::sheep_feed_mut`]), the same axes [`Self::on_bleats_key`]
    /// wires for the full-screen pane, and `b` hands that same feed to
    /// [`Self::promote_feed_to_full_screen`] rather than opening a fresh one.
    /// Every other key is inert.
    pub(super) fn on_sheep_pane_key(&mut self, key: KeyPress) -> Effect {
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.stage == Stage::Armed)
        {
            if key == KeyPress::Confirm {
                return self.confirm();
            }
            if key == KeyPress::Quit {
                return Effect::Quit;
            }
            self.action = None;
            return Effect::None;
        }
        self.notice = None;
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::Escape => {
                self.close_pane();
                Effect::None
            }
            KeyPress::Edit => self.ask_for_sheep_pane_config(),
            KeyPress::StepDown => self.step_sheep_pane(1),
            KeyPress::StepUp => self.step_sheep_pane(-1),
            KeyPress::Action(verb) => self.arm_sheep_pane(verb),
            KeyPress::SelectUp => {
                if let Some(pane) = self.sheep_pane_mut() {
                    pane.move_by(-1);
                }
                Effect::None
            }
            KeyPress::SelectDown => {
                if let Some(pane) = self.sheep_pane_mut() {
                    pane.move_by(1);
                }
                Effect::None
            }
            KeyPress::SelectFirst => {
                if let Some(pane) = self.sheep_pane_mut() {
                    pane.move_to_first();
                }
                Effect::None
            }
            KeyPress::SelectLast => {
                if let Some(pane) = self.sheep_pane_mut() {
                    pane.move_to_last();
                }
                Effect::None
            }
            // `b`: hands the embedded feed's own state to `Body::Bleats`
            // rather than rebuilding one, so a filter narrowed here survives
            // going full screen.
            KeyPress::Bleats => self.promote_feed_to_full_screen(),
            // Opens the feed's own match box: `on_text_key` routes the
            // keystrokes that follow to `on_sheep_feed_text_key` once this
            // pane owns `InputMode::Text`, the same shape
            // `on_bleats_key`'s own `FilterStart` arm follows.
            KeyPress::FilterStart => {
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.begin_match_edit();
                }
                self.mode = InputMode::Text;
                Effect::None
            }
            // `o`: cycles the embedded feed's stream axis, the same cycle
            // `on_bleats_key`'s own arm follows.
            KeyPress::StreamCycle => {
                if let Some(feed) = self.sheep_feed_mut() {
                    let next = match feed.filters().stream {
                        None => Some(Stream::Out),
                        Some(Stream::Out) => Some(Stream::Err),
                        Some(Stream::Err) => None,
                    };
                    feed.set_stream(next);
                }
                Effect::None
            }
            // `m`: cycles the embedded feed's minimum-level axis, the same
            // cycle `on_bleats_key`'s own arm follows.
            KeyPress::LevelCycle => {
                if let Some(feed) = self.sheep_feed_mut() {
                    let next = match feed.filters().min_level {
                        None => Some(Level::Trace),
                        Some(Level::Trace) => Some(Level::Debug),
                        Some(Level::Debug) => Some(Level::Info),
                        Some(Level::Info) => Some(Level::Warn),
                        Some(Level::Warn) => Some(Level::Error),
                        Some(Level::Error) => None,
                    };
                    feed.set_min_level(next);
                }
                Effect::None
            }
            // `f`: toggles following explicitly.
            KeyPress::FollowToggle => {
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.toggle_follow();
                }
                Effect::None
            }
            // `w`: toggles whether a long line wraps or truncates.
            KeyPress::WrapToggle => {
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.toggle_wrap();
                }
                Effect::None
            }
            // `n`: one match toward the newest line. A no-op with no match
            // axis set: see `BleatsPane::match_next`.
            KeyPress::MatchNext => {
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.match_next();
                }
                Effect::None
            }
            // `N`: the same, toward the oldest matching line. Unlike
            // `on_bleats_key`'s own `MatchPrev` arm, this does not clamp
            // through `bleats_full::max_scroll_offset`: the embedded feed
            // draws no scrollback of its own (there is no `j`/`k` for it
            // here, unlike the full-screen pane), and `window_range`
            // saturates a stale offset rather than reading past the end.
            // `promote_feed_to_full_screen` clamps on arrival, which is
            // where an unclamped value would otherwise be felt.
            KeyPress::MatchPrev => {
                let stepping = self
                    .sheep_pane()
                    .is_some_and(|pane| pane.feed().filters().match_text().is_some());
                if stepping && let Some(feed) = self.sheep_feed_mut() {
                    feed.scroll_up(1);
                }
                Effect::None
            }
            KeyPress::Help => self.open_keymap(),
            KeyPress::Refresh
            | KeyPress::Confirm
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Settings
            | KeyPress::Cycle
            | KeyPress::Remove
            | KeyPress::FoldView
            | KeyPress::Collapse
            | KeyPress::PageDown
            | KeyPress::PageUp
            // The secrets pane's own six. `S` opens that pane from the
            // dashboard, not from here, and the other five mean nothing
            // outside it.
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete => Effect::None,
            // The groups and the filed edit set belong to the config pane.
            // This pane lists a sheep's fields read-only, so it has no
            // group to switch to and nothing filed to take back.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo
            | KeyPress::Continue => Effect::None,
        }
    }

    /// The embedded feed's own match box, in force while it owns
    /// [`InputMode::Text`]. [`Self::on_bleats_text_key`]'s own body, against
    /// [`Self::sheep_feed_mut`] instead of [`Self::bleats_pane_mut`]: the two
    /// panes never coexist, but each opens its match box against its own
    /// filter state.
    pub(super) fn on_sheep_feed_text_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::TextChar(typed) => {
                if let Some(feed) = self.sheep_feed_mut() {
                    let mut text = feed.filters().match_text().unwrap_or_default().to_string();
                    text.push(typed);
                    feed.set_match(text);
                }
                Effect::None
            }
            KeyPress::TextBackspace => {
                if let Some(feed) = self.sheep_feed_mut() {
                    let mut text = feed.filters().match_text().unwrap_or_default().to_string();
                    text.pop();
                    feed.set_match(text);
                }
                Effect::None
            }
            KeyPress::TextApply => {
                self.mode = InputMode::Normal;
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.commit_match_edit();
                }
                Effect::None
            }
            KeyPress::TextAbandon => {
                self.mode = InputMode::Normal;
                if let Some(feed) = self.sheep_feed_mut() {
                    feed.abandon_match_edit();
                }
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// `e`'s own handler from inside the sheep pane: targets the pane's own
    /// pinned sheep, never [`Self::selected_row`]/[`Self::selected_name`].
    ///
    /// The same reasoning [`Self::arm_sheep_pane`]'s own doc gives:
    /// `Msg::Snapshot` reseats the dashboard's selection whatever screen is
    /// showing, so reading the selection here would open a neighbour's
    /// config under the pane's own title the instant the pinned sheep left
    /// the flock. Refuses instead of substituting. A pane is never pinned
    /// on a dog, so [`Self::ask_for_config`]'s dog branch has no twin here.
    pub(super) fn ask_for_sheep_pane_config(&mut self) -> Effect {
        let Some(row) = self.sheep_pane_row() else {
            self.notice = Some(Notice {
                text: "that sheep is no longer in the flock".to_string(),
                grave: true,
            });
            return Effect::None;
        };
        self.ask_for_sheep_config(row.info.name.clone(), ConfigFor::Editor)
    }

    /// `x`/`R`/`L` from inside the sheep pane: arms a confirm against the
    /// pane's own pinned sheep, never [`Self::selected`].
    ///
    /// Arming against the selection here would be the same mistake
    /// [`Self::sheep_pane_row`]'s own doc explains: `Msg::Snapshot` reseats
    /// the selection whatever screen is showing, so a pinned sheep that
    /// leaves the flock would arm an action against whichever sheep
    /// replaced it while the pane still names the first. Refuses instead.
    ///
    /// The ladder is [`Self::confirm_refusal`]'s own gate and link, then one
    /// action already in flight or held, same order [`Self::arm`] uses for
    /// those two; [`Self::arm`]'s "nothing selected" case cannot happen
    /// here, since the pane would not be open without a sheep, so its place
    /// is taken by the pinned sheep having left instead.
    pub(super) fn arm_sheep_pane(&mut self, verb: ActionVerb) -> Effect {
        if let Some(text) = self.confirm_refusal() {
            self.notice = Some(Notice { text, grave: true });
            return Effect::None;
        }
        // [`Self::arm`]'s own pair, for the reason given there: a verb the
        // close dialog is holding has not gone out yet, so `self.action` is
        // still empty. These two are the whole set of doors that arm from a
        // keypress.
        if self.action.is_some() || self.held.is_some() {
            self.notice = Some(Notice {
                text: "one action is already in flight".to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(row) = self.sheep_pane_row() else {
            self.notice = Some(Notice {
                text: "that sheep is no longer in the flock".to_string(),
                grave: true,
            });
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

    /// The open sheep pane, or `None` on any other screen.
    #[must_use]
    pub fn sheep_pane(&self) -> Option<&SheepPane> {
        match &self.body {
            Body::Sheep(pane) => Some(pane.as_ref()),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_) => None,
        }
    }

    /// The open sheep pane's own pinned sheep, or `None` once it has left
    /// the flock.
    ///
    /// Reads [`SheepPane::sheep`], never [`Self::selected`]: the same
    /// reasoning [`Self::feed_row`]'s own doc gives. `Msg::Snapshot` reseats
    /// the selection whatever screen is showing, so a pane pinned to a
    /// sheep that then leaves the flock would have this read a neighbour's
    /// row while the pane still names the first, one sheep's facts
    /// presented as another's. The identity band draws this instead of
    /// `App::selected_row`, and any figure it shows (a CPU reading among
    /// them) resolves through the row this returns, not the selection.
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
        match &mut self.body {
            Body::Sheep(pane) => Some(pane.as_mut()),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_) => None,
        }
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
    use shep_core::config::LevelRule;

    /// Two online sheep, `alpha` (id 1) and `bravo` (id 2), named so their
    /// alphabetical table order agrees with their ids: `alpha` is selected
    /// by [`App::reseat`]'s own default the moment the snapshot lands, and
    /// stepping down from it reaches `bravo` in one move.
    fn fixture_with_two_sheep() -> App {
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
                    .dog(Some(DogSource::BuiltIn))
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

    /// Ahead of the mutation check below: if `on_sheep_config` branched on
    /// the current body instead of `ConfigFor`, this reply would refill the
    /// sheep pane it found on screen rather than open the editor `e` asked
    /// for, since the pane is still `Body::Sheep` while the reply is in
    /// flight.
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
    fn fixture_with_feed() -> App {
        let mut app = fixture_with_two_sheep();
        app.update(Msg::Bleats {
            tail: crate::lookout::tail::Tail {
                lines: vec![
                    crate::lookout::tail::TailLine {
                        stream: Stream::Out,
                        text: "boom detected".to_string(),
                    },
                    crate::lookout::tail::TailLine {
                        stream: Stream::Out,
                        text: "all quiet".to_string(),
                    },
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
    fn apply_match(app: &mut App, text: &str) {
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        for ch in text.chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(ch)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
    }

    /// The embedded feed's surviving lines, filtered the way its own
    /// `Filters` would narrow them, for a test to inspect without reaching
    /// into `view::sheep::draw` for a rendered row.
    fn feed_rows(app: &App) -> Vec<String> {
        let Body::Sheep(pane) = app.body() else {
            panic!("the sheep pane is not open")
        };
        pane.feed()
            .visible(&app.feed().lines, &app.feed_classifier())
            .into_iter()
            .map(|line| line.text.clone())
            .collect()
    }

    /// The pane's own filters, not a second set. The header advertises them,
    /// so they have to work here and not only in the full-screen pane.
    #[test]
    fn a_filter_applied_in_the_sheep_pane_narrows_its_feed() {
        let mut app = fixture_with_feed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        apply_match(&mut app, "boom");
        assert!(feed_rows(&app).iter().all(|row| row.contains("boom")));
    }

    /// An app that declares `rules`, with `lines` already read off its log,
    /// and its sheep pane open. The rules arrive on the listing row, which
    /// is the only way a real one ever gets them.
    fn fixture_with_declared_levels(rules: Vec<LevelRule>, lines: &[&str]) -> App {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(1, "alpha", ProcStatus::Online)
                    .pid(Some(1001))
                    .uptime_ms(60_000)
                    .level_rules(rules)
                    .build(),
            ],
            at: t0,
        });
        app.update(Msg::Bleats {
            tail: crate::lookout::tail::Tail {
                lines: lines
                    .iter()
                    .map(|text| crate::lookout::tail::TailLine {
                        stream: Stream::Out,
                        text: (*text).to_string(),
                    })
                    .collect(),
                missed_lines: 0,
                missed_bytes: 0,
                read_bytes: 0,
                note: None,
            },
        });
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        app
    }

    /// Presses `m` until the feed's minimum sits at `wanted`, the way an
    /// operator reaches one, and fails rather than looping if the cycle
    /// stops passing through it.
    fn cycle_min_level_to(app: &mut App, wanted: Level) {
        for _ in 0..6 {
            let _ = app.update(Msg::Key(KeyPress::LevelCycle));
            if let Body::Sheep(pane) = app.body()
                && pane.feed().filters().min_level == Some(wanted)
            {
                return;
            }
        }
        panic!("the level cycle never reached {wanted}");
    }

    /// The whole point of the field: the built-in reading classifies neither
    /// of these lines, so under it both survive any minimum. The rules
    /// classify both, and the minimum then tells them apart.
    #[test]
    fn a_declared_rule_classifies_a_line_the_built_in_reading_misses() {
        let mut app = fixture_with_declared_levels(
            vec![
                LevelRule {
                    pattern: r#""severity":"ERROR""#.to_string(),
                    level: Level::Error,
                },
                LevelRule {
                    pattern: r#""severity":"DEBUG""#.to_string(),
                    level: Level::Debug,
                },
            ],
            &[
                r#"{"severity":"ERROR","msg":"boom"}"#,
                r#"{"severity":"DEBUG","msg":"tick"}"#,
            ],
        );
        cycle_min_level_to(&mut app, Level::Warn);
        let rows = feed_rows(&app);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("boom"), "{rows:?}");
    }

    /// Declared rules replace the built-in reading rather than adding to it.
    /// `WARN pool exhausted` is a line that reading classifies and no rule
    /// here matches, so it stays unclassified and survives a floor above it.
    /// Under a fallback it would read as `Warn` and be filtered out.
    #[test]
    fn a_declared_rule_set_replaces_the_built_in_reading() {
        let mut app = fixture_with_declared_levels(
            vec![LevelRule {
                pattern: "^E/".to_string(),
                level: Level::Error,
            }],
            &["WARN pool exhausted", "E/tag boom"],
        );
        cycle_min_level_to(&mut app, Level::Error);
        assert_eq!(feed_rows(&app).len(), 2);
    }

    /// `b` hands the same pane the whole screen, carrying its filters.
    #[test]
    fn b_promotes_the_feed_to_full_screen_with_its_filters() {
        let mut app = fixture_with_feed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        apply_match(&mut app, "boom");
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let Body::Bleats(pane) = app.body() else {
            panic!("full screen")
        };
        assert_eq!(pane.match_filter(), Some("boom"));
    }

    /// Promotion clamps the offset the embedded feed never clamps itself.
    ///
    /// `N` walks the stored value up one line at a time and the embedded
    /// feed draws no scrollback, so nothing bounds it there. Carried across
    /// unclamped, the full screen renders the oldest survivor while the
    /// stored value sits past it, and `j` does nothing visible until it has
    /// been pressed back down through the excess.
    #[test]
    fn promotion_clamps_an_offset_the_embedded_feed_left_out_of_range() {
        let mut app = fixture_with_feed();
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        if let Some(feed) = app.sheep_feed_mut() {
            feed.scroll_up(9_999);
        }
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let Body::Bleats(pane) = app.body() else {
            panic!("full screen")
        };
        let ceiling = crate::lookout::view::bleats_full::max_scroll_offset(&app, pane);
        assert!(
            pane.scroll_offset() <= ceiling,
            "offset {} should have been clamped to {ceiling}",
            pane.scroll_offset()
        );
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
    /// pane, the same rule the dashboard's own armed check applies, needed
    /// in the pane's own copy, since `on_key` routes here ahead of that
    /// check.
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

    /// `on_key`'s own armed-keypress prelude clears `self.notice` before its
    /// `match`; `on_sheep_pane_key` copied the prelude but not that line, so
    /// a refusal raised inside the pane never cleared, kept overriding the
    /// pane's own key hints in `status_line`, and survived `close_pane`
    /// back to the flock table.
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
