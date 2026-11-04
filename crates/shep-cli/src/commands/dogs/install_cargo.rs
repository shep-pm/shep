//! Running `cargo install` for `shep install`, on a terminal or inside a
//! session.
//!
//! On a terminal cargo narrates on the operator's stderr. With a sink set
//! its stderr is read to the end instead, so it cannot draw over lookout, and
//! a failure carries the tail of it.

use std::path::Path;
use std::process::{ExitStatus, Stdio};

use super::session::Progress;

/// The most bytes of cargo's stderr a failure message carries: the same
/// four kibibytes lookout's output modal shows.
const TAIL_CAP: usize = 4096;

/// How a `cargo install` ended.
pub(super) struct Ran {
    /// The exit status.
    pub(super) status: ExitStatus,
    /// The tail of cargo's stderr, empty when it went to the terminal.
    pub(super) tail: String,
}

/// Runs `cargo` with `args`, stdin closed and stdout dropped: stdout is the
/// verb's answer, and cargo narrates on stderr.
///
/// # Errors
/// `cargo` could not be started or waited on.
pub(super) async fn run(
    cargo: &Path,
    args: &[String],
    progress: Option<&Progress>,
) -> std::io::Result<Ran> {
    let mut command = tokio::process::Command::new(cargo);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    if progress.is_none() {
        let status = command.status().await?;
        return Ok(Ran {
            status,
            tail: String::new(),
        });
    }
    let output = command.stderr(Stdio::piped()).output().await?;
    Ok(Ran {
        status: output.status,
        tail: tail_of(&output.stderr),
    })
}

/// `message` followed by what cargo said, when it said anything.
pub(super) fn with_tail(message: String, tail: &str) -> String {
    if tail.is_empty() {
        message
    } else {
        format!("{message}\ncargo said:\n{tail}")
    }
}

/// The last [`TAIL_CAP`] bytes of `stderr` on a character boundary, with a
/// leading `...` when something was cut.
fn tail_of(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim_end();
    if text.len() <= TAIL_CAP {
        return text.to_owned();
    }
    let mut start = text.len() - TAIL_CAP;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &text[start..])
}
