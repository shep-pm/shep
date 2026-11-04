//! A dog's own config pane: the schema probe, the section it opens on, and the
//! write it sends back.

use super::*;

impl App {
    /// Whether the open pane draws its rows as a tree of tables, which is
    /// what the keymap overlay reads to caption `←/→`.
    #[must_use]
    pub(in crate::lookout) fn over_a_tree(&self) -> bool {
        self.config_pane().is_some_and(ConfigPane::is_tree)
    }

    /// A dog's schema probe answered. `Ok` parks the schema and asks the
    /// shepherd for the section; the pane is built once that lands. `Err`
    /// gets no pane, and the refusal names the file to edit instead. The
    /// settings screen stays open until then.
    pub(super) fn on_dog_pane(
        &mut self,
        name: String,
        adopted_path: Option<PathBuf>,
        env_keys: Vec<String>,
        result: Result<serde_json::Value, String>,
    ) -> Effect {
        // The settings screen asked for this dog's own tree, so the answer
        // belongs to a row rather than to a pane: the screen stays up and the
        // tree fills in under the dog's header.
        if self.settings().is_some_and(|s| s.is_reading_a_dog(&name)) {
            return match result {
                Ok(schema) => {
                    if let Some(settings) = self.settings_mut() {
                        settings.dog_schema(&name, schema);
                    }
                    Effect::Send(Sent::DogSection { name })
                }
                Err(message) => {
                    self.refuse_settings_dog(&name, message);
                    Effect::None
                }
            };
        }
        match result {
            Ok(schema) => {
                self.dog_target = Some(DogProbe {
                    name: name.clone(),
                    adopted_path,
                    schema,
                    env_keys,
                });
                Effect::Send(Sent::DogSection { name })
            }
            Err(message) => {
                self.notice = Some(Notice::grave(message));
                Effect::None
            }
        }
    }

    /// One `Request::DogConfig` reply: the dog's section, which is the
    /// second half of an open.
    ///
    /// Guarded on [`Self::dog_target`] the way [`Self::on_sheep_config`] is
    /// guarded on `config_target`, and for the same reason: a reply for a dog
    /// the operator has already left must not re-open a pane over the screen
    /// they went back to.
    pub(super) fn on_dog_section(
        &mut self,
        name: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        // The settings screen's own read, routed by the row's state rather
        // than by `dog_target`, which is what a dog pane holds its probe in.
        if self.settings().is_some_and(|s| s.is_reading_a_dog(name)) {
            return self.on_settings_dog_section(name, result);
        }
        let Some(probe) = self.dog_target.clone().filter(|probe| probe.name == name) else {
            return Effect::None;
        };
        self.config_read_in_flight = false;
        if self.closing {
            return self.on_close_reread(name, result);
        }
        match result {
            Ok(Response::DogSection { toml }) => {
                // A re-read rebuilds a pane that is already this dog's, and
                // its manifest was read when it opened.
                let opening = !self.config_pane().is_some_and(|pane| {
                    matches!(pane.target(), PaneTarget::Dog { name: open, .. } if open == name)
                });
                let adopted = probe.adopted_path.clone();
                let dropped = self.rebuild_dog_pane(probe, toml.as_str());
                if dropped > 0 {
                    self.notice =
                        Some(Notice::grave(format!("{name}: {}", moved_entries(dropped))));
                }
                // Over `--host` the manifest cache is this machine's and the
                // dog's binary is not here, so no pane is declared.
                if opening
                    && self.remote_host.is_none()
                    && let Some(adopted_path) = adopted
                {
                    return Effect::ReadDogPane {
                        name: name.to_owned(),
                        adopted_path,
                    };
                }
            }
            other => {
                self.notice = Some(Notice::grave(format!("{name}: {}", failed_reply(other))));
            }
        }
        Effect::None
    }

    /// The settings screen's own half of a `Request::DogConfig` reply: the
    /// schema this screen already parked beside the row, the section the
    /// shepherd just answered with, and a [`ConfigPane::dog`] built from both.
    /// The tree fills in under the dog's header; the screen never leaves.
    ///
    /// A refusal closes the row and puts the sentence on it, pointing the
    /// operator at `dogs.toml`.
    fn on_settings_dog_section(
        &mut self,
        name: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        let Some((schema, adopted_path)) = self.settings().and_then(|settings| {
            let schema = settings.dog_tree_named(name)?.schema()?.clone();
            let dog = settings.dog_named(name)?;
            Some((schema, dog.adopted_path.clone()))
        }) else {
            return Effect::None;
        };
        match result {
            Ok(Response::DogSection { toml }) => {
                // No `env` keys here: the `env` rows above the pane's own
                // read them off the settings snapshot instead.
                let pane = ConfigPane::dog(
                    name.to_owned(),
                    adopted_path,
                    schema.clone(),
                    toml.as_str().to_owned(),
                    Vec::new(),
                );
                self.open_settings_dog(name, pane, schema);
            }
            other => self.refuse_settings_dog(name, format!("{name}: {}", failed_reply(other))),
        }
        Effect::None
    }

    /// Replaces the open pane with one over `toml`, and returns how many of
    /// its edits the new section took no further. See
    /// [`ConfigPane::adopt_edits_from`].
    ///
    /// Everything a refresh has to carry across is read before the rebuild
    /// replaces the pane: see `Self::on_sheep_config`, which states the
    /// argument for each.
    pub(super) fn rebuild_dog_pane(&mut self, probe: DogProbe, toml: &str) -> usize {
        let carried = self.config_pane().map(|pane| pane.view().clone());
        let carried_list = self
            .config_pane()
            .and_then(ConfigPane::list)
            .map(|list| (list.key().to_owned(), list.view().clone()));
        // The tables the operator opened or closed ride along with the
        // cursor, which is an index into the rows they shape.
        let carried_open = self.config_pane().map(|pane| pane.open_tables().clone());
        let mut pane = ConfigPane::dog(
            probe.name,
            probe.adopted_path,
            probe.schema,
            toml.to_owned(),
            probe.env_keys,
        );
        let dropped = self
            .config_pane()
            .map_or(0, |previous| pane.adopt_edits_from(previous));
        if let Some(previous) = self.config_pane() {
            pane.adopt_status_from(previous);
        }
        if let Some(open) = carried_open {
            pane.adopt_open_tables(&open);
        }
        if let Some(carried) = carried {
            pane.adopt_view(carried);
        }
        if let Some((key, carried)) = carried_list {
            pane.adopt_list_view(&key, carried);
        }
        // The settings screen is what a dog pane opens over, and this one
        // assignment is what closes it: `Body` holds one variant, so the pane
        // replaces it once there is something to look at.
        self.body = Body::ConfigPane(Box::new(pane));
        self.release_text_mode_if_unowned();
        // Same as `Self::open_or_refresh_config_pane`: an overlay open when
        // this reply landed must not survive the body change under it.
        self.keymap_open = false;
        dropped
    }

    /// `Msg::DogDeclaresPane`'s arm: the manifest has been read, and a dog
    /// that declares a pane is asked for it.
    ///
    /// Dropped when the dog's config pane is not the one open. A pull already
    /// waiting is not sent a second time.
    pub(super) fn on_dog_declares_pane(
        &mut self,
        name: &str,
        declared: Option<DeclaredPane>,
    ) -> Effect {
        let Some(declared) = declared else {
            return Effect::None;
        };
        let Some(pane) = self.dog_pane_mut(name) else {
            return Effect::None;
        };
        pane.declare_pane(declared);
        if pane.status_mut().is_some_and(DogStatus::begin_pull) {
            Effect::Send(Sent::DogPane {
                name: name.to_owned(),
            })
        } else {
            Effect::None
        }
    }

    /// One `shep.pane` pull's reply, read for the dog's own pane.
    pub(super) fn on_dog_pane_pulled(
        &mut self,
        name: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        self.land_dog_pane(name, read_pulled_pane(name, result));
        Effect::None
    }

    /// Lands a pull's answer on the dog's pane, dropped when the operator has
    /// left that pane.
    pub(super) fn land_dog_pane(&mut self, name: &str, answer: Result<Pane, String>) {
        if let Some(status) = self.dog_pane_mut(name).and_then(ConfigPane::status_mut) {
            status.land(answer);
        }
    }

    /// The open config pane, when it is dog `name`'s.
    fn dog_pane_mut(&mut self, name: &str) -> Option<&mut ConfigPane> {
        self.config_pane_mut().filter(
            |pane| matches!(pane.target(), PaneTarget::Dog { name: open, .. } if open == name),
        )
    }

    /// One `Request::SetDogConfig` reply.
    ///
    /// Reaches for no pane: a write goes out when the pane closes, so this
    /// lands with the dashboard on screen. A success still re-reads the
    /// section, the same shape [`Self::on_field_applied`] has. The sentence
    /// says the change was published and stops there: whether the dog acted
    /// on it is the dog's own answer.
    pub(super) fn on_dog_section_set(
        &mut self,
        name: &str,
        ticket: u64,
        result: Result<Response, RequestError>,
    ) -> Effect {
        match result {
            Ok(Response::DogConfigSet { .. }) => {
                // The settings screen's own write: its prompt is this ticket's
                // to clear, and its tree goes back to waiting on the re-read
                // raised below so the row shows what the file now says rather
                // than what was typed into it.
                if let Some(settings) = self.settings_mut() {
                    settings.resolve(ticket);
                    settings.reload_dog(name);
                }
                self.notice = Some(Notice::info(format!(
                    "{name}: its config is written, and {name} is told"
                )));
                Effect::Send(Sent::DogSection {
                    name: name.to_owned(),
                })
            }
            other => {
                if let Some(settings) = self.settings_mut() {
                    settings.resolve(ticket);
                }
                self.notice = Some(Notice::grave(format!("{name}: {}", failed_reply(other))));
                Effect::None
            }
        }
    }
}

/// What a `shep.pane` pull came back with, as a pane or as the one line the
/// operator reads in its place.
///
/// Every line is run through [`sanitise`](crate::terminal_safe::sanitise): a
/// dog's own words are quoted in one, and nothing it writes may drive the
/// terminal or break the line.
fn read_pulled_pane(name: &str, result: Result<Response, RequestError>) -> Result<Pane, String> {
    let reason = match result {
        Ok(Response::Triggered(rows)) => {
            let row = rows
                .into_iter()
                .find(|row| row.name == name)
                .ok_or_else(|| format!("{name} gave no answer"))?;
            match row.outcome {
                ActionOutcome::Replied { body } => Pane::read(&body),
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
    };
    reason.map_err(|line| crate::terminal_safe::sanitise(&line).0)
}

/// What the notice says of edits a refresh dropped.
pub(super) fn moved_entries(dropped: usize) -> String {
    if dropped == 1 {
        "1 edit under an entry that moved in the file was dropped".to_owned()
    } else {
        format!("{dropped} edits under entries that moved in the file were dropped")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use crate::lookout::pane::PaneState;
    use shep_core::protocol::RpcError;
    use shep_core::protocol::RpcErrorCode;
    use shep_core::protocol::{ActionOutcome, ActionReply};
    use std::path::Path;

    #[test]
    fn e_on_a_dog_row_opens_its_pane_instead_of_refusing() {
        let mut app = fixtures::app_with_a_dog_selected_and_control();
        let effect = app.update(Msg::Key(KeyPress::Edit));
        let Effect::LoadDogPane { name, adopted_path } = effect else {
            panic!("expected a dog pane, got {effect:?}");
        };
        assert_eq!(name, "otel");
        // The path comes off the row, not the settings screen.
        assert_eq!(adopted_path.as_deref(), Some(Path::new("/opt/otel")));
        assert!(app.notice().is_none(), "{:?}", app.notice());
    }

    #[test]
    fn e_on_a_built_in_dog_opens_a_pane_with_no_path() {
        let mut app = fixtures::app_with_a_built_in_dog_selected_and_control();
        let effect = app.update(Msg::Key(KeyPress::Edit));
        let Effect::LoadDogPane { adopted_path, .. } = effect else {
            panic!("expected a dog pane, got {effect:?}");
        };
        // A built-in dog is the shep binary's own argv branch, so there is no
        // adopted path to probe and the pane asks the running binary instead.
        assert_eq!(adopted_path, None);
    }

    /// `EngineStopped` is the one of this request's three refusals with no
    /// subject of its own: `rpc_error` renders it as `the supervisor
    /// engine has stopped`, full stop, naming neither the sheep nor the
    /// screen it came from.
    #[test]
    fn a_refusal_that_names_nothing_still_reaches_the_operator_named() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::Internal,
                message: "the supervisor engine has stopped".to_string(),
                daemon_version: None,
            })),
        });
        let said = app.notice().map(ToString::to_string).unwrap_or_default();
        assert_eq!(said, "web: the supervisor engine has stopped");
        assert!(app.notice().unwrap().is_grave());
    }

    #[test]
    fn a_config_reply_for_a_pane_nobody_wants_is_dropped() {
        let mut app =
            fixtures::with_selection(ProcessInfo::builder(9, "web", ProcStatus::Online).build());
        let reply = || Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        };
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Key(KeyPress::Edit));
        app.update(reply());
        assert!(app.config_pane().is_some(), "the first answer opens it");

        app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());

        app.update(reply());
        assert!(
            app.config_pane().is_none(),
            "the second answer lands on a dashboard and is dropped"
        );
        assert!(app.notice().is_none(), "silently: nothing went wrong");
    }

    /// Closing must clear the target it is keyed on: left stale, it would
    /// either re-open on a stray reply or refuse the next open.
    #[test]
    fn e_still_opens_a_pane_after_one_was_closed() {
        let mut app = fixtures::app_in_sheep_pane();
        app.update(Msg::Key(KeyPress::Escape));
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert!(app.config_pane().is_some());
    }

    /// The path is what carries a probe to an adopted dog's own binary; a
    /// built-in dog answers in-process and has none.
    #[test]
    fn e_on_a_settings_dog_row_probes_that_dogs_own_binary() {
        let mut app = fixtures::app_in_settings_on_dog("otel");
        assert_eq!(
            app.update(Msg::Key(KeyPress::Edit)),
            Effect::LoadDogPane {
                name: "otel".to_string(),
                adopted_path: Some(std::path::PathBuf::from("/usr/local/bin/shep-otel")),
            }
        );
        assert!(
            app.config_pane().is_none(),
            "the pane opens on the answer, never on the keypress"
        );
        assert!(
            app.settings().is_some(),
            "and the screen stays up until it does"
        );
    }

    /// Those rows are the settings screen's own subject, and `space` and
    /// `Enter` already edit them.
    #[test]
    fn e_on_a_settings_scalar_row_does_nothing_at_all() {
        let mut app = fixtures::app_in_settings();
        assert_eq!(app.update(Msg::Key(KeyPress::Edit)), Effect::None);
        assert!(app.config_pane().is_none());
        assert!(app.notice().is_none(), "and says nothing about it");
    }

    /// The flock with the built-in dog `name` as its only row, the gate open,
    /// and the cursor already on it: the door a dog pane has always opened
    /// from, which `e` on a settings row no longer is.
    #[track_caller]
    fn flock_on_dog(name: &str) -> App {
        let dog = ProcessInfo::builder(91, name, ProcStatus::Online)
            .pid(Some(91_000))
            .dog(Some(DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            }))
            .build();
        let mut app = fixtures::app_with(vec![dog], fixtures::plain());
        app.set_control_for_tests(Control::Allowed);
        app
    }

    /// The row is the only door an adopted dog's pane has to its own binary,
    /// and `shows_env` is keyed on it: a row reporting a built-in, or none,
    /// opens a pane with no env rows at all.
    #[test]
    fn e_on_an_adopted_dogs_row_carries_its_path_into_the_pane() {
        let app = fixtures::app_in_adopted_channel_dog_pane_from_row();
        let pane = app.config_pane().expect("the pane is open");
        assert_eq!(
            pane.target(),
            &PaneTarget::Dog {
                name: "kelpie".to_string(),
                adopted_path: Some(std::path::PathBuf::from("/usr/local/bin/shep-kelpie")),
            },
            "the pane is a dog pane over the binary the row names"
        );
        assert!(pane.shows_env(), "{:?}", pane.target());
    }

    #[test]
    fn a_dog_with_no_schema_gets_no_pane_and_is_told_where_to_edit() {
        let mut app = flock_on_dog("otel");
        app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            app.update(Msg::DogPane {
                name: "otel".to_string(),
                adopted_path: None,
                env_keys: Vec::new(),
                result: Err("otel publishes no schema; edit dogs.toml with $EDITOR".to_string()),
            }),
            Effect::None
        );
        assert!(app.config_pane().is_none());
        assert!(
            app.settings().is_none(),
            "the flock table the operator pressed `e` on is still there"
        );
        let notice = app.notice().expect("a refusal is reported").to_string();
        assert!(notice.contains("dogs.toml"), "{notice}");
        assert!(notice.contains("$EDITOR"), "{notice}");
    }

    /// The schema is the first of two halves. The pane cannot be drawn
    /// until the shepherd answers with the section, the half this binary
    /// has no copy of.
    #[test]
    fn a_schema_asks_for_the_section_and_the_section_opens_the_pane() {
        let mut app = flock_on_dog("metrics");
        app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            app.update(Msg::DogPane {
                name: "metrics".to_string(),
                adopted_path: None,
                env_keys: Vec::new(),
                result: Ok(crate::dog::builtin_schema("metrics").expect("a built-in")),
            }),
            Effect::Send(Sent::DogSection {
                name: "metrics".to_string()
            })
        );
        assert!(app.config_pane().is_none(), "one half is not a pane");
        app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "metrics".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: "bind = \"0.0.0.0:9615\"\n".to_string().into(),
            }),
        });
        let pane = app.config_pane().expect("both halves are a pane");
        assert_eq!(pane.target().name(), "metrics");
        assert_eq!(pane.value("bind"), "0.0.0.0:9615");
        assert!(
            app.settings().is_none(),
            "a dog pane from the flock never goes through the settings screen"
        );
    }

    /// Same property as `a_config_reply_that_lands_with_the_overlay_up_closes_it`,
    /// on `on_dog_section`'s own body-replacing reply rather than
    /// `open_or_refresh_config_pane`'s.
    #[test]
    fn a_dog_sections_reply_that_lands_with_the_overlay_up_closes_it() {
        let mut app = flock_on_dog("metrics");
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::DogPane {
            name: "metrics".to_string(),
            adopted_path: None,
            env_keys: Vec::new(),
            result: Ok(crate::dog::builtin_schema("metrics").expect("a built-in")),
        });
        assert!(app.config_pane().is_none(), "one half is not a pane yet");
        let _ = app.update(Msg::Key(KeyPress::Help));
        assert!(app.keymap_open(), "the overlay did not open");

        app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "metrics".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: "bind = \"0.0.0.0:9615\"\n".to_string().into(),
            }),
        });
        assert!(
            app.config_pane().is_some(),
            "the reply must still open the pane"
        );
        assert!(
            !app.keymap_open(),
            "the overlay survived a body change under it"
        );
    }

    /// The same property `config_target` buys for a sheep.
    #[test]
    fn a_section_for_a_dog_nobody_is_waiting_for_is_dropped() {
        let mut app = fixtures::app_in_dog_pane();
        app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none());
        app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "bark".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: fixtures::dog_section().into(),
            }),
        });
        assert!(app.config_pane().is_none(), "a late reply re-opens nothing");
    }

    /// Its twin above proves the pane drops once nobody is waiting; this
    /// one proves the name is checked while somebody still is, the half a
    /// cleared target cannot cover.
    #[test]
    fn a_section_for_a_different_dog_does_not_replace_the_pane() {
        let mut app = fixtures::app_in_dog_pane();
        app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "metrics".to_string(),
            },
            result: Ok(Response::DogSection {
                toml: "bind = \"0.0.0.0:9615\"\n".to_string().into(),
            }),
        });
        let pane = app.config_pane().expect("the bark pane is still open");
        assert_eq!(pane.target().name(), "bark");
        assert_eq!(
            pane.value("poll"),
            "60s",
            "and still holds bark's own section"
        );
    }

    /// A dog has no override store and no Flockfile, so `ApplyConfig` has
    /// nothing to mean here: the write goes out through `SetDogConfig`
    /// instead.
    #[test]
    fn a_dog_pane_write_carries_the_whole_edited_section() {
        let mut app = fixtures::app_in_dog_pane();
        let index = app
            .config_pane()
            .expect("the pane is open")
            .fields()
            .fields()
            .iter()
            .position(|field| field.key == "poll")
            .expect("poll is a bark field");
        app.update(Msg::Key(KeyPress::SelectFirst));
        for _ in 0..index {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
        app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..3 {
            app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for typed in "30s".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        let batch = wire_batch(close_bark(&mut app));
        let [Sent::SetDogSection { name, toml, .. }] = batch.as_slice() else {
            panic!("closing the pane sends the section: {batch:?}");
        };
        assert_eq!(name, "bark");
        assert!(
            toml.as_str().contains("poll = \"30s\""),
            "{}",
            toml.as_str()
        );
        assert!(
            toml.as_str().contains("# how often"),
            "a comment shep did not write survives: {}",
            toml.as_str()
        );
    }

    /// `Request::SetDogConfig` replaces the whole table, so a dog's batch
    /// of edits closes as one write rather than one per entry, the way a
    /// sheep's own batch closes as `writes.len()` requests.
    #[test]
    fn closing_a_dog_pane_sends_one_write_for_two_edits() {
        let mut app = fixtures::app_in_dog_pane_with_two_edits();
        let Effect::SendAll(sent) = close_bark(&mut app) else {
            panic!("wanted a batch");
        };
        assert_eq!(sent.len(), 1, "a dog takes one section write, not two");
        let [Sent::SetDogSection { name, toml, .. }] = sent.as_slice() else {
            panic!("closing the pane sends the section: {sent:?}");
        };
        assert_eq!(name, "bark");
        assert!(
            toml.as_str().contains("poll = \"45s\""),
            "{}",
            toml.as_str()
        );
        assert!(
            toml.as_str().contains("history_bytes = 8192"),
            "{}",
            toml.as_str()
        );
    }

    #[test]
    fn a_landed_dog_write_re_reads_the_section_and_promises_nothing_more() {
        let mut app = fixtures::app_in_dog_pane();
        assert_eq!(
            app.update(Msg::Replied {
                sent: Sent::DogSection {
                    name: "bark".to_string(),
                },
                result: Ok(Response::DogConfigSet {
                    name: "bark".to_string()
                }),
            }),
            Effect::None,
            "a reply routed by its own request, and this one is not the write"
        );
        let Effect::Send(Sent::DogSection { name }) = app.update(Msg::Replied {
            sent: Sent::SetDogSection {
                name: "bark".to_string(),
                ticket: 0,
                toml: fixtures::dog_section().into(),
                authority: WriteAuthority::granted(&app).expect("the fixture opens the gate"),
            },
            result: Ok(Response::DogConfigSet {
                name: "bark".to_string(),
            }),
        }) else {
            panic!("a landed write re-reads");
        };
        assert_eq!(name, "bark");
        let notice = app
            .notice()
            .expect("a landed write is reported")
            .to_string();
        assert!(notice.contains("bark is told"), "{notice}");
    }

    /// The schema was read at open and is parked on the app: a keystroke
    /// that re-reads a file must not respawn somebody else's process.
    #[test]
    fn r_in_a_dog_pane_re_reads_the_section_and_never_re_probes() {
        let mut app = fixtures::app_in_dog_pane();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::Send(Sent::DogSection {
                name: "bark".to_string()
            })
        );
    }

    /// Walks the nested fixture's pane onto its `row`th row.
    fn nested_cursor_to(app: &mut App, row: usize) {
        app.update(Msg::Key(KeyPress::SelectFirst));
        for _ in 0..row {
            app.update(Msg::Key(KeyPress::SelectDown));
        }
    }

    fn nested_rows(app: &App) -> usize {
        app.config_pane().expect("the pane is open").rows().len()
    }

    /// `models` is row 0 and open, `worker` row 1 and closed, `name` row 2.
    #[test]
    fn right_and_left_open_and_close_the_table_under_the_cursor() {
        let mut app = fixtures::app_in_dog_pane_with_a_nested_table();
        assert_eq!(nested_rows(&app), 3);
        nested_cursor_to(&mut app, 1);
        app.update(Msg::Key(KeyPress::TabNext));
        assert_eq!(nested_rows(&app), 5, "worker opened onto model and token");
        app.update(Msg::Key(KeyPress::TabPrev));
        assert_eq!(nested_rows(&app), 3);
        app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(nested_rows(&app), 5, "enter toggles it open");
        app.update(Msg::Key(KeyPress::Confirm));
        assert_eq!(nested_rows(&app), 3, "and closed");
    }

    /// `e`, space and `d` on a table row file nothing and open nothing.
    #[test]
    fn e_space_and_d_on_a_table_row_do_nothing() {
        for key in [KeyPress::Edit, KeyPress::Cycle, KeyPress::Remove] {
            let mut app = fixtures::app_in_dog_pane_with_a_nested_table();
            nested_cursor_to(&mut app, 1);
            let before = app.config_pane().expect("the pane is open").cursor();
            assert!(matches!(before, Some(PaneRow::Table(_))), "{before:?}");
            let effect = app.update(Msg::Key(key));
            assert_eq!(effect, Effect::None, "{key:?}");
            let pane = app.config_pane().expect("the pane is open");
            assert!(pane.edits().is_empty(), "{key:?} filed something");
            assert!(pane.typing().is_none(), "{key:?} opened an editor");
            assert_eq!(pane.cursor(), before, "{key:?}");
            assert_eq!(pane.rows().len(), 3, "{key:?} toggled the table");
        }
    }

    /// `u` drops the newest edit in the pane, wherever the cursor stands, so
    /// it still works after `←` has climbed onto a table's row.
    #[test]
    fn u_on_a_table_row_undoes_the_newest_edit() {
        let mut app = fixtures::app_in_dog_pane_with_a_nested_table();
        nested_cursor_to(&mut app, 2);
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::Key(KeyPress::TextChar('x')));
        app.update(Msg::Key(KeyPress::TextApply));
        assert!(!app.config_pane().expect("open").edits().is_empty());
        app.update(Msg::Key(KeyPress::SelectUp));
        let on = app.config_pane().expect("open").cursor();
        assert!(matches!(on, Some(PaneRow::Table(_))), "{on:?}");
        app.update(Msg::Key(KeyPress::Undo));
        assert!(app.config_pane().expect("open").edits().is_empty());
    }

    /// `r` re-reads the section and the pane that comes back has the same
    /// tables open, with the cursor where it was.
    #[test]
    fn a_re_read_keeps_the_open_tables_and_the_cursor() {
        let mut app = fixtures::app_in_dog_pane_with_a_nested_table();
        nested_cursor_to(&mut app, 1);
        app.update(Msg::Key(KeyPress::TabNext));
        app.update(Msg::Key(KeyPress::SelectDown));
        let cursor = app.config_pane().expect("the pane is open").cursor();
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
            panic!("r re-reads the section");
        };
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::DogSection {
                toml: fixtures::nested_dog_section().into(),
            }),
        });
        let pane = app.config_pane().expect("still open");
        assert_eq!(pane.rows().len(), 5);
        assert!(pane.open_tables().contains("models.worker"));
        assert_eq!(pane.cursor(), cursor);
    }

    /// `←` and `→` mean nothing on a sheep's own pane.
    #[test]
    fn left_and_right_do_nothing_on_a_sheep_pane() {
        let mut app = fixtures::app_in_sheep_pane();
        let before = app.config_pane().expect("the pane is open").cursor();
        let effect = app.update(Msg::Key(KeyPress::TabNext));
        assert_eq!(effect, Effect::None);
        let effect = app.update(Msg::Key(KeyPress::TabPrev));
        assert_eq!(effect, Effect::None);
        assert_eq!(app.config_pane().expect("open").cursor(), before);
    }

    /// `r` re-reads the section and the sub-screen comes back over the
    /// refreshed pane at the cursor it had, showing what the operator has
    /// filed rather than what the section holds.
    #[test]
    fn a_re_read_keeps_an_open_list_at_its_cursor_with_the_filed_elements() {
        let mut app = fixtures::app_in_dog_pane_with_lists();
        pane_to(&mut app, "hosts");
        app.update(Msg::Key(KeyPress::Confirm));
        app.update(Msg::Key(KeyPress::SelectLast));
        app.update(Msg::Key(KeyPress::Edit));
        for typed in "c.example".chars() {
            app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        app.update(Msg::Key(KeyPress::TextApply));
        app.update(Msg::Key(KeyPress::SelectUp));
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
            panic!("r re-reads the section");
        };
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::DogSection {
                toml: fixtures::list_dog_section().into(),
            }),
        });
        let list = app
            .config_pane()
            .expect("still open")
            .list()
            .expect("the sub-screen rides across the refresh");
        assert_eq!(list.key(), "hosts");
        assert_eq!(
            list.elements(),
            ["a.example", "b.example", "c.example"],
            "the filed array, not the section's"
        );
        assert_eq!(list.cursor(), Some(crate::lookout::pane::ListRow::Item(1)));
    }

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
            declared: Some(declared()),
        });
        assert_eq!(effect, Effect::Send(pull_of("paddock")));
        app
    }

    fn pull_of(name: &str) -> Sent {
        Sent::DogPane {
            name: name.to_string(),
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
}
