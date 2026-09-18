//! Which rows are visible, and where the cursor sits among them.

use super::*;

/// One flock entry as `visible_rows` sorts and partitions it: name, instance
/// slot, id, and whether it names a dog.
type RowEntry<'a> = (&'a str, Option<u32>, u32, bool);

/// Splits `entries` into contiguous runs sharing a name, in the order they
/// already sit in (name-sorted, so a run is always one unbroken slice).
/// [`App::push_fold_group_rows`] and [`App::push_grouped_rows`] walk the same
/// runs and then disagree on what to do with one, which is the two-levels
/// rule itself and stays out of this helper.
fn name_runs<'a>(entries: &'a [RowEntry<'a>]) -> impl Iterator<Item = &'a [RowEntry<'a>]> {
    let mut rest = entries;
    std::iter::from_fn(move || {
        let name = rest.first()?.0;
        let end = rest
            .iter()
            .position(|entry| entry.0 != name)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some(run)
    })
}

impl App {
    /// The rows the table draws, in `(name, instance, id)` order: the whole
    /// flock, or whatever the filter leaves of it.
    ///
    /// Under [`Grouping::Flat`] that is a "Flock" section and a "Dogs"
    /// section. Under [`Grouping::ByFold`] it is one [`RowKey::Fold`] header
    /// per fold, a "no fold" section and a "Dogs" section, built by
    /// [`Self::push_fold_rows`].
    ///
    /// A [`RowKey::Group`] header comes immediately before its own
    /// [`RowKey::Sheep`] entries, and a [`RowKey::Section`] header only when
    /// its side has a row to introduce. The sort key is total on purpose: the
    /// table repolls every two seconds, and a partial one would let two
    /// instances swap places under the cursor. Every cursor move reads this
    /// sequence and nothing else.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<RowKey> {
        let needle = self.filter.to_lowercase();
        let mut visible: Vec<RowEntry<'_>> = self
            .flock
            .iter()
            .filter(|(_, row)| needle.is_empty() || row.info.name.to_lowercase().contains(&needle))
            .map(|(id, row)| {
                (
                    row.info.name.as_str(),
                    row.info.instance,
                    *id,
                    row.info.dog.is_some(),
                )
            })
            .collect();
        visible.sort_unstable_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let (dogs, sheep): (Vec<_>, Vec<_>) = visible.into_iter().partition(|entry| entry.3);

        let mut out = Vec::new();
        match self.grouping {
            Grouping::Flat => {
                if !sheep.is_empty() {
                    out.push(RowKey::Section("Flock"));
                    self.push_grouped_rows(&sheep, &mut out);
                }
            }
            Grouping::ByFold => self.push_fold_rows(&sheep, &mut out),
        }
        if !dogs.is_empty() {
            out.push(RowKey::Section("Dogs"));
            self.push_grouped_rows(&dogs, &mut out);
        }
        out
    }

    /// The `ByFold` half of [`Self::visible_rows`]: `sheep` gathered under
    /// its fold, each fold's own name-order header pushed before
    /// [`Self::push_fold_group_rows`] lays down its members, so a
    /// multi-instance app inside a fold keeps its own [`RowKey::Group`] row
    /// rather than flattening to one row per instance.
    ///
    /// Sheep carrying no fold land under [`RowKey::Section`]`("no fold")`
    /// instead of a [`RowKey::Fold`]: `SelectorSpec::Fold` cannot name "no
    /// fold" on the wire, so a header rather than an action target is the
    /// honest answer.
    ///
    /// A fold in [`Self::collapsed_folds`] still gets its own
    /// [`RowKey::Fold`] header; only [`Self::push_fold_group_rows`]'s call
    /// is skipped, so `z` hides the members and nothing else.
    fn push_fold_rows(&self, sheep: &[RowEntry<'_>], out: &mut Vec<RowKey>) {
        let mut folds: BTreeMap<String, Vec<RowEntry<'_>>> = BTreeMap::new();
        let mut unfoldered: Vec<RowEntry<'_>> = Vec::new();
        for entry in sheep {
            match self
                .flock
                .get(&entry.2)
                .and_then(|row| row.info.fold.clone())
            {
                Some(fold) => folds.entry(fold).or_default().push(*entry),
                None => unfoldered.push(*entry),
            }
        }
        for (fold, members) in &folds {
            out.push(RowKey::Fold(fold.clone()));
            if !self.collapsed_folds.contains(fold) {
                self.push_fold_group_rows(members, out);
            }
        }
        if !unfoldered.is_empty() {
            out.push(RowKey::Section("no fold"));
            self.push_fold_group_rows(&unfoldered, out);
        }
    }

    /// [`Self::push_grouped_rows`]'s fold-scoped twin: a grouped app
    /// collapses to its own [`RowKey::Group`] header alone, with no
    /// [`RowKey::Sheep`] row following it, so a fold never nests three
    /// levels deep (fold, app, instance). An app with no group still gets
    /// its ordinary sheep row, exactly as the flat path would draw it.
    fn push_fold_group_rows(&self, entries: &[RowEntry<'_>], out: &mut Vec<RowKey>) {
        for run in name_runs(entries) {
            let name = run[0].0;
            if self.is_grouped(name) {
                out.push(RowKey::Group(name.to_string()));
            } else {
                out.extend(run.iter().map(|entry| RowKey::Sheep(entry.2)));
            }
        }
    }

    /// Appends `entries`' rows to `out`, splicing a [`RowKey::Group`] header
    /// before a grouped app's instances.
    fn push_grouped_rows(&self, entries: &[RowEntry<'_>], out: &mut Vec<RowKey>) {
        for run in name_runs(entries) {
            let name = run[0].0;
            if self.is_grouped(name) {
                out.push(RowKey::Group(name.to_string()));
            }
            out.extend(run.iter().map(|entry| RowKey::Sheep(entry.2)));
        }
    }

    /// Whether `row` names a dog. A header and a group row are neither.
    #[cfg(test)]
    fn is_dog_row(&self, row: &RowKey) -> bool {
        match row {
            RowKey::Sheep(id) => self.flock.get(id).is_some_and(|r| r.info.dog.is_some()),
            RowKey::Group(_) | RowKey::Section(_) | RowKey::Fold(_) => false,
        }
    }

    pub(super) fn visible_len(&self) -> usize {
        self.visible_rows().len()
    }

    /// Puts the selection back on a real row after the flock changed, and
    /// reports whether it moved.
    ///
    /// `previous_index` is where the selection sat before the change, read
    /// while the old map was still in place. A surviving key is left alone; a
    /// lost one falls to whatever now occupies that position, clamped to the
    /// last row rather than to row 0.
    pub(super) fn reseat(&mut self, previous_index: Option<usize>) -> bool {
        // `selected_index`, not `flock.contains_key`: a selection the filter
        // hides is not seated, however present its id is. Must come before the
        // emptiness check below, which would otherwise return early for a query
        // that matches no sheep.
        if self.selected_index().is_some() {
            return false;
        }
        let before = self.selected.clone();
        if self.visible_rows().is_empty() {
            self.selected = None;
            return before != self.selected;
        }
        self.select_at(previous_index.unwrap_or(0), 1);
        before != self.selected
    }

    /// Moves the selection by `delta` rows and reports whether it moved.
    /// Clamped rather than wrapping.
    pub(super) fn select_by(&mut self, delta: isize) -> Effect {
        let Some(index) = self.selected_index() else {
            return Effect::None;
        };
        let next = index.saturating_add_signed(delta);
        let direction = if delta < 0 { -1 } else { 1 };
        self.select_at(next, direction)
    }

    /// Selects the row at `index`, clamped to the flock, and reports whether
    /// that changed anything.
    ///
    /// `direction` is which way to search past a [`RowKey::Section`] header,
    /// and a header at row 0 searches forward whatever it says.
    ///
    /// `Effect::None` when nothing changed: [`Effect::RefreshSelected`] reads
    /// two files and asks the shepherd for lambs, and a held `k` at the top
    /// must not do that once per keypress.
    pub(super) fn select_at(&mut self, index: usize, direction: isize) -> Effect {
        let visible = self.visible_rows();
        if visible.is_empty() {
            return Effect::None;
        }
        let mut index = index.min(visible.len() - 1);
        if matches!(visible[index], RowKey::Section(_)) {
            index = if direction < 0 && index > 0 {
                index - 1
            } else {
                index + 1
            };
        }
        let next = visible[index].clone();
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

    /// Every sheep the table's rows are drawn from, in name-then-id order: the
    /// whole flock, or whatever the filter leaves of it.
    ///
    /// A flat sheep list, not [`Self::visible_rows`]'s [`RowKey`] sequence: the
    /// title bar counts this, and a group header is not a sheep.
    #[must_use]
    pub fn rows(&self) -> Vec<&Row> {
        let needle = self.filter.to_lowercase();
        let mut visible: Vec<&Row> = self
            .flock
            .values()
            .filter(|row| needle.is_empty() || row.info.name.to_lowercase().contains(&needle))
            .collect();
        visible.sort_unstable_by(|a, b| {
            (a.info.name.as_str(), a.info.id).cmp(&(b.info.name.as_str(), b.info.id))
        });
        visible
    }

    /// Every sheep the shepherd last reported, in id order, whatever the filter
    /// hides.
    ///
    /// The host strip sums this rather than [`Self::rows`], so a name filter
    /// cannot narrow what `flock cpu`/`flock mem` add up to while the label
    /// still says `flock`.
    #[must_use]
    pub fn all_rows(&self) -> Vec<&Row> {
        self.flock.values().collect()
    }

    /// How many sheep the shepherd last reported, whatever the filter hides.
    #[must_use]
    pub fn flock_len(&self) -> usize {
        self.flock.len()
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
        let key = self.selected.clone()?;
        self.visible_rows().iter().position(|row| *row == key)
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

    /// Every instance of `name`, sorted by slot: the members a
    /// [`RowKey::Group`] row summarises.
    #[must_use]
    pub fn group_members(&self, name: &str) -> Vec<&Row> {
        let mut members: Vec<&Row> = self
            .flock
            .values()
            .filter(|row| row.info.name == name)
            .collect();
        members.sort_by_key(|row| row.info.instance.unwrap_or(u32::MAX));
        members
    }

    /// Whether `name`'s instances draw under a [`RowKey::Group`] header: more
    /// than one instance of the name, every one of them reporting a slot.
    ///
    /// Read over the whole flock rather than the filtered sequence, which a
    /// name query keeps whole either way.
    #[must_use]
    pub fn is_grouped(&self, name: &str) -> bool {
        let members = self.group_members(name);
        members.len() > 1 && members.iter().all(|row| row.info.instance.is_some())
    }

    /// `name`'s rolled-up numbers. [`GroupTotals`] gives the rule each field
    /// follows.
    #[must_use]
    pub fn group_totals(&self, name: &str) -> GroupTotals {
        self.totals_for(self.group_members(name))
    }

    /// Every instance whose `fold` is `fold`: the members a [`RowKey::Fold`]
    /// row summarises.
    #[must_use]
    pub fn fold_members(&self, fold: &str) -> Vec<&Row> {
        self.flock
            .values()
            .filter(|row| row.info.fold.as_deref() == Some(fold))
            .collect()
    }

    /// `fold`'s rolled-up numbers, the same rule [`Self::group_totals`]
    /// applies but over every instance in the fold rather than one app's own.
    #[must_use]
    pub fn fold_totals(&self, fold: &str) -> GroupTotals {
        self.totals_for(self.fold_members(fold))
    }

    /// `fold`'s STATUS text: [`Self::group_status_text`]'s own rule, applied
    /// over [`Self::fold_members`] instead of [`Self::group_members`].
    #[must_use]
    pub fn fold_status_text(&self, fold: &str) -> String {
        Self::status_text_for(&self.fold_members(fold))
    }

    /// Whether `fold`'s members are hidden by [`KeyPress::Collapse`].
    ///
    /// Read by the fold header's name cell, which carries the disclosure
    /// triangle: without it a collapsed fold and a fold whose members all
    /// left the flock render identically, and the design's rule 3 asks that
    /// the frame read with every colour stripped.
    #[must_use]
    pub fn is_fold_collapsed(&self, fold: &str) -> bool {
        self.collapsed_folds.contains(fold)
    }

    /// `fold`'s status when every member agrees on one.
    /// [`Self::group_uniform_status`]'s own rule, by fold rather than by
    /// name.
    #[must_use]
    pub fn fold_uniform_status(&self, fold: &str) -> Option<ProcStatus> {
        Self::uniform_status_for(&self.fold_members(fold))
    }

    /// The shared rollup [`Self::group_totals`] and [`Self::fold_totals`]
    /// both compute, over whichever members each selects.
    fn totals_for(&self, members: Vec<&Row>) -> GroupTotals {
        GroupTotals {
            count: members.len(),
            restarts: members.iter().map(|row| row.info.restarts).sum(),
            // `Self::cpu_now`, not `row.info.cpu_percent`: this rollup feeds
            // the same CPU cell a standalone row draws, and must answer the
            // same question the row and the flock figure do.
            cpu: members
                .iter()
                .filter_map(|row| self.cpu_now(row.info.id))
                .fold(None, |acc, cpu| Some(acc.unwrap_or(0.0) + cpu)),
            memory: members
                .iter()
                .filter_map(|row| row.info.memory_bytes)
                .fold(None, |acc, mem| Some(acc.unwrap_or(0) + mem)),
            uptime_ms: members
                .iter()
                .filter_map(|row| self.uptime_ms(row.info.id))
                .min(),
        }
    }

    /// `name`'s STATUS cell: the shared status word when every instance agrees,
    /// else a count per state, as `output::rows::group_status` does for
    /// `shep flock`.
    ///
    /// Reads `ProcStatus` directly, never [`Row::reported`]: a dog is never
    /// stocked to several instances, so a group has no handshake to report.
    #[must_use]
    pub fn group_status_text(&self, name: &str) -> String {
        Self::status_text_for(&self.group_members(name))
    }

    /// The status sentence [`Self::group_status_text`] and
    /// [`Self::fold_status_text`] both build, over whichever members each
    /// selects. One word when they agree, otherwise a count per status.
    ///
    /// Shares its shape with [`Self::totals_for`] deliberately: these are the
    /// same rollup question asked of two different member sets, and a second
    /// copy of the walk is how the fourth one gets written.
    fn status_text_for(members: &[&Row]) -> String {
        let Some(first) = members.first().map(|row| row.info.status) else {
            return String::new();
        };
        if members.iter().all(|row| row.info.status == first) {
            return first.to_string();
        }
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in members {
            *counts.entry(row.info.status.to_string()).or_default() += 1;
        }
        counts
            .into_iter()
            .map(|(status, n)| format!("{n} {status}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `name`'s status when every instance agrees on one, which the STATUS
    /// colouring and the detail pane's status word key off. A mixed group's
    /// plain count text wears no colour.
    #[must_use]
    pub fn group_uniform_status(&self, name: &str) -> Option<ProcStatus> {
        Self::uniform_status_for(&self.group_members(name))
    }

    /// The one status every member agrees on, or `None` when they differ.
    /// [`Self::group_uniform_status`] and [`Self::fold_uniform_status`] both
    /// key their colouring off this.
    fn uniform_status_for(members: &[&Row]) -> Option<ProcStatus> {
        let first = members.first()?.info.status;
        members
            .iter()
            .all(|row| row.info.status == first)
            .then_some(first)
    }

    /// How the flock table currently gathers its rows, toggled by
    /// [`KeyPress::FoldView`].
    ///
    /// `view::mod`'s draw loop reads this to choose between the flat column
    /// set and the fold view's own.
    #[must_use]
    pub fn grouping(&self) -> Grouping {
        self.grouping
    }

    /// Sets the filter directly, bypassing [`Self::set_filter`]'s reseat.
    #[cfg(test)]
    pub(crate) fn set_filter_for_tests(&mut self, query: &str) {
        self.filter = query.to_string();
    }

    /// Points the cursor at `key` without simulating keypresses.
    #[cfg(test)]
    pub(super) fn select(&mut self, key: RowKey) {
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

    /// The status bar's own rendered text.
    fn status_line_text(app: &App) -> String {
        crate::lookout::view::fixtures::rendered(&crate::lookout::view::status::status_line(
            app, 200,
        ))
    }

    #[test]
    fn a_multi_instance_app_shows_a_group_row_above_its_slots() {
        let app = allowed_with_instances();
        assert_eq!(
            app.visible_rows().len(),
            5,
            "the flock header, three slots and the group row above them"
        );
        assert_eq!(app.visible_rows()[0], RowKey::Section("Flock"));
        assert!(matches!(app.visible_rows()[1], RowKey::Group(ref n) if n == "web"));
    }

    /// Sheep gather under their fold, unfoldered ones under a header that
    /// names the situation, and dogs keep their own band because a dog
    /// cannot carry a fold at all.
    #[test]
    fn by_fold_groups_sheep_under_their_fold() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
                fixtures::sheep_in_fold(3, "batch", None),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        let rows = app.visible_rows();
        assert!(
            rows.iter()
                .any(|r| matches!(r, RowKey::Fold(name) if name == "edge"))
        );
        assert!(rows.iter().any(|r| matches!(r, RowKey::Section("no fold"))));
    }

    /// `F` toggles rather than opening, so pressing it twice is where it
    /// began.
    #[test]
    fn f_toggles_back_to_the_flat_list() {
        let mut app = fixtures::app_with(
            vec![fixtures::sheep_in_fold(1, "api", Some("edge"))],
            fixtures::plain(),
        );
        let flat = app.visible_rows();
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        assert_ne!(app.visible_rows(), flat);
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        assert_eq!(app.visible_rows(), flat);
    }

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

    /// Two levels, never three. A three-instance app inside a fold is one
    /// member row keeping its own rollup, or `edge ×4` stops meaning
    /// anything fixed.
    #[test]
    fn an_app_inside_a_fold_stays_one_row() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::instance_in_fold(1, "web", 0, Some("edge")),
                fixtures::instance_in_fold(2, "web", 1, Some("edge")),
                fixtures::instance_in_fold(3, "web", 2, Some("edge")),
            ],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        let rows = app.visible_rows();
        let sheep = rows
            .iter()
            .filter(|r| matches!(r, RowKey::Sheep(_)))
            .count();
        assert_eq!(sheep, 0, "instances stay behind their app's row: {rows:?}");
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, RowKey::Group(_)))
                .count(),
            1
        );
    }

    #[test]
    fn a_flock_with_a_dog_draws_a_section_header_before_each_kind() {
        let app = fixtures::app_with_a_dog();
        let rows = app.visible_rows();
        assert_eq!(rows.first(), Some(&RowKey::Section("Flock")), "{rows:?}");
        let dogs = rows
            .iter()
            .position(|row| *row == RowKey::Section("Dogs"))
            .unwrap_or_else(|| panic!("no dogs header: {rows:?}"));
        // Every sheep sorts above the header and every dog below it.
        assert!(
            rows[..dogs].iter().all(|row| !app.is_dog_row(row)),
            "{rows:?}"
        );
        assert!(
            rows[dogs + 1..].iter().all(|row| app.is_dog_row(row)),
            "{rows:?}"
        );
    }

    #[test]
    fn a_flock_with_no_dog_draws_no_dogs_header() {
        let app = started().0;
        let rows = app.visible_rows();
        assert!(!rows.contains(&RowKey::Section("Dogs")), "{rows:?}");
    }

    #[test]
    fn a_dog_only_flock_draws_no_flock_header_and_selects_past_the_dogs_one() {
        let mut app = fixtures::app_with_a_dog();
        // A filter that leaves only the dog, so the sheep side is empty.
        app.set_filter("otel".to_string());
        let rows = app.visible_rows();
        assert!(!rows.contains(&RowKey::Section("Flock")), "{rows:?}");
        assert_eq!(rows.first(), Some(&RowKey::Section("Dogs")), "{rows:?}");

        // The only header is row 0, which has nowhere to search backward.
        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        assert!(
            !matches!(app.selected(), Some(RowKey::Section(_))),
            "{:?}",
            app.selected()
        );
    }

    #[test]
    fn moving_down_steps_over_a_section_header() {
        let mut app = fixtures::app_with_a_dog();
        app.select_at(1, 1);
        let before = app.selected();
        // Walk the whole list; a header must never become the selection.
        for _ in 0..app.visible_rows().len() + 2 {
            let _ = app.update(Msg::Key(KeyPress::SelectDown));
            assert!(
                !matches!(app.selected(), Some(RowKey::Section(_))),
                "landed on a header from {before:?}"
            );
        }
    }

    #[test]
    fn moving_up_steps_over_a_section_header() {
        let mut app = fixtures::app_with_a_dog();
        app.select_at(app.visible_rows().len() - 1, -1);
        let before = app.selected();
        // Walk the whole list; a header must never become the selection.
        for _ in 0..app.visible_rows().len() + 2 {
            let _ = app.update(Msg::Key(KeyPress::SelectUp));
            assert!(
                !matches!(app.selected(), Some(RowKey::Section(_))),
                "landed on a header from {before:?}"
            );
        }
        // The walk has to actually cross the `Dogs` header going up, not
        // just avoid landing on it: it should reach the first row, `api`
        // sorting ahead of `web`.
        assert_eq!(app.selected(), Some(RowKey::Sheep(2)), "{before:?}");
    }

    #[test]
    fn an_action_on_a_group_row_targets_the_whole_app_by_name() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter sends");
        };
        assert_eq!(
            sent.request(),
            Request::Stop {
                selector: SelectorSpec::Name("web".to_string())
            }
        );
    }

    #[test]
    fn a_group_confirm_states_how_many_processes_it_reaches() {
        let mut app = allowed_with_instances();
        app.select(RowKey::Group("web".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let prompt = status_line_text(&app);
        assert!(prompt.contains('3'), "names the blast radius: {prompt}");
    }

    /// The same rule group_totals uses, whose own doc calls uptime the
    /// minimum. A second rollup rule would make two headers disagree about
    /// the same numbers.
    #[test]
    fn a_fold_rolls_up_like_a_group_does() {
        let app = fixtures::app_with(
            vec![
                fixtures::sheep_with(1, "api", Some("edge"), 120_000, Some(100 << 20), 2),
                fixtures::sheep_with(2, "cdn", Some("edge"), 30_000, Some(150 << 20), 5),
            ],
            fixtures::plain(),
        );
        let totals = app.fold_totals("edge");
        assert_eq!(totals.count, 2);
        assert_eq!(totals.restarts, 7);
        assert_eq!(totals.memory, Some(250 << 20));
        assert_eq!(
            totals.uptime_ms,
            Some(30_000),
            "the minimum, not the first or the longest"
        );
    }

    /// A fold restart that half refused says so, and names the apps.
    ///
    /// `refused` only arrives on a multi-app walk, which a fold action is
    /// and a single-app action is not. Unreported, the operator reads
    /// "restarted" over a fold where some apps did not.
    #[test]
    fn a_partly_refused_fold_restart_names_what_refused_it() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        app.set_control_for_tests(Control::Allowed);
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select(RowKey::Fold("edge".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        let _ = app.update(Msg::Key(KeyPress::Confirm));

        let _ = app.update(Msg::Replied {
            sent: Sent::Action {
                verb: ActionVerb::Restart,
                target: RowKey::Fold("edge".to_string()),
                name: "edge".to_string(),
            },
            result: Ok(Response::Restarted {
                accepted: vec![ProcessInfo::builder(1, "api", ProcStatus::Online).build()],
                refused: vec![SheepRefusal::new("cdn", "its Flockfile moved")],
            }),
        });

        let notice = app.notice().expect("a reply always leaves a notice");
        assert!(
            notice.text.contains("cdn"),
            "names the app: {}",
            notice.text
        );
        assert!(
            notice.text.contains("its Flockfile moved"),
            "and the shepherd's reason: {}",
            notice.text
        );
        assert!(
            notice.grave,
            "a half-done fold action is not a success sentence"
        );
    }

    #[test]
    fn a_fold_confirm_states_how_many_it_reaches() {
        let mut app = fixtures::app_with(
            vec![
                fixtures::sheep_in_fold(1, "api", Some("edge")),
                fixtures::sheep_in_fold(2, "cdn", Some("edge")),
            ],
            fixtures::plain(),
        );
        app.set_control_for_tests(Control::Allowed);
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        app.select(RowKey::Fold("edge".to_string()));
        app.update(Msg::Key(KeyPress::Action(ActionVerb::Stop)));
        let text = status_line_text(&app);
        assert!(
            text.contains('2'),
            "the confirm must name the count: {text}"
        );
        assert!(text.contains("edge"), "and the fold: {text}");

        // The half the confirm cannot check. `Sent::Action`'s `RowKey::Fold`
        // arm is the only thing turning a fold header into a fold selector,
        // and nothing else asserts it: change it to `SelectorSpec::Name` and
        // the confirm still reads "2 sheep in fold edge", Enter still sends,
        // and the shepherd matches no app. A fold-wide stop that silently
        // stops nothing.
        let request = wire(app.update(Msg::Key(KeyPress::Confirm)));
        let Request::Stop { selector } = request else {
            panic!("expected Stop, got {request:?}");
        };
        assert_eq!(selector, SelectorSpec::Fold("edge".to_string()));
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

    /// The no-fold header is a header, not a fold. There is no
    /// `SelectorSpec` that names "everything with no fold", so an action there
    /// would have to enumerate ids behind the operator's back.
    #[test]
    fn the_no_fold_header_is_not_selectable() {
        let mut app = fixtures::app_with(
            vec![fixtures::sheep_in_fold(1, "batch", None)],
            fixtures::plain(),
        );
        let _ = app.update(Msg::Key(KeyPress::FoldView));
        let _ = app.update(Msg::Key(KeyPress::SelectFirst));
        assert!(
            !matches!(app.selected(), Some(RowKey::Section(_))),
            "selection steps past a header, got {:?}",
            app.selected()
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

    /// The fixture separates the two answers: by id it is `web` 0, `api` 1,
    /// `web` 2; by name then id it is `api` 1, `web` 0, `web` 2. The `(name,
    /// id)` tiebreak itself is not falsifiable here, since the rows arrive in
    /// id order; what this catches is the sort going missing entirely.
    #[test]
    fn the_table_draws_by_name_then_by_id() {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(0, "web", ProcStatus::Online),
                sheep(1, "api", ProcStatus::Online),
                sheep(2, "web", ProcStatus::Online),
            ],
            at: t0,
        });

        let drawn: Vec<(&str, u32)> = app
            .rows()
            .iter()
            .map(|row| (row.info.name.as_str(), row.info.id))
            .collect();
        assert_eq!(drawn, vec![("api", 1), ("web", 0), ("web", 2)]);
    }

    #[test]
    fn a_filter_narrows_the_rows_and_leaves_the_real_size_readable() {
        let app = filtered("web");
        assert_eq!(app.rows().len(), 2, "api-web and web-worker");
        assert_eq!(app.flock_len(), 4, "the flock did not get smaller");
    }

    /// `ProcessSelector`'s `Name` compares with `==`, so borrowing the CLI's
    /// selector grammar would match nothing while `web-worker` is being typed.
    #[test]
    fn the_filter_matches_a_substring_and_not_a_whole_name() {
        assert_eq!(filtered("wor").rows().len(), 1, "web-worker, by its middle");
        assert_eq!(filtered("w").rows().len(), 2, "api-web, by its own middle");
    }

    #[test]
    fn the_filter_ignores_case_in_both_directions() {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "WebEdge", ProcStatus::Online)],
            at: t0,
        });
        app.set_filter("webedge".to_string());
        assert_eq!(
            app.rows().len(),
            1,
            "a lowercase query against a mixed name"
        );
        app.set_filter("WEBEDGE".to_string());
        assert_eq!(app.rows().len(), 1, "and an uppercase one");
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
        app.set_filter("web".to_string());
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(4)),
            "position 3 clamps to the last visible row, which is web-worker"
        );
    }

    #[test]
    fn nothing_visible_means_nothing_selected() {
        let app = filtered("zzz");
        assert_eq!(app.rows().len(), 0);
        assert_eq!(app.selected(), None);
        assert!(app.selected_row().is_none());
        assert_eq!(app.flock_len(), 4, "the flock is still four sheep");
    }

    #[test]
    fn a_filter_survives_the_two_second_snapshot() {
        let mut app = filtered("web");
        let t1 = Instant::now();
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(1, "api-web", ProcStatus::Online),
                sheep(2, "cron", ProcStatus::Online),
                sheep(3, "queue", ProcStatus::Online),
                sheep(4, "web-worker", ProcStatus::Online),
            ],
            at: t1,
        });
        assert_eq!(app.filter(), "web", "the snapshot did not clear it");
        assert_eq!(app.rows().len(), 2, "and did not widen the table");
        assert_eq!(app.flock_len(), 4);
    }

    #[test]
    fn an_empty_query_is_the_same_as_no_filter() {
        let mut app = filtered("zzz");
        app.set_filter(String::new());
        assert_eq!(app.rows().len(), 4);
        assert_eq!(app.selected(), Some(RowKey::Sheep(1)), "seated again");
    }
}
