//! Raw stdin bytes into the text a `PtyInput` carries.

use super::DETACH_KEY;

/// What one read of stdin comes to.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Typed {
    /// The keys to send, as typed.
    pub text: String,
    /// Whether the detach key was among them.
    pub detach: bool,
}

/// Turns reads of stdin into [`Typed`], holding back the start of a
/// character a read ended inside.
#[derive(Debug, Default)]
pub(super) struct Keys {
    held: Vec<u8>,
}

impl Keys {
    /// The text `bytes` adds, and whether the detach key came with it.
    ///
    /// Nothing at or after the detach key is kept. A byte that is not UTF-8
    /// becomes U+FFFD, since the wire carries text.
    pub fn push(&mut self, bytes: &[u8]) -> Typed {
        let (head, detach) = match bytes.iter().position(|byte| *byte == DETACH_KEY) {
            Some(at) => (&bytes[..at], true),
            None => (bytes, false),
        };
        self.held.extend_from_slice(head);
        let held = std::mem::take(&mut self.held);
        let mut text = String::new();
        let mut rest = held.as_slice();
        while !rest.is_empty() {
            match core::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    break;
                }
                Err(err) => {
                    let (valid, after) = rest.split_at(err.valid_up_to());
                    text.push_str(&String::from_utf8_lossy(valid));
                    match err.error_len() {
                        Some(bad) => {
                            text.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[bad..];
                        }
                        None => {
                            if !detach {
                                self.held = after.to_vec();
                            }
                            break;
                        }
                    }
                }
            }
        }
        Typed { text, detach }
    }
}
