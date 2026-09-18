//! The bleats pane and the dashboard's own feed: the axes, the scroll and the
//! match box.

use super::*;

impl App {
    /// `b`: opens the full-screen bleats pane on the selected sheep. A
    /// group row or an empty selection is refused rather than opening on a
    /// stand-in: the pane is pinned to one sheep for its whole lifetime
    /// ([`BleatsPane`]), and a group has no single sheep to pin it to.
    pub(super) fn ask_for_bleats(&mut self) -> Effect {
        if let Some(sheep @ RowKey::Sheep(_)) = self.selected() {
            self.body = Body::Bleats(BleatsPane::new(sheep));
        }
        Effect::None
    }

    /// The bleats pane's own keymap, in force while [`Self::bleats_pane`] is
    /// `Some`. `Escape` drops the newest filter chip first, one axis at a
    /// time, and only closes the pane once none are left. `j`/`k` scroll a
    /// line, `ctrl-d`/`ctrl-u` a page, `G` jumps to the tail and resumes
    /// following, `f` toggles following explicitly, `w` toggles wrapping,
    /// and `n`/`N` step toward the newest or oldest matching line.
    pub(super) fn on_bleats_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::Escape => {
                let dropped_a_chip = self
                    .bleats_pane_mut()
                    .is_some_and(BleatsPane::drop_newest_chip);
                if !dropped_a_chip {
                    self.close_pane();
                }
                Effect::None
            }
            // Opens the match box: `on_text_key` routes the keystrokes that
            // follow to `on_bleats_text_key` once this pane owns
            // `InputMode::Text`. `begin_match_edit` remembers what the axis
            // held, so an abandoned edit restores it rather than losing it.
            KeyPress::FilterStart => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.begin_match_edit();
                }
                self.mode = InputMode::Text;
                Effect::None
            }
            // `o`: `None` (both streams) -> `Out` -> `Err` -> `None`.
            KeyPress::StreamCycle => {
                if let Some(pane) = self.bleats_pane_mut() {
                    let next = match pane.filters().stream {
                        None => Some(Stream::Out),
                        Some(Stream::Out) => Some(Stream::Err),
                        Some(Stream::Err) => None,
                    };
                    pane.set_stream(next);
                }
                Effect::None
            }
            // `m`: every `Level` in ascending order, then back to `None`.
            // The cycle must reach `None` again, or an operator who sets a
            // minimum can never see an unclassifiable line again without
            // closing the pane.
            KeyPress::LevelCycle => {
                if let Some(pane) = self.bleats_pane_mut() {
                    let next = match pane.filters().min_level {
                        None => Some(Level::Trace),
                        Some(Level::Trace) => Some(Level::Debug),
                        Some(Level::Debug) => Some(Level::Info),
                        Some(Level::Info) => Some(Level::Warn),
                        Some(Level::Warn) => Some(Level::Error),
                        Some(Level::Error) => None,
                    };
                    pane.set_min_level(next);
                }
                Effect::None
            }
            // `k`/`Up`: one line toward older lines.
            KeyPress::SelectUp => {
                self.scroll_bleats_back(1);
                Effect::None
            }
            // `j`/`Down`: one line toward the newest.
            KeyPress::SelectDown => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.scroll_down(1);
                }
                Effect::None
            }
            // `G`/`End`: the tail, and following resumes — that is what an
            // operator means by "go to the end".
            KeyPress::SelectLast => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.jump_to_end();
                }
                Effect::None
            }
            // `ctrl-u`: a page toward older lines. Sized through
            // `page_amount_up` rather than `pane.body_rows()` directly: see
            // that function's own doc for why a page is a line count under
            // wrap, not a raw row count.
            KeyPress::PageUp => {
                let amount = self
                    .bleats_pane()
                    .map_or(1, |pane| crate::lookout::view::bleats_full::page_amount_up(self, pane));
                self.scroll_bleats_back(amount);
                Effect::None
            }
            // `ctrl-d`: toward the newest line, and sized by its own
            // function. The backward count `ctrl-u` uses drops lines when
            // applied forward; see `page_amount_up`'s doc.
            KeyPress::PageDown => {
                let amount = self
                    .bleats_pane()
                    .map_or(1, |pane| crate::lookout::view::bleats_full::page_amount_down(self, pane));
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.page_down(amount);
                }
                Effect::None
            }
            // `f`: toggles following explicitly.
            KeyPress::FollowToggle => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.toggle_follow();
                }
                Effect::None
            }
            // `w`: toggles whether a long line wraps or truncates.
            KeyPress::WrapToggle => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.toggle_wrap();
                }
                Effect::None
            }
            // `n`: one match toward the newest line. A no-op with no match
            // axis set: see `BleatsPane::match_next`.
            KeyPress::MatchNext => {
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.match_next();
                }
                Effect::None
            }
            // `N`: the same, toward the oldest matching line, through
            // `scroll_bleats_back` so it takes the ceiling `k` and `ctrl-u`
            // take. Calling `match_prev` directly would climb past the
            // oldest surviving match and then `n`, `j` and `ctrl-d` would
            // all stop appearing to work until the offset drained. The
            // matcher guard is read here because the scroll happens here.
            KeyPress::MatchPrev => {
                let stepping = self
                    .bleats_pane()
                    .is_some_and(|pane| pane.filters().match_text().is_some());
                if stepping {
                    self.scroll_bleats_back(1);
                }
                Effect::None
            }
            // There is nothing on this screen `g`/`Home` can move to: the
            // window has no fixed start, only a tail. Left unbound rather
            // than aliased to `ctrl-u`'s page, which would give one key two
            // different meanings depending on how far a page happens to be.
            KeyPress::Help => self.open_keymap(),
            KeyPress::SelectFirst
            | KeyPress::Refresh
            | KeyPress::Action(_)
            | KeyPress::Confirm
            // Reach here only from text mode, already branched above in
            // `on_key`; listed so a new `KeyPress` variant cannot fall
            // silently into an arm that ignores it.
            | KeyPress::TextChar(_)
            | KeyPress::TextBackspace
            | KeyPress::TextApply
            | KeyPress::TextAbandon
            | KeyPress::Settings
            | KeyPress::Cycle
            | KeyPress::Edit
            | KeyPress::Remove
            | KeyPress::StepUp
            | KeyPress::StepDown
            // `F` and `z` belong to the flock table. Regrouping a table the
            // operator cannot see, while a log pane owns the screen, is a
            // change they would meet on closing it.
            | KeyPress::FoldView
            | KeyPress::Collapse
            | KeyPress::Bleats
            // The secrets pane's own keys; nothing to move or reload while
            // the bleats pane owns the screen instead.
            | KeyPress::Secrets
            | KeyPress::Reveal
            | KeyPress::Copy
            | KeyPress::TabPrev
            | KeyPress::TabNext
            | KeyPress::SecretDelete => Effect::None,
            // `NextGroup`/`Group`/`Undo`/`Continue` belong to the config
            // pane: no other screen has groups to walk, a filed edit set
            // to undo, or a close dialog to answer.
            | KeyPress::NextGroup
            | KeyPress::Group(_)
            | KeyPress::Undo
            | KeyPress::Continue => Effect::None,
        }
    }

    /// The bleats pane's match box, in force while it owns
    /// [`InputMode::Text`]. Follows the flock table's own text keymap
    /// ([`Self::on_filter_text_key`]) with one difference the design calls
    /// for: typing narrows live through [`BleatsPane::set_match`], but
    /// `TextAbandon` restores whatever [`BleatsPane::begin_match_edit`] saw
    /// rather than clearing the axis outright, since the axis may already
    /// have held a chip from an earlier edit.
    pub(super) fn on_bleats_text_key(&mut self, key: KeyPress) -> Effect {
        match key {
            KeyPress::Quit => Effect::Quit,
            KeyPress::TextChar(typed) => {
                if let Some(pane) = self.bleats_pane_mut() {
                    let mut text = pane.filters().match_text().unwrap_or_default().to_string();
                    text.push(typed);
                    pane.set_match(text);
                }
                Effect::None
            }
            KeyPress::TextBackspace => {
                if let Some(pane) = self.bleats_pane_mut() {
                    let mut text = pane.filters().match_text().unwrap_or_default().to_string();
                    text.pop();
                    pane.set_match(text);
                }
                Effect::None
            }
            KeyPress::TextApply => {
                self.mode = InputMode::Normal;
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.commit_match_edit();
                }
                Effect::None
            }
            KeyPress::TextAbandon => {
                self.mode = InputMode::Normal;
                if let Some(pane) = self.bleats_pane_mut() {
                    pane.abandon_match_edit();
                }
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// Scrolls the bleats pane back by `amount`, then holds the offset at the
    /// last value that changes the frame.
    ///
    /// The ceiling has to live here rather than on the pane: it depends on
    /// the surviving lines and their wrapped heights, which the pane cannot
    /// see. Without it `scroll_up` saturating-adds forever and `j` stops
    /// appearing to work, since the render clamps while the stored value
    /// keeps climbing.
    fn scroll_bleats_back(&mut self, amount: usize) {
        let ceiling = self.bleats_pane().map_or(0, |pane| {
            crate::lookout::view::bleats_full::max_scroll_offset(self, pane)
        });
        if let Some(pane) = self.bleats_pane_mut() {
            pane.scroll_up(amount);
            pane.clamp_scroll(ceiling);
        }
    }

    /// The row whose log files the feed should read: the bleats pane's
    /// pinned sheep while that pane is open, the sheep pane's own pinned
    /// sheep while its embedded feed is open, and the selection otherwise.
    ///
    /// The three are not the same and the difference is operator-visible.
    /// Both panes pin one sheep for their lifetime, but `Msg::Snapshot`
    /// reseats the selection whatever screen is showing, so a pinned sheep
    /// leaving the flock moves the selection to another one. Reading the
    /// selection here would then draw that other sheep's lines under a
    /// title still naming the pinned sheep, which is one sheep's output
    /// presented as another's.
    ///
    /// `None` once the pinned sheep is gone, so the pane shows its own
    /// "no longer in the flock" title over nothing rather than over somebody
    /// else's log.
    #[must_use]
    pub fn feed_row(&self) -> Option<&Row> {
        match self.bleats_pane() {
            Some(pane) => match pane.sheep() {
                RowKey::Sheep(id) => self.flock.get(id),
                _ => None,
            },
            None => match self.sheep_pane() {
                Some(pane) => match pane.feed_sheep() {
                    RowKey::Sheep(id) => self.flock.get(id),
                    _ => None,
                },
                None => self.selected_row(),
            },
        }
    }

    /// How the lines in [`Self::feed`] are read: the rules their own sheep
    /// declares, or this client's reading of a line when it declares none.
    ///
    /// Off [`Self::feed_row`], so the rules and the lines always come from
    /// the same sheep, and off the listing, so an edit reaches the pane on
    /// the next poll rather than when it is next opened.
    #[must_use]
    pub fn feed_classifier(&self) -> Classifier {
        Classifier::new(self.feed_row().map_or(&[], |row| &row.info.level_rules))
    }

    /// The selected sheep's most recent output, as of the last refresh.
    #[must_use]
    pub fn feed(&self) -> &crate::lookout::tail::Tail {
        &self.feed
    }

    /// The open bleats pane, or `None` on any other screen.
    #[must_use]
    pub fn bleats_pane(&self) -> Option<&BleatsPane> {
        match &self.body {
            Body::Bleats(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// [`Self::bleats_pane`]'s mutable twin, for `Escape`'s chip-by-chip
    /// backout. No key sets a filter axis yet; whichever task wires one
    /// needs this too.
    pub(super) fn bleats_pane_mut(&mut self) -> Option<&mut BleatsPane> {
        match &mut self.body {
            Body::Bleats(pane) => Some(pane),
            Body::FlockTable
            | Body::Settings(_)
            | Body::ConfigPane(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }
    }

    /// [`Self::bleats_pane_mut`], exposed past this module so a fixture can
    /// stack filters onto a pane it opened without walking `o` and `m`
    /// through their cycles or typing into the match box.
    #[cfg(test)]
    pub(crate) fn bleats_pane_mut_for_tests(&mut self) -> Option<&mut BleatsPane> {
        self.bleats_pane_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::view::fixtures;

    /// The pane opens on whatever was selected and pins it: full screen
    /// leaves no table to change a selection with.
    /// The feed follows the pinned sheep, not the selection.
    ///
    /// The pane pins one sheep for its lifetime, but `Msg::Snapshot` reseats
    /// the selection whatever screen is showing. So a pinned sheep leaving
    /// the flock moved the selection to another one, and the next refresh
    /// read that sheep's log files while the title still named the pinned
    /// one: one sheep's output under another sheep's heading.
    ///
    /// Asserts the row the feed reads, which is the thing that was wrong.
    /// Asserting the rendered title would have passed throughout, because
    /// the title was always right.
    #[test]
    fn the_feed_follows_the_pinned_sheep_when_the_selection_moves() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(9, "web", ProcStatus::Online).build(),
                ProcessInfo::builder(4, "billing", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        app.select(RowKey::Sheep(9));
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        assert_eq!(
            app.feed_row().map(|row| row.info.id),
            Some(9),
            "the pane opened on 9"
        );

        // 9 leaves the flock. The reseat moves the selection to 4.
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(4, "billing", ProcStatus::Online).build()],
            at: Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "the selection did move, which is the setup for the bug"
        );
        assert_eq!(
            app.feed_row().map(|row| row.info.id),
            None,
            "and the feed reads nothing rather than billing's log"
        );
    }

    /// The same regression as [`the_feed_follows_the_pinned_sheep_when_the_selection_moves`],
    /// through the sheep pane's own embedded feed rather than the
    /// full-screen one: `Confirm` pins the pane to sheep 9, `feed_row`
    /// answers 9 while it is open, sheep 9 then leaves the flock and the
    /// reseat moves the selection to 4, and `feed_row` must still answer
    /// `None` (sheep 9 is gone) rather than 4 (the selection's own new row).
    /// Before this task, `feed_row` had no `Body::Sheep` branch at all and
    /// fell through to `self.selected_row()` unconditionally, so this would
    /// have read billing's log under a pane still naming `web`.
    #[test]
    fn the_feed_row_follows_the_sheep_panes_own_pinned_sheep_when_the_selection_moves() {
        let mut app = fixtures::app_with(
            vec![
                ProcessInfo::builder(9, "web", ProcStatus::Online).build(),
                ProcessInfo::builder(4, "billing", ProcStatus::Online).build(),
            ],
            fixtures::plain(),
        );
        app.select(RowKey::Sheep(9));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(
            app.feed_row().map(|row| row.info.id),
            Some(9),
            "the pane opened on 9"
        );

        // 9 leaves the flock. The reseat moves the selection to 4.
        let _ = app.update(Msg::Snapshot {
            rows: vec![ProcessInfo::builder(4, "billing", ProcStatus::Online).build()],
            at: Instant::now(),
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "the selection did move, which is the setup for the bug"
        );
        assert_eq!(
            app.feed_row().map(|row| row.info.id),
            None,
            "and the feed reads nothing rather than billing's log"
        );
    }

    #[test]
    fn b_opens_the_pane_on_the_selected_sheep() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let pane = app.bleats_pane().expect("the pane is open");
        assert!(matches!(pane.sheep(), RowKey::Sheep(id) if *id == 9));
    }

    /// `close_pane` always lands on the dashboard, never on whatever screen
    /// preceded the pane. Same rule the config pane follows.
    #[test]
    fn esc_from_the_bleats_pane_lands_on_the_dashboard() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// `Escape` with a chip set drops the chip and leaves the pane open; only
    /// the next one closes it. Both halves in one test, because the guard in
    /// `on_bleats_key` is invisible to a test that never sets a chip: with a
    /// freshly opened pane `drop_newest_chip` returns `false` either way, so
    /// removing the guard entirely still passes every other `esc` test in this
    /// file. Measured, not assumed.
    #[test]
    fn esc_drops_a_chip_before_it_closes_the_pane() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut()
            .expect("the pane is open")
            .set_min_level(Some(crate::lookout::level::Level::Warn));

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            matches!(app.body(), Body::Bleats(_)),
            "the first Escape spends the chip and keeps the pane"
        );
        assert!(
            app.bleats_pane()
                .expect("still open")
                .filters()
                .min_level
                .is_none(),
            "the chip it spent was the level one"
        );

        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            matches!(app.body(), Body::FlockTable),
            "with no chips left the next Escape closes"
        );
    }

    /// `o` cycles the stream axis through its three states and back. Three
    /// presses return to where it began, which is what makes it a cycle
    /// rather than a toggle that strands the operator on `err`.
    #[test]
    fn o_cycles_the_stream_axis_and_returns_to_both() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        assert!(app.bleats_pane().expect("open").filters().stream.is_none());
        let _ = app.update(Msg::Key(KeyPress::StreamCycle));
        let first = app.bleats_pane().expect("open").filters().stream;
        assert!(first.is_some(), "one press sets an axis");
        let _ = app.update(Msg::Key(KeyPress::StreamCycle));
        let _ = app.update(Msg::Key(KeyPress::StreamCycle));
        assert!(
            app.bleats_pane().expect("open").filters().stream.is_none(),
            "three presses land back on both"
        );
    }

    /// `m` raises the minimum level and eventually clears it. The unset state
    /// has to be reachable by key, or an operator who sets a minimum can never
    /// see unlevelled output again without closing the pane.
    #[test]
    fn m_cycles_the_level_axis_back_to_unset() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let mut seen_some = false;
        for _ in 0..8 {
            let _ = app.update(Msg::Key(KeyPress::LevelCycle));
            if app
                .bleats_pane()
                .expect("open")
                .filters()
                .min_level
                .is_some()
            {
                seen_some = true;
            }
        }
        assert!(seen_some, "the cycle passes through a set minimum");
        // Whatever the cycle length, it must return to unset within one lap.
        let mut cleared = false;
        for _ in 0..8 {
            let _ = app.update(Msg::Key(KeyPress::LevelCycle));
            if app
                .bleats_pane()
                .expect("open")
                .filters()
                .min_level
                .is_none()
            {
                cleared = true;
                break;
            }
        }
        assert!(cleared, "the cycle returns to unset");
    }

    /// Scrolling back stops the follow, or the next refresh undoes the
    /// operator's keypress.
    #[test]
    fn scrolling_back_stops_following() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        assert!(app.bleats_pane().expect("open").following());
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert!(
            !app.bleats_pane().expect("open").following(),
            "one line back is enough to mean the operator took over"
        );
    }

    /// `G` is the way back to the live tail, so it restores following as well
    /// as jumping.
    #[test]
    fn g_returns_to_the_end_and_resumes_following() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let pane = app.bleats_pane().expect("open");
        assert!(pane.following(), "G resumes the follow");
        assert_eq!(pane.scroll_offset(), 0, "and lands on the newest line");
    }

    /// A filter that hides most of the window must not leave the offset
    /// pointing past the end of what survives.
    #[test]
    fn a_narrowing_filter_clamps_the_scroll_offset() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        let _ = app.update(Msg::Key(KeyPress::PageUp));
        let _ = app.update(Msg::Key(KeyPress::PageUp));

        // One survivor, far fewer than the offset two pages back. Without
        // the clamp the skip underflows: it panics in debug and wraps in
        // release, and a wrapped skip yields no feed lines at all.
        app.bleats_pane_mut_for_tests()
            .expect("open")
            .set_match("line-119".to_string());
        let text = fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(
            &app, 160, 40,
        ));

        // Asserts a rendered FEED line, tagged `out`, not merely that the
        // text appears: the filter row echoes the matcher back as its own
        // `match line-119` chip, so `contains("line-119")` passes even when
        // every feed line was skipped away. `!text.is_empty()` was weaker
        // still, and passed in release with the clamp removed because the
        // title and the chip alone kept the buffer non-empty.
        assert!(
            text.contains("out  line-119"),
            "the one surviving line is drawn in the body: {text}"
        );
    }

    /// `SelectUp`/`SelectDown` actually move the window, not just the
    /// `following` flag: pins which lines are on screen before and after,
    /// so a wrong direction or an off-by-one is caught rather than passing
    /// on the presence of any text at all.
    #[test]
    fn select_up_and_down_move_which_lines_are_on_screen() {
        let mut app = fixtures::bleats_pane_with_lines(20);
        app.note_body_rows(6); // 1 title row + 5 body rows
        let before =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        assert!(
            before.contains("line-19"),
            "starts pinned to the tail: {before}"
        );
        assert!(
            !before.contains("line-14"),
            "one line older than the window: {before}"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let after =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        assert!(
            after.contains("line-14") && !after.contains("line-19"),
            "one line back drops the newest line and reveals the one above the old window: {after}"
        );

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let restored =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        assert_eq!(
            restored, before,
            "one line forward undoes the one line back"
        );
    }

    /// `ctrl-u`/`ctrl-d` move by a body height rather than a line, and the
    /// body height is what `note_body_rows` last reported, not a hardcoded
    /// guess.
    /// Two pages back leave no line unseen, with a filter chip on screen.
    ///
    /// The chip costs a row, so the body is two rows shorter than the pane,
    /// not one. A jump sized to the pane rather than the body skips a line
    /// between consecutive pages: it belongs to neither window and `ctrl-u`
    /// alone never renders it. Paging back down is symmetric either way,
    /// which is why the round trip looked fine and the gap did not.
    ///
    /// Asserts every line across the two windows, not the offset: an
    /// operator reading history by paging silently misses the gap, so a
    /// test that only watched the number move would too.
    /// Wrapped paging leaves no line unseen either, over a feed whose older
    /// lines are far longer than its newest.
    ///
    /// Measured: with the page sized from the tail, three steps into the
    /// wrapped stretch showed `old-32..old-35` where the step before showed
    /// `new-4..new-15`, skipping eight lines no screen ever drew, and the
    /// next step skipped eight more. Sized from the current window instead,
    /// the same walk is contiguous.
    ///
    /// That is the off-by-one page size one level harder: sized from the
    /// wrong place the jump was one row too many, here it is most of a
    /// screen. Both were invisible for the same reason, that paging back
    /// cancels the error out.
    ///
    /// `note_body_width` matters as much as `note_body_rows` here. The
    /// wrap-aware path is skipped entirely while the pane's width is `0`,
    /// which is what a test that never reports one leaves it as, so a test
    /// missing that call passes against a page size that was never wrapped.
    ///
    /// Walks in one direction and asserts every line across the windows,
    /// because paging back is symmetric and would hide the gap.
    /// Over-scrolling does not make `j` stop working.
    ///
    /// `scroll_up` saturating-adds and the clamp lived only in the render, so
    /// the stored offset climbed past anything that changes the frame.
    /// Measured before the ceiling: a 20-line feed with a 5-row body left the
    /// offset at 39 after 40 `k` presses, where 15 was the most that did
    /// anything, and the operator then pressed `j` 25 times before the window
    /// moved. `G` and `f` escape that; `j` is the reflex and it did nothing.
    /// Holding `N` past the oldest match does not deaden `n` either.
    ///
    /// `k` and `ctrl-u` route through the clamp; `N` called `match_prev`
    /// directly and climbed past it. The existing `n`/`N` test presses `N`
    /// once, so it never reached the ceiling.
    #[test]
    fn over_stepping_back_through_matches_does_not_deaden_the_step_forward() {
        let mut app = fixtures::bleats_pane_with_lines(20);
        app.note_body_rows(6);
        app.note_body_width(80);
        app.bleats_pane_mut_for_tests()
            .expect("open")
            .set_match("line".to_string());

        for _ in 0..40 {
            let _ = app.update(Msg::Key(KeyPress::MatchPrev));
        }
        let parked =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        let _ = app.update(Msg::Key(KeyPress::MatchNext));
        let after_one =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        assert_ne!(parked, after_one, "one step forward has to move the window");
    }

    /// A frozen lookout with the pane open stops re-reading the log files.
    ///
    /// `Msg::Bleats` throws the tail away while the link is down, so every
    /// read was work done and discarded once a second. `Msg::Snapshot` and
    /// `select_at` already guard on the same thing.
    #[test]
    fn a_frozen_lookout_does_not_re_read_the_pane_s_log() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let now = Instant::now();
        assert_eq!(app.update(Msg::Tick { now }), Effect::RefreshFeed);

        let _ = app.update(Msg::Frozen {
            at_local: "2026-09-08 09:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.update(Msg::Tick { now }),
            Effect::None,
            "a read whose result Msg::Bleats discards is not worth doing"
        );
    }

    #[test]
    fn over_scrolling_back_does_not_deaden_the_scroll_forward() {
        let mut app = fixtures::bleats_pane_with_lines(20);
        app.note_body_rows(6);
        app.note_body_width(80);

        for _ in 0..40 {
            let _ = app.update(Msg::Key(KeyPress::SelectUp));
        }
        let parked =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));

        let _ = app.update(Msg::Key(KeyPress::SelectDown));
        let after_one =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        assert_ne!(
            parked, after_one,
            "one press forward has to move the window, however far back the \
             operator scrolled"
        );
    }

    #[test]
    fn wrapped_pages_leave_no_line_unseen() {
        let mut app = fixtures::bleats_pane_with_mixed_line_lengths();
        let _ = app.update(Msg::Key(KeyPress::WrapToggle));
        app.note_body_rows(13);
        app.note_body_width(60);

        let mut seen = String::new();
        for _ in 0..12 {
            seen.push_str(&fixtures::render_all(
                &crate::lookout::view::bleats_full::draw_lines(&app, 60, 13),
            ));
            let _ = app.update(Msg::Key(KeyPress::PageUp));
        }
        for n in 30..40 {
            assert!(
                seen.contains(&format!("old-{n} ")),
                "old-{n} fell between two wrapped pages"
            );
        }
    }

    /// The same, walking `ctrl-d` instead. Its own test because its own
    /// arithmetic: a single shared page size drops lines in one direction
    /// whichever way it is measured.
    ///
    /// This one was missing when the wrapped-page fix landed, and that is
    /// exactly why the fix was half a fix. The sibling test above presses
    /// only `PageUp`, so a `PageDown` that skipped eight lines a step passed
    /// the whole suite.
    #[test]
    fn wrapped_pages_down_leave_no_line_unseen() {
        /// The `old-N`/`new-N` ids the pane is drawing, in order.
        fn shown(app: &App) -> Vec<String> {
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(app, 60, 13))
                .split_whitespace()
                .filter(|word| word.starts_with("old-") || word.starts_with("new-"))
                .map(str::to_string)
                .collect()
        }

        let mut app = fixtures::bleats_pane_with_mixed_line_lengths();
        let _ = app.update(Msg::Key(KeyPress::WrapToggle));
        app.note_body_rows(13);
        app.note_body_width(60);

        // Into the wrapped stretch, then forward one page at a time.
        for _ in 0..6 {
            let _ = app.update(Msg::Key(KeyPress::PageUp));
        }

        // Consecutive windows, not eventual coverage. A long enough walk
        // sees every line whatever the step size, so the gap only shows in
        // the join between one window and the next.
        for step in 0..5 {
            let before = shown(&app);
            let _ = app.update(Msg::Key(KeyPress::PageDown));
            let after = shown(&app);
            let (Some(last), Some(first)) = (before.last(), after.first()) else {
                continue;
            };
            // The feed is `old-0..old-39` then `new-0..new-39`, so this is
            // each id's position in it.
            let position = |id: &str| -> usize {
                let (prefix, n) = id.split_once('-').expect("id-N");
                let n: usize = n.parse().expect("a number");
                if prefix == "old" { n } else { 40 + n }
            };
            assert!(
                position(first) <= position(last) + 1,
                "step {step} jumped from {last} to {first}, leaving a gap: \
                 {before:?} then {after:?}"
            );
            // Two-sided. The check above catches a page that steps over
            // lines; this one catches a page that barely steps at all, which
            // `page_amount_down` returning a constant 1 would do while
            // satisfying the first assertion trivially.
            assert!(
                position(first) + 1 >= position(before[0].as_str()) + before.len(),
                "step {step} moved by almost nothing, so it is not a page: \
                 {before:?} then {after:?}"
            );
        }
    }

    #[test]
    fn consecutive_pages_leave_no_line_unseen_while_a_chip_is_showing() {
        let mut app = fixtures::bleats_pane_with_lines(40);
        app.bleats_pane_mut_for_tests()
            .expect("open")
            .set_match("line".to_string());
        app.note_body_rows(6); // 1 title + 1 filter row + 4 body rows

        let first =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        let _ = app.update(Msg::Key(KeyPress::PageUp));
        let second =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        let _ = app.update(Msg::Key(KeyPress::PageUp));
        let third =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));

        let seen = format!("{first}{second}{third}");
        // The three windows are contiguous, so every line from the oldest
        // one drawn through the newest must appear in one of them.
        for n in 28..=39 {
            assert!(
                seen.contains(&format!("line-{n}")),
                "line-{n} fell between two pages: {seen}"
            );
        }
    }

    #[test]
    fn page_up_and_down_move_by_a_body_height() {
        let mut app = fixtures::bleats_pane_with_lines(20);
        app.note_body_rows(6); // 1 title row + 5 body rows
        let before =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        assert!(
            before.contains("line-19"),
            "starts pinned to the tail: {before}"
        );

        let _ = app.update(Msg::Key(KeyPress::PageUp));
        let after =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        assert!(
            after.contains("line-10") && !after.contains("line-15"),
            "a page is 5 body rows, so one page up lands on 10..=14, not 1 row back: {after}"
        );
        assert!(
            !app.bleats_pane().expect("open").following(),
            "ctrl-u is backward movement too"
        );

        let _ = app.update(Msg::Key(KeyPress::PageDown));
        let restored =
            fixtures::render_all(&crate::lookout::view::bleats_full::draw_lines(&app, 80, 6));
        assert_eq!(restored, before, "one page down undoes one page up");
    }

    /// `f` toggles following explicitly, and turning it back on snaps to the
    /// tail rather than leaving the view wherever it was scrolled to.
    #[test]
    fn f_toggles_following_and_resuming_snaps_to_the_tail() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert!(!app.bleats_pane().expect("open").following());

        let _ = app.update(Msg::Key(KeyPress::FollowToggle));
        let pane = app.bleats_pane().expect("open");
        assert!(pane.following(), "f turned it back on");
        assert_eq!(pane.scroll_offset(), 0, "and snapped to the tail");

        let _ = app.update(Msg::Key(KeyPress::FollowToggle));
        assert!(
            !app.bleats_pane().expect("open").following(),
            "f is a toggle, not a one-way switch"
        );
    }

    /// `n` with no match axis set does nothing, rather than quietly becoming
    /// a line-movement key.
    /// `n` and `N` step between matches, in opposite directions, and both
    /// stop the follow.
    ///
    /// The no-matcher case below covers only the inert branch, so a swapped
    /// direction, a wrong step, or a `following` regression would all have
    /// shipped unseen.
    /// A wrapped line's height is measured in display columns, not `char`s.
    ///
    /// Sixty full-width characters occupy 120 columns, so at this width they
    /// wrap to about twice the rows sixty ASCII characters would. Every other
    /// wrap test here is ASCII, where the two counts agree and a regression
    /// to `chars().count()` would pass unnoticed. This repo has fixed that
    /// same bug on two other branches.
    #[test]
    fn a_wide_character_line_wraps_by_columns_not_char_count() {
        let mut wide = fixtures::bleats_pane_with_a_wide_line();
        let _ = wide.update(Msg::Key(KeyPress::WrapToggle));
        wide.note_body_rows(30);
        wide.note_body_width(40);
        let wide_rows = crate::lookout::view::bleats_full::draw_lines(&wide, 40, 30).len();

        // 60 double-width characters are 120 display columns. The body is
        // 40 wide less the 5-column stream tag, so 35, and 120 columns need
        // 4 rows. Counting `char`s instead gives 60 over 35, which is 2.
        // The title takes one more row, so 5 total by columns and 3 by
        // `char`s: the assertion separates the two.
        assert!(
            wide_rows >= 5,
            "wrapped by columns that is 4 body rows plus a title; by \
             `char`s it would be 2. Got {wide_rows}"
        );
    }

    #[test]
    fn n_and_shift_n_step_between_matches_in_opposite_directions() {
        let mut app = fixtures::bleats_pane_with_lines(40);
        app.note_body_rows(6);
        app.bleats_pane_mut_for_tests()
            .expect("open")
            .set_match("line".to_string());
        assert!(app.bleats_pane().expect("open").following());

        let _ = app.update(Msg::Key(KeyPress::MatchPrev));
        let back = app.bleats_pane().expect("open").scroll_offset();
        assert!(back > 0, "N steps toward older matches");
        assert!(
            !app.bleats_pane().expect("open").following(),
            "stepping back is backward movement, so the follow stops"
        );

        let _ = app.update(Msg::Key(KeyPress::MatchNext));
        assert!(
            app.bleats_pane().expect("open").scroll_offset() < back,
            "n steps the other way"
        );
    }

    #[test]
    fn n_without_a_matcher_does_nothing() {
        let mut app = fixtures::bleats_pane_with_lines(120);
        let before = app.bleats_pane().expect("open").scroll_offset();
        let _ = app.update(Msg::Key(KeyPress::MatchNext));
        assert_eq!(app.bleats_pane().expect("open").scroll_offset(), before);
        assert!(app.bleats_pane().expect("open").following());
    }

    /// Wrapping a long line makes it occupy more rows than one, which is the
    /// whole point, and the pane must still draw inside its area.
    #[test]
    fn a_wrapped_line_occupies_more_rows_and_stays_in_the_area() {
        let mut app = fixtures::bleats_pane_with_long_line();
        let unwrapped = fixtures::draw_lines(&app, 80, 20).len();
        let _ = app.update(Msg::Key(KeyPress::WrapToggle));
        let wrapped = fixtures::draw_lines(&app, 80, 20);
        assert!(wrapped.len() <= 20, "never draws past its own height");
        assert!(
            wrapped.iter().filter(|line| !line.spans.is_empty()).count() >= unwrapped,
            "wrapping uses at least as many rows as not wrapping"
        );
    }

    /// The brief's own test above (`a_wrapped_line_occupies_more_rows_and_stays_in_the_area`)
    /// only asserts `>=`, which a `w` that did nothing at all would still
    /// satisfy: the unwrapped and "wrapped" renders would be identical, and
    /// identical passes `>=` too. This pins the number changing, not merely
    /// never shrinking.
    #[test]
    fn toggling_wrap_actually_changes_how_many_rows_a_long_line_draws() {
        let mut app = fixtures::bleats_pane_with_long_line();
        let unwrapped = fixtures::draw_lines(&app, 80, 20).len();
        let _ = app.update(Msg::Key(KeyPress::WrapToggle));
        let wrapped = fixtures::draw_lines(&app, 80, 20).len();
        assert!(
            wrapped > unwrapped,
            "wrap must add rows for a line too long for one, not just permit them: \
             {unwrapped} unwrapped rows, {wrapped} wrapped: got the same window"
        );
    }

    /// `/` opens the match input rather than doing nothing, which is what it
    /// did when this pane first shipped.
    #[test]
    fn slash_opens_the_match_input_in_the_bleats_pane() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        assert_eq!(app.mode(), InputMode::Text, "typing goes to the pane");
    }

    /// Typing into the match box narrows live, the same as the flock
    /// table's own `/` box: the operator sees the filter row react to every
    /// keystroke rather than only after `Enter`.
    #[test]
    fn typing_in_the_match_box_narrows_live() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        let _ = app.update(Msg::Key(KeyPress::TextChar('p')));
        let _ = app.update(Msg::Key(KeyPress::TextChar('o')));
        assert_eq!(
            app.bleats_pane().expect("open").filters().match_text(),
            Some("po")
        );
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(
            app.bleats_pane().expect("open").filters().match_text(),
            Some("po"),
            "TextApply keeps what was already applied live"
        );
    }

    /// `TextAbandon` restores the match axis to what it held before the box
    /// opened, discarding whatever was typed since, rather than clearing it
    /// outright the way the flock table's own filter box does.
    #[test]
    fn abandoning_the_match_box_restores_the_axis_it_had_before() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        app.bleats_pane_mut()
            .expect("open")
            .set_match("pool".to_string());

        let _ = app.update(Msg::Key(KeyPress::FilterStart));
        let _ = app.update(Msg::Key(KeyPress::TextChar('x')));
        assert_eq!(
            app.bleats_pane().expect("open").filters().match_text(),
            Some("poolx"),
            "typing narrowed live"
        );
        let _ = app.update(Msg::Key(KeyPress::TextAbandon));
        assert_eq!(app.mode(), InputMode::Normal);
        assert_eq!(
            app.bleats_pane().expect("open").filters().match_text(),
            Some("pool"),
            "abandon restores what the axis held before the edit"
        );
    }

    /// `o` and `m` are global bindings, so the dashboard sees them too; with
    /// no bleats pane open there is no stream or level axis to cycle, and
    /// the reducer says so explicitly rather than falling through to a
    /// wildcard, the same way it already does for `KeyPress::Bleats` here.
    #[test]
    fn stream_and_level_cycle_are_inert_on_the_dashboard() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        assert_eq!(app.update(Msg::Key(KeyPress::StreamCycle)), Effect::None);
        assert_eq!(app.update(Msg::Key(KeyPress::LevelCycle)), Effect::None);
        assert!(matches!(app.body(), Body::FlockTable));
    }

    /// `b` with nothing selected asks for nothing, the way `e` does.
    #[test]
    fn b_with_nothing_selected_opens_no_pane() {
        let mut app = fixtures::app_with(Vec::new(), fixtures::plain());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        assert!(app.bleats_pane().is_none());
    }
}
