//! The sheep pane's own keymap, and the feed it hands to the full screen.

use super::super::*;

impl App {
    /// `b`, from inside the sheep pane: hands the embedded feed's own state
    /// to `Body::Bleats` rather than [`BleatsPane::new`]ing a fresh one, so
    /// a filter narrowed in the pane's own column survives going full
    /// screen. A no-op on any other screen; `on_sheep_pane_key` only reaches
    /// this while [`Self::sheep_pane`] is `Some`, but the match on `body`
    /// stays defensive rather than assuming that.
    pub(in crate::lookout::app) fn promote_feed_to_full_screen(&mut self) -> Effect {
        if let Body::Sheep(pane) = &self.body {
            self.body = Body::Bleats(pane.feed().clone());
        }
        // The embedded feed never clamps its own offset: it draws no
        // scrollback, so `N` can walk the stored value past anything the full
        // screen can scroll back to. Clamping on arrival is what
        // `scroll_bleats_back` already documents.
        let ceiling = self.bleats_pane().map_or(0, |pane| {
            crate::lookout::view::bleats_full::max_scroll_offset(self, pane)
        });
        if let Some(pane) = self.bleats_pane_mut() {
            pane.clamp_scroll(ceiling);
        }
        Effect::None
    }

    /// The sheep pane's own keymap, in force while [`Self::sheep_pane`] is
    /// `Some`. `esc` closes it; `e` opens the config editor over it;
    /// `J`/`K` step to the next or previous sheep; `x`/`R`/`L` arm a confirm
    /// against the pane's pinned sheep, `↵` confirms it and any other key
    /// cancels, through the same [`Self::on_armed_key`] prelude `on_key` runs.
    /// `j`/`k` and `g`/`G` scroll the config column through its own `Viewport`.
    /// Every other key is inert.
    pub(in crate::lookout::app) fn on_sheep_pane_key(&mut self, key: KeyPress) -> Effect {
        if let Some(effect) = self.on_armed_key(key) {
            return effect;
        }
        self.notice = None;
        // `o`, `m` and `/`: claimed ahead of this keymap by the same
        // dispatcher the full-screen pane runs, so the two screens cannot
        // answer one of them differently.
        if let Some(effect) = self.on_feed_key(key, Self::sheep_feed_mut) {
            return effect;
        }
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
            // Named here only to keep this keymap exhaustive: the guard above
            // already answered all three. See the same arm in
            // [`Self::on_bleats_key`].
            KeyPress::FilterStart | KeyPress::StreamCycle | KeyPress::LevelCycle => Effect::None,
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
            // `on_bleats_key`'s own `MatchPrev` arm, this does not clamp: the
            // embedded feed draws no scrollback of its own, and `window_range`
            // saturates a stale offset rather than reading past the end.
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
            | KeyPress::TextEdit(_)
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
            | KeyPress::ArmDelete => Effect::None,
            // The groups and the filed edit set belong to the config pane;
            // this read-only list has neither to switch or take back.
            | KeyPress::NextGroup
            | KeyPress::PrevGroup
            | KeyPress::NextField
            | KeyPress::PrevField
            | KeyPress::Group(_)
            | KeyPress::Undo
            | KeyPress::Continue => Effect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{apply_match, fixture_with_feed};
    use super::*;
    use shep_core::config::LevelRule;

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
                    .map(|text| crate::lookout::tail::TailLine::new(Stream::Out, text))
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
}
