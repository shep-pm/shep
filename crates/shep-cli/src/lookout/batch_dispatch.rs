use super::app::{Msg, Sent};
use tokio::sync::mpsc;

/// Delivers an [`Effect::SendAll`](crate::lookout::app::Effect::SendAll) batch to the link task, in order, without
/// blocking [`run_ui`](crate::lookout::ui_event_loop::run_ui).
///
/// One task, one cloned sender, one `link::send` per entry, awaited in sequence:
/// a channel deeper than the batch never stalls the screen, and a channel
/// merely full is a wait rather than a loss. `link::send` only fails once the
/// channel is closed, so the entry it hands back there is the first
/// casualty of a shepherd that is gone; every entry after it would fail
/// the same way, so this stops rather than piling up identical reports.
pub(super) async fn send_batch(requests: mpsc::Sender<Sent>, batch: Vec<Sent>) -> Msg {
    for sent in batch {
        if let Err(mpsc::error::SendError(sent)) = requests.send(sent).await {
            return Msg::BatchSent { unsent: Some(sent) };
        }
    }
    Msg::BatchSent { unsent: None }
}

/// Renders [`crate::commands::dogs::EnableRefusal`] for the settings screen's
/// own notice line.
///
/// `name` is this function's own argument, since `EnableRefusal::UnknownDog`
/// does not carry one, so the wrong name never lands in a sentence about
/// someone else's dog.
pub(super) fn enable_refusal_message(
    err: &crate::commands::dogs::EnableRefusal,
    name: &str,
) -> String {
    use crate::commands::dogs::EnableRefusal;
    match err {
        EnableRefusal::Config(err) => err.to_string(),
        EnableRefusal::UnknownDog { .. } => {
            format!("{name} is not a dog shep knows about")
        }
    }
}

#[cfg(test)]
mod tests {

    use super::super::app::{Msg, Sent};
    use tokio::sync::mpsc;

    use super::*;

    /// The regression this exists for: a batch bigger than the request
    /// channel used to drop its tail, because the old loop's `try_send`
    /// had no `.await` point to let the link task drain it through. Five
    /// entries into a channel of two, drained slower than they are filed,
    /// must all land, in the order they were filed.
    #[tokio::test]
    async fn send_batch_delivers_every_entry_past_a_full_channel() {
        let (request_tx, mut request_rx) = mpsc::channel(2);
        let batch: Vec<Sent> = (0..5)
            .map(|n| Sent::SheepConfig {
                name: format!("sheep-{n}"),
            })
            .collect();
        let expected = batch.clone();

        let handle = tokio::spawn(send_batch(request_tx, batch));

        let mut received = Vec::new();
        for _ in 0..5 {
            // No delay needed: the channel holds two, so the third
            // `recv` already has to wait on `send_batch` making room,
            // which is the condition under test.
            received.push(
                request_rx
                    .recv()
                    .await
                    .expect("the batch is still arriving"),
            );
        }

        assert_eq!(
            received, expected,
            "every entry landed, in the order it was filed"
        );
        let msg = handle.await.expect("send_batch does not panic");
        assert!(matches!(msg, Msg::BatchSent { unsent: None }), "{msg:?}");
    }

    /// The channel closing mid-batch, rather than merely filling, is the
    /// one case `link::send` actually fails: the shepherd going away must still
    /// report, not be swallowed by the fix for the full case above.
    #[tokio::test]
    async fn send_batch_reports_the_first_casualty_once_the_channel_is_closed() {
        let (request_tx, request_rx) = mpsc::channel(2);
        drop(request_rx);
        let batch = vec![Sent::SheepConfig {
            name: "sheep-0".to_string(),
        }];

        let Msg::BatchSent { unsent } = send_batch(request_tx, batch).await else {
            panic!("wanted a BatchSent");
        };
        assert_eq!(
            unsent,
            Some(Sent::SheepConfig {
                name: "sheep-0".to_string()
            })
        );
    }
}
