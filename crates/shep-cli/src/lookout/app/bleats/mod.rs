//! The bleats pane and the dashboard's own feed: the axes, the scroll and the
//! match box. The keys this pane and the sheep pane's embedded feed answer
//! the same way live in [`super::feed_keys`].
//!
//! Three modules, one per reason to change: this one opens the pane on the
//! selected sheep and holds the accessors for the pane and for the feed it
//! reads; [`keys`] answers the pane's own keymap and the scroll back through
//! the tail; [`match_box`] keeps the match axis apart from that scroll.

use super::*;

mod keys;
mod match_box;

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

    /// The row whose log files the feed reads: the pinned sheep of whichever
    /// feed is open, else the selection. `None` once the pinned sheep is gone.
    ///
    /// Both panes pin one sheep for their lifetime, and `Msg::Snapshot`
    /// reseats the selection whatever screen is showing, so reading the
    /// selection would draw another sheep's lines under the pinned one's title.
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

    /// The open bleats pane, or `None` on any other screen:
    /// [`Body::bleats_pane`] over this app's body.
    #[must_use]
    pub fn bleats_pane(&self) -> Option<&BleatsPane> {
        self.body.bleats_pane()
    }

    /// [`Self::bleats_pane`]'s mutable twin, for `Escape`'s chip-by-chip
    /// backout. No key sets a filter axis yet; whichever task wires one
    /// needs this too.
    pub(super) fn bleats_pane_mut(&mut self) -> Option<&mut BleatsPane> {
        self.body.bleats_pane_mut()
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
    /// leaves no table to change a selection with. The feed follows the
    /// pinned sheep, not the selection.
    ///
    /// Asserts the row the feed reads. The title was always right, so
    /// asserting it would have passed throughout.
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
    /// through the sheep pane's own embedded feed. `feed_row` must answer
    /// `None` once the pinned sheep is gone, not the selection's new row.
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

    /// `b` with nothing selected asks for nothing, the way `e` does.
    #[test]
    fn b_with_nothing_selected_opens_no_pane() {
        let mut app = fixtures::app_with(Vec::new(), fixtures::plain());
        let _ = app.update(Msg::Key(KeyPress::Bleats));
        assert!(app.bleats_pane().is_none());
    }
}
