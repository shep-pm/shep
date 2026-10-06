//! Reading and writing one newline-delimited JSON message.
//!
//! Generic over `BufRead` and `Write`, not the transport itself. That lets
//! these tests run without a live shepherd to construct the real one.

use std::io::{BufRead, Write};

use crate::{ChannelError, ChildMessage, ShepherdMessage};

/// The capacity a scratch buffer shrinks back to once it has grown far
/// past it.
///
/// A frame or batch that big is the exception. Its allocation is given
/// back rather than held for the life of the channel.
const MAX_RETAINED: usize = 64 * 1024;

/// Empties `line` and, when it holds more than twice [`MAX_RETAINED`],
/// shrinks it to that.
///
/// Twice, not once, so steady traffic just over the size does not grow
/// and shrink the buffer on every message.
fn reset(line: &mut Vec<u8>) {
    line.clear();
    if line.capacity() > MAX_RETAINED * 2 {
        line.shrink_to(MAX_RETAINED);
    }
}

/// Reads one message. `Ok(None)` is end of stream.
///
/// `line` is scratch the caller keeps between calls, so a reader pays for
/// one allocation rather than one per message. Cleared on entry and on
/// return, and released if a frame left it far past [`MAX_RETAINED`].
pub(crate) fn read_message<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> Result<Option<ShepherdMessage>, ChannelError> {
    let message = read_line(reader, line);
    reset(line);
    message
}

fn read_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> Result<Option<ShepherdMessage>, ChannelError> {
    line.clear();
    if reader.read_until(b'\n', line).map_err(ChannelError::Io)? == 0 {
        return Ok(None);
    }
    // Bytes then decode, not `read_line`. `read_line` reports non-UTF-8 as
    // `io::ErrorKind::InvalidData`, which would surface as the transport
    // failure case, `ChannelError::Io`. A non-UTF-8 frame is `Malformed`
    // instead, so the next call resumes at the following line.
    let text =
        core::str::from_utf8(line).map_err(|error| ChannelError::Malformed(error.to_string()))?;
    // `serde_json` already skips a trailing `\r`/`\n` as JSON whitespace.
    // This trim keeps that explicit rather than implicit.
    let trimmed = text.trim_end_matches(['\n', '\r']);
    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(|error| ChannelError::Malformed(error.to_string()))
}

/// Writes each message and its newline as one write, then flushes.
///
/// `line` is scratch the caller keeps between calls. Every message is
/// encoded into it before anything is written, so an encoding failure puts
/// nothing on the wire. Cleared on entry and on return, and released if a
/// batch left it far past [`MAX_RETAINED`].
pub(crate) fn write_messages<W: Write>(
    writer: &mut W,
    messages: &[ChildMessage],
    line: &mut Vec<u8>,
) -> Result<(), ChannelError> {
    let written = write_line(writer, messages, line);
    reset(line);
    written
}

fn write_line<W: Write>(
    writer: &mut W,
    messages: &[ChildMessage],
    line: &mut Vec<u8>,
) -> Result<(), ChannelError> {
    line.clear();
    for message in messages {
        serde_json::to_writer(&mut *line, message)
            .map_err(|error| ChannelError::Malformed(error.to_string()))?;
        line.push(b'\n');
    }
    writer.write_all(line).map_err(ChannelError::Io)?;
    writer.flush().map_err(ChannelError::Io)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    #[cfg(unix)]
    use std::time::Duration;

    use super::*;

    /// Bounds the one real-socket read in this module's tests. A working
    /// channel answers in microseconds; this is slack for a loaded runner.
    ///
    /// Unix-gated: Windows has no socketpair for the one test that uses
    /// this. An ungated constant is dead code there, which CI's clippy
    /// gate refuses.
    #[cfg(unix)]
    const DEADLINE: Duration = Duration::from_secs(5);

    #[test]
    fn reads_two_messages_from_one_buffer() {
        let mut line = Vec::new();
        let mut reader = Cursor::new(
            "{\"kind\":\"shutdown\"}\n{\"kind\":\"action\",\"name\":\"gc\",\"id\":7}\n".as_bytes(),
        );
        assert_eq!(
            read_message(&mut reader, &mut line).unwrap(),
            Some(ShepherdMessage::Shutdown)
        );
        assert_eq!(
            read_message(&mut reader, &mut line).unwrap(),
            Some(ShepherdMessage::Action {
                name: "gc".into(),
                params: None,
                id: 7
            })
        );
        assert_eq!(read_message(&mut reader, &mut line).unwrap(), None);
    }

    /// The Windows transport is a byte-mode pipe, so an app there may
    /// write `\r\n`. This doesn't guard `trim_end_matches`, since
    /// `serde_json` already treats a trailing `\r`/`\n` as whitespace. It
    /// catches a parser swap or framing that stops handing whole lines
    /// over.
    #[test]
    fn a_carriage_return_before_the_newline_is_tolerated() {
        let mut line = Vec::new();
        let mut reader = Cursor::new("{\"kind\":\"shutdown\"}\r\n".as_bytes());
        assert_eq!(
            read_message(&mut reader, &mut line).unwrap(),
            Some(ShepherdMessage::Shutdown)
        );
    }

    /// The daemon skips a bad frame and keeps reading (`tokio_runner.rs`).
    /// This side must match, or the two halves disagree about what a bad
    /// line costs.
    #[test]
    fn a_malformed_line_is_recoverable() {
        let mut line = Vec::new();
        let mut reader = Cursor::new("not json\n{\"kind\":\"shutdown\"}\n".as_bytes());
        assert!(matches!(
            read_message(&mut reader, &mut line),
            Err(ChannelError::Malformed(_))
        ));
        assert_eq!(
            read_message(&mut reader, &mut line).unwrap(),
            Some(ShepherdMessage::Shutdown)
        );
    }

    /// `Channel::recv` documents `Io` as a transport failure and
    /// `Malformed` as one resumable bad line. Both halves matter here:
    /// the error kind, and that the next line still arrives.
    #[test]
    fn a_frame_that_is_not_utf8_is_malformed_and_recoverable() {
        let mut raw = b"\xff\xfe\n".to_vec();
        raw.extend_from_slice(b"{\"kind\":\"shutdown\"}\n");
        let mut line = Vec::new();
        let mut reader = Cursor::new(raw);
        assert!(
            matches!(
                read_message(&mut reader, &mut line),
                Err(ChannelError::Malformed(_))
            ),
            "a non-UTF-8 frame must be Malformed, not Io"
        );
        assert_eq!(
            read_message(&mut reader, &mut line).unwrap(),
            Some(ShepherdMessage::Shutdown),
            "the reader must resume at the line after a bad frame"
        );
    }

    #[test]
    fn writes_one_line_per_message_with_a_trailing_newline() {
        let mut out = Vec::new();
        let mut line = Vec::new();
        write_messages(&mut out, &[ChildMessage::Ready], &mut line).unwrap();
        write_messages(&mut out, &[rps(42.0)], &mut line).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"kind\":\"ready\"}\n{\"kind\":\"metric\",\"name\":\"rps\",\"value\":42.0}\n"
        );
    }

    fn rps(value: f64) -> ChildMessage {
        ChildMessage::Metric {
            name: "rps".into(),
            value,
        }
    }

    /// The wire is the same bytes as one write per message, so the
    /// shepherd's line reader cannot tell a batch from a trickle.
    #[test]
    fn a_batch_is_one_line_per_message_in_order() {
        let mut out = Vec::new();
        let mut line = Vec::new();
        write_messages(
            &mut out,
            &[ChildMessage::Ready, rps(1.0), rps(2.0)],
            &mut line,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"kind\":\"ready\"}\n\
             {\"kind\":\"metric\",\"name\":\"rps\",\"value\":1.0}\n\
             {\"kind\":\"metric\",\"name\":\"rps\",\"value\":2.0}\n"
        );
    }

    /// A `Write` that counts calls, which is what a socket charges for.
    #[derive(Debug, Default)]
    struct CountingSink {
        writes: usize,
        flushes: usize,
    }

    impl Write for CountingSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    /// A burst costs the transport one write and one flush, not one per
    /// message.
    #[test]
    fn a_batch_reaches_the_transport_in_one_write() {
        let mut sink = CountingSink::default();
        let mut line = Vec::new();
        write_messages(&mut sink, &[rps(1.0), rps(2.0), rps(3.0)], &mut line).unwrap();
        assert_eq!((sink.writes, sink.flushes), (1, 1));
    }

    /// The scratch buffer is the one allocation a message used to pay
    /// for, so its address holding still is the claim. Reserved up front,
    /// so a move here means a reallocation the buffer had room to avoid.
    #[test]
    fn writing_reuses_the_callers_buffer() {
        let mut out = Vec::new();
        let mut line = Vec::with_capacity(256);
        let address = line.as_ptr();

        write_messages(&mut out, &[rps(1.0)], &mut line).unwrap();
        write_messages(&mut out, &[rps(2.0), rps(3.0)], &mut line).unwrap();

        assert_eq!(line.as_ptr(), address, "the buffer was reallocated");
        assert_eq!(
            String::from_utf8(out).unwrap().lines().count(),
            3,
            "a reused buffer must not replay an earlier message"
        );
    }

    /// One large frame must not leave its allocation behind for the life
    /// of the channel.
    #[test]
    fn a_large_frame_does_not_pin_the_read_buffer() {
        let name = "x".repeat(MAX_RETAINED * 2);
        let mut reader = Cursor::new(format!(
            "{{\"kind\":\"action\",\"name\":\"{name}\",\"id\":1}}\n"
        ));
        let mut line = Vec::new();

        let message = read_message(&mut reader, &mut line).unwrap();

        assert!(matches!(message, Some(ShepherdMessage::Action { .. })));
        assert!(
            line.capacity() <= MAX_RETAINED,
            "the buffer kept {} bytes",
            line.capacity()
        );
    }

    /// Steady traffic a little over the retained size must not pay a
    /// grow and a shrink per message.
    #[test]
    fn a_frame_just_over_the_retained_size_keeps_its_buffer() {
        let name = "x".repeat(MAX_RETAINED + 1000);
        let frame = format!("{{\"kind\":\"action\",\"name\":\"{name}\",\"id\":1}}\n");
        let mut reader = Cursor::new(frame.repeat(2));
        let mut line = Vec::new();

        read_message(&mut reader, &mut line).unwrap();
        let (address, capacity) = (line.as_ptr(), line.capacity());
        read_message(&mut reader, &mut line).unwrap();

        assert!(capacity > MAX_RETAINED, "the first frame was trimmed");
        assert_eq!(line.as_ptr(), address, "the buffer was reallocated");
    }

    #[test]
    fn a_large_batch_does_not_pin_the_write_buffer() {
        let mut out = Vec::new();
        let mut line = Vec::new();
        let big = ChildMessage::Metric {
            name: "x".repeat(MAX_RETAINED * 2),
            value: 1.0,
        };

        write_messages(&mut out, &[big], &mut line).unwrap();

        assert!(out.len() > MAX_RETAINED * 2, "the batch was not written");
        assert!(
            line.capacity() <= MAX_RETAINED,
            "the buffer kept {} bytes",
            line.capacity()
        );
    }

    #[test]
    fn reading_reuses_the_callers_buffer() {
        let mut reader = Cursor::new(
            "{\"kind\":\"shutdown\"}\n{\"kind\":\"shutdown\"}\n{\"kind\":\"shutdown\"}\n"
                .as_bytes(),
        );
        let mut line = Vec::with_capacity(256);
        let address = line.as_ptr();

        for _ in 0..3 {
            assert_eq!(
                read_message(&mut reader, &mut line).unwrap(),
                Some(ShepherdMessage::Shutdown),
                "an earlier line was replayed from the reused buffer"
            );
        }

        assert_eq!(line.as_ptr(), address, "the buffer was reallocated");
    }

    /// The generic tests above prove the framing; this proves the type
    /// wired to a socket.
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
            read_line: crate::channel::Scratch::default(),
            write_line: crate::channel::Scratch::default(),
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
