//! A dog pane's `shep.pane` pull: the manifest read that decides whether to
//! ask, and how the answer, or the reason there is none, lands on the pane.

use super::*;

impl App {
    /// `Msg::DogDeclaresPane`'s arm: the manifest has been read, and a dog
    /// that declares a pane is asked for it.
    ///
    /// Dropped when the dog's config pane is not the one open, or is a later
    /// opening than the one the read was raised for. A pull already waiting
    /// is not sent a second time.
    pub(super) fn on_dog_declares_pane(
        &mut self,
        name: &str,
        opening: u64,
        declared: Option<DeclaredPane>,
    ) -> Effect {
        let Some(declared) = declared else {
            return Effect::None;
        };
        let Some(pane) = self.dog_pane_of(name, opening) else {
            return Effect::None;
        };
        pane.declare_pane(declared);
        if pane.status_mut().is_some_and(DogStatus::begin_pull) {
            Effect::Send(Sent::DogPane {
                name: name.to_owned(),
                opening,
            })
        } else {
            Effect::None
        }
    }

    /// One `shep.pane` pull's reply, read for the dog's own pane.
    pub(super) fn on_dog_pane_pulled(
        &mut self,
        name: &str,
        opening: u64,
        result: Result<Response, RequestError>,
    ) -> Effect {
        self.land_dog_pane(name, opening, read_pulled_pane(name, result));
        Effect::None
    }

    /// Lands a pull's answer on the dog's pane, dropped when the operator has
    /// left that pane, or has opened it again since the pull was sent.
    pub(super) fn land_dog_pane(&mut self, name: &str, opening: u64, answer: Result<Pane, String>) {
        if let Some(status) = self
            .dog_pane_of(name, opening)
            .and_then(ConfigPane::status_mut)
        {
            status.land(answer);
        }
    }

    /// The open config pane, when it is dog `name`'s and the `opening` given
    /// is the latest.
    pub(super) fn dog_pane_of(&mut self, name: &str, opening: u64) -> Option<&mut ConfigPane> {
        if opening != self.dog_opening {
            return None;
        }
        self.config_pane_mut().filter(
            |pane| matches!(pane.target(), PaneTarget::Dog { name: open, .. } if open == name),
        )
    }
}

/// What a `shep.pane` pull came back with, as a pane or as the one line the
/// operator reads in its place.
///
/// Every line is run through [`sanitise`](crate::terminal_safe::sanitise): a
/// dog's own words are quoted in one, and nothing it writes may drive the
/// terminal or break the line.
fn read_pulled_pane(name: &str, result: Result<Response, RequestError>) -> Result<Pane, String> {
    replied_body(name, result)
        .and_then(|body| Pane::read(&body))
        .map_err(|line| crate::terminal_safe::sanitise(&line).0)
}

/// The body dog `name` replied to a trigger with, or the one line that says
/// why there is none.
pub(super) fn replied_body(
    name: &str,
    result: Result<Response, RequestError>,
) -> Result<String, String> {
    match result {
        Ok(Response::Triggered(rows)) => {
            let row = rows
                .into_iter()
                .find(|row| row.name == name)
                .ok_or_else(|| format!("{name} gave no answer"))?;
            match row.outcome {
                ActionOutcome::Replied { body } => Ok(body),
                ActionOutcome::TimedOut => Err(format!("{name} did not answer in time")),
                ActionOutcome::NoChannel => Err(format!("{name} has no shepherd channel")),
                ActionOutcome::DogNoChannel => {
                    Err(format!("{name} was started without a shepherd channel"))
                }
                ActionOutcome::Skipped => {
                    Err(format!("{name} is being replaced, so it was not asked"))
                }
                _unknown => Err(format!("{name} answered in a way this shep does not know")),
            }
        }
        other => Err(failed_reply(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::pane::PaneState;
    use shep_core::protocol::{ActionOutcome, ActionReply};

    /// The first opening of a pane, which every test here opens once.
    const OPENING: u64 = 1;

    /// The row's own dog, adopted from `/opt/paddock`, with the gate open.
    fn adopted_dog_row() -> ProcessInfo {
        ProcessInfo::builder(92, "paddock", ProcStatus::Online)
            .pid(Some(92_000))
            .dog(Some(DogSource::Adopted {
                grants: None,
                path: "/opt/paddock".to_string(),
                channel: true,
                events: Vec::new(),
            }))
            .build()
    }

    /// `dog`'s config pane opened the way the event loop opens it: `e`, the
    /// schema, then the section. Returns the app and what the section's reply
    /// raised.
    #[track_caller]
    fn open_dog_pane(dog: ProcessInfo) -> (App, Effect) {
        let name = dog.name.clone();
        let mut app = fixtures::app_with(vec![dog], fixtures::plain());
        app.set_control_for_tests(Control::Allowed);
        let Effect::LoadDogPane { adopted_path, .. } = app.update(Msg::Key(KeyPress::Edit)) else {
            panic!("`e` on a dog row probes its schema");
        };
        app.update(Msg::DogPane {
            name: name.clone(),
            adopted_path,
            env_keys: Vec::new(),
            result: Ok(serde_json::json!({
                "type": "object",
                "properties": { "ratio": { "type": "number" } },
            })),
        });
        let effect = app.update(Msg::Replied {
            sent: Sent::DogSection { name },
            result: Ok(Response::DogSection {
                toml: "ratio = 1.5\n".to_string().into(),
            }),
        });
        (app, effect)
    }

    fn declared() -> DeclaredPane {
        DeclaredPane {
            actions: vec!["revoke".to_string()],
            events: vec!["lease.changed".to_string()],
        }
    }

    /// An adopted dog's pane with its pull out, ready for an answer.
    #[track_caller]
    fn pulling_paddock() -> App {
        let (mut app, _) = open_dog_pane(adopted_dog_row());
        let effect = app.update(Msg::DogDeclaresPane {
            name: "paddock".to_string(),
            opening: OPENING,
            declared: Some(declared()),
        });
        assert_eq!(effect, Effect::Send(pull_of("paddock")));
        app
    }

    fn pull_of(name: &str) -> Sent {
        Sent::DogPane {
            name: name.to_string(),
            opening: OPENING,
        }
    }

    fn triggered(name: &str, outcome: ActionOutcome) -> Result<Response, RequestError> {
        Ok(Response::Triggered(vec![ActionReply {
            id: 92,
            name: name.to_string(),
            outcome,
        }]))
    }

    fn answer(app: &mut App, result: Result<Response, RequestError>) {
        let effect = app.update(Msg::Replied {
            sent: pull_of("paddock"),
            result,
        });
        assert_eq!(effect, Effect::None, "an answer raises nothing");
    }

    #[track_caller]
    fn state_of(app: &App) -> PaneState {
        app.config_pane()
            .and_then(ConfigPane::status)
            .expect("the dog declared a pane")
            .state()
            .clone()
    }

    fn a_pane() -> Pane {
        Pane::new().fields("host", [("VRAM", "20 / 24 GiB")])
    }

    #[test]
    fn opening_an_adopted_dogs_pane_reads_its_manifest() {
        let (app, effect) = open_dog_pane(adopted_dog_row());
        assert_eq!(
            effect,
            Effect::ReadDogPane {
                name: "paddock".to_string(),
                opening: OPENING,
                adopted_path: PathBuf::from("/opt/paddock"),
            }
        );
        assert!(
            app.config_pane()
                .is_some_and(|pane| pane.status().is_none()),
            "nothing is declared until the manifest says so"
        );
    }

    #[test]
    fn opening_a_built_in_dogs_pane_reads_no_manifest() {
        let bark = ProcessInfo::builder(91, "bark", ProcStatus::Online)
            .dog(Some(DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            }))
            .build();
        let (app, effect) = open_dog_pane(bark);
        assert_eq!(effect, Effect::None);
        assert!(app.config_pane().is_some(), "and the pane opens as it did");
    }

    #[test]
    fn a_pane_over_a_host_reads_no_manifest() {
        let mut app = fixtures::app_with(vec![adopted_dog_row()], fixtures::plain());
        app.set_control_for_tests(Control::Allowed);
        app.update(Msg::DogPane {
            name: "paddock".to_string(),
            adopted_path: Some(PathBuf::from("/opt/paddock")),
            env_keys: Vec::new(),
            result: Ok(serde_json::json!({ "type": "object", "properties": {} })),
        });
        app.set_remote_host("build-01");
        let effect = app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "paddock".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: String::new().into(),
            }),
        });
        assert_eq!(effect, Effect::None, "the cache is this machine's");
    }

    /// The manifest was read when the pane opened, and a re-read of the
    /// section is not an open.
    #[test]
    fn a_re_read_reads_no_manifest_and_keeps_what_was_pulled() {
        let mut app = pulling_paddock();
        answer(
            &mut app,
            triggered(
                "paddock",
                ActionOutcome::Replied {
                    body: a_pane().body().unwrap(),
                },
            ),
        );
        let effect = app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "paddock".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: "ratio = 2.0\n".to_string().into(),
            }),
        });
        assert_eq!(effect, Effect::None);
        assert_eq!(state_of(&app), PaneState::Ready(a_pane()));
        assert_eq!(
            app.config_pane()
                .and_then(ConfigPane::status)
                .map(DogStatus::declared),
            Some(&declared())
        );
    }

    #[test]
    fn a_dog_that_declares_a_pane_is_asked_for_it_and_its_actions_and_events_are_kept() {
        let (mut app, _) = open_dog_pane(adopted_dog_row());
        let effect = app.update(Msg::DogDeclaresPane {
            name: "paddock".to_string(),
            opening: OPENING,
            declared: Some(declared()),
        });
        assert_eq!(effect, Effect::Send(pull_of("paddock")));
        let status = app
            .config_pane()
            .and_then(ConfigPane::status)
            .expect("declared");
        assert_eq!(status.declared(), &declared());
        assert_eq!(status.state(), &PaneState::Waiting);
    }

    #[test]
    fn a_dog_that_declares_none_is_asked_for_nothing() {
        let (mut app, _) = open_dog_pane(adopted_dog_row());
        let effect = app.update(Msg::DogDeclaresPane {
            name: "paddock".to_string(),
            opening: OPENING,
            declared: None,
        });
        assert_eq!(effect, Effect::None);
        assert!(
            app.config_pane()
                .is_some_and(|pane| pane.status().is_none())
        );
    }

    #[test]
    fn a_second_pull_while_one_is_waiting_is_not_sent() {
        let mut app = pulling_paddock();
        let again = app.update(Msg::DogDeclaresPane {
            name: "paddock".to_string(),
            opening: OPENING,
            declared: Some(declared()),
        });
        assert_eq!(again, Effect::None);
        assert_eq!(state_of(&app), PaneState::Waiting);
    }

    #[test]
    fn a_pull_answered_is_ready_with_the_pane_read() {
        let mut app = pulling_paddock();
        answer(
            &mut app,
            triggered(
                "paddock",
                ActionOutcome::Replied {
                    body: a_pane().body().unwrap(),
                },
            ),
        );
        assert_eq!(state_of(&app), PaneState::Ready(a_pane()));
    }

    #[test]
    fn a_pull_that_comes_to_nothing_says_why_in_one_line() {
        let cases = [
            (
                triggered("paddock", ActionOutcome::TimedOut),
                "paddock did not answer in time",
            ),
            (
                triggered("paddock", ActionOutcome::NoChannel),
                "paddock has no shepherd channel",
            ),
            (
                triggered("paddock", ActionOutcome::DogNoChannel),
                "paddock was started without a shepherd channel",
            ),
            (
                triggered("paddock", ActionOutcome::Skipped),
                "paddock is being replaced, so it was not asked",
            ),
            (
                Err(RequestError::Timeout {
                    after: Duration::from_secs(60),
                }),
                "no reply within 60s",
            ),
        ];
        for (result, reason) in cases {
            let mut app = pulling_paddock();
            answer(&mut app, result);
            assert_eq!(state_of(&app), PaneState::Unavailable(reason.to_string()));
        }
    }

    /// The dog's own words are quoted at `shep.validate`'s cap, and nothing it
    /// writes breaks the line.
    #[test]
    fn an_unreadable_answer_is_quoted_at_the_cap_on_one_line() {
        let mut app = pulling_paddock();
        let body = format!("{}\n\u{1b}[2J{}", "x".repeat(150), "y".repeat(300));
        answer(
            &mut app,
            triggered("paddock", ActionOutcome::Replied { body }),
        );
        let PaneState::Unavailable(reason) = state_of(&app) else {
            panic!("an unreadable answer holds no pane");
        };
        assert!(
            reason.starts_with("the dog answered with something shep cannot read: xxx"),
            "{reason}"
        );
        assert!(reason.ends_with("..."), "{reason}");
        assert!(!reason.contains(['\n', '\u{1b}']), "{reason:?}");
        assert!(reason.len() < 300, "{} bytes", reason.len());
    }

    /// One slow answer does not blank the tab.
    #[test]
    fn a_pull_that_fails_keeps_the_last_good_pane_and_the_reason() {
        let mut app = pulling_paddock();
        answer(
            &mut app,
            triggered(
                "paddock",
                ActionOutcome::Replied {
                    body: a_pane().body().unwrap(),
                },
            ),
        );
        let status = |app: &App| {
            app.config_pane()
                .and_then(ConfigPane::status)
                .cloned()
                .unwrap()
        };
        assert_eq!(status(&app).failed(), None);

        answer(&mut app, triggered("paddock", ActionOutcome::TimedOut));
        assert_eq!(state_of(&app), PaneState::Ready(a_pane()));
        assert_eq!(
            status(&app).failed(),
            Some("paddock did not answer in time")
        );

        answer(
            &mut app,
            triggered(
                "paddock",
                ActionOutcome::Replied {
                    body: a_pane().body().unwrap(),
                },
            ),
        );
        assert_eq!(status(&app).failed(), None, "a good answer clears it");
    }

    #[test]
    fn a_pull_nobody_took_ends_the_wait() {
        let mut app = pulling_paddock();
        app.update(Msg::Unsent {
            sent: pull_of("paddock"),
        });
        assert_eq!(
            state_of(&app),
            PaneState::Unavailable("paddock: the pane was not asked for".to_string())
        );
        let again = app.update(Msg::DogDeclaresPane {
            name: "paddock".to_string(),
            opening: OPENING,
            declared: Some(declared()),
        });
        assert_eq!(
            again,
            Effect::Send(pull_of("paddock")),
            "and may be asked again"
        );
    }

    #[test]
    fn an_answer_for_a_dog_the_operator_has_left_is_dropped() {
        let mut app = pulling_paddock();
        app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
        answer(&mut app, triggered("paddock", ActionOutcome::TimedOut));
        assert!(app.config_pane().is_none());
        assert!(app.notice().is_none(), "silently: nothing went wrong");

        // And a manifest read that lands late asks for nothing.
        let late = app.update(Msg::DogDeclaresPane {
            name: "paddock".to_string(),
            opening: OPENING,
            declared: Some(declared()),
        });
        assert_eq!(late, Effect::None);
    }

    #[test]
    fn an_answer_for_another_dogs_pull_does_not_land_on_this_pane() {
        let mut app = pulling_paddock();
        app.update(Msg::Replied {
            sent: pull_of("kelpie"),
            result: triggered("kelpie", ActionOutcome::TimedOut),
        });
        assert_eq!(state_of(&app), PaneState::Waiting);
    }

    /// The pane left and opened again: what was asked of the first is not
    /// what the second is waiting for.
    #[test]
    fn a_read_or_a_pull_from_an_earlier_opening_does_not_land_on_a_later_pane() {
        let mut app = pulling_paddock();
        app.update(Msg::Key(KeyPress::Escape));
        let (_, raised) = reopen(&mut app);
        assert_eq!(
            raised,
            Effect::ReadDogPane {
                name: "paddock".to_string(),
                opening: OPENING + 1,
                adopted_path: PathBuf::from("/opt/paddock"),
            }
        );

        // The first opening's manifest read lands on the second pane.
        let late = app.update(Msg::DogDeclaresPane {
            name: "paddock".to_string(),
            opening: OPENING,
            declared: Some(declared()),
        });
        assert_eq!(late, Effect::None);
        assert!(
            app.config_pane()
                .is_some_and(|pane| pane.status().is_none())
        );

        // The second's own read is sent, and the first's pull answers into it.
        let own = app.update(Msg::DogDeclaresPane {
            name: "paddock".to_string(),
            opening: OPENING + 1,
            declared: Some(declared()),
        });
        let Effect::Send(second) = own else {
            panic!("the second opening's own read asks");
        };
        answer(&mut app, triggered("paddock", ActionOutcome::TimedOut));
        assert_eq!(state_of(&app), PaneState::Waiting, "still waiting");
        app.update(Msg::Replied {
            sent: second,
            result: triggered("paddock", ActionOutcome::TimedOut),
        });
        assert_eq!(
            state_of(&app),
            PaneState::Unavailable("paddock did not answer in time".to_string())
        );
    }

    /// Opens `paddock`'s pane again on `app`, as [`open_dog_pane`] does.
    fn reopen(app: &mut App) -> (&mut App, Effect) {
        let Effect::LoadDogPane { adopted_path, .. } = app.update(Msg::Key(KeyPress::Edit)) else {
            panic!("`e` on a dog row probes its schema");
        };
        app.update(Msg::DogPane {
            name: "paddock".to_string(),
            adopted_path,
            env_keys: Vec::new(),
            result: Ok(serde_json::json!({
                "type": "object",
                "properties": { "ratio": { "type": "number" } },
            })),
        });
        let effect = app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "paddock".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: "ratio = 1.5\n".to_string().into(),
            }),
        });
        (app, effect)
    }

    /// A re-read is not an opening, so a pull out under the pane still lands.
    #[test]
    fn a_re_read_does_not_make_an_opening_of_its_own() {
        let mut app = pulling_paddock();
        let again = app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "paddock".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: "ratio = 2.5\n".to_string().into(),
            }),
        });
        assert_eq!(again, Effect::None);
        answer(&mut app, triggered("paddock", ActionOutcome::TimedOut));
        assert_eq!(
            state_of(&app),
            PaneState::Unavailable("paddock did not answer in time".to_string())
        );
    }

    /// A section reply for one dog can land while another's pane is open.
    #[test]
    fn a_pane_rebuilt_for_another_dog_takes_no_pull_from_the_open_one() {
        let pane_of = |name: &str| {
            ConfigPane::dog(
                name.to_string(),
                Some(PathBuf::from("/opt/x")),
                serde_json::json!({ "type": "object", "properties": {} }),
                String::new(),
                Vec::new(),
            )
        };
        let mut open = pane_of("paddock");
        open.declare_pane(declared());

        let mut other = pane_of("kelpie");
        other.adopt_status_from(&open);
        assert!(other.status().is_none(), "kelpie never declared a pane");

        let mut same = pane_of("paddock");
        same.adopt_status_from(&open);
        assert_eq!(
            same.status().map(DogStatus::declared),
            Some(&declared()),
            "a re-read of paddock's own keeps it"
        );
    }
}
