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
/// [`super::Streams`] can use [`super::Streams::note`] instead.
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

#[cfg(test)]
mod tests {
    use crate::exit::ExitCode;

    use super::*;

    /// An implementation that always wrote prose (ignoring `fmt`) would fail
    /// this: `--format json` must still be parseable on a failure, not just
    /// on success.
    #[test]
    fn an_error_under_format_json_is_a_parseable_object() {
        let mut err = Vec::new();
        emit_error(
            &mut err,
            Format::Json,
            ExitCode::NotFound.code_str(),
            "no sheep matched",
        )
        .unwrap();

        let json: serde_json::Value = serde_json::from_slice(&err)
            .expect("under --format json a failure must be parseable, not prose");
        assert_eq!(json["schema_version"], SCHEMA_VERSION);
        assert_eq!(json["error"]["code"], "not_found");
        assert_eq!(json["error"]["message"], "no sheep matched");
    }

    /// An implementation that always JSON-encoded (ignoring `fmt`) would
    /// fail this: table mode is for a human at a terminal, not a script.
    #[test]
    fn an_error_under_format_table_is_plain_text() {
        let mut err = Vec::new();
        emit_error(
            &mut err,
            Format::Table,
            ExitCode::NotFound.code_str(),
            "no sheep matched",
        )
        .unwrap();
        let text = String::from_utf8(err).unwrap();
        assert!(text.contains("no sheep matched"));
        assert!(
            text.contains("not_found"),
            "table mode used to drop `code` silently; a human at a terminal needs the same \
             failure name a script would get from JSON: {text}"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&text).is_err(),
            "table mode is not JSON"
        );
    }

    /// A notice's JSON envelope keys on `notice`, not `error`: a consumer
    /// parsing `--format json` stderr must tell a diagnostic from a
    /// failure without also reading the process exit code.
    #[test]
    fn a_notice_under_format_json_uses_the_notice_key_not_the_error_key() {
        let mut err = Vec::new();
        emit_notice(
            &mut err,
            Format::Json,
            "daemon_shutdown",
            "the daemon is shutting down",
        )
        .unwrap();

        let json: serde_json::Value = serde_json::from_slice(&err)
            .expect("under --format json a notice must be parseable, not prose");
        assert_eq!(json["schema_version"], SCHEMA_VERSION);
        assert_eq!(json["notice"]["code"], "daemon_shutdown");
        assert_eq!(json["notice"]["message"], "the daemon is shutting down");
        assert!(
            json.get("error").is_none(),
            "a notice must not also carry an `error` key: {json}"
        );
    }

    /// `notice[code]: message`, not `error[code]: message`: the same
    /// grammar `emit_error` uses, but a different word.
    #[test]
    fn a_notice_under_format_table_is_plain_text_prefixed_notice() {
        let mut err = Vec::new();
        emit_notice(
            &mut err,
            Format::Table,
            "dropped",
            "the daemon dropped 3 events",
        )
        .unwrap();
        let text = String::from_utf8(err).unwrap();
        assert!(text.starts_with("notice[dropped]:"), "{text}");
        assert!(text.contains("the daemon dropped 3 events"));
    }

    // --- pin the wire bytes ----------------------------------------------

    // These three tests snapshot the literal bytes `emit_error`/
    // `emit_notice` write, in both formats, so a refactor across call
    // sites has something byte-exact to answer to.

    /// Both emitters, both formats: an ESC or BEL in the message must
    /// never reach the stream.
    #[test]
    fn no_escape_reaches_a_stream_through_either_emitter() {
        // `fetch.rs` sanitises the two error texts that come off the wire,
        // but the guarantee has to live where every caller passes through.
        let hostile = "cleared\u{1b}[2Jand\u{1b}]0;retitled\u{7}";
        for fmt in [Format::Table, Format::Json] {
            for (what, mut out) in [("error", Vec::new()), ("notice", Vec::new())] {
                if what == "error" {
                    emit_error(&mut out, fmt, "failure", hostile).unwrap();
                } else {
                    emit_notice(&mut out, fmt, "whatever", hostile).unwrap();
                }
                assert!(
                    !out.contains(&0x1b),
                    "{what} in {fmt:?} let an ESC through: {:?}",
                    String::from_utf8_lossy(&out)
                );
                assert!(
                    !out.contains(&0x07),
                    "{what} in {fmt:?} let a BEL through: {:?}",
                    String::from_utf8_lossy(&out)
                );
            }
        }
    }

    #[test]
    fn what_an_error_looks_like_on_the_wire() {
        for (fmt, name) in [(Format::Table, "table"), (Format::Json, "json")] {
            let mut out = Vec::new();
            emit_error(
                &mut out,
                fmt,
                ExitCode::Usage.code_str(),
                "no flock at /tmp/x",
            )
            .unwrap();
            insta::assert_snapshot!(format!("error_{name}"), String::from_utf8(out).unwrap());
        }
    }

    /// fails if a refusal's layout stops depending on the format. The
    /// remedy line is the point: an operator copies it, and a JSON consumer
    /// gets the same facts with no layout to parse around. The message is
    /// written with a plain `\n`; the indent in the table snapshot comes
    /// from the emitter.
    #[test]
    fn a_multiline_refusal_keeps_its_lines_in_a_table_and_loses_them_in_json() {
        let written = "no flock at /tmp/x\nto set up a flock there deliberately: mkdir -p /tmp/x";
        for (fmt, name) in [(Format::Table, "table"), (Format::Json, "json")] {
            let mut out = Vec::new();
            emit_error(&mut out, fmt, ExitCode::Usage.code_str(), written).unwrap();
            insta::assert_snapshot!(
                format!("error_multiline_{name}"),
                String::from_utf8(out).unwrap()
            );
        }
    }

    /// fails if a value carrying a newline can start a line of its own. Only
    /// the first line of a table message begins at column 0, so a forged
    /// `error[...]` lands indented and neither reads as shep's nor matches a
    /// script anchoring on the start of a line. This is what makes keeping
    /// `\n` safe rather than only useful, and it holds for every error type
    /// without one of them having to know the rule.
    #[test]
    fn an_interpolated_newline_cannot_start_a_line_of_its_own() {
        let forged = "could not read /tmp/a\nerror[internal]: shepherd compromised";
        let mut out = Vec::new();
        emit_error(
            &mut out,
            Format::Table,
            ExitCode::Failure.code_str(),
            forged,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some("error[failure]: could not read /tmp/a"),
            "{text:?}"
        );
        for line in lines {
            assert!(
                line.starts_with("  ") || line.is_empty(),
                "a line started at column 0: {line:?} in {text:?}"
            );
        }
    }

    /// fails if `code` can start a line. It prints ahead of the message on
    /// the first line, which is the one line `indent_continuations` cannot
    /// reach, so a newline there would forge a diagnostic at column 0 and the
    /// indent would never see it. Every caller passes a literal today; this
    /// is what keeps that from being load-bearing.
    #[test]
    fn a_newline_in_the_code_cannot_start_a_line_either() {
        for (what, mut out) in [("error", Vec::new()), ("notice", Vec::new())] {
            let forged = "usage\nnotice[ok]: forged";
            if what == "error" {
                emit_error(&mut out, Format::Table, forged, "a message").unwrap();
            } else {
                emit_notice(&mut out, Format::Table, forged, "a message").unwrap();
            }
            let text = String::from_utf8(out).unwrap();
            assert_eq!(text.lines().count(), 1, "{what}: {text:?}");
            assert!(
                text.starts_with(&format!("{what}[usage notice[ok]: forged]: ")),
                "{what}: the newline must collapse inside the code: {text:?}"
            );
        }
    }

    /// fails if a paragraph break gains trailing whitespace. `refuse_version_skew`
    /// separates its three parts with blank lines, and two spaces on one of
    /// them is invisible until it reaches a diff or a terminal that shows it.
    #[test]
    fn a_blank_line_between_paragraphs_stays_empty() {
        let mut out = Vec::new();
        emit_error(
            &mut out,
            Format::Table,
            ExitCode::VersionSkew.code_str(),
            "lead\n\nmiddle\n\ntail",
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains("\n  \n"),
            "blank line carried an indent: {text:?}"
        );
        assert!(text.contains("\n\n  middle\n\n  tail"), "{text:?}");
    }

    /// fails if a message's own trailing newline reaches the stream, where
    /// `writeln!` would add a second and print a blank line. `toml_edit`
    /// ends its parse errors with one.
    #[test]
    fn a_message_that_ends_in_a_newline_does_not_print_a_blank_line() {
        let mut out = Vec::new();
        emit_error(
            &mut out,
            Format::Table,
            ExitCode::InvalidConfig.code_str(),
            "invalid table header\nexpected `.`, `]`\n",
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with("]`\n"), "{text:?}");
        assert!(!text.ends_with("\n\n"), "{text:?}");
    }

    #[test]
    fn what_a_notice_looks_like_on_the_wire() {
        for (fmt, name) in [(Format::Table, "table"), (Format::Json, "json")] {
            let mut out = Vec::new();
            emit_notice(&mut out, fmt, "init", "wrote /tmp/x/Flockfile.toml").unwrap();
            insta::assert_snapshot!(format!("notice_{name}"), String::from_utf8(out).unwrap());
        }
    }

    /// Quotes and a backslash render differently in the two formats (JSON
    /// escapes them, the table surface prints them raw), so a message
    /// carrying both is what would catch a change to either rendering path
    /// that a plain-ASCII message would not.
    #[test]
    fn an_error_message_with_awkward_bytes_survives_both_formats() {
        for (fmt, name) in [(Format::Table, "table"), (Format::Json, "json")] {
            let mut out = Vec::new();
            emit_error(
                &mut out,
                fmt,
                ExitCode::InvalidConfig.code_str(),
                r#"bad "quoted" \path"#,
            )
            .unwrap();
            insta::assert_snapshot!(
                format!("error_awkward_{name}"),
                String::from_utf8(out).unwrap()
            );
        }
    }

    // --- Colour, and the face in the STATUS column ------------------------
}
