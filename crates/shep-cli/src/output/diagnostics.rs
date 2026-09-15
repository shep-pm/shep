//! `emit_error` and `emit_notice`: the two envelope shapes for a failure and
//! a non-failure diagnostic, and the sanitising both pass through.

use std::io;

use serde::Serialize;

use crate::cli::Format;

use super::SCHEMA_VERSION;

/// The `--format json` shape of a failure: `{"schema_version", "error":
/// {"code", "message"}}`.
#[derive(Debug, Serialize)]
struct ErrorEnvelope<'a> {
    schema_version: u32,
    error: ErrorBody<'a>,
}

/// The `error` object inside [`ErrorEnvelope`].
#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

/// The two strings an emitter prints, both cleaned: `code` stripped of
/// anything that could drive a terminal, `message` in the shape `fmt`
/// renders.
///
/// Both emitters go through here, so the two cannot sanitise differently.
fn safe_parts(fmt: Format, code: &str, message: &str) -> (String, String) {
    (
        crate::terminal_safe::sanitise(code).0,
        safe_message(fmt, message),
    )
}

/// `message` with everything that could drive a terminal stripped, in the
/// shape `fmt` renders.
///
/// The seam every emitted message passes through, which is why the
/// guarantee lives here rather than at each caller. JSON collapses to one
/// line: `jq -r .error.message` unescapes a control byte straight back
/// onto a terminal. A table keeps the line breaks shep wrote, indents every
/// one of them, and loses its trailing whitespace, which the caller's
/// `writeln!` would otherwise print as a blank line.
fn safe_message(fmt: Format, message: &str) -> String {
    match fmt {
        Format::Json => crate::terminal_safe::sanitise(message).0,
        Format::Table => {
            let clean = crate::terminal_safe::sanitise_multiline(message).0;
            indent_continuations(clean.trim_end())
        }
    }
}

/// `message` with every line after the first indented by two spaces.
///
/// Only the first line of a table message starts at column 0, so a newline
/// inside an interpolated value cannot forge a line that reads as shep's own
/// or that a script anchoring `error[` there will match. It holds because
/// `code` is sanitised too, printing ahead of it on that same line. The base
/// indent is this function's, and a message adds its own only to nest a line
/// under a label. An empty line stays empty rather than gaining whitespace.
fn indent_continuations(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    for (n, line) in message.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
            if !line.is_empty() {
                out.push_str("  ");
            }
        }
        out.push_str(line);
    }
    out
}

/// Renders a failure to `err` in `fmt`. `code` is `ExitCode::code_str()`.
///
/// `code` is a string this function only prints, not the exit code, but
/// it prints on both surfaces: JSON carries it in `error.code`, and
/// table mode names it too, so a human at a terminal sees the same
/// failure name a script would.
///
/// # Errors
/// The underlying write failed.
pub fn emit_error(
    err: &mut dyn io::Write,
    fmt: Format,
    code: &str,
    message: &str,
) -> io::Result<()> {
    let (code, message) = safe_parts(fmt, code, message);
    let (code, message) = (code.as_str(), message.as_str());
    match fmt {
        Format::Json => {
            let envelope = ErrorEnvelope {
                schema_version: SCHEMA_VERSION,
                error: ErrorBody { code, message },
            };
            serde_json::to_writer(&mut *err, &envelope)?;
            writeln!(err)
        }
        Format::Table => writeln!(err, "error[{code}]: {message}"),
    }
}

/// The `--format json` shape of a non-failure diagnostic: `{"schema_version",
/// "notice": {"code", "message"}}`.
///
/// A sibling of [`ErrorEnvelope`], not a reuse of it: a notice must not
/// read as a failure on the wire, so it gets its own envelope key.
///
/// Only ever constructed by [`emit_notice`]. `#[cfg_attr(windows,
/// allow(dead_code))]`: every caller lives in `commands/` or `lib.rs`'s
/// `#[cfg(unix)]` arms.
#[derive(Debug, Serialize)]
#[cfg_attr(windows, allow(dead_code))]
struct NoticeEnvelope<'a> {
    schema_version: u32,
    notice: NoticeBody<'a>,
}

/// The `notice` object inside [`NoticeEnvelope`].
#[derive(Debug, Serialize)]
#[cfg_attr(windows, allow(dead_code))]
struct NoticeBody<'a> {
    code: &'a str,
    message: &'a str,
}

/// Renders a non-failure diagnostic to `out` in `fmt`, keyed differently
/// than [`emit_error`] so a `--format json` consumer can tell a
/// diagnostic from a failure without checking the exit code.
///
/// `out` is a plain parameter: a notice beside a separate primary output
/// passes `streams.err`; one that is the command's whole answer passes
/// `streams.out`. `code` is caller-defined, never part of
/// `emit_error`'s exit-code taxonomy. A caller already holding a
/// [`Streams`] can use [`Streams::note`] instead.
///
/// # Errors
/// The underlying write failed.
#[cfg_attr(windows, allow(dead_code))]
pub fn emit_notice(
    out: &mut dyn io::Write,
    fmt: Format,
    code: &str,
    message: &str,
) -> io::Result<()> {
    let (code, message) = safe_parts(fmt, code, message);
    let (code, message) = (code.as_str(), message.as_str());
    match fmt {
        Format::Json => {
            let envelope = NoticeEnvelope {
                schema_version: SCHEMA_VERSION,
                notice: NoticeBody { code, message },
            };
            serde_json::to_writer(&mut *out, &envelope)?;
            writeln!(out)
        }
        Format::Table => writeln!(out, "notice[{code}]: {message}"),
    }
}
