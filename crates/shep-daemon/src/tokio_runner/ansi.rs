//! [`AnsiLines`], the filter between a pty's master and the line reader.
//!
//! A full-screen sheep repaints by moving its cursor up and printing the
//! frame again. The filter keeps the rows it wrote, so a repaint writes only
//! the rows that changed.

use core::pin::Pin;
use core::task::{Context, Poll, ready};

use std::collections::VecDeque;
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
const LINE_BREAKING_FINALS: &[u8] = b"ABCDEFGHJKXadef`";

/// Rows the filter keeps, so a repaint can be compared against what it
/// replaces.
///
/// A pty opens at [`super::pty::PTY_ROWS`] 50 and `shep attach` resizes it to
/// the operator's terminal. Past this the oldest rows go.
const SCREEN_ROWS: usize = 256;

/// Bytes one kept row may hold. [`SCRATCH`] is what one read carries.
const SCREEN_ROW_BYTES: usize = SCRATCH;

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
/// A sequence vanishes even when a read boundary falls inside it. A run of
/// `\r` counts as one, and with a CSI that moves the cursor or erases ends a
/// line that has text; `\r\n` becomes `\n`. The output is valid UTF-8 for
/// `LineReader`, with `U+FFFD` for each invalid sequence.
pub(super) struct AnsiLines<R> {
    inner: R,
    state: State,
    /// A `\r` was last seen and not yet resolved by the byte after it. A run
    /// of them counts as one.
    cr_pending: bool,
    /// The line being written has a visible byte.
    line_has_text: bool,
    /// Bytes of the row being written, until it ends.
    row: Vec<u8>,
    /// Rows written so far, oldest first, capped at [`SCREEN_ROWS`]. `None`
    /// is one too long to keep, so a repaint against it is written.
    screen: VecDeque<Option<Vec<u8>>>,
    /// Rows ahead that replace the tail of [`Self::screen`], from a cursor-up.
    /// Never past it unless [`Self::blind`] holds.
    replacing: usize,
    /// The window reaches past what is kept, so it names nothing to compare
    /// against. Every row in it is written.
    blind: bool,
    /// Digits of the CSI's first parameter, saturating.
    csi_param: u32,
    /// That parameter ended at `;`, so the digits after it are another's.
    csi_closed: bool,
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
            row: Vec::new(),
            screen: VecDeque::new(),
            replacing: 0,
            blind: false,
            csi_param: 0,
            csi_closed: false,
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
            self.end_row(text);
        }
    }

    /// Ends the row being written. A row that repeats the one it replaces is
    /// dropped. Every other row is written and kept.
    fn end_row(&mut self, text: &mut Vec<u8>) {
        let row = core::mem::take(&mut self.row);
        self.line_has_text = false;
        // `cursor_up` keeps `replacing` within `screen.len()` unless the
        // window is blind, so the subtraction below cannot underflow.
        let target = match self.replacing {
            0 => None,
            _ if self.blind => None,
            n => Some(self.screen.len() - n),
        };
        let repeat = match target {
            Some(i) => self.screen[i].as_deref() == Some(row.as_slice()),
            None => false,
        };
        if !repeat {
            text.extend_from_slice(&row);
            text.push(b'\n');
        }
        // A row too long to keep still holds a slot, as `None`. A repaint
        // against it is then written.
        let kept = (row.len() <= SCREEN_ROW_BYTES).then_some(row);
        match (target, repeat) {
            (Some(i), false) => self.screen[i] = kept,
            (None, false) => {
                if self.screen.len() == SCREEN_ROWS {
                    self.screen.pop_front();
                }
                self.screen.push_back(kept);
            }
            (_, true) => {}
        }
        self.replacing = self.replacing.saturating_sub(1);
    }

    /// Resolves a pending `\r` as a bare one.
    fn settle_cr(&mut self, text: &mut Vec<u8>) {
        if self.cr_pending {
            self.cr_pending = false;
            self.break_line(text);
        }
    }

    /// Marks the rows ahead as replacing the last `rows` kept rows.
    ///
    /// A window past what is kept is blind: it names rows the filter never
    /// saw. Every row in it is written. The bound at [`SCREEN_ROWS`] stops
    /// one stray cursor-up from blinding the filter for good.
    fn cursor_up(&mut self, rows: usize) {
        let total = self.replacing.saturating_add(rows);
        self.blind = total > self.screen.len();
        self.replacing = total.min(SCREEN_ROWS);
    }

    /// Feeds one byte through the state machine, appending plain bytes to
    /// `text`.
    fn step(&mut self, byte: u8, text: &mut Vec<u8>) {
        match self.state {
            State::Ground => self.ground(byte, text),
            State::Escape => match byte {
                b'[' => {
                    self.state = State::Csi;
                    self.csi_param = 0;
                    self.csi_closed = false;
                }
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
                0x20..=0x3f => match byte {
                    b'0'..=b'9' if !self.csi_closed => {
                        self.csi_param = self
                            .csi_param
                            .saturating_mul(10)
                            .saturating_add(u32::from(byte - b'0'));
                    }
                    b';' => self.csi_closed = true,
                    _ => {}
                },
                0x40..=0x7e => {
                    self.state = State::Ground;
                    let up = byte == b'A';
                    // ECMA-48 reads an omitted or zero parameter as one.
                    let rows = usize::try_from(self.csi_param.max(1)).unwrap_or(usize::MAX);
                    if LINE_BREAKING_FINALS.contains(&byte) {
                        self.settle_cr(text);
                        self.break_line(text);
                    }
                    if up {
                        self.cursor_up(rows);
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
            b'\r' => self.cr_pending = true,
            b'\n' => {
                self.cr_pending = false;
                self.end_row(text);
            }
            0x1b => {
                self.settle_cr(text);
                self.state = State::Escape;
            }
            // The other C0 controls and DEL draw nothing in a log.
            0x00..=0x08 | 0x0b..=0x0c | 0x0e..=0x1a | 0x1c..=0x1f | 0x7f => {}
            _ => {
                self.settle_cr(text);
                self.row.push(byte);
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
                // A last row with no line end goes out as it stands: no
                // newline is owed to a stream that has none left.
                text.extend_from_slice(&this.row);
                this.row.clear();
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
        // HPR and VPR move the cursor relative to it, like `C` and `B`.
        assert_eq!(filtered(&[b"a\x1b[1ab\x1b[1ec\n"]).await, "a\nb\nc\n");
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
    async fn a_doubled_carriage_return_before_a_newline_is_just_the_newline() {
        // A tty with a full output queue writes `\r\r\n` where the program
        // wrote `\n`. A terminal draws that as one line break.
        assert_eq!(filtered(&[b"a\r\r\nb\n"]).await, "a\nb\n");
        assert_eq!(filtered(&[b"a\r", b"\r\nb\n"]).await, "a\nb\n");
        assert_eq!(filtered(&[b"a\r\r\r\nb\n"]).await, "a\nb\n");
    }

    #[tokio::test]
    async fn a_doubled_carriage_return_before_text_still_ends_the_line() {
        assert_eq!(filtered(&[b"a\r\rb\n"]).await, "a\nb\n");
        assert_eq!(filtered(&[b"a\r", b"\rb\n"]).await, "a\nb\n");
    }

    #[tokio::test]
    async fn a_redraw_leaves_no_blank_line_between_its_parts() {
        assert_eq!(filtered(&[b"a\n\r\x1b[Kb\n"]).await, "a\nb\n");
    }

    #[tokio::test]
    async fn a_repainted_frame_identical_to_the_last_writes_nothing() {
        assert_eq!(
            filtered(&[b"one\ntwo\n\x1b[2Aone\ntwo\n"]).await,
            "one\ntwo\n"
        );
    }

    #[tokio::test]
    async fn a_repainted_frame_writes_only_its_changed_rows() {
        assert_eq!(
            filtered(&[b"one\ntwo\n\x1b[2Aone\nTWO\n"]).await,
            "one\ntwo\nTWO\n"
        );
    }

    #[tokio::test]
    async fn a_frame_longer_than_the_one_it_replaces_writes_its_extra_rows() {
        assert_eq!(
            filtered(&[b"one\ntwo\n\x1b[2Aone\ntwo\nthree\n"]).await,
            "one\ntwo\nthree\n"
        );
    }

    #[tokio::test]
    async fn a_repainted_frame_split_across_reads_is_still_recognised() {
        assert_eq!(
            filtered(&[b"one\ntwo\n\x1b[2", b"Aone\ntw", b"o\n"]).await,
            "one\ntwo\n"
        );
    }

    #[tokio::test]
    async fn a_cursor_up_past_the_kept_rows_writes_every_row() {
        assert_eq!(
            filtered(&[b"one\ntwo\n\x1b[9Aone\ntwo\n"]).await,
            "one\ntwo\none\ntwo\n"
        );
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
