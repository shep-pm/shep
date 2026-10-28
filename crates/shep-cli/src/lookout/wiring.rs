use std::time::Duration;

/// How often the uptime column is re-derived.
///
/// One second. Nothing on the wire changes on this tick: it exists so a
/// running sheep's UPTIME advances between the two-second polls instead of
/// stepping.
pub const HEARTBEAT: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests {
    use super::super::MIN_REDRAW;
    use super::super::ui_event_loop::run_ui;

    use std::time::{Duration, Instant};

    use ratatui::Terminal;

    use super::super::app::Msg;
    use tokio::sync::mpsc;

    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use crate::lookout::app::KeyPress;
    use futures_util::stream;
    use ratatui::backend::TestBackend;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::super::testing::*;

    /// Ordinary terminals deliver auto-repeat as Press events, so a held `j`
    /// is twenty to thirty moved selections a second. An uncoalesced
    /// `Effect::RefreshFeed` would put a synchronous 128 KiB read behind
    /// every one, on the task that owns the redraw.
    ///
    /// Not `start_paused`: `MIN_REDRAW` reads a real [`std::time::Instant`],
    /// which a virtual clock never advances.
    #[tokio::test]
    async fn a_burst_of_selection_moves_costs_one_read_and_not_one_per_key() {
        let (msg_tx, msg_rx) = mpsc::channel(64);
        let (poll_tx, _poll_rx) = mpsc::channel(4);
        let (request_tx, _request_rx) = mpsc::channel(2);
        let local = FakeLocal::default();
        let tails = Arc::clone(&local.tails);

        let at = Instant::now();
        msg_tx
            .send(Msg::Snapshot {
                rows: (0..8)
                    .map(|id| {
                        ProcessInfo::builder(id, format!("sheep-{id}"), ProcStatus::Online).build()
                    })
                    .collect(),
                at,
            })
            .await
            .unwrap();
        for _ in 0..20 {
            msg_tx.send(Msg::Key(KeyPress::SelectDown)).await.unwrap();
        }
        tokio::spawn(async move {
            // A nudge, not the quit: the redraw gate is read once per loop
            // iteration, right before the blocking receive, so real time
            // elapsing while the loop waits is only seen on the next one.
            // `Msg::Resize` wakes it after `MIN_REDRAW` has cleared.
            tokio::time::sleep(MIN_REDRAW * 3).await;
            let _ = msg_tx.send(Msg::Resize).await;
            tokio::time::sleep(MIN_REDRAW).await;
            let _ = msg_tx.send(Msg::Key(KeyPress::Quit)).await;
        });

        let app = read_only_app();
        let terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            run_ui(
                app,
                terminal,
                stream::empty(),
                msg_rx,
                poll_tx,
                request_tx,
                shared_test_paths(),
                local,
            ),
        )
        .await
        .expect("the loop left within five seconds");

        assert_eq!(
            tails.load(Ordering::Relaxed),
            1,
            "a snapshot and twenty selection moves must coalesce into one read"
        );
    }
}
