//! `barks.jsonl`: the size-capped ring of fired alerts.
//!
//! One [`Bark`] per line, appended by the bark dog on a rule fire and by
//! the shepherd on a dog's exhausted restart budget, and read by `shep
//! barks`. [`append`] evicts oldest-first under a byte cap by rewriting
//! survivors to a temp file and `rename`ing it over the original, so a
//! writer that dies mid-rewrite never leaves a fragment. [`read`] skips
//! any unparseable line, since this file is read during an incident, and
//! [`read_last`] answers the same question over a window of the file's
//! end so a caller that wants only the recent records does not pay for
//! the whole ring.
//!
//! The two writers are separate OS processes, so a [`crate::file_lock`] on
//! a sibling `<path>.lock` serializes them.

// The lock type lives in shep-core rather than shep-daemon so both writers
// can name it.
use core::fmt;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::file_lock::FileLock;

/// Cap the ring keeps itself under when nobody configured one.
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;

/// The first window [`read_last`] reads from the end of the ring, and the
/// factor it grows by while the window holds fewer records than were
/// asked for. A record is a few hundred bytes, so this covers a typical
/// tail request in one read; the growth is for a caller that asked for
/// more, or a ring of unusually large records.
const TAIL_WINDOW_BYTES: u64 = 64 * 1024;

/// One fired alert, as it lands in `$SHEP_HOME/barks.jsonl`.
///
/// One JSON object per line: an interrupted write then costs the reader
/// one record, not the whole file.
///
/// `Debug` is derived, not redacted. Every field here is shep's own prose
/// or a config key, never a sink's target; a field that ever carried a
/// webhook URL or token would need its own redacted `Debug`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bark {
    /// Unix millis when the alert fired.
    pub at_ms: u64,
    /// The rule that fired, or `daemon` when the shepherd wrote this
    /// itself.
    pub rule: String,
    /// What it is about: a sheep's name, or a dog's.
    pub subject: String,
    /// The human-readable line. Plain English, no theme: this is read
    /// during an incident.
    pub message: String,
    /// Which sinks the alert was delivered to, and whether each took it.
    /// Empty when the shepherd wrote the record itself: it has no sinks
    /// and no webhook code, and says so by carrying none.
    pub sinks: Vec<SinkOutcome>,
}

/// What one sink made of one alert.
///
/// Names the sink by its `[dog.bark.sinks]` config key, never by its
/// webhook URL or bearer token, so [`Bark`] stays safe to print with a
/// derived `Debug`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkOutcome {
    /// The sink's name from `[dog.bark.sinks]`.
    pub sink: String,
    /// `None` when it was delivered; the failure otherwise.
    pub error: Option<String>,
}

/// Error type returned by [`append`] and [`read`].
///
/// Wraps `io::Error`/`serde_json::Error` directly so callers keep the
/// underlying diagnostic via [`core::error::Error::source`]; this enum
/// cannot derive `Clone`/`PartialEq`/`Eq` as a result.
///
/// `#[non_exhaustive]`: shep-core is a published library, so a future
/// failure variant must not break an out-of-tree consumer's `match`.
#[non_exhaustive]
#[derive(Debug)]
pub enum BarkError {
    /// The ring file could not be read, written, or replaced.
    Io(std::io::Error),
    /// A [`Bark`] could not be serialized to JSON.
    Encode(serde_json::Error),
}

impl fmt::Display for BarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "bark ring I/O failed: {err}"),
            Self::Encode(err) => write!(f, "bark record failed to serialize: {err}"),
        }
    }
}

impl core::error::Error for BarkError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Encode(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for BarkError {
    fn from(source: std::io::Error) -> Self {
        Self::Io(source)
    }
}

impl From<serde_json::Error> for BarkError {
    fn from(source: serde_json::Error) -> Self {
        Self::Encode(source)
    }
}

/// Appends `bark` to `path`, evicting oldest-first to keep the file under
/// `max_bytes`.
///
/// Eviction drops a prefix of whole lines, atomically; an oversized
/// record is written anyway, since dropping it would silently lose the
/// one alert too big to fit. Serialized against other appenders by an
/// advisory lock on a sibling `<path>.lock`. Concurrent [`read`]s are not
/// blocked: the ring is only ever replaced whole.
///
/// # Errors
/// - [`BarkError::Io`]: the file or its lock could not be read, written, or replaced.
/// - [`BarkError::Encode`]: the record could not be serialized.
pub fn append(path: &Path, bark: &Bark, max_bytes: u64) -> Result<(), BarkError> {
    // Held until this returns, so the read and the final rename are one
    // transaction as far as any other writer is concerned.
    let _lock = FileLock::acquire(path)?;

    let mut lines = read_lines(path)?;
    let new_line = serde_json::to_string(bark)?;
    lines.push(new_line);

    // Oldest-out: drop the front line until the ring fits under the cap,
    // or only the record just appended is left.
    loop {
        if lines.len() <= 1 || ring_bytes(&lines) <= max_bytes {
            break;
        }
        lines.remove(0);
    }

    write_ring(path, &lines)
}

/// Reads every bark in `path`, oldest first, skipping any line that will
/// not parse.
///
/// A line that will not parse is a partially-written record from a writer
/// that died mid-append, or a record from a future shep. Neither refuses
/// the whole history during an incident, which is the one time this file
/// is read.
///
/// # Errors
/// - [`BarkError::Io`]: the file exists and could not be read. A missing
///   file is `Ok(Vec::new())`: no barks yet is not a fault.
pub fn read(path: &Path) -> Result<Vec<Bark>, BarkError> {
    Ok(read_text(path)?
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// The last `limit` barks in `path`, oldest first, without reading the
/// whole ring: a byte window from the end of the file, grown until it
/// holds `limit` records or the file runs out.
///
/// The same answer [`read`] gives for its last `limit` records — a
/// missing file is an empty ring, an unparseable line is skipped — so the
/// only difference is what the reader pays for. A byte sequence the
/// window cuts mid-character is replaced rather than refused: this file
/// is read during an incident, and a record the reader cannot have is
/// not worth failing the read over.
///
/// # Errors
/// - [`BarkError::Io`]: the file exists and could not be opened, stat'ed,
///   seeked or read.
pub fn read_last(path: &Path, limit: usize) -> Result<Vec<Bark>, BarkError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(BarkError::Io(err)),
    };
    let len = file.metadata()?.len();
    let mut window = TAIL_WINDOW_BYTES.min(len);
    loop {
        let mut barks = read_window(&mut file, len, window)?;
        if barks.len() >= limit || window >= len {
            let keep_from = barks.len().saturating_sub(limit);
            barks.drain(..keep_from);
            return Ok(barks);
        }
        // Doubling rather than a fixed step: the window that finally
        // suffices is within a factor of two of this one, so the total
        // read is bounded by twice the answer's size rather than by the
        // file's.
        window = (window * 2).min(len);
    }
}

/// The parseable records in the last `window` bytes of the file, oldest
/// first. A window that starts mid-file starts mid-record, so everything
/// up to the first newline is that record's tail and is dropped rather
/// than offered to the parser as a fragment.
fn read_window(file: &mut File, len: u64, window: u64) -> Result<Vec<Bark>, BarkError> {
    // Every iteration seeks, including the first and including a grown
    // window that now covers the whole file: the previous read left the
    // position at the end, and a `read_to_end` from there would see
    // nothing.
    file.seek(SeekFrom::Start(len - window))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = if window < len {
        match bytes.iter().position(|&b| b == b'\n') {
            Some(newline) => String::from_utf8_lossy(&bytes[newline + 1..]),
            None => return Ok(Vec::new()),
        }
    } else {
        String::from_utf8_lossy(&bytes)
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// `path`'s existing lines, raw and unparsed, or an empty ring if the
/// file does not exist yet.
///
/// Does not parse: eviction operates on whole lines exactly as they sit
/// on disk, so a line a future shep wrote, or a fragment a dead writer
/// left, still counts toward the byte cap and survives an eviction it
/// does not trigger. [`read`] is where an unparseable line is dropped.
fn read_lines(path: &Path) -> Result<Vec<String>, BarkError> {
    Ok(read_text(path)?.lines().map(str::to_owned).collect())
}

/// `path`'s bytes, or an empty string if the file is not there yet.
///
/// A ring nobody has written to is an empty ring, not a fault, and both
/// readers above want exactly that reading before they go their separate
/// ways on what to do with the lines.
fn read_text(path: &Path) -> Result<String, BarkError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(BarkError::Io(err)),
    }
}

/// Total on-disk size, in bytes, if `lines` were written one per line
/// (each line plus its trailing `\n`).
fn ring_bytes(lines: &[String]) -> u64 {
    lines.iter().map(|line| line.len() as u64 + 1).sum()
}

/// Rewrites `path` to hold exactly `lines`: the content lands in a
/// uniquely-named sibling temp file, is `fsync`ed, then `rename`d over
/// `path`, so an interrupted write leaves the original untouched.
///
/// The name is unique per call, not a fixed `<path>.tmp`: two writers
/// racing on a shared name can have one `rename` consume the other's
/// staging file. [`FileLock`] already keeps two appenders apart; this is
/// the second lock for a caller that reaches `write_ring` another way.
fn write_ring(path: &Path, lines: &[String]) -> Result<(), BarkError> {
    let parent = crate::atomic_file::parent_of(path);
    let mut tmp = crate::atomic_file::create_staging_file(parent, "barks", ".tmp")?;

    for line in lines {
        tmp.write_all(line.as_bytes())?;
        tmp.write_all(b"\n")?;
    }
    crate::atomic_file::publish(tmp, path).map_err(BarkError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative fired alert. `at_ms` is a caller-chosen tag, not a
    /// real timestamp: tests use it to tell records apart.
    fn bark_for(subject: &str, at_ms: u64) -> Bark {
        Bark {
            at_ms,
            rule: "watchdog".to_string(),
            subject: subject.to_string(),
            message: "restart budget exhausted".to_string(),
            sinks: vec![SinkOutcome {
                sink: "discord".to_string(),
                error: None,
            }],
        }
    }

    /// The serialized length, plus its trailing newline, of one
    /// `bark_for`-shaped line, computed here so it cannot happen to equal
    /// the implementation's own byte count.
    fn one_bark_len() -> u64 {
        let line = serde_json::to_string(&bark_for("second", 1)).unwrap();
        line.len() as u64 + 1
    }

    /// Cap set to force eviction on the third write.
    #[test]
    fn the_ring_drops_the_oldest_bark_to_stay_under_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barks.jsonl");
        let cap = 2 * one_bark_len();

        for (i, subject) in ["first", "second", "third"].iter().enumerate() {
            append(&path, &bark_for(subject, i as u64), cap).unwrap();
        }

        let barks = read(&path).unwrap();
        let subjects: Vec<&str> = barks.iter().map(|b| b.subject.as_str()).collect();
        assert_eq!(subjects, ["second", "third"], "oldest out, newest kept");
        assert!(
            std::fs::metadata(&path).unwrap().len() <= cap,
            "the cap is a cap"
        );
    }

    #[test]
    fn a_bark_bigger_than_the_cap_is_written_anyway() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barks.jsonl");
        let huge = Bark {
            message: "x".repeat(4096),
            ..bark_for("web", 0)
        };
        append(&path, &huge, 64).unwrap();
        assert_eq!(read(&path).unwrap().len(), 1);
    }

    #[test]
    fn a_line_that_will_not_parse_costs_one_record_and_not_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barks.jsonl");
        append(&path, &bark_for("web", 1), DEFAULT_MAX_BYTES).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"at_ms\": 2, \"rul\n")
            .unwrap();
        append(&path, &bark_for("api", 3), DEFAULT_MAX_BYTES).unwrap();

        let barks = read(&path).unwrap();
        assert_eq!(
            barks.iter().map(|b| b.subject.as_str()).collect::<Vec<_>>(),
            ["web", "api"]
        );
    }

    #[test]
    fn no_file_yet_is_no_barks_rather_than_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(&dir.path().join("nothing.jsonl")).unwrap(), vec![]);
    }

    #[test]
    fn read_last_on_a_missing_file_is_no_barks_rather_than_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_last(&dir.path().join("nothing.jsonl"), 20).unwrap(),
            vec![]
        );
    }

    /// Zero asked for is zero returned, and the file is not even opened.
    #[test]
    fn read_last_of_zero_records_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barks.jsonl");
        append(&path, &bark_for("web", 1), DEFAULT_MAX_BYTES).unwrap();
        assert_eq!(read_last(&path, 0).unwrap(), vec![]);
    }

    /// The window reads from the end of the file, so a ring larger than
    /// the window still answers with its newest records.
    #[test]
    fn read_last_returns_the_newest_records_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barks.jsonl");
        for i in 0..10u64 {
            append(&path, &bark_for(&format!("s{i}"), i), DEFAULT_MAX_BYTES).unwrap();
        }
        let barks = read_last(&path, 3).unwrap();
        assert_eq!(
            barks.iter().map(|b| b.subject.as_str()).collect::<Vec<_>>(),
            ["s7", "s8", "s9"]
        );
    }

    /// Records large enough that the first window cannot hold the number
    /// asked for: the window must grow, and the answer must still be the
    /// last `limit` records — the oldest of them absent, not truncated.
    #[test]
    fn read_last_grows_the_window_until_it_holds_enough_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barks.jsonl");
        // About 20 KiB a record, so the 64 KiB first window holds three
        // of the six — fewer than the five asked for below.
        let big = |subject: &str, at_ms: u64| Bark {
            message: "x".repeat(20 * 1024),
            ..bark_for(subject, at_ms)
        };
        for (i, subject) in ["s0", "s1", "s2", "s3", "s4", "s5"].iter().enumerate() {
            append(&path, &big(subject, i as u64), DEFAULT_MAX_BYTES).unwrap();
        }
        assert_eq!(read(&path).unwrap().len(), 6, "the fixture is six records");

        let barks = read_last(&path, 5).unwrap();
        assert_eq!(
            barks.iter().map(|b| b.subject.as_str()).collect::<Vec<_>>(),
            ["s1", "s2", "s3", "s4", "s5"]
        );
    }

    /// `read_last` must answer what `read` plus a drain answers, for every
    /// limit: the point of the window is what the reader pays, not what it
    /// returns. The file carries the two kinds of line no parser takes — a
    /// fragment a dead writer left, and a well-formed record from a shep
    /// that grew a field — since those are what a real ring accumulates.
    /// The records are padded past the first window, so the limits that
    /// force a grow are in the range too.
    #[test]
    fn read_last_answers_what_a_full_read_would_for_every_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barks.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        for i in 0..12u64 {
            let bark = Bark {
                message: "x".repeat(8 * 1024),
                ..bark_for(&format!("s{i}"), i)
            };
            writeln!(file, "{}", serde_json::to_string(&bark).unwrap()).unwrap();
            if i % 3 == 0 {
                writeln!(file, "{{\"at_ms\": 99, \"subj").unwrap();
            }
            if i % 4 == 0 {
                writeln!(file, "{{\"at_ms\": 99, \"future\": []}}").unwrap();
            }
        }
        drop(file);

        let everything = read(&path).unwrap();
        assert_eq!(
            everything.len(),
            12,
            "the fixture is twelve parseable records"
        );
        for limit in 0..everything.len() + 3 {
            let keep_from = everything.len().saturating_sub(limit);
            assert_eq!(
                read_last(&path, limit).unwrap(),
                &everything[keep_from..],
                "limit {limit} must answer exactly what a full read plus a drain answers"
            );
        }
    }

    /// Env var naming the ring file the re-executed child should append
    /// to. Its presence is also what tells the child it is a child.
    #[cfg(any(unix, windows))]
    const CHILD_PATH_VAR: &str = "SHEP_BARK_RACE_PATH";
    /// Env var carrying the child's tag, which it stamps into every
    /// record's `subject` so the parent can tell the two writers apart.
    #[cfg(any(unix, windows))]
    const CHILD_TAG_VAR: &str = "SHEP_BARK_RACE_TAG";
    /// How many records each of the two writers appends. Large enough
    /// that the two read-modify-rename sequences overlap many times over.
    #[cfg(any(unix, windows))]
    const RECORDS_PER_WRITER: u64 = 200;

    /// Not a test: the child half of
    /// [`two_writer_processes_do_not_lose_each_other_s_barks`], re-executed
    /// as a separate OS process via `--ignored --exact`. Asserts nothing;
    /// its job is to hammer [`append`] while the parent judges the result.
    #[cfg(any(unix, windows))]
    #[test]
    #[ignore = "child process of two_writer_processes_do_not_lose_each_other_s_barks"]
    fn bark_race_child() {
        let Ok(path) = std::env::var(CHILD_PATH_VAR) else {
            panic!("{CHILD_PATH_VAR} unset — this test is only run as a child process");
        };
        let tag = std::env::var(CHILD_TAG_VAR).expect("child needs a tag");
        let path = std::path::PathBuf::from(path);

        for i in 0..RECORDS_PER_WRITER {
            append(&path, &bark_for(&tag, i), DEFAULT_MAX_BYTES).expect("child append");
        }
    }

    /// Two OS processes, not threads: an in-process mutex would prove
    /// nothing about a race that crosses address spaces via `rename`.
    /// Covers Windows too: reverting `acquire`'s Windows arm to
    /// `Ok(Self {})` reddens this rather than passing quietly. Without
    /// the lock this can still pass on a lucky serial schedule.
    #[cfg(any(unix, windows))]
    #[test]
    fn two_writer_processes_do_not_lose_each_other_s_barks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barks.jsonl");
        let exe = std::env::current_exe().expect("test binary path");

        let children: Vec<_> = ["alpha", "beta"]
            .iter()
            .map(|tag| {
                std::process::Command::new(&exe)
                    .args(["--exact", "--ignored", "barks::tests::bark_race_child"])
                    .env(CHILD_PATH_VAR, &path)
                    .env(CHILD_TAG_VAR, tag)
                    // Piped, not inherited: a passing run should not
                    // interleave two child harnesses' output into this
                    // one's, and a failing child's harness output is
                    // exactly what the assertion below needs to show.
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .expect("spawn writer")
            })
            .collect();

        for child in children {
            let out = child.wait_with_output().expect("wait for writer");
            assert!(
                out.status.success(),
                "a writer process failed: {}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stdout)
            );
        }

        let barks = read(&path).unwrap();
        for tag in ["alpha", "beta"] {
            let mut seen: Vec<u64> = barks
                .iter()
                .filter(|b| b.subject == tag)
                .map(|b| b.at_ms)
                .collect();
            seen.sort_unstable();
            let expected: Vec<u64> = (0..RECORDS_PER_WRITER).collect();
            assert_eq!(
                seen, expected,
                "{tag}'s records did not all survive the other writer"
            );
        }
        assert_eq!(
            barks.len() as u64,
            2 * RECORDS_PER_WRITER,
            "the ring holds records nobody wrote"
        );
    }

    /// No field here is a credential today; the mode stays narrow so a
    /// future one that is arrives already protected.
    #[cfg(unix)]
    #[test]
    fn append_creates_the_ring_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barks.jsonl");

        append(&path, &bark_for("web", 0), DEFAULT_MAX_BYTES).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "barks.jsonl is not the credential file, but stays narrow anyway"
        );
    }
}
