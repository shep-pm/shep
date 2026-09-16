use super::super::*;

/// The question `esc` asks when a pane closes over changes the running
/// child has not taken.
///
/// Raised by [`App::close_offer`], answered by [`App::on_close_dialog_key`],
/// and expired by the same [`CONFIRM_EXPIRY`] every other prompt gets.
///
/// Everything is carried rather than recomputed: the unsent names are
/// taken from the pane when the dialog goes up, and `parked` is the
/// shepherd's own answer from the last fetch.
///
/// `Debug` is derived (IR-41): field names the operator is already
/// reading on screen, two counts, an instance count, two durations
/// rendered for display, a reload mode, a name, a status, a pid and a
/// time. No value of any field, and no env value, since the wire never
/// sends one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseDialog {
    unsent: Vec<String>,
    parked: usize,
    live: usize,
    reload: ReloadKind,
    instances: u32,
    kill_timeout: String,
    graceful_timeout: String,
    name: String,
    status: ProcStatus,
    pid: Option<u32>,
    at: Instant,
}

impl CloseDialog {
    /// One, over `unsent` filed edits and `parked` fields, reading
    /// everything else off `pane`: which reload it would get, its own
    /// `kill_timeout` and `graceful_timeout`, its name, and how many other
    /// filed edits (`live`) the running sheep already takes without one.
    ///
    /// `status` and `pid` are the one pair a [`ConfigPane`] cannot answer,
    /// since only the flock map carries them, and the heading names both:
    /// an operator answering a question that restarts a process should not
    /// have to read the dimmed pane behind the box to learn which one.
    #[must_use]
    pub(in crate::lookout) fn new(
        unsent: Vec<String>,
        parked: usize,
        pane: &ConfigPane,
        status: ProcStatus,
        pid: Option<u32>,
        at: Instant,
    ) -> Self {
        Self {
            unsent,
            parked,
            live: pane.live_edit_count(),
            reload: pane.reload_kind(),
            // `value` renders the pane's own map as JSON and `instances`
            // is a plain `u32` every `AppConfig` carries, so the parse
            // cannot fail for a sheep. The only target without the field
            // is a dog, and `verb_commit::close_offer` refuses a dog before it builds
            // one of these. A fallback that ever fired would understate
            // how many processes the reload row is describing, which is
            // the one number that row exists to give.
            instances: pane.value("instances").parse().unwrap_or(1),
            kill_timeout: pane.display_value("kill_timeout"),
            graceful_timeout: pane.display_value("graceful_timeout"),
            name: pane.target().name().to_owned(),
            status,
            pid,
            at,
        }
    }

    /// When it opened. A dialog that outlives `CONFIRM_EXPIRY` is dropped
    /// by the tick, so a later keypress cannot answer a question nobody is
    /// still looking at.
    #[must_use]
    pub const fn at(&self) -> Instant {
        self.at
    }

    /// How many filed edits a respawn is what applies. The heading's own
    /// number; [`Self::unsent_fields`] is the sentence underneath it.
    #[must_use]
    pub fn unsent(&self) -> usize {
        self.unsent.len()
    }

    /// Those edits' own field names, in the set's key order.
    #[must_use]
    pub fn unsent_fields(&self) -> &[String] {
        &self.unsent
    }

    /// How many fields the shepherd already parked, from the last fetch.
    #[must_use]
    pub const fn parked(&self) -> usize {
        self.parked
    }

    /// How many other filed edits the running sheep already takes without
    /// a respawn. What "everything else you changed is already live" draws
    /// on: zero when the whole filed set needs one.
    #[must_use]
    pub const fn live(&self) -> usize {
        self.live
    }

    /// Which reload this sheep would get, so `L`'s row can name its cost.
    #[must_use]
    pub const fn reload(&self) -> ReloadKind {
        self.reload
    }

    /// How many instances a reload or restart would reach.
    #[must_use]
    pub const fn instances(&self) -> u32 {
        self.instances
    }

    /// The sheep's own `kill_timeout`, resolved for display: what `R`'s row
    /// names as the stop's own grace before SIGKILL.
    #[must_use]
    pub fn kill_timeout(&self) -> &str {
        &self.kill_timeout
    }

    /// The sheep's own `graceful_timeout`, resolved the same way: the drain
    /// window a serial reload gets.
    #[must_use]
    pub fn graceful_timeout(&self) -> &str {
        &self.graceful_timeout
    }

    /// The sheep this dialog is asking about.
    ///
    /// Unread by this frame's own render: `close_dialog_lines` names no
    /// target, only what changed. [`App::answer_close`] reads it, since a
    /// held verb has to name the sheep after the dialog itself is gone.
    #[must_use]
    pub fn target_name(&self) -> &str {
        &self.name
    }

    /// What the flock reported this sheep doing when the dialog went up.
    #[must_use]
    pub const fn status(&self) -> ProcStatus {
        self.status
    }

    /// The OS pid this sheep runs under, when there is exactly one running
    /// instance to name. [`None`] for a sheep the shepherd runs several of,
    /// where no single pid is the answer.
    #[must_use]
    pub const fn pid(&self) -> Option<u32> {
        self.pid
    }
}

impl App {
    /// Every instance of `name` the flock reports running.
    ///
    /// `Stopping` does not count: the drainee and its replacement hold the
    /// same slot ([`crate::lookout::pane::ReloadKind`]'s own reasoning), and
    /// a sheep with every instance stopping holds no config a respawn would
    /// replace.
    fn running_instances<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Row> {
        self.flock.values().filter(move |row| {
            row.info.name == name
                && matches!(
                    row.info.status,
                    ProcStatus::Online | ProcStatus::Starting | ProcStatus::WaitingRestart
                )
        })
    }

    /// What the close dialog's heading says about the sheep itself: the
    /// status of the first running instance, and its pid when it is the
    /// only one. Several instances name no single pid, so the heading
    /// names none.
    pub(super) fn running_state(&self, name: &str) -> Option<(ProcStatus, Option<u32>)> {
        let mut running = self.running_instances(name);
        let first = running.next()?;
        let alone = running.next().is_none();
        Some((first.info.status, if alone { first.info.pid } else { None }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::*;
    use super::*;
    use crate::lookout::app::testing::*;

    use crate::lookout::view::fixtures;

    #[test]
    fn escape_closes_a_pane_with_nothing_parked() {
        let mut app = fixtures::app_in_sheep_pane_with_nothing_parked();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.config_pane().is_none(),
            "no dialog when nothing is parked"
        );
        assert!(app.close_dialog().is_none());
    }

    /// A parked field alone (nothing filed to write) has nothing for `R` to
    /// wait on, so the restart goes at once rather than holding for a batch
    /// that is empty.
    #[test]
    fn r_over_a_parked_field_alone_sends_the_restart_at_once() {
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let effect = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert!(
            matches!(
                effect,
                Effect::Send(Sent::Action {
                    verb: ActionVerb::Restart,
                    ..
                })
            ),
            "got {effect:?}"
        );
        assert!(app.config_pane().is_none());
        assert!(app.close_dialog().is_none());
    }

    /// Several instances name no one pid, so the heading names none. The
    /// `api` row alongside is the control: it proves the fixture really
    /// carries pids, without which the `web` assertion would pass on a
    /// flock that had none to offer.
    #[test]
    fn a_sheep_with_two_running_instances_offers_no_single_pid() {
        let mut app = allowed();
        let at = app.now();
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(1, "web", ProcStatus::Online),
                sheep(4, "web", ProcStatus::Online),
                sheep(2, "api", ProcStatus::Online),
            ],
            at,
        });
        assert_eq!(app.running_state("web"), Some((ProcStatus::Online, None)));
        assert_eq!(
            app.running_state("api"),
            Some((ProcStatus::Online, Some(1002))),
            "one instance still names its pid"
        );
    }

    /// The gap that let a second held verb through: two `R` answers in a
    /// row, with the first batch's replies still outstanding when the
    /// second is asked. The bug was `self.held` being overwritten by the
    /// second session, so the first session's own replies (an unrelated
    /// batch, and the wrong count) resolved the second session's verb
    /// early. Proof of the fix: the first write's reply alone must not
    /// finish anything (the held count is still the first batch's own two,
    /// not the second batch's one), and the second session's `R` never
    /// gets an action at all, in flight or otherwise.
    #[test]
    fn a_second_r_never_holds_over_the_first() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(first_batch) =
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected the first batch");
        };
        assert_eq!(first_batch.len(), 2);

        // Reopen the pane, file another edit, and answer the dialog again
        // while the first batch's replies are still outstanding.
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        fixtures::file_edit(&mut app, "cwd", "/srv/second");
        app.update(Msg::Key(KeyPress::Escape));
        let second_answer = app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)));
        assert!(
            matches!(second_answer, Effect::SendAll(ref batch) if batch.len() == 1),
            "the edit still goes out: {second_answer:?}"
        );
        assert!(
            app.notice()
                .is_some_and(|n| n.to_string().contains("already in flight")),
            "got {:?}",
            app.notice()
        );

        // The first write's own reply must not finish anything: the held
        // count is still the first batch's own two, not the second
        // batch's one that a bug would have overwritten it with.
        let first_reply = app.update(Msg::Replied {
            sent: first_batch[0].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "cwd".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(first_reply, Effect::None),
            "fired early: {first_reply:?}"
        );

        // The first batch's own second reply completes it, correctly: this
        // is the first session's own action, not the second's.
        let second_reply = app.update(Msg::Replied {
            sent: first_batch[1].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "max_memory".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(
                second_reply,
                Effect::Send(Sent::Action {
                    verb: ActionVerb::Restart,
                    ..
                })
            ),
            "got {second_reply:?}"
        );
    }

    /// The reopened door into the same bug: the refused session's own
    /// write still goes out (per `verb_commit::answer_close`'s own doc, edits are never
    /// held back), and its reply must not count toward the held session's
    /// batch just because it is the only thing held at the time. A bare
    /// counter cannot tell the two apart; a ticket can.
    #[test]
    fn a_refused_sessions_reply_does_not_count_toward_the_held_batch() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(first_batch) =
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected the first batch");
        };
        assert_eq!(first_batch.len(), 2, "session A holds on two writes");

        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        fixtures::file_edit(&mut app, "cwd", "/srv/second");
        app.update(Msg::Key(KeyPress::Escape));
        let Effect::SendAll(second_batch) =
            app.update(Msg::Key(KeyPress::Action(ActionVerb::Restart)))
        else {
            panic!("expected session B's write to still go out");
        };
        assert_eq!(
            second_batch.len(),
            1,
            "session B holds on nothing, but writes"
        );

        // Session B's own reply lands first. It must be entirely inert:
        // not held, so it cannot bring session A's batch any closer to
        // done.
        let after_b = app.update(Msg::Replied {
            sent: second_batch[0].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "cwd".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            !matches!(after_b, Effect::Send(Sent::Action { .. })),
            "session B holds no verb to send: {after_b:?}"
        );

        // Only one of session A's own two writes has answered. Nothing
        // must have gone out yet, from either session.
        let after_a_first = app.update(Msg::Replied {
            sent: first_batch[0].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "cwd".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(after_a_first, Effect::None),
            "session A's own batch still has one outstanding: {after_a_first:?}"
        );

        // Session A's second and last write answers. Now, and only now,
        // its restart goes.
        let after_a_second = app.update(Msg::Replied {
            sent: first_batch[1].clone(),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_string(),
                key: "max_memory".to_string(),
                pending: false,
                warning: None,
            }),
        });
        assert!(
            matches!(
                after_a_second,
                Effect::Send(Sent::Action {
                    verb: ActionVerb::Restart,
                    ..
                })
            ),
            "got {after_a_second:?}"
        );
    }

    /// `esc` leaves the config pane on the first press: the blurb draws
    /// unconditionally now, so nothing else waits for a second press.
    ///
    /// 89 columns, one under `panel_width`'s floor, is what makes the
    /// blurb the only thing drawing the cursor's own help at this width;
    /// at 160 the panel draws it instead and the assertion below would
    /// pass regardless.
    #[test]
    fn esc_leaves_the_pane_on_one_press_with_a_blurb_showing() {
        let mut app = fixtures::app_in_sheep_pane_with_nothing_parked();
        let pane = app.config_pane().expect("the pane is open");
        let Some(PaneRow::Field(index)) = pane.cursor() else {
            panic!("the cursor is not on a field");
        };
        // The longest word, not the whole help string. The blurb wraps to
        // BLURB_WRAP, and `render_all` joins rows with newlines, so a help
        // text over that budget is present on screen and absent from this
        // assertion.
        let anchor = pane.fields().fields()[index]
            .help
            .split_whitespace()
            .max_by_key(|word| word.len())
            .expect("the field help is empty")
            .to_owned();
        let lines = crate::lookout::view::pane::pane_lines(pane, fixtures::plain(), 89, 40);
        assert!(
            fixtures::render_all(&lines).contains(&anchor),
            "the blurb is not on screen before esc"
        );
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.config_pane().is_none(),
            "one esc did not leave the pane"
        );
    }

    /// The case the old menu missed: an edit made in this pane, on a sheep
    /// with nothing parked before it opened.
    #[test]
    fn esc_with_a_respawn_edit_asks_before_it_writes() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_some(), "the dialog is up");
        assert!(
            matches!(effect, Effect::None),
            "nothing is written until the dialog is answered, got {effect:?}"
        );
        assert!(app.config_pane().is_some(), "the pane is still open");
    }

    #[test]
    fn esc_with_only_live_edits_writes_and_closes_with_no_dialog() {
        // `app_in_sheep_pane` parks `kill_signal` unconditionally
        // (`sheep_config_view`'s own default), which alone would raise the
        // dialog regardless of what is filed. This test is about the
        // unsent half alone, so it needs a pane starting with nothing
        // parked.
        let mut app = fixtures::app_in_sheep_pane_with_nothing_parked();
        // `max_restarts` is `ApplyGroup::Live`.
        fixtures::file_edit(&mut app, "max_restarts", "9");
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.close_dialog().is_none());
        assert!(app.config_pane().is_none(), "the pane closed");
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
    }

    /// A `Stopping` sheep's drainee and its replacement hold the same
    /// instance slot, so it is excluded from `App::sheep_is_running`
    /// alongside `Stopped`: no live config for a respawn to replace, and
    /// `R` would race the reload already under way rather than restart
    /// anything.
    #[test]
    fn no_dialog_for_a_sheep_mid_drain() {
        let mut app = fixtures::app_in_sheep_pane_on_a_draining_sheep();
        fixtures::file_edit(&mut app, "cwd", "/srv/app");
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(
            app.close_dialog().is_none(),
            "a sheep mid-drain is not asked about"
        );
        assert!(matches!(effect, Effect::SendAll(_)), "got {effect:?}");
    }

    /// The parked half: no edit of the operator's own, fields the shepherd
    /// is already holding.
    #[test]
    fn esc_over_parked_fields_alone_still_asks() {
        // `app_in_sheep_pane` is read-only by default, and a closed gate
        // is a separate reason for no dialog (`verb_commit::read_only_is_never_asked`).
        // This test is about the parked half alone, so it needs the gate
        // open and nothing filed.
        let mut app = fixtures::app_in_sheep_pane_with_a_parked_field();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        let dialog = app.close_dialog().expect("one field is parked");
        assert_eq!(dialog.unsent(), 0);
        assert_eq!(dialog.parked(), 1);
        assert!(matches!(effect, Effect::None), "got {effect:?}");
    }
}
