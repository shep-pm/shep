//! `bleats` (alias `logs`): following a sheep's log stream. The only
//! streaming verb, and the only one whose output skips
//! [`crate::output`]'s envelope: a follow has no end, so there is nothing
//! to wrap.
//!
//! One `Request::ListFlock` resolves an id -> [`ProcessInfo`] cache before
//! subscribing, since the daemon's topic filter carries no sheep identity:
//! selector filtering happens client-side against that cache.
//!
//! `--no-follow` never subscribes: it tails each matched sheep's log files
//! and exits. `--follow` prints that same tail first, then subscribes.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use futures_util::FutureExt;
use serde::Serialize;

use shep_client::{Client, EventStream, Lagged};
use shep_core::protocol::{BusEvent, ProcessInfo, Request, Response};
use shep_core::selector::ProcessSelector;

use crate::cli::{BleatsArgs, Format};
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::selector::parse_selector;
use crate::exit::ExitCode;
use crate::output::{self, Streams, write_outcome};

/// One line of `bleats` output under `--format json`. A stability surface of
/// its own, not wrapped in [`output::OutputEnvelope`]: a follow has no end.
#[derive(Debug, Serialize)]
struct BleatLine<'a> {
    /// [`output::SCHEMA_VERSION`] at the time this line was produced.
    schema_version: u32,
    /// The sheep's id.
    id: u32,
    /// The sheep's name if the initial listing resolved it, else the bare
    /// id rendered as a string.
    name: &'a str,
    /// Which of a sheep's two output streams this line came from.
    stream: &'static str,
    /// The instance slot this line's sheep occupies, when its app has more
    /// than one instance registered; `null` when the app has only one, or
    /// when the line's origin cannot be attributed to a single instance (a
    /// backlog line read from a file several instances share).
    instance: Option<u32>,
    /// The line itself, no trailing newline.
    line: &'a str,
}

/// Issues the one `Request::ListFlock` `bleats` sends, before it ever
/// subscribes, and turns the answer into an id -> [`ProcessInfo`] cache.
///
/// # Errors
/// Renders and returns the exit code for a request that failed to reach
/// the daemon, or a response this client does not recognise (`Response`
/// is `#[non_exhaustive]`).
async fn resolve_names(
    client: &Client,
    streams: &mut Streams<'_>,
) -> Result<HashMap<u32, ProcessInfo>, ExitCode> {
    match client.request(Request::ListFlock).await {
        Ok(Response::Flock(procs)) => Ok(procs.into_iter().map(|p| (p.id, p)).collect()),
        Ok(_unrecognised) => Err(unexpected_response(streams)),
        Err(err) => Err(client_error(streams, &err)),
    }
}

/// Subscribes to every topic `bleats` needs: `log.*` for the lines
/// themselves, `daemon.*` so a `BusEvent::DaemonShutdown` is observed
/// rather than the connection simply vanishing unexplained.
///
/// # Errors
/// Renders and returns the exit code for a subscribe request that failed.
async fn subscribe(client: &Client, streams: &mut Streams<'_>) -> Result<EventStream, ExitCode> {
    let topics = vec!["log.*".to_string(), "daemon.*".to_string()];
    match client.subscribe(topics).await {
        Ok(stream) => Ok(stream),
        Err(err) => {
            let code = ExitCode::from(&err);
            Err(streams.fail(code, &err.to_string()))
        }
    }
}

/// Resolves `id` to a name from `cache`, or the bare id if `id` was not in
/// the one listing `resolve_names` took. An id that shows up on the bus
/// later than that snapshot is not a reason to block on a second listing.
fn resolved_name(cache: &HashMap<u32, ProcessInfo>, id: u32) -> String {
    cache
        .get(&id)
        .map_or_else(|| id.to_string(), |info| info.name.clone())
}

/// The slot a followed line's sheep occupies, or `None` when `id` was not
/// in the one listing `resolve_names` took, or its app has only one
/// instance.
///
/// The daemon emits [`BusEvent::LogOut`]/[`LogErr`](BusEvent::LogErr) per
/// sheep, so a follow labels a line even when several instances share one
/// log file, unlike the backlog path.
fn resolved_instance(cache: &HashMap<u32, ProcessInfo>, id: u32) -> Option<u32> {
    let info = cache.get(&id)?;
    if instance_count(cache, &info.name) > 1 {
        info.instance
    } else {
        None
    }
}

/// Whether `selector` (parsed client-side) admits `id`, matched against
/// `cache`'s snapshot of that sheep if it has one.
///
/// An id the initial listing never saw has no name or fold to match
/// against, so it is matched with an empty name and no fold: enough for
/// `all` and for `ProcessSelector::Id`, while a name, regex or fold
/// selector excludes it.
fn selector_allows(selector: &ProcessSelector, cache: &HashMap<u32, ProcessInfo>, id: u32) -> bool {
    match cache.get(&id) {
        Some(info) => selector.matches(&info.name, info.id, info.fold.as_deref(), info.instance),
        None => selector.matches("", id, None, None),
    }
}

/// Writes one rendered line to `out`. `stream` is `"out"` or `"err"`, the
/// sheep stream the line came from, not `out`'s own identity: every line
/// this function is called with lands on [`Streams::out`].
fn write_line(
    out: &mut dyn io::Write,
    fmt: Format,
    id: u32,
    name: &str,
    instance: Option<u32>,
    stream: &'static str,
    line: &str,
) -> io::Result<()> {
    match fmt {
        Format::Json => {
            let payload = BleatLine {
                schema_version: output::SCHEMA_VERSION,
                id,
                name,
                instance,
                stream,
                line,
            };
            serde_json::to_writer(&mut *out, &payload)?;
            writeln!(out)
        }
        Format::Table => match instance {
            Some(slot) => writeln!(out, "{name}:{slot} | {line}"),
            None => writeln!(out, "{name} | {line}"),
        },
    }
}

/// How many rows of `cache` carry `name`, counted over the whole cache and
/// never a selector's matched subset, so a selector cannot change how a line
/// is labelled: `shep bleats web:0` still prints `web:0`, not `web`.
fn instance_count(cache: &HashMap<u32, ProcessInfo>, name: &str) -> usize {
    cache.values().filter(|info| info.name == name).count()
}

/// One of `bleats`' own notices, unless `--quiet` asked for silence.
///
/// Goes out through [`output::emit_notice`], not [`output::emit_error`]:
/// its code is not part of [`crate::exit::ExitCode`]'s taxonomy, and a
/// clean run can emit one on its way to exit 0. The `quiet` gate is this
/// verb's own, not [`Streams::aside`]'s; a sheep's own line and a real
/// error both still print under it.
fn write_notice(streams: &mut Streams<'_>, quiet: bool, code: &str, message: &str) {
    if quiet {
        return;
    }
    streams.aside(code, message);
}

/// The most of one log file a tail will read to find the lines it wants.
///
/// Binds only when lines average over 5 KiB, so in ordinary use the caller's
/// line count is the bound that decides. A line count alone cannot bound
/// memory: one arbitrarily long line with no newline would defeat it.
const TAIL_WINDOW_BYTES: u64 = 256 * 1024;

/// The last `limit` lines of one log file, bounded twice: a
/// [`TAIL_WINDOW_BYTES`] window from the end of the file, then `limit`
/// lines within it. Returns the lines and whether either bound cut them
/// short.
///
/// `std::fs`, not `tokio::fs`: shep-cli's tokio has no `fs` feature. Each
/// line loses its daemon-added timestamp ([`shep_core::logstamp`]), so it
/// reads the same as a line from the bus. A non-zero seek discards bytes
/// up to the first `\n`, rather than rendering a mid-line fragment.
///
/// # Errors
/// The file could not be opened, `stat`ed, seeked, or read.
pub(crate) fn read_tail(path: &Path, limit: usize) -> io::Result<(Vec<String>, bool)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL_WINDOW_BYTES);
    // `start > 0` means the byte window itself left content behind, before
    // a single line has been counted: the file is bigger than the window.
    let window_truncated = start > 0;
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }

    let mut window = Vec::new();
    file.read_to_end(&mut window)?;

    let window: &[u8] = if start > 0 {
        match window.iter().position(|&b| b == b'\n') {
            Some(newline) => &window[newline + 1..],
            None => &[],
        }
    } else {
        &window
    };

    let text = String::from_utf8_lossy(window);
    // The daemon's per-line stamp comes off here, so a `line` has one
    // meaning across both of this verb's paths: the follow path reads the
    // bus, which carries a sheep's own bytes. The stamp stays in the file
    // for `tail`, `less` and `grep`.
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| shep_core::logstamp::strip(line).to_string())
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let keep_from = lines.len().saturating_sub(limit);
    let truncated = window_truncated || keep_from > 0;
    lines.drain(..keep_from);
    Ok((lines, truncated))
}

/// Why a log file is not there, naming the path this process tried.
///
/// `stream` is `"out"` or `"err"`. Each process resolves a relative
/// `out_file`/`err_file` against its own directory, so the absolute form is
/// what shows that the shepherd is writing a different file.
pub(crate) fn missing_log_note(stream: &str, path: &Path) -> String {
    if !path.is_relative() {
        return format!("no {stream} log at {} yet", path.display());
    }
    let tried = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    format!(
        "no {stream} log at {}; {stream}_file is relative, so the shepherd \
         resolves it against its own directory instead",
        tried.display()
    )
}

/// Renders the selected files of every sheep the selector admits, in flock
/// order, and returns the exit code that reports how that went.
///
/// Within one sheep, `out_file` (unless `--err`) prints before `err_file`
/// (unless `--out`), with no merge between them. `cache` is a `HashMap`, so
/// matched sheep are sorted first, by name, then instance slot, then id.
///
/// A `None` path (a shepherd predating
/// [`shep_core::protocol::ProcessInfo::out_file`]) is a `log_path_unknown`
/// notice. A file that is not there is a `log_missing` notice and leaves
/// the exit code alone. Any other read failure is a `log_unreadable`
/// notice and sets [`ExitCode::Failure`]; the rest of the flock still
/// prints.
fn tail_log_files(
    streams: &mut Streams<'_>,
    quiet: bool,
    cache: &HashMap<u32, ProcessInfo>,
    selector: &ProcessSelector,
    args: &BleatsArgs,
) -> ExitCode {
    let mut matched: Vec<&ProcessInfo> = cache
        .values()
        .filter(|info| selector.matches(&info.name, info.id, info.fold.as_deref(), info.instance))
        .collect();
    // `(name, instance, id)`, the key `shep_core::protocol::sort_flock`
    // takes, though not that helper: these are `&ProcessInfo` borrowed out of
    // the cache. Without the slot, a reloaded app's instances sort wrong, a
    // reload giving slot 0 a fresh high id.
    matched.sort_unstable_by(|a, b| {
        (a.name.as_str(), a.instance, a.id).cmp(&(b.name.as_str(), b.instance, b.id))
    });

    let mut failure = false;

    // One file, one read. Several instances can resolve to one path: every
    // `merge_logs` app does, and so does any app that set `out_file`
    // explicitly.
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut seen_notices: HashSet<String> = HashSet::new();

    // Whether a path is shared between several rows, over the whole cache
    // rather than the matched subset: a selector narrowing to one row must
    // not hide that the file is still shared.
    let mut path_owners: HashMap<(&'static str, String), usize> = HashMap::new();
    for info in cache.values() {
        for (stream_name, path) in [
            ("out", info.out_file.as_deref()),
            ("err", info.err_file.as_deref()),
        ] {
            if let Some(path) = path {
                *path_owners
                    .entry((stream_name, path.to_string()))
                    .or_insert(0) += 1;
            }
        }
    }

    for info in matched {
        let name = &info.name;
        let wanted: [(&'static str, Option<&str>, bool); 2] = [
            ("out", info.out_file.as_deref(), !args.err),
            ("err", info.err_file.as_deref(), !args.out),
        ];
        for (stream_name, path, show) in wanted {
            if !show {
                continue;
            }
            match path {
                None => {
                    // No path to key on, since the missing field is why this
                    // fires. The message already names the pair that varies.
                    let message =
                        format!("{name}: the daemon did not report a {stream_name} log path");
                    if seen_notices.insert(message.clone()) {
                        write_notice(streams, quiet, "log_path_unknown", &message);
                    }
                }
                Some(path) => {
                    if !seen_paths.insert(path.to_string()) {
                        continue;
                    }
                    // Only label a backlog line with a slot when this path
                    // belongs to exactly one row: instances sharing one file
                    // interleave in it and no line says who wrote it.
                    let shared = path_owners
                        .get(&(stream_name, path.to_string()))
                        .copied()
                        .unwrap_or(0)
                        > 1;
                    let label_instance = if !shared && instance_count(cache, name) > 1 {
                        info.instance
                    } else {
                        None
                    };
                    match read_tail(Path::new(path), args.lines) {
                        Ok((lines, _truncated)) => {
                            for line in lines {
                                if let Err(write_err) = write_line(
                                    streams.out,
                                    streams.fmt,
                                    info.id,
                                    name,
                                    label_instance,
                                    stream_name,
                                    &line,
                                ) {
                                    let code = write_outcome(Err(write_err));
                                    let _ = streams.out.flush();
                                    return code;
                                }
                            }
                        }
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            write_notice(
                                streams,
                                quiet,
                                "log_missing",
                                &format!(
                                    "{name}: {}",
                                    missing_log_note(stream_name, Path::new(path))
                                ),
                            );
                        }
                        Err(err) => {
                            failure = true;
                            write_notice(
                                streams,
                                quiet,
                                "log_unreadable",
                                &format!("failed to read {path}: {err}"),
                            );
                        }
                    }
                }
            }
        }
    }

    let _ = streams.out.flush();
    if failure {
        ExitCode::Failure
    } else {
        ExitCode::Success
    }
}

/// Handles one [`BusEvent`] already known to be `Ok` (a `Lagged` item is
/// handled by the caller, not here).
///
/// `BusEvent` is `#[non_exhaustive]`: the `_` arm silently ignores anything
/// this client does not recognise, since a follow must not die on a bus event
/// a newer daemon added. `Dropped` is a named variant this client
/// understands, so it gets its own arm.
fn handle_event(
    streams: &mut Streams<'_>,
    quiet: bool,
    cache: &HashMap<u32, ProcessInfo>,
    selector: &ProcessSelector,
    args: &BleatsArgs,
    event: BusEvent,
) -> io::Result<()> {
    match event {
        BusEvent::LogOut { id, line } => {
            if !args.err && selector_allows(selector, cache, id) {
                let name = resolved_name(cache, id);
                let instance = resolved_instance(cache, id);
                write_line(streams.out, streams.fmt, id, &name, instance, "out", &line)?;
            }
            Ok(())
        }
        BusEvent::LogErr { id, line } => {
            if !args.out && selector_allows(selector, cache, id) {
                let name = resolved_name(cache, id);
                let instance = resolved_instance(cache, id);
                write_line(streams.out, streams.fmt, id, &name, instance, "err", &line)?;
            }
            Ok(())
        }
        BusEvent::Dropped { count } => {
            // Worded apart from the `Lagged` arm below: `Dropped` is the
            // daemon's own outbound queue overflowing for this subscriber,
            // `Lagged` is this client's receiver falling behind reading its
            // socket. Opposite ends of the connection to investigate.
            write_notice(
                streams,
                quiet,
                "dropped",
                &format!("the daemon dropped {count} events (its own queue overflowed)"),
            );
            Ok(())
        }
        BusEvent::DaemonShutdown => {
            // Shep's own diagnostic, not a sheep's line: `streams.err`.
            write_notice(
                streams,
                quiet,
                "daemon_shutdown",
                "the daemon is shutting down",
            );
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Follows the bleats (log output) of the sheep matching `args.selector`.
///
/// `quiet` is `cli::GlobalArgs::quiet`: it silences this module's own notices
/// and nothing else. A sheep's own line and a real error both still print.
///
/// Delegates to [`bleats_with_signal`] with a real `SIGINT` as the interrupt
/// source.
pub async fn bleats(
    client: &Client,
    streams: &mut Streams<'_>,
    quiet: bool,
    args: &BleatsArgs,
) -> ExitCode {
    bleats_with_signal(
        client,
        streams,
        quiet,
        args,
        tokio::signal::ctrl_c().map(|_| ()),
    )
    .await
}

/// [`bleats`] with the interrupt injected, so the Ctrl-C branch has a test
/// that does not need a real `SIGINT`.
///
/// One `Request::ListFlock` builds the id -> name cache both paths share.
/// `--no-follow` stops there and hands off to [`tail_log_files`], issuing
/// no `Request::Subscribe`. `--follow` subscribes and loops on
/// `tokio::select!`: a line renders, a `Lagged` item is noted and the
/// follow continues, the stream ending means the daemon is gone
/// ([`ExitCode::DaemonUnreachable`]), and `interrupt` firing exits
/// [`ExitCode::Success`].
///
/// `streams.out` is flushed on every exit path, or buffered lines are lost.
pub async fn bleats_with_signal(
    client: &Client,
    streams: &mut Streams<'_>,
    quiet: bool,
    args: &BleatsArgs,
    interrupt: impl std::future::Future<Output = ()> + Send,
) -> ExitCode {
    let selector = match parse_selector(streams, &args.selector) {
        Ok(selector) => selector,
        Err(code) => return code,
    };

    // The id/name cache is built from one listing taken before subscribing.
    // Subscribing first would lose every line pushed while the listing is
    // still in flight.
    let cache = match resolve_names(client, streams).await {
        Ok(cache) => cache,
        Err(code) => return code,
    };

    if args.no_follow {
        return tail_log_files(streams, quiet, &cache, &selector, args);
    }

    // Backlog before subscribing, so a line in the gap is missed rather
    // than printed twice. The tail's exit code is discarded: an unreadable
    // log for one sheep must not stop the follow over the whole flock.
    if args.lines > 0 {
        let _ = tail_log_files(streams, quiet, &cache, &selector, args);
    }

    let mut stream = match subscribe(client, streams).await {
        Ok(stream) => stream,
        Err(code) => return code,
    };

    tokio::pin!(interrupt);

    loop {
        tokio::select! {
            biased;
            item = stream.next() => {
                match item {
                    Some(Ok(event)) => {
                        if let Err(write_err) =
                            handle_event(streams, quiet, &cache, &selector, args, event)
                        {
                            let code = write_outcome(Err(write_err));
                            let _ = streams.out.flush();
                            return code;
                        }
                    }
                    Some(Err(Lagged { count })) => {
                        write_notice(
                            streams,
                            quiet,
                            "lagged",
                            &format!("{count} events dropped locally (lagged)"),
                        );
                    }
                    None => {
                        let _ = streams.out.flush();
                        return ExitCode::DaemonUnreachable;
                    }
                }
            }
            () = &mut interrupt => {
                let _ = streams.out.flush();
                return ExitCode::Success;
            }
        }
    }
}

#[cfg(test)]
mod tests;
