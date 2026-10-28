//! The dog rows on the settings screen: the enable toggle and the two halves
//! it writes, `e` opening a row onto the dog's other `[daemon] keys` and its
//! own config tree, and the two writes a grant and a boot-first toggle are.

use super::*;

impl App {
    /// One dog toggle's answer: the `Pending::Sent` line this ticket raised
    /// clears, one sentence lands in the status bar, and the screen re-reads
    /// `shep.toml`.
    ///
    /// The sentence lands whether or not the screen is still waiting on this
    /// ticket, since the operator asked for this toggle either way. Only the
    /// prompt is `ticket`'s to clear.
    pub(super) fn on_dog_reply(
        &mut self,
        name: String,
        enable: bool,
        ticket: u64,
        result: Result<Response, RequestError>,
    ) -> Effect {
        if let Some(settings) = self.settings_mut() {
            settings.resolve(ticket);
        }
        let verb = if enable { "enable" } else { "disable" };
        let prefix = format!("{verb} {name}");
        // `EnableDog` answers `Response::DogStarted`; `DisableDog` answers
        // `Response::Deleted`, the reply an older CLI accepts, though the dog
        // stays registered.
        match result {
            Ok(Response::DogStarted(_)) if enable => {
                self.notice = Some(Notice::info(format!("{prefix}: the shepherd started it")));
            }
            Ok(Response::Deleted(_)) if !enable => {
                self.notice = Some(Notice::info(format!(
                    "{prefix}: the shepherd stopped it, and it stays listed as disabled"
                )));
            }
            // Also a mismatched guard above: an `EnableDog` answered by
            // `Response::Deleted`, or the reverse.
            other => {
                self.notice = Some(Notice::grave(format!("{prefix}: {}", failed_reply(other))));
            }
        }
        self.reread_settings()
    }

    /// `e` on a dog's row: open it, or close it.
    ///
    /// The probe is what opening costs, and it is spent here rather than when
    /// the screen opens: a probe runs the dog's binary, so a screen listing six
    /// dogs pays for none until an operator opens one. Silent on every other
    /// row, not refused: `space` and `Enter` already edit those.
    pub(super) fn fold_dog_row(&mut self) -> Effect {
        let Some(index) = self
            .settings()
            .and_then(|settings| match settings.cursor() {
                Some(SettingsRow::Dog(index)) => Some(index),
                _ => None,
            })
        else {
            return Effect::None;
        };
        let Some(settings) = self.body.settings_mut() else {
            return Effect::None;
        };
        match settings.toggle_dog(index) {
            // Mid-read: the read this screen already asked for is the one
            // coming back, and asking again would spend a second probe.
            DogFold::Nothing => Effect::None,
            DogFold::Probe { name, adopted_path } => Effect::LoadDogPane { name, adopted_path },
            // A tree this screen already holds needs no read to open, and a
            // closed one keeps holding its pane for the same reason a dog pane
            // keeps its [`DogProbe`]: nothing else recorded the schema.
            DogFold::Opened | DogFold::Closed => Effect::None,
        }
    }

    /// `space` on one of a dog's other `[daemon] key` rows.
    ///
    /// Only `boot first` arms here: `source` and `channel` are refused in
    /// [`App::cycle_setting`] before the gate is even consulted, and `grants`
    /// is the row its checklist hangs under rather than a toggle of its own.
    pub(super) fn cycle_dog_key(&mut self, dog: usize, key: DogKey) -> Effect {
        let DogKey::BootFirst = key else {
            return Effect::None;
        };
        let Some(settings) = self.body.settings_mut() else {
            return Effect::None;
        };
        let Some((name, first)) = settings
            .dog(dog)
            .map(|dog| dog.name.clone())
            .zip(settings.next_boot_first(dog))
        else {
            return Effect::None;
        };
        let tune = DogTune::BootFirst { name, first };
        let text = tune.text();
        settings.pending = Some(Pending::DogTuneArmed {
            tune,
            text,
            at: self.now,
        });
        Effect::None
    }

    /// `space` on one item of a dog's grants checklist: arms the opposite of
    /// what `[daemon] dog_grants` records for that permission.
    ///
    /// Arming is not granting. What decides whether the permission can be held
    /// at all is [`crate::commands::dogs::record_change`], which both this
    /// screen and `shep dogs grant` write through, so a row this screen offers
    /// and the verb would refuse cannot be written by either.
    pub(super) fn cycle_dog_grant(&mut self, dog: usize, index: usize) -> Effect {
        let Some(settings) = self.body.settings_mut() else {
            return Effect::None;
        };
        let Some((name, permission, grant)) = settings
            .dog(dog)
            .map(|dog| dog.name.clone())
            .zip(settings.next_grant(dog, index))
            .map(|(name, (permission, grant))| (name, permission, grant))
        else {
            return Effect::None;
        };
        let tune = DogTune::Grant {
            name,
            permission,
            grant,
        };
        let text = tune.text();
        settings.pending = Some(Pending::DogTuneArmed {
            tune,
            text,
            at: self.now,
        });
        Effect::None
    }

    /// One `Sent::GrantDog` answer: the prompt this grant raised clears, one
    /// sentence lands, and the screen re-reads `shep.toml` so the checklist
    /// shows the record rather than what was armed.
    pub(super) fn on_dog_grant_reply(
        &mut self,
        name: String,
        ticket: u64,
        result: Result<Response, RequestError>,
    ) -> Effect {
        if let Some(settings) = self.settings_mut() {
            settings.resolve(ticket);
        }
        match result {
            Ok(Response::DogStarted(_)) => {
                self.notice = Some(Notice::info(format!(
                    "{name}: the running dog is told its new grants"
                )));
            }
            other => {
                self.notice = Some(Notice::grave(format!("{name}: {}", failed_reply(other))));
            }
        }
        self.reread_settings()
    }

    /// Says on the dog named `name`'s own row that its config could not be
    /// read, naming `dogs.toml` as the place to edit instead, which is what a
    /// dog pane does when the same probe fails.
    pub(super) fn refuse_settings_dog(&mut self, name: &str, why: String) {
        let Some(settings) = self.body.settings_mut() else {
            return;
        };
        let Some(index) = settings
            .snapshot
            .dogs
            .iter()
            .position(|dog| dog.name == name)
        else {
            return;
        };
        settings.refuse_dog(index, why);
    }

    /// Opens the dog named `name`'s row onto `pane`, built from `schema` and
    /// the section the shepherd answered with.
    pub(super) fn open_settings_dog(
        &mut self,
        name: &str,
        pane: ConfigPane,
        schema: serde_json::Value,
    ) {
        let Some(settings) = self.body.settings_mut() else {
            return;
        };
        let Some(index) = settings
            .snapshot
            .dogs
            .iter()
            .position(|dog| dog.name == name)
        else {
            return;
        };
        settings.open_dog(index, pane, schema);
    }

    /// `space` on a [`SettingsRow::Dog`] row: arms the opposite of the file's
    /// `enabled` bit, refusing in [`LINK_GONE`]'s words while the link is gone.
    ///
    /// The link check is this row's own, unlike [`Self::cycle_key`]: a
    /// confirmed toggle ends in a request to the shepherd. Replaces a
    /// [`Pending::Sent`] for that method's reason.
    pub(super) fn cycle_dog(&mut self, index: usize) -> Effect {
        if self.refuse_over_host() {
            return Effect::None;
        }
        if matches!(self.link, Link::Lost { .. }) {
            self.notice = Some(Notice::grave(LINK_GONE.to_string()));
            return Effect::None;
        }
        let Some(settings) = self.body.settings_mut() else {
            return Effect::None;
        };
        let Some(dog) = settings.snapshot.dogs.get(index) else {
            return Effect::None;
        };
        let name = dog.name.clone();
        let enable = !dog.enabled;
        let text = if enable {
            format!("enable {name}? it starts now, no reload")
        } else {
            format!("disable {name}? it stops now and stays listed as disabled")
        };
        settings.pending = Some(Pending::DogArmed {
            edit: DogEdit { name, enable },
            text,
            at: self.now,
        });
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::view::fixtures;

    #[test]
    fn arming_a_dog_names_the_live_apply_and_not_a_reload() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("it starts now, no reload"), "got: {text}");
    }

    #[test]
    fn disabling_says_it_stays_listed() {
        let mut app = fixtures::app_in_settings_on_enabled_dog("otel");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("stays listed as disabled"), "got: {text}");
    }

    /// One message still yields one effect.
    #[test]
    fn a_written_dog_toggle_raises_the_daemon_half() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteDog { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter must send the file half first");
        };
        let effect = app.update(Msg::DogWritten {
            edit,
            ticket,
            result: Ok(DogEntry {
                source: DogSource::BuiltIn { grants: None },
                env: BTreeMap::default(),
            }),
        });
        assert!(matches!(
            effect,
            Effect::Send(Sent::Dog { enable: true, .. })
        ));
    }

    #[test]
    fn a_refused_file_half_never_reaches_the_shepherd() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteDog { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter must send the file half first");
        };
        let effect = app.update(Msg::DogWritten {
            edit,
            ticket,
            result: Err("permission denied".into()),
        });
        assert_eq!(
            effect,
            Effect::None,
            "a failed write must not ask the shepherd"
        );
        assert!(app.notice().unwrap().is_grave());
    }

    /// The scalars never leave the machine; a dog's second half does.
    #[test]
    fn a_dog_toggle_refuses_while_the_link_is_gone() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".into(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let effect = app.update(Msg::Key(KeyPress::Cycle));
        assert_eq!(effect, Effect::None);
        assert!(app.settings().unwrap().pending().is_none(), "nothing arms");
        assert!(app.notice().unwrap().is_grave());
    }

    #[test]
    fn a_scalar_still_edits_while_the_link_is_gone() {
        let mut app = fixtures::app_in_settings_on("daemon.log_level");
        let _ = app.update(Msg::Frozen {
            at_local: "12:00:00".into(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        assert!(
            app.settings().unwrap().pending().is_some(),
            "a scalar is local file I/O and needs no shepherd"
        );
    }

    /// Drives a dog toggle to `Effect::Send`: arm, confirm the file half, then
    /// land `Msg::DogWritten` so the daemon half goes out.
    fn armed_and_sent_dog(name: &str) -> (App, Sent) {
        let mut app = fixtures::app_in_settings_on_dog(name);
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let Effect::WriteDog { edit, ticket, .. } = app.update(Msg::Key(KeyPress::Confirm)) else {
            panic!("Enter must send the file half first");
        };
        let Effect::Send(sent) = app.update(Msg::DogWritten {
            edit,
            ticket,
            result: Ok(DogEntry {
                source: DogSource::BuiltIn { grants: None },
                env: BTreeMap::default(),
            }),
        }) else {
            panic!("a landed write must raise the daemon half");
        };
        (app, sent)
    }

    /// `EnableDog` answers `Response::DogStarted`. The sentence names what the
    /// shepherd did rather than a bare "done".
    #[test]
    fn a_landed_enable_names_what_the_shepherd_did() {
        let (mut app, sent) = armed_and_sent_dog("metrics");
        assert!(
            app.settings().unwrap().pending().is_some(),
            "the sent line stays up until the reply lands"
        );
        let info = ProcessInfo::builder(50, "metrics", ProcStatus::Online)
            .pid(Some(50_000))
            .dog(Some(DogSource::BuiltIn { grants: None }))
            .build();
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::DogStarted(info)),
        });
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("enable metrics: the shepherd started it")
        );
        assert!(!app.notice().unwrap().is_grave());
        assert!(
            app.settings().unwrap().pending().is_none(),
            "the sent line clears once the reply lands"
        );
    }

    /// `DisableDog` answers `Response::Deleted`, though the dog stays
    /// registered. The sentence says so, since the confirm is gone by now.
    #[test]
    fn a_landed_disable_says_the_dog_stays_listed() {
        let (mut app, sent) = armed_and_sent_dog("otel");
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::Deleted(vec![50])),
        });
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("disable otel: the shepherd stopped it, and it stays listed as disabled")
        );
        assert!(!app.notice().unwrap().is_grave());
    }

    /// Whether the settings screen's own dogs list still says `name` is
    /// enabled. Reads the snapshot, not the file: the two can disagree.
    #[track_caller]
    fn dog_enabled_in_view(app: &App, name: &str) -> bool {
        app.settings()
            .expect("the settings screen is open")
            .snapshot()
            .dogs
            .iter()
            .find(|dog| dog.name == name)
            .expect("the fixture carries this dog")
            .enabled
    }

    /// The file half lands first, so `DogView.enabled` is stale by the time
    /// this reply arrives while running keeps updating off the poll. Without
    /// the re-read a landed `enable metrics` reads `metrics | no | online`.
    #[test]
    fn a_landed_toggle_re_reads_the_file_in_both_directions() {
        for (name, enable) in [("metrics", true), ("otel", false)] {
            let (mut app, sent) = armed_and_sent_dog(name);
            assert_eq!(
                dog_enabled_in_view(&app, name),
                !enable,
                "{name}: the fixture starts on the other bit"
            );
            let reply = if enable {
                Response::DogStarted(
                    ProcessInfo::builder(50, name, ProcStatus::Online)
                        .pid(Some(50_000))
                        .dog(Some(DogSource::BuiltIn { grants: None }))
                        .build(),
                )
            } else {
                Response::Deleted(vec![50])
            };

            let effect = app.update(Msg::Replied {
                sent,
                result: Ok(reply),
            });

            assert_eq!(
                effect,
                Effect::LoadSettings,
                "{name}: a landed toggle has to re-read the file it changed"
            );
            assert_eq!(
                dog_enabled_in_view(&app, name),
                !enable,
                "{name}: nothing is folded into the row by hand -- the re-read is the repair"
            );

            // The re-read landing, with the bit the write put in the file.
            let mut fresh = app.settings().unwrap().snapshot().clone();
            for dog in &mut fresh.dogs {
                if dog.name == name {
                    dog.enabled = enable;
                }
            }
            app.update(Msg::Settings { result: Ok(fresh) });

            assert_eq!(
                dog_enabled_in_view(&app, name),
                enable,
                "{name}: and the row agrees with the file once it lands"
            );
        }
    }

    /// `metrics` is armed as an `enable`, so `Response::Deleted` is the right
    /// shape for the wrong verb and `Response::Pong` is a reply this binary has
    /// never heard of.
    #[test]
    fn an_unrecognised_dog_reply_says_so_rather_than_reading_as_success() {
        for reply in [Response::Pong, Response::Deleted(vec![1])] {
            let (mut app, sent) = armed_and_sent_dog("metrics");
            app.update(Msg::Replied {
                sent,
                result: Ok(reply),
            });
            assert_eq!(
                app.notice().map(ToString::to_string).as_deref(),
                Some(
                    "enable metrics: the shepherd answered something this lookout does not understand"
                )
            );
            assert!(app.notice().unwrap().is_grave());
        }
    }

    #[test]
    fn a_dog_reply_that_failed_to_send_says_so_under_the_same_prefix() {
        let (mut app, sent) = armed_and_sent_dog("metrics");
        app.update(Msg::Replied {
            sent,
            result: Err(RequestError::Closed),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert!(said.starts_with("enable metrics: "), "got {said:?}");
        assert!(said.contains(&RequestError::Closed.to_string()));
    }
}
