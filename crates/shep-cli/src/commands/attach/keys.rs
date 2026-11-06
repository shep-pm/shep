//! Raw stdin bytes into the bytes a `PtyInput` carries.

use super::DETACH_KEY;

/// What one read of stdin comes to.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Typed {
    /// The bytes to send, as read.
    pub bytes: Vec<u8>,
    /// Whether the detach key was among them.
    pub detach: bool,
}

/// Turns reads of stdin into [`Typed`], holding nothing between reads.
///
/// A read that stops inside a multibyte character is sent as it stands: the
/// sheep's terminal is a byte stream, and a byte that starts no character is
/// what the operator's terminal received.
#[derive(Debug)]
pub(super) struct Keys;

impl Keys {
    /// The bytes `read` adds, and whether the detach key came with it.
    ///
    /// Nothing at or after the detach key is kept.
    #[must_use]
    pub fn push(&self, read: &[u8]) -> Typed {
        let (head, detach) = match read.iter().position(|byte| *byte == DETACH_KEY) {
            Some(at) => (&read[..at], true),
            None => (read, false),
        };
        Typed {
            bytes: head.to_vec(),
            detach,
        }
    }
}
