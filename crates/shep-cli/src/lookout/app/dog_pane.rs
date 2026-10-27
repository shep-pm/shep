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
        result: Result<serde_json::Value, String>,
    ) -> Effect {
        match result {
            Ok(schema) => {
                self.dog_target = Some(DogProbe {
                    name: name.clone(),
                    adopted_path,
                    schema,
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
    /// guarded on `config_target`, and for the same reason: a reply for a
    /// dog the operator has already left must not re-open a pane over the
    /// screen they went back to.
    ///
    /// The schema comes off `dog_target` rather than off the wire. Nothing
    /// records a dog's schema, so the probe that ran at open is the only
    /// copy, and a refresh reuses it rather than respawning the binary.
    pub(super) fn on_dog_section(
        &mut self,
        name: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        let Some(probe) = self.dog_target.clone().filter(|probe| probe.name == name) else {
            return Effect::None;
        };
        self.config_read_in_flight = false;
        if self.closing {
            return self.on_close_reread(name, result);
        }
        match result {
            Ok(Response::DogSection { toml }) => {
                let dropped = self.rebuild_dog_pane(probe, toml.as_str());
                if dropped > 0 {
                    self.notice =
                        Some(Notice::grave(format!("{name}: {}", moved_entries(dropped))));
                }
            }
            other => {
                self.notice = Some(Notice::grave(format!("{name}: {}", failed_reply(other))));
            }
        }
        Effect::None
    }

    /// Replaces the open pane with one over `toml`, and returns how many of
    /// its edits the new section took no further. See
    /// [`ConfigPane::adopt_edits_from`].
    ///
    /// Everything a refresh has to carry across is read before the rebuild
    /// replaces the pane: see `Self::on_sheep_config`, which states the
    /// argument for each. A dog pane has no env sub-screen, so its view, its
    /// open tables, its list sub-screen and its edits carry across.
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
        );
        let dropped = self
            .config_pane()
            .map_or(0, |previous| pane.adopt_edits_from(previous));
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

    /// One `Request::SetDogConfig` reply.
    ///
    /// Reaches for no pane: a write goes out when the pane closes, so
    /// this lands with the dashboard on screen. A success still re-reads
    /// the section, the same shape [`Self::on_field_applied`] has, and
    /// [`Self::on_dog_section`] drops the answer when nobody is waiting
    /// for it.
    ///
    /// The sentence says the change was published and stops there.
    /// Whether the dog acted on it is the dog's own answer, which this
    /// reply does not carry and shep cannot predict.
    pub(super) fn on_dog_section_set(
        &mut self,
        name: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        match result {
            Ok(Response::DogConfigSet { .. }) => {
                self.notice = Some(Notice::info(format!(
                    "{name}: its config is written, and {name} is told"
                )));
                Effect::Send(Sent::DogSection {
                    name: name.to_owned(),
                })
            }
            other => {
                self.notice = Some(Notice::grave(format!("{name}: {}", failed_reply(other))));
                Effect::None
            }
        }
    }
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
    use shep_core::protocol::RpcError;
    use shep_core::protocol::RpcErrorCode;
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

    #[test]
    fn a_dog_with_no_schema_gets_no_pane_and_is_told_where_to_edit() {
        let mut app = fixtures::app_in_settings_on_dog("otel");
        app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            app.update(Msg::DogPane {
                name: "otel".to_string(),
                adopted_path: None,
                result: Err("otel publishes no schema; edit dogs.toml with $EDITOR".to_string()),
            }),
            Effect::None
        );
        assert!(app.config_pane().is_none());
        assert!(
            app.settings().is_some(),
            "the screen the operator pressed `e` on is still there"
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
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        app.update(Msg::Key(KeyPress::Edit));
        assert_eq!(
            app.update(Msg::DogPane {
                name: "metrics".to_string(),
                adopted_path: None,
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
            "the settings screen closes once there is something to look at"
        );
    }

    /// Same property as `a_config_reply_that_lands_with_the_overlay_up_closes_it`,
    /// on `on_dog_section`'s own body-replacing reply rather than
    /// `open_or_refresh_config_pane`'s.
    #[test]
    fn a_dog_sections_reply_that_lands_with_the_overlay_up_closes_it() {
        let mut app = fixtures::app_in_settings_on_dog("metrics");
        app.update(Msg::Key(KeyPress::Edit));
        app.update(Msg::DogPane {
            name: "metrics".to_string(),
            adopted_path: None,
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
}
