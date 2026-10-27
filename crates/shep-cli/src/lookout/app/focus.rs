//! `shep lookout <sheep> [tab] [dog]`: the screen the operator asked for,
//! resolved against the first flock answer.
//!
//! The command line can name a sheep the shepherd is still restoring, so the
//! ask is held until the first `ListFlock` reply can say whether it is there.
//! A sheep the flock does not have is reported and dropped: the dashboard
//! opens on the flock table with a notice naming it, and the operator's own
//! keys are the only way to try again.

use shep_core::config::GROUP_ORDER;

use super::*;

/// The screen `shep lookout <sheep> [tab] [dog]` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Focus {
    /// The sheep's name, as `shep describe` takes it.
    pub sheep: String,
    /// The config pane's group to open on, or `None` for the sheep's own
    /// status page.
    pub tab: Option<&'static str>,
    /// The dog whose table to open. The parse admits one only with the
    /// `dogs` group as its tab.
    pub dog: Option<String>,
}

impl Focus {
    /// Builds the ask from what the parse accepted.
    ///
    /// `tab` is resolved against [`GROUP_ORDER`], the same list the pane
    /// draws its tabs from. A name outside it cannot reach here, because
    /// clap's own possible values are that list; one that somehow did opens
    /// the pane on its first group rather than on a group that does not
    /// exist.
    #[must_use]
    pub(crate) fn new(sheep: String, tab: Option<&str>, dog: Option<String>) -> Self {
        Self {
            sheep,
            tab: tab.map(|name| {
                GROUP_ORDER
                    .iter()
                    .copied()
                    .find(|group| *group == name)
                    .unwrap_or(GROUP_ORDER[0])
            }),
            dog,
        }
    }
}

impl App {
    /// Says which screen this lookout should open on, before it has a flock
    /// to open it on. Called once, from `crate::lookout::entry`.
    pub(crate) fn set_focus(&mut self, focus: Focus) {
        self.focus = Some(focus);
    }

    /// The first flock answer's chance to open the screen the command line
    /// asked for.
    ///
    /// `None` when nothing was asked for, when the answer has already been
    /// spent, and when it could not be honoured, in which case the notice
    /// names what was not found. Spent on the first answer whatever that
    /// answer says: a shepherd still restoring the flock is a reason to
    /// report a missing sheep, not a reason to keep trying, and a lookout
    /// that reopened a pane on every poll would outrace the operator's own
    /// keys.
    ///
    /// The sheep is the one the flock answers with, not the row the table
    /// happens to select: an app with instances has several, and the lowest
    /// id is the one the pane pins. A dog is not a sheep and is reported as
    /// not found, the same way a name in no flock at all is.
    pub(super) fn resolve_focus(&mut self) -> Option<Effect> {
        let focus = self.focus.take()?;
        let Some((id, name)) = self
            .flock
            .iter()
            .find(|(_, row)| row.info.dog.is_none() && row.info.name == focus.sheep)
            .map(|(id, row)| (*id, row.info.name.clone()))
        else {
            self.notice = Some(Notice::grave(format!("no sheep named {}", focus.sheep)));
            return None;
        };
        self.selected = Some(RowKey::Sheep(id));
        let Some(tab) = focus.tab else {
            return Some(self.open_sheep_pane());
        };
        self.focus_tab = Some(tab);
        self.focus_dog = focus.dog;
        Some(self.ask_for_sheep_config(name, ConfigFor::Editor))
    }

    /// The tab [`Self::resolve_focus`] asked the config pane to open on,
    /// spent by the pane that opens.
    pub(super) fn take_focus_tab(&mut self) -> Option<&'static str> {
        self.focus_tab.take()
    }

    /// The dog [`Self::resolve_focus`] asked the `dogs` tab to open, spent by
    /// the listing that names it.
    pub(super) fn take_focus_dog(&mut self) -> Option<String> {
        self.focus_dog.take()
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::*;
    use crate::lookout::view::fixtures;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use shep_core::protocol::{RpcError, RpcErrorCode};

    /// A dashboard that was told to open on `focus`, with no flock yet: the
    /// state `crate::lookout::entry` leaves `App` in before the first poll.
    fn awaiting(focus: Focus) -> App {
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            Instant::now(),
        );
        app.set_focus(focus);
        app
    }

    /// `web`, `api` and `worker`, the flock every test here polls.
    fn flock() -> Vec<ProcessInfo> {
        vec![
            sheep(1, "web", ProcStatus::Online),
            sheep(2, "api", ProcStatus::Online),
            sheep(3, "worker", ProcStatus::Online),
        ]
    }

    /// The first frame, sized and prepared the way `run_ui` prepares one.
    fn first_frame(app: &mut App) -> String {
        let area = Rect::new(0, 0, 160, 48);
        app.note_body_rows(crate::lookout::view::body_rows(area));
        app.note_body_width(area.width);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| crate::lookout::view::draw(app, frame))
            .unwrap();
        crate::lookout::frames::render_text(terminal.backend().buffer())
    }

    /// The `jobs` row of the open pane.
    fn jobs_row(app: &App) -> DogRow {
        app.config_pane()
            .expect("a pane")
            .dogs_tab()
            .rows()
            .iter()
            .find(|row| row.name() == "jobs")
            .expect("jobs is listed")
            .clone()
    }

    /// `<sheep>` opens the sheep pane and asks for the config its own column
    /// has nothing to draw without, the same pair `↵` raises.
    #[test]
    fn a_named_sheep_opens_its_status_page_on_the_first_answer() {
        let mut app = awaiting(Focus::new("web".into(), None, None));
        let effect = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        assert_eq!(
            effect,
            Effect::Send(Sent::SheepConfig {
                name: "web".to_string()
            }),
            "the pane's read goes out with the answer that found the sheep"
        );
        assert!(matches!(app.body(), Body::Sheep(_)));
        assert_eq!(app.selected(), Some(RowKey::Sheep(1)));

        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        let frame = first_frame(&mut app);
        assert!(
            frame.contains("██ SHEEP 1  web   online"),
            "the first frame is the sheep pane:\n{frame}"
        );
        assert!(!frame.contains("STATUS"), "and not the flock table");
    }

    /// `<sheep> <tab>` opens the config pane `e` opens, already on that tab.
    #[test]
    fn a_named_sheep_and_a_tab_open_the_config_pane_on_that_tab() {
        let mut app = awaiting(Focus::new("web".into(), Some("watch"), None));
        let _ = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(
                fixtures::sheep_config_view(),
            ))),
        });
        assert_eq!(
            app.config_pane().expect("the pane is open").group(),
            "watch"
        );

        let frame = first_frame(&mut app);
        assert!(
            frame.contains("██ web  (sheep config)"),
            "the first frame is the config pane:\n{frame}"
        );
        assert!(
            frame.contains(">  ignore_watch") && frame.contains("watch_delay"),
            "and it is drawn on the watch tab's own fields:\n{frame}"
        );
        assert!(
            !frame.contains(">  cwd"),
            "the cursor is not on the process tab's first field:\n{frame}"
        );
    }

    /// `<sheep> dogs <dog>` asks for the listing the tab needs and then for
    /// that dog's schema, without a keystroke between the two.
    #[test]
    fn a_named_dog_opens_its_table_on_the_dogs_tab() {
        let mut app = awaiting(Focus::new("web".into(), Some("dogs"), Some("jobs".into())));
        let _ = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        let effect = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(web_view(true)))),
        });
        let Effect::ListSheepDogs { sheep, ask } = effect else {
            panic!("the dogs tab asks for its listing: {effect:?}");
        };
        assert_eq!(sheep, "web");

        let effect = app.update(Msg::SheepDogNames {
            sheep: "web".into(),
            ask,
            dogs: listing(),
        });
        assert_eq!(
            effect,
            Effect::LoadSheepDog {
                sheep: "web".into(),
                dog: "jobs".into(),
                adopted_path: Some("/opt/jobs".into()),
            },
            "the listing's answer opens the asked-for row"
        );
        let _ = app.update(Msg::SheepDog {
            sheep: "web".into(),
            entry: jobs_entry(),
        });

        let frame = first_frame(&mut app);
        assert!(
            frame.contains("██ web  (sheep config)"),
            "the first frame is the config pane:\n{frame}"
        );
        assert!(frame.contains("▾ jobs"), "with jobs's table open");
        assert!(frame.contains("concurrency"), "and its rows drawn");
    }

    /// A sheep the first answer does not have is reported, not retried, and
    /// the dashboard is what opens.
    #[test]
    fn an_unknown_sheep_lands_on_the_dashboard_with_a_notice() {
        let mut app = awaiting(Focus::new("ghost".into(), None, None));
        let effect = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        assert_eq!(effect, Effect::RefreshFeed, "the dashboard's own read");
        assert!(matches!(app.body(), Body::FlockTable));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("no sheep named ghost")
        );

        let frame = first_frame(&mut app);
        assert!(
            frame.contains("STATUS"),
            "the flock table is drawn:\n{frame}"
        );
        assert!(
            frame.contains("no sheep named ghost"),
            "and the notice names what was not found:\n{frame}"
        );
    }

    /// A dog the tab's listing does not name is reported the same way, and
    /// the tab it was asked for stays open.
    #[test]
    fn an_unknown_dog_lands_on_the_dogs_tab_with_a_notice() {
        let mut app = awaiting(Focus::new("web".into(), Some("dogs"), Some("nope".into())));
        let _ = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        let Effect::ListSheepDogs { ask, .. } = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(web_view(true)))),
        }) else {
            panic!("the dogs tab asks for its listing");
        };
        let effect = app.update(Msg::SheepDogNames {
            sheep: "web".into(),
            ask,
            dogs: listing(),
        });
        assert_eq!(effect, Effect::None);
        let notice = app.notice().expect("a refusal").to_string();
        assert_eq!(notice, "web: no dog named nope", "{notice}");
        assert_eq!(
            app.config_pane().expect("the tab stays open").group(),
            "dogs"
        );
    }

    /// Over `--host` the pane still opens on the tab the command line named.
    /// The listing a dog's table needs is read off this machine's files, so
    /// the tab says that instead of opening it, exactly as `tab` over a host
    /// does.
    #[test]
    fn a_dog_ask_over_a_host_stops_at_the_tab() {
        let mut app = awaiting(Focus::new("web".into(), Some("dogs"), Some("jobs".into())));
        app.set_remote_host("build-01");
        let _ = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        let effect = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(web_view(true)))),
        });
        assert_eq!(effect, Effect::None, "no listing ask over a host");
        assert_eq!(
            app.config_pane().expect("the tab still opens").group(),
            "dogs"
        );
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some(NOT_OVER_HOST),
            "and the tab says why it has no rows"
        );
    }

    /// The ask is spent on the first answer, so a later poll cannot reopen a
    /// screen the operator has since closed.
    #[test]
    fn the_ask_is_spent_once() {
        let mut app = awaiting(Focus::new("web".into(), None, None));
        let _ = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(matches!(app.body(), Body::FlockTable));
        let _ = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        assert!(matches!(app.body(), Body::FlockTable), "no reopening");
    }

    /// Closing a pane the command line opened drops the rest of the ask: the
    /// operator is driving from here, and a dog left unopened is not a promise
    /// the next pane has to keep.
    #[test]
    fn closing_a_pane_drops_the_rest_of_the_ask() {
        let mut app = awaiting(Focus::new("web".into(), Some("dogs"), Some("jobs".into())));
        let _ = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(web_view(true)))),
        });
        let _ = app.update(Msg::Key(KeyPress::Escape));
        assert!(app.config_pane().is_none(), "the pane closed");

        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(web_view(true)))),
        });
        let Effect::ListSheepDogs { ask, .. } = app.update(Msg::Key(KeyPress::Group(9))) else {
            panic!("the tab asks for its listing again");
        };
        let _ = app.update(Msg::SheepDogNames {
            sheep: "web".into(),
            ask,
            dogs: listing(),
        });
        assert!(
            app.notice().is_none(),
            "{:?}",
            app.notice().map(ToString::to_string)
        );
        assert!(!jobs_row(&app).is_open(), "jobs stays closed");
    }

    /// A refused config read reports itself and drops the tab and the dog
    /// that were waiting on it, so a later `e` opens on the pane's own first
    /// tab rather than on a stale ask.
    #[test]
    fn a_refused_config_read_drops_the_ask_it_was_waiting_on() {
        let mut app = awaiting(Focus::new("web".into(), Some("dogs"), Some("jobs".into())));
        let _ = app.update(Msg::Snapshot {
            rows: flock(),
            at: Instant::now(),
        });
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Err(RequestError::Rpc(RpcError {
                code: RpcErrorCode::NotFound,
                message: "no sheep named web".to_string(),
                daemon_version: None,
            })),
        });
        assert!(app.config_pane().is_none());

        let _ = app.update(Msg::Key(KeyPress::Edit));
        let _ = app.update(Msg::Replied {
            sent: Sent::SheepConfig {
                name: "web".to_string(),
            },
            result: Ok(Response::SheepConfig(Box::new(web_view(true)))),
        });
        assert_eq!(
            app.config_pane().expect("the pane opens").group(),
            "process",
            "the pane opens on its own first tab"
        );
    }

    /// A name the flock answers with as a dog is not a sheep, and the notice
    /// says the same sentence it says for a name in no flock at all.
    #[test]
    fn a_dog_is_not_a_sheep() {
        let mut app = awaiting(Focus::new("otel".into(), None, None));
        let _ = app.update(Msg::Snapshot {
            rows: vec![
                ProcessInfo::builder(90, "otel", ProcStatus::Online)
                    .dog(Some(DogSource::BuiltIn { grants: None }))
                    .build(),
            ],
            at: Instant::now(),
        });
        assert!(matches!(app.body(), Body::FlockTable));
        assert_eq!(
            app.notice().map(ToString::to_string).as_deref(),
            Some("no sheep named otel")
        );
    }
}
