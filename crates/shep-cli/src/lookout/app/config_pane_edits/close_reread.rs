//! Closing a pane whose write replaces a whole table.
//!
//! `Request::SetSheepDogSettings` and `Request::SetDogConfig` each replace
//! the table they name. A write built on the table read at open undoes
//! every change made since. So `Escape` reads the table again first. An
//! unchanged table is written and the pane closes. A moved one stays on
//! screen under the operator's edits. A sheep pane holds one such table per
//! open dog, and the one read covers them all.

use super::super::dog_pane::moved_entries;
use super::super::*;

impl App {
    /// `Escape` on a dog pane, or a sheep pane whose dogs hold tables with
    /// edits filed: reads the table again and holds the pane for the answer.
    ///
    /// [`None`] for every other pane, and for one with nothing filed. Those
    /// close at once. A second `Escape` while the read is out asks nothing
    /// more, and neither does one arriving while a plain `r` is still out:
    /// [`Self::reread_pane`] refuses a second read of its own accord, so
    /// this only ever marks the one already in flight as the close's
    /// answer instead of racing it with a fresh request neither reply could
    /// be told apart from.
    pub(in crate::lookout::app) fn reread_before_closing(&mut self) -> Option<Effect> {
        let pane = self.config_pane()?;
        let holds_whole_table = match pane.target() {
            PaneTarget::Dog { .. } => !pane.edits().is_empty(),
            PaneTarget::Sheep { .. } => !pane.dogs_tab().edited().is_empty(),
            PaneTarget::SheepDog { .. } => false,
        };
        if !holds_whole_table {
            return None;
        }
        if self.closing {
            return Some(Effect::None);
        }
        self.closing = true;
        Some(self.reread_pane())
    }

    /// The answer to [`Self::reread_before_closing`]'s read.
    ///
    /// The tables the pane holds: the write goes out and the pane closes, or
    /// the close dialog asks its question first. A
    /// moved table: the pane takes it under the same edits and writes
    /// nothing. The next `Escape` writes. A failed read changes nothing.
    pub(in crate::lookout::app) fn on_close_reread(
        &mut self,
        name: &str,
        result: Result<Response, RequestError>,
    ) -> Effect {
        self.closing = false;
        let Some(pane) = self.config_pane() else {
            return Effect::None;
        };
        let why = match (result, pane.target().clone()) {
            (Ok(Response::SheepConfig(view)), PaneTarget::Sheep { name: sheep }) => {
                let moved = pane.moved_tables(&view);
                if moved.is_empty() {
                    return self.offer_or_write_and_close();
                }
                // The pane is rebuilt under the same edits, the open tables
                // taking the sheep's current ones.
                self.open_or_refresh_config_pane(*view);
                let tables = if moved.len() == 1 { "table" } else { "tables" };
                return self.hold(format!("{sheep}: its {} {tables}", moved.join(", ")), 0);
            }
            (Ok(Response::DogSection { toml }), PaneTarget::Dog { .. }) => {
                if pane.holds_section(toml.as_str()) {
                    return self.write_and_close();
                }
                let dropped = match self.dog_target.clone() {
                    Some(probe) => self.rebuild_dog_pane(probe, toml.as_str()),
                    None => 0,
                };
                return self.hold(format!("{name}: its section in dogs.toml"), dropped);
            }
            (other, _) => failed_reply(other),
        };
        self.notice = Some(Notice::grave(format!(
            "{name}: {why}, so nothing was written"
        )));
        Effect::None
    }

    /// Ends a close's wait on a read the link task never took. Returns what
    /// that read's notice adds.
    pub(in crate::lookout::app) fn end_close(&mut self) -> &'static str {
        if core::mem::take(&mut self.closing) {
            ", so nothing was written"
        } else {
            ""
        }
    }

    /// Asks the close dialog's question when the pane has one, else
    /// [`Self::write_and_close`]. The tables it would write are fresh.
    fn offer_or_write_and_close(&mut self) -> Effect {
        if let Some(dialog) = self.close_offer() {
            self.close_dialog = Some(dialog);
            return Effect::None;
        }
        self.write_and_close()
    }

    /// Sends everything the pane has filed and closes it, unless a filed
    /// union lacks a field its arm requires: a key pressed while the read
    /// was out can have picked a tag after `Escape` checked.
    fn write_and_close(&mut self) -> Effect {
        if let Some(text) = self.config_pane().and_then(ConfigPane::incomplete_union) {
            self.notice = Some(Notice::grave(text));
            return Effect::None;
        }
        let writes = self.take_pane_writes();
        self.close_pane();
        if writes.is_empty() {
            Effect::None
        } else {
            Effect::SendAll(writes)
        }
    }

    /// Says why a pane that took a moved table is still up. `subject`
    /// names the table and never a value in it. `dropped` is how many edits
    /// the new table took no further.
    fn hold(&mut self, subject: String, dropped: usize) -> Effect {
        let lost = if dropped > 0 {
            format!("; {}", moved_entries(dropped))
        } else {
            String::new()
        };
        self.notice = Some(Notice::grave(format!(
            "{subject} changed while this pane was open, so nothing was written; \
             esc writes your edits over the new values{lost}"
        )));
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use shep_core::protocol::{RpcError, RpcErrorCode};

    use super::super::super::*;
    use crate::lookout::app::testing::*;

    /// The token a second writer gives `jobs` while the pane is open.
    const ROTATED: &str = "sk-live-rotated-77Zq";

    /// `web_view(true)` with `jobs` replaced by `table`.
    fn web_with_jobs(table: &serde_json::Value) -> SheepConfigView {
        let mut view = web_view(true);
        view.config.dogs.insert(
            "jobs".into(),
            table.as_object().cloned().expect("a table").into(),
        );
        view
    }

    /// `web`'s config read, answered with `view`.
    fn answer_web(app: &mut App, view: SheepConfigView) -> Effect {
        app.update(Msg::Replied {
            sent: Sent::SheepConfig { name: "web".into() },
            result: Ok(Response::SheepConfig(Box::new(view))),
        })
    }

    /// `app_in_jobs_table` with `concurrency` edited to 4 and `Escape`
    /// pressed, so the re-read is out. `close_jobs_table` answers it.
    fn closing_jobs() -> App {
        let mut app = app_in_jobs_table();
        type_concurrency(&mut app, "4");
        assert_eq!(
            app.update(Msg::Key(KeyPress::Escape)),
            Effect::Send(Sent::SheepConfig { name: "web".into() }),
            "esc reads the sheep again before writing"
        );
        app
    }

    /// The one table write a batch carries, as JSON.
    fn table_written(effect: Effect) -> serde_json::Value {
        let batch = wire_batch(effect);
        let [
            Sent::SetSheepDogTable {
                name,
                dog,
                table: Some(table),
                ..
            },
        ] = batch.as_slice()
        else {
            panic!("one table write: {batch:?}");
        };
        assert_eq!((name.as_str(), dog.as_str()), ("web", "jobs"));
        serde_json::Value::Object(table.as_map().clone())
    }

    #[test]
    fn esc_on_an_edited_table_pane_holds_it_until_the_re_read_answers() {
        let app = closing_jobs();
        let pane = app.config_pane().expect("the pane waits on the answer");
        assert!(
            matches!(pane.target(), PaneTarget::Sheep { .. }),
            "{pane:?}"
        );
        assert_eq!(table_edits(&app), 1, "nothing has left the pane yet");
    }

    /// A second writer rotated `token` and added `retries` while the pane
    /// was open. Both survive the operator's own write, which the next
    /// `Escape` sends.
    #[test]
    fn a_moved_table_holds_the_pane_and_the_next_close_keeps_both_writes() {
        let mut app = closing_jobs();
        let moved = json!({ "concurrency": 2, "token": ROTATED, "retries": 5 });
        assert_eq!(answer_web(&mut app, web_with_jobs(&moved)), Effect::None);
        let pane = app.config_pane().expect("the pane stays up");
        assert!(
            matches!(pane.target(), PaneTarget::Sheep { .. }),
            "{pane:?}"
        );
        assert_eq!(table_edits(&app), 1, "the edit rides the new table");
        let notice = app.notice().expect("the hold is said");
        assert!(notice.is_grave());
        assert_eq!(
            notice.to_string(),
            "web: its jobs table changed while this pane was open, so nothing was written; \
             esc writes your edits over the new values"
        );

        assert_eq!(
            app.update(Msg::Key(KeyPress::Escape)),
            Effect::Send(Sent::SheepConfig { name: "web".into() })
        );
        let written = table_written(answer_web(&mut app, web_with_jobs(&moved)));
        assert_eq!(
            written,
            json!({ "concurrency": 4, "token": ROTATED, "retries": 5 })
        );
    }

    /// The dialog's `c` writes the tables too, so it asks only after the
    /// re-read found them unmoved.
    #[test]
    fn the_close_dialog_waits_for_the_re_read_and_a_moved_table_skips_it() {
        let mut app = app_in_jobs_table();
        type_concurrency(&mut app, "4");
        crate::lookout::view::fixtures::file_edit(&mut app, "cwd", "/srv/second");
        assert_eq!(
            app.update(Msg::Key(KeyPress::Escape)),
            Effect::Send(Sent::SheepConfig { name: "web".into() }),
            "the tables are read before the dialog asks"
        );
        assert!(
            app.close_dialog().is_none(),
            "no question while the read is out"
        );
        let moved = json!({ "concurrency": 2, "token": ROTATED });
        assert_eq!(answer_web(&mut app, web_with_jobs(&moved)), Effect::None);
        assert!(app.close_dialog().is_none(), "a moved table holds the pane");
        assert!(
            app.notice()
                .is_some_and(|notice| notice.to_string().contains("nothing was written"))
        );

        assert_eq!(
            app.update(Msg::Key(KeyPress::Escape)),
            Effect::Send(Sent::SheepConfig { name: "web".into() })
        );
        assert_eq!(answer_web(&mut app, web_with_jobs(&moved)), Effect::None);
        assert!(
            app.close_dialog().is_some(),
            "an unmoved table lets the dialog ask"
        );
        let batch = wire_batch(app.update(Msg::Key(KeyPress::Continue)));
        assert!(
            batch.iter().any(|sent| matches!(
                sent,
                Sent::SetSheepDogTable { dog, table: Some(table), .. }
                    if dog == "jobs" && table.as_map().get("token") == Some(&json!(ROTATED))
            )),
            "{batch:?}"
        );
    }

    /// A second writer removed the table: writing the pane's copy back
    /// would restore its `token` as well as the edit.
    #[test]
    fn a_table_removed_while_the_pane_was_open_holds_it() {
        let mut app = closing_jobs();
        assert_eq!(answer_web(&mut app, web_view(false)), Effect::None);
        assert_eq!(table_edits(&app), 1);
        let notice = app.notice().expect("the hold is said").to_string();
        assert!(notice.contains("nothing was written"), "{notice}");

        let _ = app.update(Msg::Key(KeyPress::Escape));
        let written = table_written(answer_web(&mut app, web_view(false)));
        assert_eq!(written, json!({ "concurrency": 4 }));
    }

    #[test]
    fn a_failed_re_read_holds_the_pane_and_writes_nothing() {
        let mut app = closing_jobs();
        let effect = app.update(Msg::Replied {
            sent: Sent::SheepConfig { name: "web".into() },
            result: Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::NotFound,
                message: "no sheep named web".into(),
                daemon_version: None,
            })),
        });
        assert_eq!(effect, Effect::None);
        assert_eq!(table_edits(&app), 1);
        let notice = app.notice().expect("reported");
        assert!(notice.is_grave());
        assert_eq!(
            notice.to_string(),
            "web: no sheep named web, so nothing was written"
        );
    }

    #[test]
    fn a_re_read_answered_with_the_wrong_kind_writes_nothing() {
        let mut app = closing_jobs();
        let effect = app.update(Msg::Replied {
            sent: Sent::SheepConfig { name: "web".into() },
            result: Ok(Response::SheepDogSettingsSet {
                name: "web".into(),
                dog: "jobs".into(),
            }),
        });
        assert_eq!(effect, Effect::None);
        assert_eq!(table_edits(&app), 1);
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some(
                "web: the shepherd answered something this lookout does not understand, \
                 so nothing was written"
            )
        );
    }

    /// The wait ends with the read the link task never took, so a later
    /// `r` is a refresh and not a close.
    #[test]
    fn an_unsent_re_read_ends_the_wait_and_a_later_r_only_refreshes() {
        let mut app = closing_jobs();
        let _ = app.update(Msg::Unsent {
            sent: Sent::SheepConfig { name: "web".into() },
        });
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("web: its config was not asked for, so nothing was written")
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::Send(Sent::SheepConfig { name: "web".into() })
        );
        assert_eq!(answer_web(&mut app, web_view(true)), Effect::None);
        assert_eq!(table_edits(&app), 1);
    }

    #[test]
    fn a_second_esc_while_the_re_read_is_out_asks_nothing_more() {
        let mut app = closing_jobs();
        assert_eq!(app.update(Msg::Key(KeyPress::Escape)), Effect::None);
        let written = table_written(answer_web(&mut app, web_view(true)));
        assert_eq!(written, json!({ "concurrency": 4, "token": TOKEN }));
    }

    /// `r` sends a plain read that is still out when `Escape` follows.
    /// `Escape` must not send a second one: two outstanding reads for the
    /// same target would answer to the same `Sent::SheepConfig`, and
    /// nothing in the reply says which request it settles, so whichever
    /// landed first would be free to answer the other's. `reread_pane`
    /// refuses the second send on its own, so `Escape` comes back with
    /// nothing to do, and the one reply that does land still closes the
    /// pane.
    #[test]
    fn an_escape_after_a_refresh_still_out_does_not_race_it_with_a_second_read() {
        let mut app = app_in_jobs_table();
        type_concurrency(&mut app, "4");
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::Send(Sent::SheepConfig { name: "web".into() }),
            "r sends the read"
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::Escape)),
            Effect::None,
            "esc must not send a second read while r's is still out"
        );
        let written = table_written(answer_web(&mut app, web_view(true)));
        assert_eq!(written, json!({ "concurrency": 4, "token": TOKEN }));
    }

    /// `bark`'s section read, answered with `section`.
    fn answer_bark(app: &mut App, section: String) -> Effect {
        app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "bark".into(),
            },
            result: Ok(Response::DogSection {
                toml: section.into(),
            }),
        })
    }

    /// `app_in_dog_pane_with_two_edits` with `Escape` pressed, so the
    /// re-read is out.
    fn closing_bark() -> App {
        let mut app = fixtures::app_in_dog_pane_with_two_edits();
        assert_eq!(
            app.update(Msg::Key(KeyPress::Escape)),
            Effect::Send(Sent::DogSection {
                name: "bark".into()
            }),
            "esc reads the section again before writing"
        );
        app
    }

    /// The one section write a batch carries.
    fn section_written(effect: Effect) -> String {
        let batch = wire_batch(effect);
        let [Sent::SetDogSection { name, toml, .. }] = batch.as_slice() else {
            panic!("one section write: {batch:?}");
        };
        assert_eq!(name, "bark");
        toml.as_str().to_owned()
    }

    #[test]
    fn an_unchanged_section_is_written_and_the_dog_pane_closes() {
        let mut app = closing_bark();
        assert!(app.config_pane().is_some(), "held until the answer");
        let written = section_written(answer_bark(&mut app, fixtures::dog_section()));
        assert!(written.contains("poll = \"45s\""), "{written}");
        assert!(written.contains("history_bytes = 8192"), "{written}");
        assert!(matches!(app.body(), Body::FlockTable), "the pane closed");
    }

    /// The operator rotated the webhook by hand while the pane was open.
    /// The next close writes the pane's edits over the rotated section.
    #[test]
    fn a_moved_section_holds_the_dog_pane_and_the_next_close_keeps_both_writes() {
        const ROTATED_URL: &str = "https://hooks.example/rotated";
        let moved = || {
            fixtures::dog_section()
                .replace("https://hooks.example/x", ROTATED_URL)
                .replace("# how often", "# how often, set by ops")
        };
        let mut app = closing_bark();
        assert_eq!(answer_bark(&mut app, moved()), Effect::None);
        let pane = app.config_pane().expect("the pane stays up");
        assert_eq!(pane.target().name(), "bark");
        assert_eq!(pane.edits().len(), 2, "both edits ride the new section");
        let notice = app.notice().expect("the hold is said");
        assert!(notice.is_grave());
        assert_eq!(
            notice.to_string(),
            "bark: its section in dogs.toml changed while this pane was open, so nothing \
             was written; esc writes your edits over the new values"
        );

        let _ = app.update(Msg::Key(KeyPress::Escape));
        let written = section_written(answer_bark(&mut app, moved()));
        assert!(written.contains(ROTATED_URL), "{written}");
        assert!(written.contains("# how often, set by ops"), "{written}");
        assert!(written.contains("poll = \"45s\""), "{written}");
    }

    #[test]
    fn a_failed_section_re_read_holds_the_dog_pane_and_writes_nothing() {
        let mut app = closing_bark();
        let effect = app.update(Msg::Replied {
            sent: Sent::DogSection {
                name: "bark".into(),
            },
            result: Err(fixtures::a_refusal()),
        });
        assert_eq!(effect, Effect::None);
        assert_eq!(app.config_pane().expect("still up").edits().len(), 2);
        let notice = app.notice().expect("reported");
        assert!(notice.is_grave());
        assert!(
            notice.to_string().ends_with(", so nothing was written"),
            "{notice}"
        );
    }

    #[test]
    fn an_unsent_section_re_read_says_nothing_was_written() {
        let mut app = closing_bark();
        let _ = app.update(Msg::Unsent {
            sent: Sent::DogSection {
                name: "bark".into(),
            },
        });
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("bark: its config was not asked for, so nothing was written")
        );
        assert_eq!(app.config_pane().expect("still up").edits().len(), 2);
    }
}
