//! [`AnsiLines`], the filter between a pty's master and the line reader.

use core::pin::Pin;
use core::task::{Context, Poll, ready};

use std::io;

use tokio::io::{AsyncRead, ReadBuf};

/// Bytes read off the inner reader per poll.
const SCRATCH: usize = 4096;

/// Bytes of an OSC, DCS or similar string sequence kept before the filter
/// gives up on its terminator and returns to plain text.
///
/// Real titles and hyperlinks are far shorter; a stray introducer must not
/// swallow the rest of the log.
const STRING_LIMIT: usize = 4096;

/// CSI final bytes that move the cursor or erase: each ends the line being
/// written, since what follows overwrites or replaces it.
const LINE_BREAKING_FINALS: &[u8] = b"ABCDEFGHJKXdf`";

/// Where the filter is inside an escape sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// After `ESC`.
    Escape,
    /// After `ESC` and an intermediate byte, as in `ESC ( B`.
    EscapeIntermediate,
    /// Inside `ESC [`, up to its final byte.
    Csi,
    /// Inside an OSC, DCS, SOS, PM or APC string. Only OSC ends on `BEL`.
    Text {
        bell_ends: bool,
        seen: usize,
    },
    /// An `ESC` inside such a string, which ends it if `\` follows.
    TextEscape,
}

/// Reads another reader's bytes as plain text: escape sequences removed, line
/// ends normalised, and invalid UTF-8 replaced.
///
/// A sequence vanishes even when a read boundary falls inside it. A bare `\r`
/// and a CSI that moves the cursor or erases each end a line that has text,
/// and `\r\n` becomes `\n`. The output is valid UTF-8 for `LineReader`, with
/// `U+FFFD` for each invalid sequence.
pub(super) struct AnsiLines<R> {
    inner: R,
    state: State,
    /// A `\r` was last seen and not yet resolved by the byte after it.
    cr_pending: bool,
    /// The line being written has a visible byte.
    line_has_text: bool,
    /// Leading bytes of a character the last read cut short.
    carry: Vec<u8>,
    /// Filtered, valid UTF-8 not yet handed to the caller.
    ready: Vec<u8>,
    sent: usize,
    eof: bool,
}

impl<R> AnsiLines<R> {
    /// A filter over `inner`.
    pub(super) fn new(inner: R) -> Self {
        Self {
            inner,
            state: State::Ground,
            cr_pending: false,
            line_has_text: false,
            carry: Vec::new(),
            ready: Vec::new(),
            sent: 0,
            eof: false,
        }
    }

    /// Ends the line being written if it has text. A line with none is
    /// already ended.
    fn break_line(&mut self, text: &mut Vec<u8>) {
        if self.line_has_text {
            text.push(b'\n');
            self.line_has_text = false;
        }
    }

    /// Resolves a pending `\r` as a bare one.
    fn settle_cr(&mut self, text: &mut Vec<u8>) {
        if self.cr_pending {
            self.cr_pending = false;
            self.break_line(text);
        }
    }

    /// Feeds one byte through the state machine, appending plain bytes to
    /// `text`.
    fn step(&mut self, byte: u8, text: &mut Vec<u8>) {
        match self.state {
            State::Ground => self.ground(byte, text),
            State::Escape => match byte {
                b'[' => self.state = State::Csi,
                b']' => {
                    self.state = State::Text {
                        bell_ends: true,
                        seen: 0,
                    }
                }
                b'P' | b'X' | b'^' | b'_' => {
                    self.state = State::Text {
                        bell_ends: false,
                        seen: 0,
                    }
                }
                0x20..=0x2f => self.state = State::EscapeIntermediate,
                0x1b => {}
                0x30..=0x7e => self.state = State::Ground,
                _ => {
                    self.state = State::Ground;
                    self.ground(byte, text);
                }
            },
            State::EscapeIntermediate => match byte {
                0x20..=0x2f => {}
                0x30..=0x7e => self.state = State::Ground,
                _ => {
                    self.state = State::Ground;
                    self.ground(byte, text);
                }
            },
            State::Csi => match byte {
                0x20..=0x3f => {}
                0x40..=0x7e => {
                    self.state = State::Ground;
                    if LINE_BREAKING_FINALS.contains(&byte) {
                        self.settle_cr(text);
                        self.break_line(text);
                    }
                }
                0x1b => self.state = State::Escape,
                _ => {
                    self.state = State::Ground;
                    self.ground(byte, text);
                }
            },
            State::Text { bell_ends, seen } => match byte {
                0x07 if bell_ends => self.state = State::Ground,
                0x1b => self.state = State::TextEscape,
                _ if seen >= STRING_LIMIT => {
                    self.state = State::Ground;
                    self.ground(byte, text);
                }
                _ => {
                    self.state = State::Text {
                        bell_ends,
                        seen: seen + 1,
                    }
                }
            },
            State::TextEscape => match byte {
                b'\\' => self.state = State::Ground,
                _ => {
                    self.state = State::Escape;
                    self.step(byte, text);
                }
            },
        }
    }

    /// A byte outside any escape sequence.
    fn ground(&mut self, byte: u8, text: &mut Vec<u8>) {
        match byte {
            b'\r' => {
                self.settle_cr(text);
                self.cr_pending = true;
            }
            b'\n' => {
                self.cr_pending = false;
                text.push(b'\n');
                self.line_has_text = false;
            }
            0x1b => {
                self.settle_cr(text);
                self.state = State::Escape;
            }
            // The other C0 controls and DEL draw nothing in a log.
            0x00..=0x08 | 0x0b..=0x0c | 0x0e..=0x1a | 0x1c..=0x1f | 0x7f => {}
            _ => {
                self.settle_cr(text);
                text.push(byte);
                if byte != b'\t' && byte != b' ' {
                    self.line_has_text = true;
                }
            }
        }
    }

    /// Moves `text` into `ready`, holding back a character cut short at its
    /// end unless `last`, and replacing each invalid sequence.
    fn decode(&mut self, text: &[u8], last: bool) {
        let mut bytes = core::mem::take(&mut self.carry);
        bytes.extend_from_slice(text);
        let mut rest = bytes.as_slice();
        loop {
            match core::str::from_utf8(rest) {
                Ok(valid) => {
                    self.ready.extend_from_slice(valid.as_bytes());
                    return;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    self.ready.extend_from_slice(valid);
                    match error.error_len() {
                        Some(skip) => {
                            self.ready.extend_from_slice("\u{fffd}".as_bytes());
                            rest = &after[skip..];
                        }
                        None if last => {
                            self.ready.extend_from_slice("\u{fffd}".as_bytes());
                            return;
                        }
                        None => {
                            self.carry = after.to_vec();
                            return;
                        }
                    }
                }
            }
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for AnsiLines<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        while this.sent == this.ready.len() && !this.eof {
            this.ready.clear();
            this.sent = 0;
            let mut scratch = [0u8; SCRATCH];
            let mut read = ReadBuf::new(&mut scratch);
            ready!(Pin::new(&mut this.inner).poll_read(cx, &mut read))?;
            let mut text = Vec::with_capacity(read.filled().len());
            if read.filled().is_empty() {
                this.eof = true;
                this.settle_cr(&mut text);
            }
            for &byte in read.filled() {
                this.step(byte, &mut text);
            }
            let last = this.eof;
            this.decode(&text, last);
        }
        let pending = &this.ready[this.sent..];
        let count = pending.len().min(buf.remaining());
        buf.put_slice(&pending[..count]);
        this.sent += count;
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;

    use tokio::io::AsyncReadExt as _;

    /// Hands out one prepared chunk per read, so a test chooses where every
    /// read boundary falls.
    struct Chunks(VecDeque<Vec<u8>>);

    impl AsyncRead for Chunks {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if let Some(chunk) = self.0.pop_front() {
                buf.put_slice(&chunk);
            }
            Poll::Ready(Ok(()))
        }
    }

    async fn filtered(chunks: &[&[u8]]) -> String {
        let reader = Chunks(chunks.iter().map(|chunk| chunk.to_vec()).collect());
        let mut text = String::new();
        AnsiLines::new(reader)
            .read_to_string(&mut text)
            .await
            .expect("the filter's output must be valid UTF-8");
        text
    }

    #[tokio::test]
    async fn a_sequence_split_across_two_reads_is_removed_whole() {
        assert_eq!(
            filtered(&[b"\x1b[3", b"1mred\x1b[0", b"m\n"]).await,
            "red\n"
        );
        assert_eq!(filtered(&[b"a\x1b]0;ti", b"tle\x07b\n"]).await, "ab\n");
        assert_eq!(
            filtered(&[b"a\x1b]8;;http://x\x1b", b"\\link\n"]).await,
            "alink\n"
        );
    }

    #[tokio::test]
    async fn a_cursor_move_and_a_bare_carriage_return_end_a_line() {
        assert_eq!(filtered(&[b"10%\r20%\r30%\n"]).await, "10%\n20%\n30%\n");
        assert_eq!(filtered(&[b"one\x1b[2;1Htwo\n"]).await, "one\ntwo\n");
        assert_eq!(filtered(&[b"gone\x1b[Kthere\n"]).await, "gone\nthere\n");
    }

    #[tokio::test]
    async fn a_multibyte_character_split_across_two_reads_decodes_whole() {
        assert_eq!(
            filtered(&[b"na\xc3", b"\xafve \xe2\x82", b"\xac\n"]).await,
            "naïve €\n"
        );
    }

    #[tokio::test]
    async fn a_carriage_return_before_a_newline_is_just_the_newline() {
        assert_eq!(filtered(&[b"a\r", b"\nb\r\n\r\n"]).await, "a\nb\n\n");
    }

    #[tokio::test]
    async fn a_redraw_leaves_no_blank_line_between_its_parts() {
        assert_eq!(filtered(&[b"a\n\r\x1b[Kb\n"]).await, "a\nb\n");
    }

    #[tokio::test]
    async fn invalid_bytes_and_a_cut_short_end_become_replacement_characters() {
        assert_eq!(filtered(&[b"a\xffb\xc3"]).await, "a\u{fffd}b\u{fffd}");
    }

    #[tokio::test]
    async fn colour_and_other_escapes_leave_only_the_text() {
        assert_eq!(
            filtered(&[b"\x1b[1;32mok\x1b[0m \x1b(B\x1b=done\x1bPq\x1b\\\n"]).await,
            "ok done\n"
        );
    }

    #[tokio::test]
    async fn an_unterminated_string_sequence_gives_the_log_back() {
        let mut input = b"\x1b]0;".to_vec();
        input.extend(core::iter::repeat_n(b'x', STRING_LIMIT + 8));
        input.extend_from_slice(b"\nlater\n");
        let chunks: Vec<&[u8]> = input.chunks(512).collect();
        assert!(filtered(&chunks).await.ends_with("later\n"));
    }
}
