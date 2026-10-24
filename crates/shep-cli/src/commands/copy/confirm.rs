//! The confirmation gate: what the flags and a terminal add up to, and the
//! question a copy asks before it writes.
//!
//! A copy prints its change list and then asks. `--yes` skips the question,
//! `--dry-run` writes nothing, and a run with no terminal to ask on refuses
//! rather than guessing. Nothing here decides what a copy writes: the plan is
//! built before any of this runs, and the plan is what a write follows.

use std::io::BufRead;

use crate::output::Streams;

/// Whether this run writes, asks, refuses, or has nothing to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Go {
    /// Nothing differs, so there is nothing to write and nothing to ask about.
    Nothing,
    /// The operator asked for the changes and no write.
    DryRun,
    /// Write without asking.
    Write,
    /// Ask first, and write only on a yes.
    Ask,
    /// There is no terminal to ask on, and no `--yes` to skip it.
    Refuse,
}

/// What `--dry-run`, `--yes` and a terminal add up to.
#[must_use]
pub(crate) fn decide(dry_run: bool, yes: bool, stdin_is_terminal: bool, has_changes: bool) -> Go {
    if !has_changes {
        return Go::Nothing;
    }
    if dry_run {
        Go::DryRun
    } else if yes {
        Go::Write
    } else if stdin_is_terminal {
        Go::Ask
    } else {
        Go::Refuse
    }
}

/// What the operator answered, or why the answer could not be read.
#[derive(Debug)]
pub(crate) enum Answer {
    /// `y` or `yes`.
    Yes,
    /// Anything else, including no answer at all.
    No,
    /// The read itself failed, which is not a refusal to answer.
    Failed(String),
}

/// Asks the operator.
///
/// The question goes on stderr so a piped stdout stays what it was. A read
/// error is reported as one rather than folded into "no": an operator who
/// never saw the question is not the same as one who said no to it.
pub(crate) fn confirm(
    streams: &mut Streams<'_>,
    from: &str,
    to: &str,
    path: &str,
    changes: usize,
    reader: &mut dyn BufRead,
) -> Answer {
    let question = format!(
        "copy {changes} change{} from `{from}` onto `{to}` at `{path}`? [y/N] ",
        if changes == 1 { "" } else { "s" }
    );
    streams.aside("confirm", &question);
    read_answer(reader)
}

/// Reads one line from `reader` as a yes or a no.
///
/// A read error is reported as one rather than folded into "no": an operator
/// who never saw the question is not the same as one who said no to it.
pub(crate) fn read_answer(reader: &mut dyn BufRead) -> Answer {
    let mut answer = String::new();
    match reader.read_line(&mut answer) {
        Ok(0) => Answer::No,
        Ok(_) => {
            if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                Answer::Yes
            } else {
                Answer::No
            }
        }
        Err(err) => Answer::Failed(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Error as IoError, ErrorKind};

    use super::*;
    use crate::cli::Format;

    /// A read failure is not an answer: an operator who never saw the question
    /// is not one who said no to it.
    #[test]
    fn a_confirmation_read_failure_is_not_an_answer() {
        struct Unreadable;
        impl std::io::Read for Unreadable {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(IoError::new(ErrorKind::BrokenPipe, "stdin is gone"))
            }
        }
        impl BufRead for Unreadable {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                Err(IoError::new(ErrorKind::BrokenPipe, "stdin is gone"))
            }
            fn consume(&mut self, _len: usize) {}
        }

        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = crate::output::Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let mut ask = |reader: &mut dyn BufRead| {
            confirm(&mut streams, "web", "api", "dogs.kelpie", 2, reader)
        };

        let mut broken = Unreadable;
        let Answer::Failed(reason) = ask(&mut broken) else {
            panic!("a failed read is not an answer");
        };
        assert!(reason.contains("stdin is gone"), "{reason}");

        let mut yes = Cursor::new(b"y\n".as_slice());
        assert!(matches!(ask(&mut yes), Answer::Yes));
        let mut maybe = Cursor::new(b"maybe\n".as_slice());
        assert!(matches!(ask(&mut maybe), Answer::No));
        let mut silent = Cursor::new(b"".as_slice());
        assert!(matches!(ask(&mut silent), Answer::No));

        let asked = String::from_utf8(err).unwrap();
        assert_eq!(
            asked.matches("[y/N]").count(),
            4,
            "every one of them was asked the same question, on stderr"
        );
        assert!(asked.contains("copy 2 changes from `web` onto `api` at `dogs.kelpie`?"));
    }
}
