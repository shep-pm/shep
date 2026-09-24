//! The exit codes a dog shares with `shep` itself, from spec §9.
//!
//! `shep dogs` shows a dog's last exit code, so a dog stopping on one of
//! these causes exits on shep's number for it. The CLI's own exit codes are
//! defined from these, so the two cannot drift. Codes only the CLI can reach,
//! such as a flock emptying, stay there.

/// An error with no more specific code.
pub const FAILURE: u8 = 1;

/// Bad arguments: clap's own convention, and so any other parser's.
pub const USAGE: u8 = 2;

/// A selector matched no registered sheep.
pub const NOT_FOUND: u8 = 3;

/// A configuration failed validation. For a dog, its own section.
pub const INVALID_CONFIG: u8 = 4;

/// No shepherd answered.
pub const DAEMON_UNREACHABLE: u8 = 5;

/// The shepherd refused the handshake on protocol-version skew.
pub const PROTOCOL_MISMATCH: u8 = 6;

/// The shepherd could not spawn a sheep.
pub const SPAWN_FAILED: u8 = 7;

/// A request outlived its deadline.
pub const DEADLINE_EXCEEDED: u8 = 8;

/// An unexpected failure, including a shepherd answering out of turn.
pub const INTERNAL: u8 = 9;

/// The shepherd understood the handshake but does not implement the
/// request: a newer shepherd is the remedy.
pub const UNSUPPORTED: u8 = 13;

#[cfg(test)]
mod tests {
    /// fails if two causes ever share a number, or one takes success's.
    #[test]
    fn every_code_is_distinct_and_none_is_success() {
        let codes = [
            super::FAILURE,
            super::USAGE,
            super::NOT_FOUND,
            super::INVALID_CONFIG,
            super::DAEMON_UNREACHABLE,
            super::PROTOCOL_MISMATCH,
            super::SPAWN_FAILED,
            super::DEADLINE_EXCEEDED,
            super::INTERNAL,
            super::UNSUPPORTED,
        ];
        let unique: std::collections::BTreeSet<u8> = codes.into_iter().collect();
        assert_eq!(unique.len(), codes.len(), "{codes:?}");
        assert!(!unique.contains(&0));
    }
}
