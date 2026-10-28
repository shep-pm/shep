//! The numbers and the status word a group or fold header rolls up from
//! its members.

use super::*;

impl App {
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

    /// Every name whose instances draw under a [`RowKey::Group`] header: more
    /// than one instance of the name, every one of them reporting a slot.
    /// Read over the whole flock rather than the filtered sequence, which a
    /// name query keeps whole either way.
    ///
    /// Every name at once, in one pass, rather than a `name -> bool` call:
    /// both callers ask for a row or a name run at a time.
    #[must_use]
    pub fn grouped_names(&self) -> HashSet<&str> {
        // `(instances, every one of them reports a slot)`, the pair the rule
        // above decides on.
        let mut tally: HashMap<&str, (usize, bool)> = HashMap::new();
        for row in self.flock.values() {
            let entry = tally.entry(row.info.name.as_str()).or_insert((0, true));
            entry.0 += 1;
            entry.1 &= row.info.instance.is_some();
        }
        tally
            .into_iter()
            .filter(|(_, (instances, all_slotted))| *instances > 1 && *all_slotted)
            .map(|(name, _)| name)
            .collect()
    }

    /// `name`'s rolled-up numbers. [`GroupTotals`] gives the rule each field
    /// follows.
    #[must_use]
    pub fn group_totals(&self, name: &str) -> GroupTotals {
        self.totals_for(&self.group_members(name))
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
        self.totals_for(&self.fold_members(fold))
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
    ///
    /// Public and slice-taking so a caller already holding the members can
    /// roll them up without asking for them a second time: a group row draws
    /// this alongside [`Self::status_text_for`] and
    /// [`Self::uniform_status_for`] off one `group_members` call.
    #[must_use]
    pub fn totals_for(&self, members: &[&Row]) -> GroupTotals {
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
    /// Reads [`Row::reported`], as `output::rows::group_status` does, so a
    /// disabled app's header says `disabled` over the rows that do.
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
    #[must_use]
    pub fn status_text_for(members: &[&Row]) -> String {
        let Some(first) = members.first().map(|row| row.reported()) else {
            return String::new();
        };
        if members.iter().all(|row| row.reported() == first) {
            return first.word();
        }
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in members {
            *counts.entry(row.reported().word()).or_default() += 1;
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
    #[must_use]
    pub fn uniform_status_for(members: &[&Row]) -> Option<ProcStatus> {
        let first = members.first()?.info.status;
        members
            .iter()
            .all(|row| row.info.status == first)
            .then_some(first)
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
        // and nothing else asserts it: a fold-wide stop that silently stops
        // nothing.
        let request = wire(app.update(Msg::Key(KeyPress::Confirm)));
        let Request::Stop { selector } = request else {
            panic!("expected Stop, got {request:?}");
        };
        assert_eq!(selector, SelectorSpec::Fold("edge".to_string()));
    }
}
