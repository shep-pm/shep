//! What the unix init units and the Windows logon task share: the result
//! words of a step row, how a run of rows is reported, and XML text escaping.
//! Compiled on every target, so the unix-only `startup` module can stay `cfg(unix)`.

use crate::exit::ExitCode;
use crate::output::{StartupStep, StartupSteps, Streams, emit, write_outcome};

/// A step that did what it was asked.
pub(crate) const OK: &str = "ok";

/// A step that found nothing to do, and that being the state it was asked to
/// produce. Only `unstartup` reaches it.
pub(crate) const ABSENT: &str = "absent";

/// A refusal that never got as far as a step: the exit code to return, and
/// the one line explaining it.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub(crate) code: ExitCode,
    pub(crate) message: String,
}

/// Emits the steps and returns the code they earned.
///
/// A step that failed fails the verb, but only after every remaining step has
/// run: a half-installed unit is worse than a fully-attempted one, and the
/// operator needs every row to know which half.
pub(crate) fn report(
    streams: &mut Streams<'_>,
    command: &str,
    steps: Vec<StartupStep>,
) -> ExitCode {
    let failed = steps
        .iter()
        .any(|step| step.result != OK && step.result != ABSENT);
    let written = write_outcome(emit(
        &mut *streams.out,
        streams.fmt,
        command,
        StartupSteps(steps),
        streams.style,
    ));
    if failed { ExitCode::Failure } else { written }
}

/// The one line a failed command is reported by: the first non-blank line of
/// its stderr, or its exit status when it failed without saying anything.
///
/// One line because a row is one line, and systemd answers a refusal with
/// several of its own advice.
pub(crate) fn failure_line(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(|| format!("failed: {}", output.status), ToString::to_string)
}

/// Escapes XML text content: `&` first, so it cannot re-escape the
/// entities this function just produced, then `<` and `>`.
pub(crate) fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
