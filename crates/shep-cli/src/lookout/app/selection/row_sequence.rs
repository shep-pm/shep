//! The row sequence itself: which rows the table draws, and how a fold,
//! a group or a section header splices into it.

use super::*;

/// One flock entry as `visible_rows` sorts and partitions it: name, instance
/// slot, id, and whether it names a dog.
pub(super) type RowEntry<'a> = (&'a str, Option<u32>, u32, bool);

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
    /// A [`RowKey::Group`] header comes immediately before its own
    /// [`RowKey::Sheep`] entries, and a [`RowKey::Section`] header only when
    /// its side has a row to introduce. The sort key is total on purpose: a
    /// partial one would let two instances swap places under the cursor.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<RowKey> {
        let needle = self.filter.as_str().to_lowercase();
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

        // Once, rather than per name run below: the rule reads the whole flock
        // to answer for one name.
        let grouped = self.grouped_names();
        let mut out = Vec::new();
        match self.grouping {
            Grouping::Flat => {
                if !sheep.is_empty() {
                    out.push(RowKey::Section("Flock"));
                    self.push_grouped_rows(&sheep, &grouped, &mut out);
                }
            }
            Grouping::ByFold => self.push_fold_rows(&sheep, &grouped, &mut out),
        }
        if !dogs.is_empty() {
            out.push(RowKey::Section("Dogs"));
            self.push_grouped_rows(&dogs, &grouped, &mut out);
        }
        out
    }

    /// The `ByFold` half of [`Self::visible_rows`]: `sheep` gathered under
    /// its fold, each fold's header pushed before
    /// [`Self::push_fold_group_rows`] lays down its members, so a
    /// multi-instance app inside a fold keeps its own [`RowKey::Group`] row.
    ///
    /// Sheep carrying no fold land under [`RowKey::Section`]`("no fold")`:
    /// `SelectorSpec::Fold` cannot name "no fold" on the wire.
    fn push_fold_rows(
        &self,
        sheep: &[RowEntry<'_>],
        grouped: &HashSet<&str>,
        out: &mut Vec<RowKey>,
    ) {
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
                self.push_fold_group_rows(members, grouped, out);
            }
        }
        if !unfoldered.is_empty() {
            out.push(RowKey::Section("no fold"));
            self.push_fold_group_rows(&unfoldered, grouped, out);
        }
    }

    /// [`Self::push_grouped_rows`]'s fold-scoped twin: a grouped app
    /// collapses to its own [`RowKey::Group`] header alone, with no
    /// [`RowKey::Sheep`] row following it, so a fold never nests three
    /// levels deep (fold, app, instance). An app with no group still gets
    /// its ordinary sheep row, exactly as the flat path would draw it.
    fn push_fold_group_rows(
        &self,
        entries: &[RowEntry<'_>],
        grouped: &HashSet<&str>,
        out: &mut Vec<RowKey>,
    ) {
        for run in name_runs(entries) {
            let name = run[0].0;
            if grouped.contains(name) {
                out.push(RowKey::Group(name.to_string()));
            } else {
                out.extend(run.iter().map(|entry| RowKey::Sheep(entry.2)));
            }
        }
    }

    /// Appends `entries`' rows to `out`, splicing a [`RowKey::Group`] header
    /// before a grouped app's instances.
    fn push_grouped_rows(
        &self,
        entries: &[RowEntry<'_>],
        grouped: &HashSet<&str>,
        out: &mut Vec<RowKey>,
    ) {
        for run in name_runs(entries) {
            let name = run[0].0;
            if grouped.contains(name) {
                out.push(RowKey::Group(name.to_string()));
            }
            out.extend(run.iter().map(|entry| RowKey::Sheep(entry.2)));
        }
    }

    /// Whether `row` names a dog. A header and a group row are neither.
    #[cfg(test)]
    pub(super) fn is_dog_row(&self, row: &RowKey) -> bool {
        match row {
            RowKey::Sheep(id) => self.flock.get(id).is_some_and(|r| r.info.dog.is_some()),
            RowKey::Group(_) | RowKey::Section(_) | RowKey::Fold(_) => false,
        }
    }

    pub(in crate::lookout::app) fn visible_len(&self) -> usize {
        self.visible_rows().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::view::fixtures;

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
        app.set_filter(TextBox::from("otel"));
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
}
