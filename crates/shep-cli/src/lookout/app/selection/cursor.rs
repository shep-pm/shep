//! Where the cursor sits in that sequence, and what a move or a reseat
//! does to it.

use super::*;

impl App {
    /// Puts the selection back on a real row after the flock changed, and
    /// reports whether it moved.
    ///
    /// `previous_index` is where the selection sat before the change, read
    /// while the old map was still in place. A surviving key is left alone; a
    /// lost one falls to whatever now occupies that position, clamped to the
    /// last row rather than to row 0.
    pub(in crate::lookout::app) fn reseat(&mut self, previous_index: Option<usize>) -> bool {
        let rows = self.visible_rows();
        // `index_in`, not `flock.contains_key`: a selection the filter hides is
        // not seated, however present its id is. Must come before the emptiness
        // check below, which would otherwise return early for a query that
        // matches no sheep.
        if self.index_in(&rows).is_some() {
            return false;
        }
        let before = self.selected.clone();
        if rows.is_empty() {
            self.selected = None;
            return before != self.selected;
        }
        self.select_in(&rows, previous_index.unwrap_or(0), 1);
        before != self.selected
    }

    /// Moves the selection by `delta` rows and reports whether it moved.
    /// Clamped rather than wrapping.
    pub(in crate::lookout::app) fn select_by(&mut self, delta: isize) -> Effect {
        let rows = self.visible_rows();
        let Some(index) = self.index_in(&rows) else {
            return Effect::None;
        };
        let next = index.saturating_add_signed(delta);
        let direction = if delta < 0 { -1 } else { 1 };
        self.select_in(&rows, next, direction)
    }

    /// Selects the row at `index`, clamped to the flock, and reports whether
    /// that changed anything. `direction` is which way to search past a
    /// [`RowKey::Section`] header, and a header at row 0 searches forward.
    ///
    /// `Effect::None` when nothing changed: [`Effect::RefreshSelected`] reads
    /// two files and asks the shepherd for lambs, and a held `k` at the top
    /// must not do that once per keypress.
    pub(in crate::lookout::app) fn select_at(&mut self, index: usize, direction: isize) -> Effect {
        let rows = self.visible_rows();
        self.select_in(&rows, index, direction)
    }

    /// [`Self::select_at`] over a row sequence the caller already built, so a
    /// move that has to read the sequence anyway reads it once rather than
    /// once per step it takes.
    ///
    /// `rows` must be [`Self::visible_rows`] read before anything in the flock
    /// or the filter changed: a sequence taken at another moment seats the
    /// cursor on a row the table no longer draws.
    fn select_in(&mut self, rows: &[RowKey], index: usize, direction: isize) -> Effect {
        if rows.is_empty() {
            return Effect::None;
        }
        let mut index = index.min(rows.len() - 1);
        if matches!(rows[index], RowKey::Section(_)) {
            index = if direction < 0 && index > 0 {
                index - 1
            } else {
                index + 1
            };
        }
        let next = rows[index].clone();
        if Some(&next) == self.selected.as_ref() {
            return Effect::None;
        }
        self.selected = Some(next);
        // A frozen dashboard re-reading live log files would put content on
        // screen newer than the banner over it. The cursor still moves, and the
        // detail pane re-renders from the frozen listing.
        if matches!(self.link, Link::Lost { .. }) {
            return Effect::None;
        }
        Effect::RefreshSelected
    }

    /// The selected row, or `None` for an empty flock.
    #[must_use]
    pub fn selected(&self) -> Option<RowKey> {
        self.selected.clone()
    }

    /// Which row of [`Self::visible_rows`] the selection sits on, derived every
    /// call rather than stored.
    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        self.index_in(&self.visible_rows())
    }

    /// [`Self::selected_index`]'s search over a sequence the caller already
    /// built, on the same terms as [`Self::select_in`].
    fn index_in(&self, rows: &[RowKey]) -> Option<usize> {
        let key = self.selected.as_ref()?;
        rows.iter().position(|row| row == key)
    }

    /// The selected sheep's row, which the detail pane and the feed read.
    /// `None` for a [`RowKey::Group`] selection as well as for none at all: a
    /// group has no single sheep to describe.
    #[must_use]
    pub fn selected_row(&self) -> Option<&Row> {
        match &self.selected {
            Some(RowKey::Sheep(id)) => self.flock.get(id),
            _ => None,
        }
    }

    /// The selected row's app name: a sheep's own, or a group row's.
    ///
    /// Unlike [`Self::selected_row`] this answers for a group too: a config
    /// pane is about the stored spec every instance of an app shares, which
    /// a group row names exactly, while the detail pane and feed describe
    /// one process and have nothing to show for a group.
    #[must_use]
    pub fn selected_name(&self) -> Option<String> {
        match &self.selected {
            Some(RowKey::Group(name)) => Some(name.clone()),
            Some(RowKey::Sheep(id)) => self.flock.get(id).map(|row| row.info.name.clone()),
            // A fold names no single app config to fetch: `e` on a fold row
            // has nothing to open, the same answer a group gives the
            // detail pane and feed.
            Some(RowKey::Fold(_)) => None,
            Some(RowKey::Section(_)) => unreachable!("a header is never selectable"),
            None => None,
        }
    }

    /// Points the cursor at `key` without simulating keypresses.
    #[cfg(test)]
    pub(in crate::lookout::app) fn select(&mut self, key: RowKey) {
        self.selected = Some(key);
    }

    /// [`Self::select`]'s [`RowKey::Fold`] case, `pub(crate)` so a test in
    /// another module (`view::detail`'s own, in particular) can select a
    /// fold header without reaching into `App`'s private fields.
    #[cfg(test)]
    pub(crate) fn select_fold_for_tests(&mut self, name: &str) {
        self.select(RowKey::Fold(name.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::view::fixtures;

    /// A selected instance of a grouped app has no [`RowKey::Sheep`] row of
    /// its own once `F` collapses it under a [`RowKey::Group`] header: the
    /// selection must reseat onto something visible rather than sit on a row
    /// `visible_rows()` no longer draws.
    #[test]
    fn f_reseats_a_selection_that_folds_away() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Sheep(1));
        assert!(
            app.selected_index().is_some(),
            "sanity: seated before the toggle"
        );

        let _ = app.update(Msg::Key(KeyPress::FoldView));

        assert!(
            app.selected().is_some(),
            "F must not orphan the selection: {:?}",
            app.visible_rows()
        );
        assert!(app.selected_index().is_some());
    }

    /// `z` hides a fold's members and leaves its header, so a big flock can
    /// be read a fold at a time.
    #[test]
    fn z_collapses_a_fold_and_keeps_its_header() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select_fold_for_tests("edge");
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        let rows = app.visible_rows();
        assert!(
            rows.iter()
                .any(|r| matches!(r, RowKey::Fold(n) if n == "edge"))
        );
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, RowKey::Sheep(_)))
                .count(),
            0
        );
    }

    /// Pressed a second time on the same fold, `z` shows its members again.
    #[test]
    fn z_again_expands_a_collapsed_fold() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select_fold_for_tests("edge");
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        let rows = app.visible_rows();
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, RowKey::Sheep(_)))
                .count(),
            2,
            "both members are back: {rows:?}"
        );
    }

    /// `z` on anything other than a fold header is a no-op, even inside the
    /// fold view: a sheep row does not vanish because the wrong key was
    /// pressed near it.
    #[test]
    fn z_on_a_sheep_row_does_nothing() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select(RowKey::Sheep(1));
        let before = app.visible_rows();
        let _ = app.update(Msg::Key(KeyPress::Collapse));
        assert_eq!(
            app.visible_rows(),
            before,
            "the selection is a sheep row, not a fold header"
        );
    }

    #[test]
    fn selection_survives_a_poll_on_both_row_kinds() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Snapshot {
            rows: instanced_rows(),
            at: Instant::now(),
        });
        assert_eq!(app.selected(), Some(RowKey::Group("web".to_string())));
    }

    #[test]
    fn the_selection_clamps_at_both_ends() {
        let (mut app, _) = started();
        for _ in 0..10 {
            app.update(Msg::Key(KeyPress::SelectUp));
        }
        assert_eq!(
            app.selected_index(),
            Some(1),
            "up past the first row stays on it, below the header"
        );
        for _ in 0..10 {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
        assert_eq!(
            app.selected_index(),
            Some(3),
            "down past the last row stays on it"
        );
        app.update(Msg::Key(KeyPress::SelectFirst));
        assert_eq!(app.selected_index(), Some(1));
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.selected_index(), Some(3));
    }

    #[test]
    fn j_and_k_step_only_over_visible_rows() {
        let mut app = filtered("web");
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(1)),
            "api-web, the first visible sheep"
        );
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "web-worker, skipping the hidden cron and queue"
        );
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "clamped at the last visible row"
        );
        app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(app.selected(), Some(RowKey::Sheep(1)));
    }

    #[test]
    fn select_last_lands_on_the_last_visible_row() {
        let mut app = filtered("web");
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "web-worker, not queue at id 3"
        );
    }

    #[test]
    fn a_filter_that_hides_the_selection_clamps_to_the_nearest_visible_row() {
        let mut app = filtered("");
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "web-worker, position 3 of 4"
        );
        app.set_filter(TextBox::from("web"));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "position 3 clamps to the last visible row, which is web-worker"
        );
    }
}
