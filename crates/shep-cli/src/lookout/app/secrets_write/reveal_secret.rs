use super::super::*;

impl App {
    /// Takes any revealed value off the screen, on any screen: every
    /// trigger calls this rather than reaching for [`SecretsPane::hide`]
    /// through a pane it first has to find.
    pub(in crate::lookout::app) fn hide_revealed(&mut self) {
        if let Some(pane) = self.secrets_pane_mut() {
            pane.hide();
        }
    }

    /// Whether `[secrets] allow_read` lets this pane show a value.
    ///
    /// Read off the model the last [`Effect::LoadSecrets`] built, so the
    /// answer is the one `shep.toml` gave when the rows were gathered and
    /// the pane never opens that file itself. Fails closed everywhere it
    /// cannot be answered: a missing key, an unreadable file
    /// (`crate::lookout::secrets::model`) and no open pane all read as `false`.
    pub(crate) fn reveal_gate_open(&self) -> bool {
        matches!(&self.body, Body::Secrets(pane) if pane.model.allow_read)
    }

    /// `v`'s answer: a read of the selected row's stored value, or a refusal
    /// naming the gate and the file.
    ///
    /// The value is not on screen when this returns. [`Self::on_revealed`]
    /// puts it there once the read lands.
    ///
    /// A visibility check on `pane.selected` here, because `move_by` cannot
    /// keep it inside the visible set when that set is empty: every group
    /// folded away and no operator row standing leaves `selected` naming a
    /// hidden row, and nothing else writes it back. Refused silently,
    /// the same answer every other `v` press against a pane that is not
    /// open gives.
    pub(in crate::lookout::app) fn reveal_selected(&mut self) -> Effect {
        if !self.reveal_gate_open() {
            self.notice = Some(Notice {
                text: crate::commands::secret::HOW_TO_ALLOW_READ.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(pane) = self.secrets_pane_mut() else {
            return Effect::None;
        };
        if !pane.visible_row_indices().contains(&pane.selected) {
            return Effect::None;
        }
        let (Some(row), Some(environment)) = (
            pane.model.rows.get(pane.selected).cloned(),
            pane.environment().map(str::to_string),
        ) else {
            return Effect::None;
        };
        // Through `hide`, so a value already on screen goes now rather than
        // sitting there under a read that answers for another key.
        pane.hide();
        pane.pending_reveal = Some(row.key.clone());
        Effect::RevealSecret {
            store: pane.model.store.clone(),
            provider_cache: pane.model.provider_cache.clone(),
            row,
            environment,
        }
    }

    /// `y`'s answer: the already-revealed value, on its way to
    /// [`Effect::CopyToClipboard`], or the `allow_read` refusal.
    ///
    /// A reveal by another route, so it takes [`Self::reveal_gate_open`]'s
    /// own gate rather than a second one, and it copies what
    /// [`SecretsPane::reveal`] already holds on screen rather than reading
    /// the store afresh: a fresh read would let `y` show a value the
    /// operator never asked [`KeyPress::Reveal`] to put on screen, past the
    /// same gate a reveal takes.
    ///
    /// Silent, not the `allow_read` refusal, when the gate is open but
    /// nothing is revealed: the gate is not what is missing there, the same
    /// silence [`Self::reveal_selected`] falls back to for an unrelated
    /// selection.
    pub(in crate::lookout::app) fn copy_revealed(&mut self) -> Effect {
        if !self.reveal_gate_open() {
            self.notice = Some(Notice {
                text: crate::commands::secret::HOW_TO_ALLOW_READ.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        let Some(value) = self
            .secrets_pane_mut()
            .and_then(|pane| pane.reveal.as_ref())
            .map(|reveal| reveal.value.clone())
        else {
            return Effect::None;
        };
        self.notice = Some(Notice {
            text: COPY_SENT_NOTICE.to_string(),
            grave: false,
        });
        Effect::CopyToClipboard(ClipboardValue(value))
    }

    /// An [`Effect::RevealSecret`] has landed.
    ///
    /// Drawn only when the reveal is still the one that was asked for: the
    /// gate can have shut under a fresh model, the tab can have moved, and
    /// every clear trigger drops the pending key. A value that reached the
    /// screen past any of those would be a value nobody asked for, which
    /// for a shut gate is the failure the gate exists to stop.
    pub(in crate::lookout::app) fn on_revealed(
        &mut self,
        key: &str,
        environment: &str,
        value: Option<RevealedValue>,
    ) {
        let gate_open = self.reveal_gate_open();
        let until = self.now + REVEAL_HOLDS;
        let Some(pane) = self.secrets_pane_mut() else {
            return;
        };
        if pane.pending_reveal.as_deref() != Some(key) || pane.environment() != Some(environment) {
            return;
        }
        pane.pending_reveal = None;
        let Some(RevealedValue(value)) = value.filter(|_| gate_open) else {
            return;
        };
        pane.reveal = Some(Reveal {
            key: key.to_string(),
            value,
            until,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::*;
    use super::super::testing::*;
    use super::*;
    use crate::lookout::secrets::SecretRow;
    use crate::lookout::secrets::Source;
    use crate::lookout::view::fixtures;

    /// The gate is why the round trip needs a guard at all: a value drawn
    /// after `allow_read` went false is the failure the gate exists to
    /// stop, and nothing hides a reveal when a fresh model arrives.
    #[test]
    fn a_reveal_that_lands_after_the_gate_closed_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
        let answer = fixtures::ask_to_reveal(&mut app);
        let mut shut = fixtures::secrets_model(dir.path(), false);
        shut.environments = vec!["all".to_string(), "production".to_string()];
        let _ = app.update(Msg::Secrets {
            environment: "production".to_string(),
            result: Ok(Box::new(shut)),
        });

        let _ = app.update(answer);

        assert!(reveal_of(&app).is_none(), "the gate shut while it was read");
    }

    /// A selection move or a tab move, each hiding through
    /// [`SecretsPane::hide`]: the pane is still on screen, still pending
    /// nothing, and a late answer has to find that out rather than land on
    /// a row or a tab the operator has moved past.
    #[test]
    fn a_reveal_that_lands_after_its_reason_went_away_shows_nothing() {
        for (name, press) in [
            ("selection", KeyPress::SelectDown),
            ("tab", KeyPress::TabNext),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
            let answer = fixtures::ask_to_reveal(&mut app);
            let _ = app.update(Msg::Key(press));

            let _ = app.update(answer);

            assert!(
                reveal_of(&app).is_none(),
                "{name} moved on and the answer put the value back"
            );
        }
    }

    /// `close` and `escape` do not leave `SecretsPane` in place the way a
    /// selection or a tab move does: they replace `self.body` with
    /// `Body::FlockTable` outright, so a late answer landing there has
    /// nowhere to write and would show nothing whether or not the guard
    /// works. Reopening the pane before delivering it puts a real
    /// `SecretsPane` back on screen, one with no pending reveal of its
    /// own, so the guard actually has something to refuse.
    #[test]
    fn a_reveal_that_lands_after_the_pane_closed_and_reopened_shows_nothing() {
        for (name, press) in [("close", KeyPress::Secrets), ("escape", KeyPress::Escape)] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
            let answer = fixtures::ask_to_reveal(&mut app);
            let _ = app.update(Msg::Key(press));

            let _ = app.update(Msg::Key(KeyPress::Secrets));
            let _ = app.update(Msg::Secrets {
                environment: "production".to_string(),
                result: Ok(Box::new(fixtures::secrets_model(dir.path(), true))),
            });
            let _ = app.update(answer);

            assert!(
                reveal_of(&app).is_none(),
                "{name} reopened a pane the stale answer names no pending read for"
            );
        }
    }

    /// A tab change reloads, so the answer can arrive against a pane whose
    /// rows are a different environment's: the echo, not the key alone,
    /// says whether it is still the answer that was asked for.
    #[test]
    fn a_reveal_that_lands_for_another_environment_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
        let Msg::Revealed { key, value, .. } = fixtures::ask_to_reveal(&mut app) else {
            panic!("a reveal answers with a value");
        };

        let _ = app.update(Msg::Revealed {
            key,
            environment: "ci".to_string(),
            value,
        });

        assert!(reveal_of(&app).is_none(), "that is another tab's value");
    }

    #[test]
    fn v_reveals_only_when_allow_read_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let mut shut = fixtures::app_with_secrets_and_reads(dir.path(), false);

        let effect = shut.update(Msg::Key(KeyPress::Reveal));

        assert_eq!(effect, Effect::None, "a shut gate does not read the store");
        assert!(reveal_of(&shut).is_none(), "the gate is shut");
        assert!(
            shut.notice()
                .is_some_and(|notice| notice.to_string().contains("allow_read")),
            "and it says which gate and where"
        );

        let mut open = fixtures::app_with_secrets_and_reads(dir.path(), true);
        let answer = fixtures::ask_to_reveal(&mut open);

        let _ = open.update(answer);

        assert_eq!(
            reveal_of(&open).map(|reveal| reveal.key.as_str()),
            Some("DB_PASSWORD")
        );
        assert_eq!(
            reveal_of(&open).map(|reveal| reveal.value.as_str()),
            Some(fixtures::REVEALED_VALUE),
            "the value comes off the store, not out of the model"
        );
    }

    #[test]
    fn copying_says_it_was_sent_rather_than_that_it_arrived() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());

        let _ = app.update(Msg::Key(KeyPress::Copy));

        let notice = notice_of(&app).expect("a notice");
        assert!(notice.contains("sent to the terminal"), "got {notice:?}");
        assert!(
            !notice.contains("copied"),
            "OSC 52 is write-only and many terminals refuse it, so claiming \
                 success is a claim nothing can check: {notice:?}"
        );
    }

    #[test]
    fn copy_needs_a_revealed_value_rather_than_reading_the_store_behind_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), false);

        let _ = app.update(Msg::Key(KeyPress::Copy));

        assert!(
            notice_of(&app).is_some_and(|n| n.contains("allow_read")),
            "copy is a reveal by another route and takes the same gate"
        );
    }

    #[test]
    fn copy_carries_the_revealed_value_to_the_effect() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());

        let Effect::CopyToClipboard(value) = app.update(Msg::Key(KeyPress::Copy)) else {
            panic!("an open gate over a revealed value copies it");
        };

        assert_eq!(value.0, fixtures::REVEALED_VALUE);
    }

    #[test]
    fn a_reveal_clears_on_every_one_of_its_triggers_that_exists_yet() {
        for (name, press) in [
            ("k", KeyPress::SelectUp),
            ("j", KeyPress::SelectDown),
            ("g", KeyPress::SelectFirst),
            ("G", KeyPress::SelectLast),
            ("shift-tab", KeyPress::TabPrev),
            ("tab", KeyPress::TabNext),
            ("escape", KeyPress::Escape),
            ("close", KeyPress::Secrets),
            ("refresh", KeyPress::Refresh),
            ("quit", KeyPress::Quit),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixtures::app_revealing(dir.path());

            let _ = app.update(Msg::Key(press));

            assert!(reveal_of(&app).is_none(), "{name} left the value on screen");
        }

        let dir = tempfile::tempdir().unwrap();
        let mut timed = fixtures::app_revealing(dir.path());
        let start = timed.now();

        let _ = timed.update(Msg::Tick {
            now: start + REVEAL_HOLDS,
        });

        assert!(
            reveal_of(&timed).is_none(),
            "the tenth second is the last one, so the value is gone by it"
        );
    }

    /// Stops a clear-on-every-tick implementation passing the test above for
    /// the wrong reason.
    #[test]
    fn a_reveal_survives_the_tick_before_it_expires() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());
        let start = app.now();

        let _ = app.update(Msg::Tick {
            now: start + REVEAL_HOLDS - Duration::from_millis(1),
        });

        assert!(
            reveal_of(&app).is_some(),
            "clearing early makes the countdown a lie"
        );
    }

    /// The expiry rides the tick's own clock, not `self.now`, which stops
    /// advancing on a dead link. A value that outlived a link failure would
    /// sit on screen until the operator pressed something.
    #[test]
    fn a_frozen_link_does_not_hold_a_value_on_screen() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixtures::app_revealing(dir.path());
        let start = app.now();
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });

        let _ = app.update(Msg::Tick {
            now: start + REVEAL_HOLDS,
        });

        assert!(reveal_of(&app).is_none());
    }

    #[test]
    fn a_key_with_no_value_in_this_tab_reveals_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("secrets.json");
        let mut app = fixtures::app_with_secrets_and_reads(dir.path(), true);
        // The same key, set only in an environment this tab is not showing:
        // `secrets::get` would find a value under `ci` and must not be asked
        // for one.
        let _ = app.update(Msg::Secrets {
            environment: "production".to_string(),
            result: Ok(Box::new(SecretsModel {
                environments: vec!["all".to_string(), "production".to_string()],
                rows: vec![SecretRow {
                    key: "DB_PASSWORD".to_string(),
                    source: Source::Operator,
                    in_force: None,
                    set_in: vec!["ci".to_string()],
                    byte_len: None,
                    readers: Vec::new(),
                }],
                allow_read: true,
                store,
                ..SecretsModel::default()
            })),
        });

        let answer = fixtures::ask_to_reveal(&mut app);
        let _ = app.update(answer);

        assert!(
            reveal_of(&app).is_none(),
            "nothing resolves here, so there is nothing to show"
        );
    }
}
