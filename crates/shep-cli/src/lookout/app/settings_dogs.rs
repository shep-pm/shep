//! The dog rows on the settings screen: the toggle, the two halves it writes,
//! and the schema probe.

use super::*;

impl App {
    /// One dog toggle's answer: the `Pending::Sent` line this ticket raised
    /// clears, one sentence lands in the status bar, and the screen re-reads
    /// `shep.toml`.
    ///
    /// The sentence lands whether or not the screen is still waiting on this
    /// ticket, since the operator asked for this toggle either way. Only the
    /// prompt is `ticket`'s to clear, and only [`Self::reread_settings`]
    /// decides whether the re-read runs: the file half has already landed, so
    /// `DogView.enabled` is stale whatever the shepherd said. No row is
    /// upserted; the next `ListFlock` repairs RUNNING.
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
        // `Response::Deleted`, the same reply `Delete` gives.
        match result {
            Ok(Response::DogStarted(_)) if enable => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: the shepherd started it"),
                    grave: false,
                });
            }
            Ok(Response::Deleted(_)) if !enable => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: the shepherd stopped and deregistered it"),
                    grave: false,
                });
            }
            // Also a mismatched guard above: an `EnableDog` answered by
            // `Response::Deleted`, or the reverse.
            Ok(_unrecognised) => {
                self.notice = Some(Notice {
                    text: format!(
                        "{prefix}: the shepherd answered something this lookout does not understand"
                    ),
                    grave: true,
                });
            }
            Err(RequestError::Rpc(err)) => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: {}", err.message),
                    grave: true,
                });
            }
            Err(other) => {
                self.notice = Some(Notice {
                    text: format!("{prefix}: {other}"),
                    grave: true,
                });
            }
        }
        self.reread_settings()
    }

    /// `e` on the settings screen: probe the dog under the cursor for its
    /// schema.
    ///
    /// Silent on a scalar row, not refused: `space` and `Enter` already
    /// edit those, and a refusal that was never going to act trains an
    /// operator to ignore the status bar.
    ///
    /// The dogs this can reach are exactly the rows already shown, so no
    /// listing request is needed. An adopted-but-disabled dog stays in
    /// that list: configure-then-enable is the ordinary order.
    pub(super) fn probe_dog_schema(&mut self) -> Effect {
        let Some(settings) = self.settings() else {
            return Effect::None;
        };
        let Some(SettingsRow::Dog(index)) = settings.cursor() else {
            return Effect::None;
        };
        let Some(dog) = settings.snapshot().dogs.get(index) else {
            return Effect::None;
        };
        Effect::LoadDogPane {
            name: dog.name.clone(),
            adopted_path: dog.adopted_path.clone(),
        }
    }

    /// `space` on a [`SettingsRow::Dog`] row: arms the opposite of the file's
    /// `enabled` bit, refusing in [`LINK_GONE`]'s words while the link is gone.
    ///
    /// The link check is this row's own, unlike [`Self::cycle_scalar`]: a
    /// confirmed toggle ends in a request to the shepherd. Replaces a
    /// [`Pending::Sent`] for that method's reason.
    pub(super) fn cycle_dog(&mut self, index: usize) -> Effect {
        if matches!(self.link, Link::Lost { .. }) {
            self.notice = Some(Notice {
                text: LINK_GONE.to_string(),
                grave: true,
            });
            return Effect::None;
        }
        // See the comment in `Self::take_pane_writes`: a direct field
        // match, not `Self::settings_mut`, so `self.now` stays reachable
        // below.
        let Some(settings) = (match &mut self.body {
            Body::Settings(settings) => Some(settings),
            Body::FlockTable
            | Body::ConfigPane(_)
            | Body::Bleats(_)
            | Body::Secrets(_)
            | Body::Sheep(_) => None,
        }) else {
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
            format!("disable {name}? it stops now and is deregistered")
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
    fn disabling_says_it_deregisters() {
        let mut app = fixtures::app_in_settings_on_enabled_dog("otel");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let text = app.settings().unwrap().pending().unwrap().text.to_string();
        assert!(text.contains("deregistered"), "got: {text}");
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
            result: Ok(DogSource::BuiltIn),
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
        let mut app = fixtures::app_in_settings_with_control(); // on log_level
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
            result: Ok(DogSource::BuiltIn),
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
            .dog(Some(DogSource::BuiltIn))
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

    /// `DisableDog` answers `Response::Deleted`, the reply `Delete` gives. The
    /// sentence names the deregistration, since the confirm is gone by now.
    #[test]
    fn a_landed_disable_names_the_deregistration() {
        let (mut app, sent) = armed_and_sent_dog("otel");
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::Deleted(vec![50])),
        });
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("disable otel: the shepherd stopped and deregistered it")
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
                        .dog(Some(DogSource::BuiltIn))
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
