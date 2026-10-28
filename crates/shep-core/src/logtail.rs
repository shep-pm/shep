//! Reading the end of one sheep's log file.
//!
//! Shared between `shep bleats --no-follow`, the `whistle` tool and the
//! shepherd's `Request::TailLog` answer, so a tail read on this host and a
//! tail read on a remote one cut a file the same way.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// The most of one log file a tail will read to find the lines it wants.
///
/// Binds only when lines average over 5 KiB, so in ordinary use the caller's
/// line count is the bound that decides. A line count alone cannot bound
/// memory: one arbitrarily long line with no newline would defeat it.
pub const TAIL_WINDOW_BYTES: u64 = 256 * 1024;

/// The last `limit` lines of one log file, bounded twice.
/// A [`TAIL_WINDOW_BYTES`] window from the end of the file, then `limit`
/// lines within it. Returns the lines and whether either bound cut them
/// short.
///
/// `std::fs`, not `tokio::fs`: the callers' tokio builds have no `fs` feature.
///
/// `stamped` is a row's `log_timestamps` read the absent way. An app that
/// turned the stamp off wrote its own prefix. Cutting 30 bytes off that
/// cuts into the app's text.
///
/// # Errors
/// The file could not be opened, `stat`ed, seeked, or read.
pub fn read_tail(path: &Path, limit: usize, stamped: bool) -> io::Result<(Vec<String>, bool)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let (window, window_truncated) = read_window(&mut file, len)?;
    let window: &[u8] = &window;

    let text = String::from_utf8_lossy(window);
    // The daemon's per-line stamp comes off here. A `line` then means the
    // same thing on both of `bleats`' paths. The follow path reads the
    // bus, which carries a sheep's own bytes. The stamp stays in the file
    // for `tail`, `less` and `grep`.
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            if stamped {
                crate::logstamp::strip(line)
            } else {
                line
            }
        })
        .map(str::to_string)
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let keep_from = lines.len().saturating_sub(limit);
    let truncated = window_truncated || keep_from > 0;
    lines.drain(..keep_from);
    Ok((lines, truncated))
}

/// The bytes of the last [`TAIL_WINDOW_BYTES`] of `source`, which holds
/// `len` bytes. Cut to whole lines, with whether the window left content
/// behind.
///
/// Reads one byte before the window when content precedes it. It drops
/// everything up to the first `\n`: the partial line a mid-line cut
/// leaves. Never reads more than the window plus that byte.
fn read_window<R: Read + Seek>(source: &mut R, len: u64) -> io::Result<(Vec<u8>, bool)> {
    let start = len.saturating_sub(TAIL_WINDOW_BYTES);
    let window_truncated = start > 0;
    let from = start.saturating_sub(1);
    source.seek(SeekFrom::Start(from))?;

    let mut window = Vec::new();
    source
        .take(TAIL_WINDOW_BYTES + u64::from(window_truncated))
        .read_to_end(&mut window)?;

    if window_truncated {
        match window.iter().position(|&b| b == b'\n') {
            Some(newline) => {
                window.drain(..=newline);
            }
            None => window.clear(),
        }
    }
    Ok((window, window_truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shepherd's stamp spelled out, so the fixture does not come from
    /// the code under test.
    const STAMP: &str = "2026-09-02T14:22:31.412+02:00 ";

    fn file_with(dir: &Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("web-out.log");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn the_last_n_lines_are_returned() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(dir.path(), "one\ntwo\nthree\nfour\n");

        let (lines, truncated) = read_tail(&path, 2, false).unwrap();

        assert_eq!(lines, vec!["three".to_string(), "four".to_string()]);
        assert!(truncated, "two lines were left behind");
        let (all, truncated) = read_tail(&path, 10, false).unwrap();
        assert_eq!(all.len(), 4);
        assert!(!truncated, "the whole file fits");
    }

    #[test]
    fn a_file_larger_than_the_window_reports_truncated_and_drops_the_partial_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let filler = "x".repeat(usize::try_from(TAIL_WINDOW_BYTES).unwrap() + 1024);
        let path = file_with(dir.path(), &format!("{filler}\nshort\n"));

        let (lines, truncated) = read_tail(&path, 50, false).unwrap();

        assert_eq!(lines, vec!["short".to_string()]);
        assert!(truncated);
    }

    #[test]
    fn a_stamped_line_loses_its_stamp_and_an_unstamped_one_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_with(
            dir.path(),
            &format!("{STAMP}stamped\nno stamp on this one\n"),
        );

        let (stripped, _) = read_tail(&path, 10, true).unwrap();
        assert_eq!(
            stripped,
            vec!["stamped".to_string(), "no stamp on this one".to_string()]
        );
        let (kept, _) = read_tail(&path, 10, false).unwrap();
        assert_eq!(kept[0], format!("{STAMP}stamped"));
    }

    #[test]
    fn a_missing_file_is_an_error_of_kind_not_found() {
        let dir = tempfile::tempdir().unwrap();

        let err = read_tail(&dir.path().join("absent.log"), 10, true).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    /// The window starts on the first byte of a line, so that line is whole
    /// and the line before it is the one cut off.
    #[test]
    fn a_window_starting_on_a_line_boundary_keeps_its_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let window = usize::try_from(TAIL_WINDOW_BYTES).unwrap();
        let whole = "w".repeat(window - 1);
        let path = file_with(dir.path(), &format!("older\n{whole}\n"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            TAIL_WINDOW_BYTES + 6,
            "the window starts at `{whole}`'s first byte"
        );

        let (lines, truncated) = read_tail(&path, 10, false).unwrap();

        assert_eq!(lines, vec![whole]);
        assert!(truncated);
    }

    /// A file that grew after its length was taken must not be read past the
    /// window.
    #[test]
    fn a_file_that_grew_since_its_length_was_taken_is_read_no_further_than_the_window() {
        let grown = vec![b'x'; usize::try_from(TAIL_WINDOW_BYTES).unwrap() * 4];
        let mut source = io::Cursor::new(grown);

        let (window, _) = read_window(&mut source, 10).unwrap();

        assert!(window.len() <= usize::try_from(TAIL_WINDOW_BYTES).unwrap());
    }
}
