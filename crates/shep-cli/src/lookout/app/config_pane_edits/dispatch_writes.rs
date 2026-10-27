use super::super::*;

use crate::dog_settings::touched_refusal;
use crate::lookout::pane::toml_table;

impl App {
    /// Everything the open pane has filed, as the requests that carry it,
    /// leaving the pane holding nothing.
    ///
    /// Empty for no pane, for an empty set, and for a dog whose every filed
    /// edit is one its section already carries, which needs no request and no
    /// notice. Reported, not sent, for a dog whose section stopped parsing
    /// between the read and the keystroke, or whose text holds something other
    /// than the shape the row edits.
    ///
    /// A sheep's set is one request per entry and a dog's is one request
    /// for the lot: [`Sent::SetDogSection`] and [`Sent::SetSheepDogTable`]
    /// go out as `Request::SetDogConfig` and `Request::SetSheepDogSettings`,
    /// each of which replaces a whole table, so a batch of edits to one is
    /// one write. See `ConfigPane::edited_section_with` and
    /// `ConfigPane::edited_table_with`.
    ///
    /// A sheep pane's open dog tables are written with it, each as its own
    /// whole-table request.
    pub(in crate::lookout::app) fn take_pane_writes(&mut self) -> Vec<Sent> {
        let mut writes = self.take_seated_writes();
        let edited = self
            .config_pane()
            .map(|pane| pane.dogs_tab().edited())
            .unwrap_or_default();
        for dog in edited {
            writes.extend(
                self.in_dog_table(dog, Self::take_seated_writes)
                    .unwrap_or_default(),
            );
        }
        writes
    }

    /// [`Self::take_pane_writes`] for the pane that is the body, without the
    /// tables a sheep pane holds.
    fn take_seated_writes(&mut self) -> Vec<Sent> {
        // `WriteAuthority::granted`, not `Self::authorize_write`: the gate
        // is checked on the keystroke that files an edit, so a read-only
        // pane reaches here with an empty set and must not be told off for
        // leaving.
        let Some(authority) = WriteAuthority::granted(self) else {
            return Vec::new();
        };
        let Some(pane) = self.body.config_pane_mut() else {
            return Vec::new();
        };
        let edits = pane.close();
        if edits.is_empty() {
            return Vec::new();
        }
        let mut ticket = self.next_write_ticket;
        let sent = match pane.target().clone() {
            PaneTarget::Dog { name, .. } => {
                let schema = pane.schema().cloned();
                match pane.edited_section_with(&edits) {
                    SectionWrite::Written(toml) => {
                        // The gate is the one `shep set` uses, asked about
                        // the keys this close changed. A value the pane filed
                        // already passed the same rules on the keystroke that
                        // filed it. A value the operator wrote into
                        // `dogs.toml` by hand is the shepherd's own: the pane
                        // never read it and does not speak for it, so it rides
                        // out as it came.
                        if let Some(refused) = schema.as_ref().and_then(|schema| {
                            touched_refusal(
                                schema,
                                &toml_table(toml.as_str()),
                                &pane.touched_paths(&edits),
                            )
                        }) {
                            self.notice = Some(Notice::grave(format!("{name}: {}", refused.text)));
                            return Vec::new();
                        }
                        let section = vec![Sent::SetDogSection {
                            name,
                            ticket,
                            toml,
                            authority,
                        }];
                        ticket += 1;
                        section
                    }
                    // The section already says what every filed edit asked for, so
                    // the close is finished and has nothing to send. Not a
                    // refusal, and nothing to report: the edit reached this arm
                    // through a re-read that landed it under the operator's hand.
                    SectionWrite::Nothing => Vec::new(),
                    SectionWrite::Refused => {
                        self.notice = Some(Notice::grave(format!(
                            "{name}: its section in dogs.toml does not parse, or is not shaped for the edit"
                        )));
                        return Vec::new();
                    }
                }
            }
            PaneTarget::Sheep { name } => {
                let writes = edits.into_writes();
                let mut requests = Vec::with_capacity(writes.len());
                for edit in writes {
                    let this = ticket;
                    ticket += 1;
                    requests.push(match edit {
                        PaneEdit::SetEnv { key, value } => Sent::SetEnv {
                            name: name.clone(),
                            ticket: this,
                            key,
                            value,
                            authority,
                        },
                        // No client-side validation, deliberately: the
                        // daemon already re-normalizes untrusted input, so
                        // a weaker copy here would drift the moment
                        // `AppConfig` grows a field. An empty buffer on a
                        // non-nullable field files `null`, refused as
                        // `InvalidConfig`.
                        PaneEdit::Set { key, value } => Sent::ApplyField {
                            name: name.clone(),
                            ticket: this,
                            key,
                            value,
                            authority,
                        },
                    });
                }
                requests
            }
            // One write for the whole table, since the request replaces
            // it: every key no edit touched rides along unchanged.
            PaneTarget::SheepDog { sheep, dog, .. } => {
                let schema = pane.schema().cloned();
                let table = pane.edited_table_with(&edits);
                let touched = pane.touched_paths(&edits);
                if let Some(refused) = schema
                    .as_ref()
                    .and_then(|schema| touched_refusal(schema, &table, &touched))
                {
                    self.notice = Some(Notice::grave(format!("{sheep}.{dog}: {}", refused.text)));
                    return Vec::new();
                }
                let write = vec![Sent::SetSheepDogTable {
                    name: sheep,
                    dog,
                    ticket,
                    table: Some(table.into()),
                    authority,
                }];
                ticket += 1;
                write
            }
        };
        // One ticket per request that goes out, and never reused: the
        // counter is what keeps two `Sent` values for the same field
        // distinguishable.
        self.next_write_ticket = ticket;
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;

    /// One field from each of the groups the pane draws, since the
    /// daemon's routing classification is per field. `rpc.rs`'s
    /// `a_field_edit_is_reported_as_an_operator_override` asserts the
    /// other half, where the marker is actually built.
    #[test]
    fn one_edit_reaches_the_wire_as_a_single_field_override() {
        for key in [
            "autorestart", // control
            "watch",       // process
            "merge_logs",  // inputs
        ] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, key);
            let _ = app.update(Msg::Key(KeyPress::Cycle));
            assert_eq!(
                app.config_pane().unwrap().edits().len(),
                1,
                "{key}: space files an edit and sends nothing"
            );
            let request = one_wire(close_writing(&mut app));
            let Request::SetSheepField {
                name,
                key: sent,
                value,
            } = request
            else {
                panic!("{key}: expected SetSheepField, got {request:?}");
            };
            assert_eq!(name, "web", "{key}");
            assert_eq!(sent, key, "{key}");
            assert!(value.is_boolean(), "{key}: {value}");
        }
    }

    #[test]
    fn escape_sends_every_filed_edit_at_once() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let effect = close_writing(&mut app);
        let Effect::SendAll(sent) = effect else {
            panic!("wanted a batch, got {effect:?}");
        };
        assert_eq!(sent.len(), 2);
        assert!(
            app.config_pane().is_none(),
            "the pane closes on the same key"
        );
    }

    #[test]
    fn escape_with_nothing_filed_closes_and_sends_nothing() {
        let mut app = fixtures::app_in_sheep_pane();
        let effect = app.update(Msg::Key(KeyPress::Escape));
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(app.config_pane().is_none());
    }

    /// A refusal now lands after the pane has gone, so its arm must not
    /// assume a pane is open.
    #[test]
    fn a_refused_write_notices_after_the_pane_has_closed() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let Effect::SendAll(mut sent) = close_writing(&mut app) else {
            panic!("wanted a batch");
        };
        let first = sent.remove(0);
        assert!(app.config_pane().is_none());
        app.update(Msg::Replied {
            sent: first,
            result: Err(fixtures::a_refusal()),
        });
        let notice = app
            .notice()
            .map(ToString::to_string)
            .expect("a refusal says so");
        assert!(
            notice.contains("cwd"),
            "the notice names the field: {notice}"
        );
    }

    /// The set is the operator's and the values are the shepherd's.
    #[test]
    fn a_config_re_read_replaces_the_values_and_keeps_the_edits() {
        let mut app = fixtures::app_in_sheep_pane_with_two_edits();
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Refresh)) else {
            panic!("refresh asks the shepherd for the config again");
        };
        app.update(Msg::Replied {
            sent,
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert_eq!(app.config_pane().unwrap().edits().len(), 2);
    }

    /// Two edits, one close, two requests, each naming its own key: a
    /// batch is not one write carrying a map, so neither entry can carry
    /// the other's value.
    #[test]
    fn a_batch_names_each_key_in_its_own_request() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        pane_to(&mut app, "watch");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let requests = wire_all(close_writing(&mut app));
        let named: Vec<String> = requests
            .iter()
            .map(|request| match request {
                Request::SetSheepField { key, .. } => key.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(named, vec!["autorestart".to_owned(), "watch".to_owned()]);
    }

    /// `pending` is the shepherd's answer, reported verbatim rather than
    /// re-derived from `apply_group`: the two disagree on `autostart`, and
    /// on a field whose config subset would not normalize.
    #[test]
    fn a_landed_write_re_reads_the_config_and_reports_what_the_shepherd_said() {
        for (pending, wanted) in [(false, "set to false"), (true, "shep reload")] {
            let mut app = fixtures::app_in_sheep_pane_with_control();
            pane_to(&mut app, "autorestart");
            let _ = app.update(Msg::Key(KeyPress::Cycle));
            let mut batch = wire_batch(close_writing(&mut app));
            let effect = app.update(Msg::Replied {
                sent: batch.remove(0),
                result: Ok(Response::SheepFieldSet {
                    name: "web".to_owned(),
                    key: "autorestart".to_owned(),
                    pending,
                    warning: None,
                }),
            });
            assert_eq!(
                effect,
                Effect::Send(Sent::SheepConfig {
                    name: "web".to_owned()
                }),
                "a landed write re-reads what the shepherd now holds"
            );
            let notice = app.notice().expect("the outcome is reported");
            assert!(!notice.is_grave(), "{notice:?}");
            assert!(notice.to_string().contains(wanted), "{notice:?}");
        }
    }

    /// A `cwd`/`script`/`out_file`/`err_file` warning rides the same
    /// notice as the write it came back on, not a second one: the write
    /// still landed, and `grave` stays `false` since this is advisory.
    #[test]
    fn a_path_warning_rides_the_same_notice_as_the_write() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        fixtures::type_into_the_open_editor(&mut app, "/does/not/exist");
        // `cwd` needs a respawn, so `esc` raises the close dialog rather
        // than writing on the keypress. `c` is the answer that writes and
        // leaves the sheep alone, which is the same batch this test always
        // read, reached through the question the dialog now asks first.
        app.update(Msg::Key(KeyPress::Escape));
        let mut batch = wire_batch(app.update(Msg::Key(KeyPress::Continue)));
        let effect = app.update(Msg::Replied {
            sent: batch.remove(0),
            result: Ok(Response::SheepFieldSet {
                name: "web".to_owned(),
                key: "cwd".to_owned(),
                pending: true,
                warning: Some("/does/not/exist does not exist yet".to_owned()),
            }),
        });
        // The re-read still goes out. This is the only test that answers
        // with a warning at all, so discarding the effect here would let a
        // refactor gate the re-read on there not being one.
        assert!(
            matches!(effect, Effect::Send(Sent::SheepConfig { .. })),
            "{effect:?}"
        );
        let notice = app.notice().expect("the outcome is reported");
        assert!(!notice.is_grave(), "{notice:?}");
        let text = notice.to_string();
        assert!(text.contains("shep reload"), "{text}");
        assert!(text.contains("does not exist yet"), "{text}");
    }

    /// Every refusal this door can meet is an `Err`, which is why
    /// `Response::SheepFieldSet` carries no `refused` field: two ways to
    /// say no is one a client forgets to check.
    #[test]
    fn a_refused_write_is_reported_and_does_not_re_read() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let mut batch = wire_batch(close_writing(&mut app));
        let effect = app.update(Msg::Replied {
            sent: batch.remove(0),
            result: Err(fixtures::a_refusal()),
        });
        assert_eq!(effect, Effect::None, "a refusal does not re-read");
        let notice = app.notice().expect("the refusal is reported");
        assert!(notice.is_grave());
        assert!(
            notice.to_string().contains("the store is locked"),
            "{notice:?}"
        );
    }

    /// Asserted on the whole `Effect`, since that is what a diagnostic
    /// would print. Nothing between here and the wire unwraps the value.
    #[test]
    fn a_write_effects_debug_names_no_value() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "cwd");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..40 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for typed in "/home/ada/secret-project".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        let field = close_writing(&mut app);
        assert_eq!(
            format!("{field:?}"),
            "SendAll([ApplyField { name: \"web\", ticket: 0, key: \"cwd\", \
                 value: FieldValue(<string>), authority: WriteAuthority(()) }])"
        );

        let mut app = fixtures::app_in_sheep_pane_with_control();
        // `SelectLast` lands on `+ add a key`; two steps up is `DB_HOST`,
        // the fixture's first env key.
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        let _ = app.update(Msg::Key(KeyPress::SelectUp));
        assert_eq!(app.config_pane().unwrap().cursor(), Some(PaneRow::Env(0)));
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for typed in "hunter2".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        // One `Escape`, not two: there is no sub-screen level to back out
        // of any more, so the write goes out once the close dialog it
        // raises (this fixture parks `kill_signal`) is answered.
        let env = close_writing(&mut app);
        assert_eq!(
            format!("{env:?}"),
            "SendAll([SetEnv { name: \"web\", ticket: 0, key: \"DB_HOST\", \
                 value: Some(EnvValue(<7 bytes>)), authority: WriteAuthority(()) }])"
        );
    }

    /// Two writes to the same field in one batch is unreachable, since
    /// the set holds one entry per key, but two closes of two panes are
    /// not: the counter is monotonic and never reused, so no two `Sent`
    /// values are ever equal.
    #[test]
    fn no_two_writes_share_a_ticket() {
        let mut app = fixtures::app_in_sheep_pane_with_control();
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        pane_to(&mut app, "watch");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let first = wire_batch(close_writing(&mut app));
        assert_ne!(first[0], first[1], "two entries are two tickets");

        // The same lookout, a second pane. `app_in_sheep_pane_with_control`
        // parks a field, so the close above stopped on the apply menu and
        // this `Escape` is what leaves it.
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_owned(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        pane_to(&mut app, "autorestart");
        let _ = app.update(Msg::Key(KeyPress::Cycle));
        let second = wire_batch(close_writing(&mut app));
        assert_ne!(
            first[0], second[0],
            "a second close does not reuse the first's tickets"
        );
    }

    /// A dog pane's rows are dotted for a nested table, and the write walks
    /// that path into the section: the text that comes back differs from the
    /// text that went in in the one line the operator typed.
    #[test]
    fn a_dotted_dog_row_changes_one_line_of_its_section() {
        let mut app = fixtures::app_in_dog_pane_with_a_nested_table();
        pane_to(&mut app, "models.worker.model");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        for _ in 0..64 {
            let _ = app.update(Msg::Key(KeyPress::TextBackspace));
        }
        for typed in "large".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        // `esc` reads the section again before writing it, the same door a
        // sheep's batch closes through.
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Escape)) else {
            panic!("a dog close reads its section first");
        };
        let effect = app.update(Msg::Replied {
            sent,
            result: Ok(Response::DogSection {
                toml: fixtures::nested_dog_section().into(),
            }),
        });
        let mut batch = wire_batch(effect);
        let Sent::SetDogSection { name, toml, .. } = batch.remove(0) else {
            panic!("closing the pane sends the section: {batch:?}");
        };
        assert_eq!(name, "jobs");
        let before = fixtures::nested_dog_section();
        let sent = toml.as_str();
        let changed: Vec<(usize, &str, &str)> = sent
            .lines()
            .zip(before.lines())
            .enumerate()
            .filter(|(_, pair)| pair.0 != pair.1)
            .map(|(line, pair)| (line, pair.0, pair.1))
            .collect();
        assert_eq!(
            changed,
            vec![(6, "model = \"large\"", "model = \"gpt\"")],
            "{sent}",
        );
    }

    /// The trigger: an unset filed on a key that someone else removes from
    /// `dogs.toml` while the pane is open. The close re-reads, sees the section
    /// moved, and holds the pane under the same edit. The second close finds
    /// the key already gone, which is the edit already done, not a broken file.
    #[test]
    fn a_dog_unset_the_section_already_carries_closes_without_a_notice() {
        let mut app = fixtures::app_in_dog_pane_with_a_nested_table();
        pane_to(&mut app, "models.worker.model");
        let _ = app.update(Msg::Key(KeyPress::Remove));
        assert_eq!(
            app.config_pane().expect("the pane is open").edits().len(),
            1,
            "d on a dog row files the removal"
        );
        let removed = fixtures::nested_dog_section().replace("model = \"gpt\"\n", "");

        // First close: the section moved under the edit, so nothing is written
        // and the pane stays up with it.
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Escape)) else {
            panic!("a dog close reads its section first");
        };
        let effect = app.update(Msg::Replied {
            sent,
            result: Ok(Response::DogSection {
                toml: removed.clone().into(),
            }),
        });
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(
            app.config_pane().is_some(),
            "a moved section holds the pane"
        );

        // Second close: the section is the one the pane holds now, and the one
        // edit it carries is the removal the file already made.
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Escape)) else {
            panic!("the second close reads again");
        };
        let effect = app.update(Msg::Replied {
            sent,
            result: Ok(Response::DogSection {
                toml: removed.into(),
            }),
        });
        assert!(matches!(effect, Effect::None), "{effect:?}");
        assert!(
            app.config_pane().is_none(),
            "the close finishes: nothing to send is not a refusal"
        );
        assert_eq!(
            app.notice().map(ToString::to_string),
            None,
            "the file parses and already says what the operator asked for"
        );
    }

    /// `Enter` opens the sub-screen over a dog's list, an element added
    /// there is filed whole, and the close writes the section back with that
    /// one line changed: the comment above the list stays where it was.
    #[test]
    fn an_element_added_to_a_dogs_list_changes_one_line_of_its_section() {
        let mut app = fixtures::app_in_dog_pane_with_lists();
        pane_to(&mut app, "hosts");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        for typed in "c.example".chars() {
            let _ = app.update(Msg::Key(KeyPress::TextChar(typed)));
        }
        let _ = app.update(Msg::Key(KeyPress::TextApply));
        assert_eq!(
            filed_value(&app, "hosts"),
            serde_json::json!(["a.example", "b.example", "c.example"])
        );
        let _ = app.update(Msg::Key(KeyPress::Escape));
        let Effect::Send(sent) = app.update(Msg::Key(KeyPress::Escape)) else {
            panic!("a dog close reads its section first");
        };
        let effect = app.update(Msg::Replied {
            sent,
            result: Ok(Response::DogSection {
                toml: fixtures::list_dog_section().into(),
            }),
        });
        let mut batch = wire_batch(effect);
        let Sent::SetDogSection { name, toml, .. } = batch.remove(0) else {
            panic!("closing the pane sends the section: {batch:?}");
        };
        assert_eq!(name, "watch");
        let before = fixtures::list_dog_section();
        let sent = toml.as_str();
        let changed: Vec<(usize, &str, &str)> = sent
            .lines()
            .zip(before.lines())
            .enumerate()
            .filter(|(_, pair)| pair.0 != pair.1)
            .map(|(line, pair)| (line, pair.0, pair.1))
            .collect();
        assert_eq!(
            changed,
            vec![(
                1,
                "hosts = [\"a.example\", \"b.example\", \"c.example\"]",
                "hosts = [\"a.example\", \"b.example\"]"
            )],
            "{sent}",
        );
        assert_eq!(sent.lines().count(), before.lines().count(), "{sent}");
    }

    /// The item schema's rules are the sub-screen's, not the sheep pane's:
    /// `hosts` items need three characters, and a shorter one files nothing.
    #[test]
    fn an_element_a_dogs_item_schema_refuses_files_nothing() {
        let mut app = fixtures::app_in_dog_pane_with_lists();
        pane_to(&mut app, "hosts");
        let _ = app.update(Msg::Key(KeyPress::Confirm));
        let _ = app.update(Msg::Key(KeyPress::SelectLast));
        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Key(KeyPress::TextChar('x')));
        assert_eq!(app.update(Msg::Key(KeyPress::TextApply)), Effect::None);
        assert_eq!(app.mode(), InputMode::Text, "the editor stays open");
        let notice = app.notice().expect("a refusal answers");
        assert!(notice.is_grave(), "{notice}");
        assert!(notice.to_string().starts_with("hosts is \"x\""), "{notice}");
        assert!(
            app.config_pane()
                .expect("the pane is open")
                .edits()
                .is_empty(),
            "nothing was filed"
        );
    }

    /// `Enter` on a secret list or a map raises the no-editor refusal and
    /// opens nothing: the sub-screen would draw a secret's elements bare.
    #[test]
    fn enter_on_a_dogs_secret_list_or_map_opens_no_sub_screen() {
        for key in ["keys", "labels"] {
            let mut app = fixtures::app_in_dog_pane_with_lists();
            pane_to(&mut app, key);
            let _ = app.update(Msg::Key(KeyPress::Confirm));
            let pane = app.config_pane().expect("the pane is open");
            assert!(pane.list().is_none(), "{key}");
            let notice = app.notice().expect("a locked row answers").to_string();
            assert!(notice.contains("no editor in this pane"), "{key}: {notice}");
        }
    }
}
