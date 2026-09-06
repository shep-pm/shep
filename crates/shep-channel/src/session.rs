//! Reading and writing one newline-delimited JSON message.
//!
//! Generic over `BufRead` and `Write` rather than over the transport,
//! because that is what lets these tests run on a platform where the real
//! transport cannot be constructed without a live shepherd.

use std::io::{BufRead, Write};

use crate::{ChannelError, ChildMessage, ShepherdMessage};

/// Reads one message. `Ok(None)` is end of stream.
pub(crate) fn read_message<R: BufRead>(
    reader: &mut R,
) -> Result<Option<ShepherdMessage>, ChannelError> {
    let mut line = Vec::new();
    if reader
        .read_until(b'\n', &mut line)
        .map_err(ChannelError::Io)?
        == 0
    {
        return Ok(None);
    }
    // Bytes then decode, rather than `read_line`, because of how the
    // failure has to be classified. `read_line` reports a line that is not
    // UTF-8 as `io::ErrorKind::InvalidData`, which lands here as
    // `ChannelError::Io` -- and `Channel::recv` documents `Io` as the
    // transport failing, so a caller that ends its loop on `Io` would stop
    // on one garbage byte. A frame that is not UTF-8 is a bad frame, so it
    // is `Malformed` and the next call resumes at the following line, which
    // is the same bargain `a_malformed_line_is_recoverable` pins for a line
    // that is valid UTF-8 and not valid JSON.
    let text =
        core::str::from_utf8(&line).map_err(|error| ChannelError::Malformed(error.to_string()))?;
    // Belt and braces, not load-bearing: serde_json already skips a
    // trailing `\r`/`\n` as JSON whitespace before parsing. Kept explicit
    // so a bare line does not quietly depend on that.
    let trimmed = text.trim_end_matches(['\n', '\r']);
    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(|error| ChannelError::Malformed(error.to_string()))
}

/// Writes one message and its newline, then flushes.
pub(crate) fn write_message<W: Write>(
    writer: &mut W,
    message: &ChildMessage,
) -> Result<(), ChannelError> {
    let mut line =
        serde_json::to_vec(message).map_err(|error| ChannelError::Malformed(error.to_string()))?;
    line.push(b'\n');
    writer.write_all(&line).map_err(ChannelError::Io)?;
    writer.flush().map_err(ChannelError::Io)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    #[cfg(unix)]
    use std::time::Duration;

    use super::*;

    /// Bounds the one real-socket read in this module's tests. A working
    /// channel answers in microseconds; this is slack for a loaded runner,
    /// not an expected duration.
    ///
    /// Unix-gated with the one test that uses it. Windows has no socketpair
    /// to point that test at, and an ungated constant is dead code there --
    /// which `clippy --all-targets -- -D warnings` fails on, on a platform
    /// CI only ever runs `cargo test` against.
    #[cfg(unix)]
    const DEADLINE: Duration = Duration::from_secs(5);

    #[test]
    fn reads_two_messages_from_one_buffer() {
        let mut reader = Cursor::new(
            "{\"kind\":\"shutdown\"}\n{\"kind\":\"action\",\"name\":\"gc\",\"id\":7}\n".as_bytes(),
        );
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Some(ShepherdMessage::Shutdown)
        );
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Some(ShepherdMessage::Action {
                name: "gc".into(),
                params: None,
                id: 7
            })
        );
        assert_eq!(read_message(&mut reader).unwrap(), None);
    }

    /// pins that a `\r\n`-terminated line still parses -- the Windows
    /// transport is a byte-mode pipe and an app on the far side may well
    /// write one. Doesn't guard `trim_end_matches` in `read_message` above:
    /// serde_json already treats a trailing `\r`/`\n` as JSON whitespace,
    /// so removing that call would not fail this test. What it would catch
    /// is a parser swap, or framing that stops handing whole lines to the
    /// decoder.
    #[test]
    fn a_carriage_return_before_the_newline_is_tolerated() {
        let mut reader = Cursor::new("{\"kind\":\"shutdown\"}\r\n".as_bytes());
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Some(ShepherdMessage::Shutdown)
        );
    }

    /// fails if a malformed line ends the stream instead of being one
    /// recoverable error. The daemon skips a bad frame and keeps reading
    /// (`tokio_runner.rs`, the channel pumps); this side must be able to do
    /// the same or the two halves disagree about what a bad line costs.
    #[test]
    fn a_malformed_line_is_recoverable() {
        let mut reader = Cursor::new("not json\n{\"kind\":\"shutdown\"}\n".as_bytes());
        assert!(matches!(
            read_message(&mut reader),
            Err(ChannelError::Malformed(_))
        ));
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Some(ShepherdMessage::Shutdown)
        );
    }

    /// fails if a frame that is not UTF-8 is reported as `Io`.
    /// `Channel::recv` documents `Io` as the transport failing and
    /// `Malformed` as one bad line the caller can resume past, so the
    /// difference decides whether a caller's loop ends on a garbage byte.
    /// `read_line` classified it the first way, because it returns
    /// `InvalidData` rather than handing back the bytes it read.
    ///
    /// Both halves matter: the error kind, and that the next line still
    /// arrives. A decode that consumed nothing would loop on the bad frame
    /// forever instead.
    #[test]
    fn a_frame_that_is_not_utf8_is_malformed_and_recoverable() {
        let mut raw = b"\xff\xfe\n".to_vec();
        raw.extend_from_slice(b"{\"kind\":\"shutdown\"}\n");
        let mut reader = Cursor::new(raw);
        assert!(
            matches!(read_message(&mut reader), Err(ChannelError::Malformed(_))),
            "a non-UTF-8 frame must be Malformed, not Io"
        );
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Some(ShepherdMessage::Shutdown),
            "the reader must resume at the line after a bad frame"
        );
    }

    #[test]
    fn writes_one_line_per_message_with_a_trailing_newline() {
        let mut out = Vec::new();
        write_message(&mut out, &ChildMessage::Ready).unwrap();
        write_message(
            &mut out,
            &ChildMessage::Metric {
                name: "rps".into(),
                value: 42.0,
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"kind\":\"ready\"}\n{\"kind\":\"metric\",\"name\":\"rps\",\"value\":42.0}\n"
        );
    }

    /// fails if `Channel` cannot drive a real duplex. The generic tests
    /// above prove the framing; this proves the type wired to a socket.
    #[cfg(unix)]
    #[test]
    fn a_channel_over_a_socketpair_round_trips() {
        use std::io::{BufRead as _, BufReader, Write as _};
        use std::os::unix::net::UnixStream;

        let (ours, theirs) = UnixStream::pair().expect("socketpair");
        let mut channel = crate::Channel {
            reader: BufReader::new(ours.try_clone().expect("clone")),
            writer: ours,
            version: Some("1".to_string()),
        };
        let shepherd_reader = theirs.try_clone().expect("clone");
        shepherd_reader
            .set_read_timeout(Some(DEADLINE))
            .expect("set the read deadline");
        let mut shepherd = BufReader::new(shepherd_reader);
        let mut shepherd_writer = theirs;

        shepherd_writer
            .write_all(b"{\"kind\":\"action\",\"name\":\"gc\",\"id\":7}\n")
            .expect("write");
        assert_eq!(
            channel.recv().expect("recv"),
            Some(ShepherdMessage::Action {
                name: "gc".into(),
                params: None,
                id: 7
            })
        );

        channel
            .send(&ChildMessage::ActionReply {
                action: "gc".into(),
                body: "ok".into(),
                id: Some(7),
            })
            .expect("send");
        let mut back = String::new();
        shepherd
            .read_line(&mut back)
            .expect("the channel never answered within the deadline");
        assert_eq!(
            back,
            "{\"kind\":\"action-reply\",\"action\":\"gc\",\"body\":\"ok\",\"id\":7}\n"
        );
    }
}
