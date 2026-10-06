//! [`LineReader`], the pump's line source.

use std::io;

use tokio::io::{AsyncBufReadExt as _, AsyncRead, BufReader};

/// The scratch capacity kept between lines, in bytes.
///
/// Two reader buffers' worth: a line past it is rare, and one such line
/// should not pin its memory for as long as the stream lives.
const RETAINED_LINE: usize = 16 * 1024;

/// A [`BufReader`] that hands out one line at a time from a scratch buffer it
/// keeps, so a line costs no allocation of its own.
///
/// `tokio::io::Lines` moves its buffer out with every line it returns, and
/// the next line grows a new one from empty.
pub(super) struct LineReader<R> {
    reader: BufReader<R>,
    /// The line being read, delimiter included until [`Self::read_line`]
    /// completes it.
    buf: Vec<u8>,
    /// `buf` holds a line already handed out, to clear before the next read.
    handed_out: bool,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    /// A line source over `reader`.
    pub(super) fn new(reader: BufReader<R>) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            handed_out: false,
        }
    }

    /// The reader, for what it has taken off the pipe and not yet handed out.
    #[cfg(unix)]
    pub(super) fn get_ref(&self) -> &BufReader<R> {
        &self.reader
    }

    /// The next line without its `\n` or `\r\n`, or `None` at EOF. A last
    /// line with no terminator is still a line.
    ///
    /// The line borrows the scratch buffer, so it is gone at the next call.
    ///
    /// # Errors
    ///
    /// The read failed, or the line is not UTF-8 ([`io::ErrorKind::InvalidData`]).
    ///
    /// # Cancellation safety
    ///
    /// Cancel-safe. A partially read line stays in the scratch buffer and the
    /// next call carries on from it: the buffer is cleared only once a line
    /// has been handed out, never at the start of a call. `read_line` would
    /// not do, since it loses a partial line when cancelled.
    pub(super) async fn read_line(&mut self) -> io::Result<Option<&str>> {
        if self.handed_out {
            self.buf.clear();
            self.buf.shrink_to(RETAINED_LINE);
            self.handed_out = false;
        }
        let read = self.reader.read_until(b'\n', &mut self.buf).await?;
        if read == 0 && self.buf.is_empty() {
            return Ok(None);
        }
        self.handed_out = true;
        if self.buf.ends_with(b"\n") {
            self.buf.pop();
            if self.buf.ends_with(b"\r") {
                self.buf.pop();
            }
        }
        match core::str::from_utf8(&self.buf) {
            Ok(line) => Ok(Some(line)),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncWriteExt as _, DuplexStream, duplex};
    use tokio::time::timeout;

    use super::*;

    fn over(stream: DuplexStream) -> LineReader<DuplexStream> {
        LineReader::new(BufReader::new(stream))
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_of_lines_comes_back_one_at_a_time() {
        let (mut writer, reader) = duplex(64);
        writer.write_all(b"one\ntwo\n\nfour\n").await.unwrap();
        drop(writer);
        let mut lines = over(reader);

        assert_eq!(lines.read_line().await.unwrap(), Some("one"));
        assert_eq!(lines.read_line().await.unwrap(), Some("two"));
        assert_eq!(lines.read_line().await.unwrap(), Some(""));
        assert_eq!(lines.read_line().await.unwrap(), Some("four"));
        assert_eq!(lines.read_line().await.unwrap(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_carriage_return_before_the_newline_is_not_part_of_the_line() {
        let (mut writer, reader) = duplex(64);
        writer.write_all(b"crlf\r\nbare\rcr\r\n").await.unwrap();
        drop(writer);
        let mut lines = over(reader);

        assert_eq!(lines.read_line().await.unwrap(), Some("crlf"));
        assert_eq!(lines.read_line().await.unwrap(), Some("bare\rcr"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_last_line_with_no_terminator_is_still_a_line() {
        let (mut writer, reader) = duplex(64);
        writer.write_all(b"first\nlast").await.unwrap();
        drop(writer);
        let mut lines = over(reader);

        assert_eq!(lines.read_line().await.unwrap(), Some("first"));
        assert_eq!(lines.read_line().await.unwrap(), Some("last"));
        assert_eq!(lines.read_line().await.unwrap(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_line_that_is_not_utf8_is_an_error_not_a_lossy_line() {
        let (mut writer, reader) = duplex(64);
        writer.write_all(b"\xff\xfe\nafter\n").await.unwrap();
        drop(writer);
        let mut lines = over(reader);

        let error = lines.read_line().await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(lines.read_line().await.unwrap(), Some("after"));
    }

    /// The pump's `select!` drops this future whenever a control request wins
    /// the race, with a line half read.
    #[tokio::test(start_paused = true)]
    async fn a_line_cancelled_part_way_is_whole_on_the_next_call() {
        let (mut writer, reader) = duplex(64);
        let mut lines = over(reader);

        writer.write_all(b"par").await.unwrap();
        let cancelled = timeout(Duration::from_secs(1), lines.read_line()).await;
        assert!(cancelled.is_err(), "no line is complete yet");
        writer.write_all(b"tial\nnext\n").await.unwrap();

        assert_eq!(lines.read_line().await.unwrap(), Some("partial"));
        assert_eq!(lines.read_line().await.unwrap(), Some("next"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancellation_between_lines_does_not_replay_the_last_one() {
        let (mut writer, reader) = duplex(64);
        let mut lines = over(reader);

        writer.write_all(b"done\n").await.unwrap();
        assert_eq!(lines.read_line().await.unwrap(), Some("done"));
        let cancelled = timeout(Duration::from_secs(1), lines.read_line()).await;
        assert!(cancelled.is_err(), "nothing more was written");
        writer.write_all(b"fresh\n").await.unwrap();

        assert_eq!(lines.read_line().await.unwrap(), Some("fresh"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_very_long_line_does_not_leave_its_capacity_behind() {
        let (mut writer, reader) = duplex(64);
        let mut lines = over(reader);
        let long = "x".repeat(RETAINED_LINE * 4);
        let send = async move {
            writer.write_all(long.as_bytes()).await.unwrap();
            writer.write_all(b"\nshort\n").await.unwrap();
        };

        let (_, ()) = tokio::join!(send, async {
            assert_eq!(
                lines.read_line().await.unwrap().map(str::len),
                Some(RETAINED_LINE * 4)
            );
            assert_eq!(lines.read_line().await.unwrap(), Some("short"));
            assert!(
                lines.buf.capacity() <= RETAINED_LINE,
                "the scratch buffer kept {} bytes after one long line",
                lines.buf.capacity()
            );
        });
    }

    #[tokio::test(start_paused = true)]
    async fn a_line_longer_than_the_reader_buffer_comes_back_whole() {
        let (mut writer, reader) = duplex(64);
        let mut lines = LineReader::new(BufReader::with_capacity(8, reader));
        let long = "x".repeat(100);
        let send = async move {
            writer.write_all(long.as_bytes()).await.unwrap();
            writer.write_all(b"\n").await.unwrap();
        };

        let (_, line) = tokio::join!(send, async {
            lines.read_line().await.unwrap().map(str::to_owned)
        });

        assert_eq!(line.as_deref(), Some("x".repeat(100).as_str()));
    }
}
