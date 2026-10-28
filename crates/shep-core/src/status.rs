//! Process lifecycle status

use core::fmt;

use serde::{Deserialize, Serialize};

/// Lifecycle state of a sheep (one managed process)
///
/// The serialized strings are the wire contract; `waiting-restart` means a
/// backoff or restart delay is pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ProcStatus {
    /// Spawned, not yet ready
    Starting,
    /// Running and (if configured) ready
    Online,
    /// This instance is going away and is not a restart target
    ///
    /// Set by whichever step marks the drainee before its replacement takes
    /// the slot. `SpawnNew` for an overlap reload, `DrainOld` for a serial
    /// one. The old and new instance never both count as running. An
    /// operator's `stop` leaves a sheep `Online` through its whole kill
    /// ladder. A scheduled restart must reject a sheep in this status.
    Stopping,
    /// Cleanly stopped; not scheduled to run
    Stopped,
    /// Restart budget exhausted or spawn failed
    Errored,
    /// Restart pending after a backoff or configured delay
    WaitingRestart,
    /// A status this build has not been taught.
    ///
    /// Only ever produced by decoding: an unrecognized string falls through
    /// to this variant via `#[serde(other)]` instead of failing the whole
    /// listing. Nothing constructs one to send, which is a call-site
    /// invariant rather than a type-level one: `#[serde(other)]` governs
    /// decoding only, so serializing this would emit `"unrecognized"`.
    #[serde(other)]
    Unrecognized,
}

impl ProcStatus {
    /// The status word, exactly as [`fmt::Display`] and the JSON encoding
    /// spell it.
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Online => "online",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Errored => "errored",
            Self::WaitingRestart => "waiting-restart",
            Self::Unrecognized => "unrecognized",
        }
    }
}

/// The status word, for a caller putting it in a cell or a payload without
/// going through [`fmt::Display`] and an allocation. By value, since
/// `ProcStatus` is `Copy`.
impl From<ProcStatus> for &'static str {
    fn from(source: ProcStatus) -> Self {
        source.as_str()
    }
}

impl fmt::Display for ProcStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_strings_are_stable() {
        let cases = [
            (ProcStatus::Starting, "\"starting\""),
            (ProcStatus::Online, "\"online\""),
            (ProcStatus::Stopping, "\"stopping\""),
            (ProcStatus::Stopped, "\"stopped\""),
            (ProcStatus::Errored, "\"errored\""),
            (ProcStatus::WaitingRestart, "\"waiting-restart\""),
        ];
        for (status, json) in cases {
            assert_eq!(serde_json::to_string(&status).unwrap(), json);
            assert_eq!(serde_json::from_str::<ProcStatus>(json).unwrap(), status);
            assert_eq!(format!("\"{status}\""), json);
        }
    }

    /// A status a newer shepherd adds must decode, not fail the listing it
    /// rides in.
    #[test]
    fn an_unknown_status_decodes_as_unrecognized() {
        for json in [r#""invented_next_year""#, r#""unrecognized""#] {
            assert_eq!(
                serde_json::from_str::<ProcStatus>(json).unwrap(),
                ProcStatus::Unrecognized
            );
        }
        assert_eq!(ProcStatus::Unrecognized.to_string(), "unrecognized");
    }
}
