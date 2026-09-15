//! `shep barks`: the alert history, newest last.

use shep_core::barks;
use shep_core::paths::ShepPaths;

use crate::cli::BarksArgs;
use crate::exit::ExitCode;
use crate::output::{BarkRows, Streams, emit, write_outcome};

/// `shep barks`: the alert history, newest last.
///
/// Reads `barks.jsonl` straight off disk and never connects to the
/// shepherd. [`barks::read`] is the forgiving half of that file's contract:
/// a line a writer died mid-append leaves unparseable costs that one record,
/// not the whole read.
///
/// `--tail N` takes the last N records, since [`barks::read`] answers oldest
/// first.
pub fn barks(streams: &mut Streams<'_>, paths: &ShepPaths, args: &BarksArgs) -> ExitCode {
    let mut history = match barks::read(&paths.barks) {
        Ok(history) => history,
        Err(err) => {
            return streams.fail(ExitCode::Failure, &err.to_string());
        }
    };
    if let Some(tail) = args.tail {
        let keep_from = history.len().saturating_sub(tail);
        history.drain(..keep_from);
    }
    write_outcome(emit(
        &mut *streams.out,
        streams.fmt,
        "barks",
        BarkRows(history),
        streams.style,
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use shep_core::barks::{self, Bark, SinkOutcome};

    use super::*;
    use crate::cli::Format;

    /// Every test here drives a dog verb under `--format table`.
    fn streams<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>) -> Streams<'a> {
        Streams {
            out,
            err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        }
    }

    /// A bark named `subject`, at `at_ms`, delivered to one sink named
    /// `ops`; every other field fixed.
    fn bark_for(subject: &str, at_ms: u64) -> Bark {
        Bark {
            at_ms,
            rule: "watchdog".to_string(),
            subject: subject.to_string(),
            message: "restart budget exhausted".to_string(),
            sinks: vec![SinkOutcome {
                sink: "ops".to_string(),
                error: None,
            }],
        }
    }

    /// `#[test]`, not `#[tokio::test]`: `barks` answers from a file, with
    /// no socket in reach.
    #[test]
    fn barks_renders_the_ring_newest_last_with_no_client_anywhere_in_reach() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.home).unwrap();
        barks::append(&paths.barks, &bark_for("web", 1), barks::DEFAULT_MAX_BYTES).unwrap();
        barks::append(
            &paths.barks,
            &bark_for("worker", 2),
            barks::DEFAULT_MAX_BYTES,
        )
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = barks(
            &mut streams(&mut out, &mut err),
            &paths,
            &BarksArgs { tail: None },
        );

        assert_eq!(code, ExitCode::Success);
        let text = String::from_utf8(out).unwrap();
        let web_at = text.find("web").expect("the older bark must be rendered");
        let worker_at = text
            .find("worker")
            .expect("the newer bark must be rendered");
        assert!(
            web_at < worker_at,
            "newest last: web (older) must render before worker (newer): {text}"
        );
    }

    #[test]
    fn tail_shows_only_the_most_recent_n_barks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.home).unwrap();
        for (subject, at_ms) in [("first", 1), ("second", 2), ("third", 3)] {
            barks::append(
                &paths.barks,
                &bark_for(subject, at_ms),
                barks::DEFAULT_MAX_BYTES,
            )
            .unwrap();
        }

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = barks(
            &mut streams(&mut out, &mut err),
            &paths,
            &BarksArgs { tail: Some(2) },
        );

        assert_eq!(code, ExitCode::Success);
        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains("first"),
            "--tail 2 must drop the oldest of three: {text}"
        );
        assert!(text.contains("second"), "{text}");
        assert!(text.contains("third"), "{text}");
    }

    /// `history.len().saturating_sub(tail)`'s reason for being `saturating`.
    #[test]
    fn tail_larger_than_the_ring_shows_everything() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.home).unwrap();
        barks::append(&paths.barks, &bark_for("web", 1), barks::DEFAULT_MAX_BYTES).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = barks(
            &mut streams(&mut out, &mut err),
            &paths,
            &BarksArgs { tail: Some(50) },
        );

        assert_eq!(code, ExitCode::Success);
        assert!(String::from_utf8(out).unwrap().contains("web"));
    }

    /// `barks::read` answers `Ok(vec![])` rather than an I/O error, and
    /// `dogs::barks` must still exit `Success` and print headers.
    #[test]
    fn no_ring_file_yet_is_an_empty_history_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = barks(
            &mut streams(&mut out, &mut err),
            &paths,
            &BarksArgs { tail: None },
        );

        assert_eq!(code, ExitCode::Success);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("WHEN"),
            "an empty history still prints its header row: {text}"
        );
    }

    /// The tolerance lives in `barks::read`; this is `dogs::barks`' own
    /// proof that nothing between here and there swallows it.
    #[test]
    fn a_corrupt_trailing_line_costs_one_record_not_the_whole_read() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.home).unwrap();
        barks::append(&paths.barks, &bark_for("web", 1), barks::DEFAULT_MAX_BYTES).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&paths.barks)
            .unwrap()
            .write_all(b"{\"at_ms\": 2, \"rul\n")
            .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = barks(
            &mut streams(&mut out, &mut err),
            &paths,
            &BarksArgs { tail: None },
        );

        assert_eq!(code, ExitCode::Success);
        assert!(String::from_utf8(out).unwrap().contains("web"));
    }
}
