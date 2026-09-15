//! The lamb walk: what the shepherd found under one sheep, and when it was
//! read.

use super::*;

impl App {
    /// Records one lamb reading. Always [`Effect::None`]: a reducer that
    /// answered a reading with another request would spin the UI task.
    pub(super) fn on_lambs(&mut self, id: u32, result: Result<Response, RequestError>) -> Effect {
        // Armed before the freeze could land: a reading reaching the frame now
        // would be newer than the banner over it.
        if matches!(self.link, Link::Lost { .. }) {
            return Effect::None;
        }
        let walk = match result {
            Ok(Response::Described(rows)) => rows
                .into_iter()
                .find(|info| info.id == id)
                .map_or(LambWalk::Failed, |info| {
                    info.lambs.map_or(LambWalk::NotWalked, LambWalk::Walked)
                }),
            // Neither an `Err` nor an unrecognised reply is an empty walk:
            // reporting one would say "none found" about nothing read.
            _ => LambWalk::Failed,
        };
        self.lambs = Some(LambReading {
            id,
            at: self.now,
            walk,
        });
        Effect::None
    }

    /// The lamb reading for sheep `id`, with its age in milliseconds.
    ///
    /// `None` when there is no reading, or when the one there is was taken for
    /// a different sheep. The age stops when the dashboard freezes.
    #[must_use]
    pub fn lambs_for(&self, id: u32) -> Option<(&LambWalk, u64)> {
        let reading = self.lambs.as_ref().filter(|reading| reading.id == id)?;
        Some((
            &reading.walk,
            millis(self.now.saturating_duration_since(reading.at)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::testing::*;

    /// `None` means the reply did not walk, `Some(vec![])` means it walked and
    /// found nothing.
    #[test]
    fn a_lamb_reply_records_which_of_the_three_states_it_saw() {
        let (mut app, t0) = started();
        let walked = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .lambs(Some(vec![Lamb::new(48_220, "node")]))
            .build();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![walked])),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::Walked(lambs), _)) if lambs.len() == 1));

        let empty = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .lambs(Some(Vec::new()))
            .build();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![empty])),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::Walked(lambs), _)) if lambs.is_empty()));

        let unwalked = ProcessInfo::builder(1, "web", ProcStatus::Stopped).build();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![unwalked])),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::NotWalked, _))));
        let _ = t0;
    }

    #[test]
    fn a_reading_for_another_sheep_reads_as_not_read_yet() {
        let (mut app, _t0) = started();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![
                ProcessInfo::builder(1, "web", ProcStatus::Online)
                    .lambs(Some(vec![Lamb::new(48_220, "node")]))
                    .build(),
            ])),
        });
        assert!(app.lambs_for(1).is_some());
        assert!(app.lambs_for(2).is_none(), "not this sheep's reading");
    }

    #[test]
    fn a_failed_lamb_fetch_says_so_in_the_pane_and_raises_no_notice() {
        let (mut app, _t0) = started();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Err(RequestError::Closed),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::Failed, _))));
        assert!(app.notice().is_none(), "no notice for a decoration");
    }

    #[test]
    fn an_unrecognised_lamb_reply_is_a_failure_and_not_an_empty_walk() {
        let (mut app, _t0) = started();
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Pong),
        });
        assert!(matches!(app.lambs_for(1), Some((LambWalk::Failed, _))));
    }

    /// The fetch is armed before the freeze can land, so a reply can still be
    /// in flight when `Msg::Frozen` arrives.
    #[test]
    fn a_lamb_reply_after_a_freeze_is_refused() {
        let (mut app, _t0) = started();
        app.update(Msg::Frozen {
            at_local: "2026-08-16 09:00:00".to_string(),
            why: fixtures::FROZEN_WHY.to_string(),
        });
        app.update(Msg::Replied {
            sent: Sent::Lambs { id: 1 },
            result: Ok(Response::Described(vec![
                ProcessInfo::builder(1, "web", ProcStatus::Online)
                    .lambs(Some(vec![Lamb::new(48_220, "node")]))
                    .build(),
            ])),
        });
        assert!(
            app.lambs_for(1).is_none(),
            "the frozen frame learned nothing"
        );
    }
}
