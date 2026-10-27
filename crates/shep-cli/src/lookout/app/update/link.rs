//! What the link, the clock and the reads report: the flock map a
//! snapshot replaces, the bus events that patch it, and everything a lost
//! link stops.

use super::*;

impl App {
    /// `Msg::Snapshot`'s arm: the flock map replaced wholesale, the cursor
    /// reseated on the new one, and the selected row's feed asked for.
    pub(super) fn on_flock_snapshot(&mut self, rows: Vec<ProcessInfo>, at: Instant) -> Effect {
        // The link task has ended, so nothing is left to produce a
        // snapshot. Accepting one would un-freeze the dashboard.
        if matches!(self.link, Link::Lost { .. }) {
            return Effect::None;
        }
        let previous = self.selected_index();
        self.flock = rows
            .into_iter()
            .map(|info| (info.id, Row { info, anchor: at }))
            .collect();
        self.record_samples(at);
        self.reseat(previous);
        self.forget_missing_target();
        // The command line's ask outranks the ordinary poll's feed refresh:
        // it is the one thing here that can change which screen is up, and
        // the feed re-reads on the next poll anyway.
        if let Some(effect) = self.resolve_focus() {
            return effect;
        }
        // Unconditional: the selected row's log paths can change even
        // when the selection does not, and this is the feed's cadence.
        Effect::RefreshFeed
    }

    /// `Msg::Event`'s arm: one bus frame patching the flock map.
    pub(super) fn on_event(&mut self, event: BusEvent) -> Effect {
        match event {
            BusEvent::Process { event, info, .. } => {
                if matches!(event, ProcessEventKind::Delete) {
                    let previous = self.selected_index();
                    self.flock.remove(&info.id);
                    self.forget_missing_target();
                    return if self.reseat(previous) {
                        Effect::RefreshSelected
                    } else {
                        Effect::None
                    };
                }
                // An upsert can orphan the selection from `visible_rows()`
                // without touching `self.flock`: a rename can move the selected
                // row out of the filter. `reseat` is a no-op read while the
                // selection is still seated.
                let previous = self.selected_index();
                let anchor = self.now;
                self.flock.insert(info.id, Row { info, anchor });
                if self.reseat(previous) {
                    return Effect::RefreshSelected;
                }
                Effect::None
            }
            // The shepherd's own outbound queue overflowed. Worded differently
            // from `Msg::BusLagged`: an operator cannot tell which end of the
            // connection to investigate if the two read the same.
            BusEvent::Dropped { count } => {
                self.notice = Some(Notice::info(format!(
                    "the shepherd dropped {count} events; re-reading the flock"
                )));
                Effect::PollNow
            }
            // A notice, not an exit: a dashboard that vanished would take the
            // last known state with it.
            BusEvent::DaemonShutdown => {
                self.notice = Some(Notice::grave("the shepherd is shutting down".to_string()));
                Effect::None
            }
            // `BusEvent` is `#[non_exhaustive]`: a newer shepherd's variant
            // must not take the dashboard down.
            _ => Effect::None,
        }
    }

    /// `Msg::BusLagged`'s arm: this client's own receiver dropped frames.
    pub(super) fn on_bus_lagged(&mut self, count: u64) -> Effect {
        self.notice = Some(Notice::info(format!(
            "lookout fell behind and lost {count} events; re-reading the flock"
        )));
        Effect::PollNow
    }

    /// `Msg::Retrying`'s arm: the link task is re-dialling.
    pub(super) fn on_retrying(&mut self, attempt: u32) -> Effect {
        if !matches!(self.link, Link::Lost { .. }) {
            self.link = Link::Retrying { attempt };
            self.disarm_on_link_change();
        }
        Effect::None
    }

    /// `Msg::Relinked`'s arm: the link task reconnected and re-subscribed.
    pub(super) fn on_relinked(&mut self) -> Effect {
        if !matches!(self.link, Link::Lost { .. }) {
            self.link = Link::Live;
        }
        Effect::None
    }

    /// `Msg::Frozen`'s arm: the reconnect ladder is exhausted, and every
    /// armed thing on screen is disarmed with it.
    pub(super) fn on_frozen(&mut self, at_local: String, why: String) -> Effect {
        self.link = Link::Lost { at_local, why };
        self.froze_at = Some(self.now);
        self.frozen_for = Duration::ZERO;
        self.disarm_on_link_change();
        // Every notice is about a shepherd that no longer exists,
        // and none of them can be acted on. Left standing, the last
        // one outranks the key hint for the rest of the session
        // (`view::status::status_line`'s own ordering), so an
        // operator reads `the shepherd is shutting down` where the
        // bar should be naming the keys that still work. Whether the
        // shutdown was clean is on the screen either way: the link
        // panel quotes an error that says the socket was removed
        // rather than refusing.
        self.notice = None;
        Effect::None
    }

    /// `Msg::Tick`'s arm: advance the clock, expire what rides it, and
    /// refresh the full-screen feeds.
    pub(super) fn on_tick(&mut self, now: Instant) -> Effect {
        if !matches!(self.link, Link::Lost { .. }) {
            self.now = now;
            let expired = self.action.as_ref().is_some_and(|action| {
                action.stage == Stage::Armed
                    && now.saturating_duration_since(action.at) >= CONFIRM_EXPIRY
            });
            if expired {
                self.action = None;
            }
            let stale = self
                .close_dialog
                .as_ref()
                .is_some_and(|dialog| now.saturating_duration_since(dialog.at()) >= CONFIRM_EXPIRY);
            if stale {
                self.close_dialog = None;
            }
            // A reply that never comes cannot strand the verb: it
            // rides the same clock as the dialog it followed from.
            let stale_held = self
                .held
                .as_ref()
                .is_some_and(|held| now.saturating_duration_since(held.at) >= CONFIRM_EXPIRY);
            if stale_held {
                self.held = None;
            }
        }
        // The tick's own `now` again, for the same reason: how long
        // the shepherd has been gone is the one number a frozen
        // dashboard keeps counting.
        if let Some(at) = self.froze_at {
            self.frozen_for = now.saturating_duration_since(at);
        }
        // Against the tick's own `now`, not `self.now`, which stops on
        // a dead link: a settings edit describes a local file that is
        // no staler for the shepherd being gone.
        if let Some(settings) = self.settings_mut() {
            let expired = matches!(
                settings.pending,
                Some(Pending::Armed { at, .. } | Pending::DogArmed { at, .. })
                    if now.saturating_duration_since(at) >= CONFIRM_EXPIRY
            );
            if expired {
                settings.pending = None;
            }
        }
        // The config pane has no expiry of its own any more:
        // nothing on it is a question waiting for an answer. Its
        // edits sit until the operator writes them or undoes them,
        // and a set that timed out would throw work away silently.
        // Outside the link guard for the same reason, and one more:
        // `until` was set off `self.now`, which a dead link stops
        // advancing, so the tick's own `now` is what expires a
        // reveal at all once the link has gone.
        if let Some(pane) = self.secrets_pane_mut()
            && pane
                .reveal
                .as_ref()
                .is_some_and(|reveal| now >= reveal.until)
        {
            pane.hide();
        }
        // Outside the link guard for the same reason as the reveal
        // above: an armed delete that can never be sent is no less
        // stale for the shepherd being gone. `now`, not `self.now`.
        if let Some(pane) = self.secrets_pane_mut()
            && pane
                .armed
                .as_ref()
                .is_some_and(|a| now.saturating_duration_since(a.at) >= CONFIRM_EXPIRY)
        {
            pane.armed = None;
        }
        // Neither full-screen feed has a timer of its own; each
        // rides every tick instead of the dashboard's own cadence,
        // which is fixed for the connection's lifetime (see
        // `RefreshFeed`'s own doc). The dashboard raises nothing
        // here, or every lookout would poll twice as often for
        // nothing. `Link::Lost` too: `Msg::Bleats` throws the tail
        // away while the link is down, so every read would be work
        // done and discarded once a second. `Msg::Snapshot` and
        // `select_at` guard on the same thing, and
        // `a_frozen_dashboard_does_not_re_read_anything` states the
        // rule.
        if matches!(self.body, Body::Bleats(_) | Body::Sheep(_))
            && !matches!(self.link, Link::Lost { .. })
        {
            Effect::RefreshFeed
        } else {
            Effect::None
        }
    }

    /// `Msg::Host`'s arm: one reading of the machine this lookout runs on.
    pub(super) fn on_host(&mut self, sample: Option<crate::lookout::source::HostSample>) -> Effect {
        // A strip ticking over under a banner saying the values are
        // frozen contradicts it on one frame.
        if matches!(self.link, Link::Lost { .. }) {
            return Effect::None;
        }
        self.host_unsupported = sample.is_none();
        self.host = sample;
        Effect::None
    }

    /// `Msg::LogSize`'s arm.
    ///
    /// `Effect::None` for `Msg::Bleats`' reason: a refresh in answer would spin the UI task.
    pub(super) fn on_log_size(&mut self, id: u32, total_bytes: Option<u64>) -> Effect {
        self.log_size = Some(LogSize { id, total_bytes });
        Effect::None
    }

    /// `Msg::Bleats`'s arm.
    ///
    /// Always `Effect::None`: answering a feed update with another
    /// refresh would spin the UI task. The guard catches a read `run_ui`
    /// armed before the freeze landed.
    pub(super) fn on_bleats(&mut self, tail: crate::lookout::tail::Tail) -> Effect {
        if matches!(self.link, Link::Lost { .. }) {
            return Effect::None;
        }
        self.feed = tail;
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;
    use shep_core::protocol::ProcessEventKind;

    #[test]
    fn a_snapshot_replaces_the_flock_wholesale() {
        let (mut app, t0) = started();
        app.update(Msg::Event(BusEvent::Process {
            event: ProcessEventKind::Start,
            info: sheep(9, "ghost", ProcStatus::Starting),
            manually: true,
            at_ms: 0,
        }));
        assert_eq!(app.rows().len(), 4, "the bus event upserted");

        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "web", ProcStatus::Online)],
            at: t0,
        });
        assert_eq!(app.rows().len(), 1);
        assert!(app.rows().iter().all(|row| row.info.id == 1));
    }

    #[test]
    fn a_snapshot_that_shrinks_the_flock_pulls_the_selection_back() {
        let (mut app, t0) = started();
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.selected_index(), Some(3), "past the flock header");

        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "web", ProcStatus::Online)],
            at: t0,
        });
        assert_eq!(
            app.selected_index(),
            Some(1),
            "the selection came back with the flock, past the header"
        );

        app.update(Msg::Snapshot {
            rows: vec![],
            at: t0,
        });
        assert_eq!(app.selected_index(), None, "an empty flock selects nothing");
    }

    #[test]
    fn the_selection_follows_the_sheep_and_not_the_row_number() {
        let (mut app, t0) = started();
        app.update(Msg::Key(KeyPress::SelectDown));
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(3)),
            "the third row, worker"
        );

        // Sheep 1 goes away. `worker` is now row 1 rather than row 2, where
        // an index cursor would be pointing at `api`.
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(2, "api", ProcStatus::Errored),
                sheep(3, "worker", ProcStatus::Online),
            ],
            at: t0,
        });
        assert_eq!(app.selected(), Some(RowKey::Sheep(3)), "still worker");
        assert_eq!(
            app.selected_index(),
            Some(2),
            "which is now row 2, past the header"
        );
    }

    #[test]
    fn a_deleted_selection_falls_to_the_row_that_took_its_place() {
        let (mut app, t0) = started();
        app.update(Msg::Key(KeyPress::SelectDown));
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(1)),
            "web, at index 1 by name"
        );

        // web dies; api and worker remain. Index 1 is now worker.
        app.update(Msg::Snapshot {
            rows: vec![
                sheep(2, "api", ProcStatus::Online),
                sheep(3, "worker", ProcStatus::Online),
            ],
            at: t0,
        });
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(3)),
            "the row that took index 1"
        );

        // The last row dying clamps rather than leaving the cursor past the end.
        app.update(Msg::Key(KeyPress::SelectLast));
        assert_eq!(app.selected(), Some(RowKey::Sheep(3)));
        app.update(Msg::Snapshot {
            rows: vec![sheep(2, "api", ProcStatus::Online)],
            at: t0,
        });
        assert_eq!(app.selected(), Some(RowKey::Sheep(2)));

        app.update(Msg::Snapshot {
            rows: vec![],
            at: t0,
        });
        assert_eq!(app.selected(), None);
        assert_eq!(app.selected_index(), None);
    }

    #[test]
    fn a_selection_that_moves_refreshes_the_feed_and_one_that_cannot_does_not() {
        let (mut app, _) = started();
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::RefreshSelected
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectFirst)),
            Effect::RefreshSelected
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectUp)),
            Effect::None,
            "already at the top: nothing moved, so nothing is re-read"
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectLast)),
            Effect::RefreshSelected
        );
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::None,
            "already at the bottom"
        );
    }

    #[test]
    fn moving_the_selection_asks_for_lambs() {
        let (mut app, _t0) = started();
        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::RefreshSelected
        );
    }

    /// `ListFlock` declines the lamb walk: a full machine enumeration every two
    /// seconds, times every open lookout.
    #[test]
    fn a_snapshot_refreshes_the_feed_and_does_not_ask_for_lambs() {
        let (mut app, t0) = started();
        assert_eq!(
            app.update(Msg::Snapshot {
                rows: vec![sheep(1, "web", ProcStatus::Online)],
                at: t0,
            }),
            Effect::RefreshFeed
        );
    }

    #[test]
    fn nothing_is_requested_while_the_link_is_lost() {
        let (mut app, _t0) = started();
        app.update(Msg::Frozen {
            at_local: "2026-08-16 09:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(app.update(Msg::Key(KeyPress::SelectDown)), Effect::None);
    }

    #[test]
    fn a_snapshot_refreshes_the_feed_unless_the_link_is_frozen() {
        let (mut app, t0) = started();
        assert_eq!(
            app.update(Msg::Snapshot {
                rows: vec![sheep(1, "web", ProcStatus::Online)],
                at: t0
            }),
            Effect::RefreshFeed
        );
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.update(Msg::Snapshot {
                rows: vec![sheep(1, "web", ProcStatus::Online)],
                at: t0
            }),
            Effect::None,
            "a frozen dashboard does not re-read anything"
        );
    }

    /// The cursor still moves: re-rendering the detail pane from the frozen
    /// listing is data already on the frame. Touching the disk is not.
    #[test]
    fn a_frozen_dashboard_moves_the_cursor_without_touching_a_file() {
        let (mut app, _) = started();
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });

        assert_eq!(
            app.update(Msg::Key(KeyPress::SelectDown)),
            Effect::None,
            "no file is read once the link is lost"
        );
        assert_eq!(
            app.selected(),
            Some(RowKey::Sheep(1)),
            "but the cursor moved anyway"
        );
        assert_eq!(app.update(Msg::Key(KeyPress::SelectLast)), Effect::None);
        assert_eq!(app.selected(), Some(RowKey::Sheep(3)));
    }

    #[test]
    fn a_drop_and_a_lag_both_ask_for_an_immediate_poll() {
        let (mut app, _) = started();
        assert_eq!(
            app.update(Msg::Event(BusEvent::Dropped { count: 12 })),
            Effect::PollNow
        );
        assert_eq!(app.update(Msg::BusLagged { count: 3 }), Effect::PollNow);
        assert_eq!(
            app.update(Msg::Event(BusEvent::Process {
                event: ProcessEventKind::Online,
                info: sheep(1, "web", ProcStatus::Online),
                manually: false,
                at_ms: 0,
            })),
            Effect::None,
            "an ordinary event needs no repair"
        );
    }

    #[test]
    fn a_shepherd_side_drop_and_a_local_lag_read_differently() {
        let (mut app, _) = started();
        app.update(Msg::Event(BusEvent::Dropped { count: 12 }));
        let shepherd_side = app.notice().expect("a drop leaves a notice").to_string();
        app.update(Msg::BusLagged { count: 3 });
        let local = app.notice().expect("a lag leaves a notice").to_string();

        assert!(shepherd_side.contains("the shepherd dropped"));
        assert!(local.contains("lookout fell behind"));
        assert_ne!(shepherd_side, local);
    }

    #[test]
    fn a_running_sheeps_uptime_advances_with_the_heartbeat() {
        let (mut app, t0) = started();
        assert_eq!(app.uptime_ms(app.rows()[0].info.id), Some(60_000));
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(5),
        });
        assert_eq!(app.uptime_ms(1), Some(65_000));
    }

    #[test]
    fn a_frozen_dashboard_stops_the_uptime_clock() {
        let (mut app, t0) = started();
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(5),
        });
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let at_freeze = app.uptime_ms(1);
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(400),
        });
        assert_eq!(
            app.uptime_ms(1),
            at_freeze,
            "the clock stopped with the link"
        );
        assert_eq!(at_freeze, Some(65_000));
    }

    #[test]
    fn a_stopped_sheeps_uptime_does_not_advance() {
        let t0 = Instant::now();
        let mut app = App::new(
            Palette::detect(None, None, None),
            Control::ReadOnly,
            "/home/ada/.shep".to_string(),
            t0,
        );
        app.update(Msg::Snapshot {
            rows: vec![sheep(1, "web", ProcStatus::Stopped)],
            at: t0,
        });
        app.update(Msg::Tick {
            now: t0 + Duration::from_secs(30),
        });
        assert_eq!(app.uptime_ms(1), Some(60_000));
    }

    #[test]
    fn the_link_state_walks_live_to_retrying_to_lost_and_back() {
        let (mut app, t0) = started();
        assert_eq!(app.link(), &Link::Live);

        app.update(Msg::Retrying { attempt: 1 });
        assert_eq!(app.link(), &Link::Retrying { attempt: 1 });

        app.update(Msg::Relinked);
        assert_eq!(app.link(), &Link::Live);

        app.update(Msg::Retrying { attempt: 5 });
        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.link(),
            &Link::Lost {
                at_local: "2026-08-14 14:32:07".to_string(),
                why: fixtures::FROZEN_WHY.to_string(),
            }
        );

        // A late snapshot must not unfreeze it.
        app.update(Msg::Snapshot {
            rows: vec![],
            at: t0,
        });
        assert!(matches!(app.link(), Link::Lost { .. }));
    }

    #[test]
    fn refresh_polls_while_live_and_says_why_it_cannot_once_frozen() {
        let (mut app, _) = started();
        assert_eq!(app.update(Msg::Key(KeyPress::Refresh)), Effect::PollNow);

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        assert_eq!(
            app.update(Msg::Key(KeyPress::Refresh)),
            Effect::None,
            "there is no link task left to ask"
        );
        let notice = app.notice().expect("a refusal is a notice").to_string();
        assert!(notice.contains("the shepherd is gone"));
        assert!(notice.contains("nothing left to ask"));
    }

    /// The strip reads this machine, which lookout can still see after the
    /// shepherd dies, so it is the one pane that could keep ticking under a
    /// banner saying the values are frozen.
    #[test]
    fn a_frozen_dashboard_ignores_a_host_sample() {
        let (mut app, _) = started();
        app.update(Msg::Host {
            sample: Some(crate::lookout::source::HostSample {
                load: (2.31, 4.10, 3.88),
                cores: Some(10),
                memory_total_bytes: 32 << 30,
                memory_used_bytes: 12 << 30,
                uptime_seconds: 600,
            }),
        });
        assert!(app.host().is_some(), "a live dashboard takes the sample");

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        let frozen = app.host();
        assert_eq!(app.update(Msg::Host { sample: None }), Effect::None);
        assert_eq!(app.host(), frozen, "the last values stay, unchanged");
        assert!(
            !app.host_unsupported(),
            "and a refused sample changes no flag"
        );
    }

    #[test]
    fn applying_a_tail_does_not_ask_for_another_one() {
        let (mut app, _) = started();
        assert_eq!(
            app.update(Msg::Bleats {
                tail: crate::lookout::tail::Tail::default()
            }),
            Effect::None
        );
    }

    /// `run_ui`'s coalesced read is armed before the freeze, so a read can
    /// still be in flight when `Msg::Frozen` lands.
    #[test]
    fn a_frozen_dashboard_ignores_a_bleats_tail_in_flight_at_the_freeze() {
        let (mut app, _) = started();
        let live_tail = crate::lookout::tail::Tail {
            lines: vec![crate::lookout::tail::TailLine::new(
                crate::lookout::tail::Stream::Out,
                "read before the freeze",
            )],
            ..Default::default()
        };
        app.update(Msg::Bleats {
            tail: live_tail.clone(),
        });
        assert_eq!(app.feed(), &live_tail, "a live dashboard takes the tail");

        app.update(Msg::Frozen {
            at_local: "2026-08-14 14:32:07".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });

        let in_flight_tail = crate::lookout::tail::Tail {
            lines: vec![crate::lookout::tail::TailLine::new(
                crate::lookout::tail::Stream::Out,
                "read after the freeze",
            )],
            ..Default::default()
        };
        assert_eq!(
            app.update(Msg::Bleats {
                tail: in_flight_tail
            }),
            Effect::None
        );
        assert_eq!(
            app.feed(),
            &live_tail,
            "the tail read after the freeze must not reach the rendered frame"
        );
    }
}
