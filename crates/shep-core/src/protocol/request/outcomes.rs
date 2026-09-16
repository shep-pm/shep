//! Per-row outcome and reply pairs for the action, signal, and line verbs.

use serde::{Deserialize, Serialize};

// Named by intra-doc links and by nothing rustc compiles, so the
// import is behind `cfg(doc)` rather than flagged unused.
#[cfg(doc)]
use super::ProcessInfo;

/// What happened when the daemon tried to deliver one sheep's triggered
/// action.
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActionOutcome {
    /// The app answered on the shepherd channel.
    Replied {
        /// The reply body, exactly as the app sent it.
        body: String,
    },
    /// The sheep had no reachable shepherd channel for the daemon to
    /// deliver the action over.
    NoChannel,
    /// The sheep is a reload drainee, mid-swap and on its way out, so the
    /// daemon skipped it rather than deliver the action to a process already
    /// being replaced.
    Skipped,
    /// The daemon delivered the action, but no reply arrived before the
    /// app's configured action timeout elapsed.
    TimedOut,
}

/// One matched sheep's row in a `Trigger` reply.
///
/// Not a [`ProcessInfo`]: a reply body has nowhere to live on one.
/// [`Self::outcome`] is per-row, since the selector grammar makes a mixed
/// flock the normal case.
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionReply {
    /// The sheep's stable id.
    pub id: u32,
    /// The sheep's name.
    pub name: String,
    /// What happened when the daemon tried to deliver the action.
    pub outcome: ActionOutcome,
}

/// What happened when the shepherd tried to deliver one signal.
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SignalOutcome {
    /// The kernel accepted the signal for this sheep's pid.
    ///
    /// Says the signal was delivered, not that the app did anything with it.
    /// A signal the app blocks or ignores is `Delivered` too.
    Delivered,
    /// The sheep is registered but has no live process to signal: stopped,
    /// errored, or waiting out a restart backoff.
    NotRunning,
    /// The kernel refused the delivery; carries its reason (`ESRCH` for a
    /// process reaped between the lookup and the syscall, `EPERM` for one this
    /// daemon may not signal).
    Failed {
        /// The refusal, as the OS worded it.
        reason: String,
    },
}

/// One matched sheep's row in a `Signal` reply.
///
/// Per-row like [`ActionReply`]: the selector grammar makes a mixed flock
/// the normal case.
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalReply {
    /// The sheep's stable id.
    pub id: u32,
    /// The sheep's name.
    pub name: String,
    /// What happened when the shepherd tried to deliver the signal.
    pub outcome: SignalOutcome,
}

/// What happened when the shepherd tried to write one line to a sheep's stdin.
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LineOutcome {
    /// The line was written to the pipe and flushed.
    ///
    /// Says the bytes left the shepherd, not that the app read them. A pipe
    /// holds 64 KiB before it blocks.
    Sent,
    /// The sheep has no stdin pipe: its config does not set `stdin = true`, or
    /// it is not running.
    ///
    /// One outcome for two causes: both answer "there is no pipe here".
    NoStdin,
    /// The shepherd had a pipe and did not confirm a write to it; carries
    /// why.
    ///
    /// Three shapes reach it: the write failed (the far end is gone), the
    /// line found the sheep's queue already full, or the write did not
    /// finish inside the shepherd's own bound. The reason names which.
    ///
    /// A timed-out write is not a promise the line was never written: the
    /// bytes may be part-written into a pipe the app is not draining, and
    /// land in full the moment it drains. A line still queued behind that one
    /// is dropped once its caller gives up, so treat a retry as a second
    /// command.
    NotWritten {
        /// What went wrong, in plain English.
        reason: String,
    },
}

/// One matched sheep's row in a `SendLine` reply.
///
/// Per-row like [`ActionReply`] and [`SignalReply`].
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineReply {
    /// The sheep's stable id.
    pub id: u32,
    /// The sheep's name.
    pub name: String,
    /// What happened.
    pub outcome: LineOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_outcome_kinds_serialize_snake_case_and_round_trip() {
        // The shared snapshots exercise only `Replied`, the struct-shaped
        // variant.
        let cases = [
            (
                ActionOutcome::Replied {
                    body: "pong".to_string(),
                },
                r#"{"kind":"replied","body":"pong"}"#,
            ),
            (ActionOutcome::NoChannel, r#"{"kind":"no_channel"}"#),
            (ActionOutcome::Skipped, r#"{"kind":"skipped"}"#),
            (ActionOutcome::TimedOut, r#"{"kind":"timed_out"}"#),
        ];
        for (outcome, wire) in cases {
            assert_eq!(
                serde_json::to_string(&outcome).unwrap(),
                wire,
                "{outcome:?}"
            );
            assert_eq!(
                serde_json::from_str::<ActionOutcome>(wire).unwrap(),
                outcome
            );
        }
    }
}
