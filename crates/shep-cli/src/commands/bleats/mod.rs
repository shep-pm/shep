//! `bleats` (alias `logs`): following a sheep's log stream, the only
//! streaming verb and the only one whose output skips [`crate::output`]'s
//! envelope: a follow has no end, so there is nothing to wrap.
//!
//! One `Request::ListFlock` resolves an id -> [`ProcessInfo`] cache before
//! subscribing: the daemon's topic filter carries no sheep identity.
//!
//! `--no-follow` tails each matched sheep's log files and exits; `--follow`
//! prints that same tail, then subscribes. Under `--host` the remote's
//! shepherd supplies each tail through [`source::LogSource`].

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use futures_util::FutureExt;
use serde::Serialize;

use shep_client::{Client, EventStream, Lagged, RequestError};
use shep_core::protocol::{BusEvent, LogStream, ProcessInfo};
use shep_core::selector::ProcessSelector;

use crate::cli::{BleatsArgs, Format};
use crate::commands::rpc::typed_request_error;
use crate::commands::selector::parse_selector;
use crate::exit::ExitCode;
use crate::output::{self, Streams, write_outcome};
use crate::remote::interrupt_within_grace;
use crate::shutdown::Interrupt;

mod source;
use source::{FileTail, LogSource, stream_label};

pub(crate) use shep_core::logtail::read_tail;
// Only the tests size a file against the window.
#[cfg(test)]
pub(crate) use shep_core::logtail::TAIL_WINDOW_BYTES;

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
/// The request failed to reach the daemon, or the answer is one this client
/// does not recognise (`Response` is `#[non_exhaustive]`). Reporting it is
/// the caller's, so one can be explained away as the operator's interrupt
/// before anything is printed.
async fn resolve_names(client: &Client) -> Result<HashMap<u32, ProcessInfo>, RequestError> {
    client
        .list_flock()
        .await
        .map(|procs| procs.into_iter().map(|p| (p.id, p)).collect())
}

/// Subscribes to every topic `bleats` needs: `log.*` for the lines
/// themselves, `daemon.*` so a `BusEvent::DaemonShutdown` is observed
/// rather than the connection simply vanishing unexplained.
///
/// # Errors
/// The subscribe request failed. Reporting it is the caller's, for the same
/// reason as [`resolve_names`].
async fn subscribe(client: &Client) -> Result<EventStream, RequestError> {
    let topics = vec!["log.*".to_string(), "daemon.*".to_string()];
    client.subscribe(topics).await
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
fn resolved_instance(
    cache: &HashMap<u32, ProcessInfo>,
    counts: &HashMap<String, usize>,
    id: u32,
) -> Option<u32> {
    let info = cache.get(&id)?;
    if counts.get(info.name.as_str()).copied().unwrap_or(0) > 1 {
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

/// Writes one line, the backlog path and the follow path both through
/// here, so a replayed line and a followed line render identically.
///
/// `stream` is `"out"` or `"err"`, the sheep stream the line came from,
/// not `out`'s own identity: every line here lands on [`Streams::out`].
/// Table format sanitises the line and colours the line text, never the
/// `name |` label; JSON carries the raw line.
fn write_line(
    streams: &mut Streams<'_>,
    id: u32,
    name: &str,
    instance: Option<u32>,
    stream: &'static str,
    line: &str,
) -> io::Result<()> {
    match streams.fmt {
        Format::Json => {
            let payload = BleatLine {
                schema_version: output::SCHEMA_VERSION,
                id,
                name,
                instance,
                stream,
                line,
            };
            serde_json::to_writer(&mut *streams.out, &payload)?;
            writeln!(streams.out)
        }
        Format::Table => {
            let clean = output::log_line::sanitize_log_line(line);
            let text = output::log_line::paint_default(&clean, stream, streams.style);
            match instance {
                Some(slot) => writeln!(streams.out, "{name}:{slot} | {text}"),
                None => writeln!(streams.out, "{name} | {text}"),
            }
        }
    }
}

/// How many rows of `cache` carry `name`, counted over the whole cache and
/// never a selector's matched subset, so a selector cannot change how a line
/// is labelled: `shep bleats web:0` still prints `web:0`, not `web`.
fn instance_count(cache: &HashMap<u32, ProcessInfo>, name: &str) -> usize {
    cache.values().filter(|info| info.name == name).count()
}

/// Every name `cache` holds, with how many rows carry it. Same counting
/// rule as [`instance_count`]: over the whole cache, never a selector's
/// matched subset.
///
/// Built once per follow, from the one listing [`resolve_names`] took.
/// That cache never changes after it, so a followed line's label costs one
/// lookup here rather than a scan of every process in the flock.
fn instance_counts(cache: &HashMap<u32, ProcessInfo>) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::with_capacity(cache.len());
    for info in cache.values() {
        *counts.entry(info.name.clone()).or_insert(0) += 1;
    }
    counts
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

/// The `log_path_unknown` notice for `name`'s `stream`, said once however
/// many rows share it.
///
/// No path to key on, since the missing field is why this fires. The message
/// already names the pair that varies.
fn path_unknown_notice(
    streams: &mut Streams<'_>,
    quiet: bool,
    seen_notices: &mut HashSet<String>,
    name: &str,
    stream: LogStream,
) {
    let message = format!(
        "{name}: the daemon did not report a {} log path",
        stream_label(stream)
    );
    if seen_notices.insert(message.clone()) {
        write_notice(streams, quiet, "log_path_unknown", &message);
    }
}

/// Renders the selected files of every sheep the selector admits, in flock
/// order, and returns the exit code that reports how that went.
///
/// Within one sheep, `out_file` (unless `--err`) prints before `err_file`
/// (unless `--out`), with no merge. `cache` is a `HashMap`, so matched sheep
/// are sorted by name, then instance slot, then id.
///
/// A `None` path is a `log_path_unknown` notice, a missing file a
/// `log_missing` notice that leaves the exit code alone, and any other read
/// failure a `log_unreadable` notice that sets [`ExitCode::Failure`].
///
/// # Errors
/// The [`RequestError`] from a read over a host that got no answer.
async fn tail_log_files(
    streams: &mut Streams<'_>,
    quiet: bool,
    cache: &HashMap<u32, ProcessInfo>,
    selector: &ProcessSelector,
    args: &BleatsArgs,
    source: LogSource<'_>,
) -> Result<ExitCode, RequestError> {
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
    // Every path here is borrowed out of `cache`, which outlives this call.
    let mut seen_paths: HashSet<&str> = HashSet::new();
    let mut seen_notices: HashSet<String> = HashSet::new();

    // Whether a path is shared between several rows, over the whole cache
    // rather than the matched subset: a selector narrowing to one row must
    // not hide that the file is still shared.
    let mut path_owners: HashMap<(&'static str, &str), usize> = HashMap::new();
    for info in cache.values() {
        for (stream_name, path) in [
            ("out", info.out_file.as_deref()),
            ("err", info.err_file.as_deref()),
        ] {
            if let Some(path) = path {
                *path_owners.entry((stream_name, path)).or_insert(0) += 1;
            }
        }
    }

    for info in matched {
        let name = &info.name;
        let wanted: [(LogStream, Option<&str>, bool); 2] = [
            (LogStream::Out, info.out_file.as_deref(), !args.err),
            (LogStream::Err, info.err_file.as_deref(), !args.out),
        ];
        for (stream, path, show) in wanted {
            if !show {
                continue;
            }
            let stream_name = stream_label(stream);
            let Some(path) = path else {
                path_unknown_notice(streams, quiet, &mut seen_notices, name, stream);
                continue;
            };
            if !seen_paths.insert(path) {
                continue;
            }
            // Only label a backlog line with a slot when this path belongs to
            // exactly one row: instances sharing one file interleave in it and
            // no line says who wrote it.
            let shared = path_owners.get(&(stream_name, path)).copied().unwrap_or(0) > 1;
            let label_instance = if !shared && instance_count(cache, name) > 1 {
                info.instance
            } else {
                None
            };
            match source.read(info, stream, path, args.lines).await {
                Ok(FileTail::Lines(lines)) => {
                    for line in lines {
                        if let Err(write_err) =
                            write_line(streams, info.id, name, label_instance, stream_name, &line)
                        {
                            let code = write_outcome(Err(write_err));
                            let _ = streams.out.flush();
                            return Ok(code);
                        }
                    }
                }
                Ok(FileTail::NoPath) => {
                    path_unknown_notice(streams, quiet, &mut seen_notices, name, stream);
                }
                Ok(FileTail::Missing(note)) => {
                    write_notice(streams, quiet, "log_missing", &format!("{name}: {note}"));
                }
                Ok(FileTail::Unreadable(message)) => {
                    failure = true;
                    write_notice(streams, quiet, "log_unreadable", &message);
                }
                Err(err) => return Err(err),
            }
        }
    }

    let _ = streams.out.flush();
    Ok(if failure {
        ExitCode::Failure
    } else {
        ExitCode::Success
    })
}

/// The reporter [`link_gone`] uses for a tail read a host gave no answer to:
/// one failure naming the host and the remedy, not one per file.
fn stop_a_read(source: LogSource<'_>) -> impl FnOnce(&mut Streams<'_>, &RequestError) -> ExitCode {
    move |streams, err| source.stop(streams, err)
}

/// Handles one followed log line, shared by the `LogOut` and `LogErr` arms
/// of [`handle_event`], which differ only in `stream` and `suppressed`.
///
/// `stream` is `"out"` or `"err"`, the sheep stream the line came from.
/// `suppressed` is the flag saying the operator asked not to see it:
/// `args.err` for an out line, `args.out` for an err one.
#[allow(clippy::too_many_arguments)]
fn handle_log_line(
    streams: &mut Streams<'_>,
    cache: &HashMap<u32, ProcessInfo>,
    counts: &HashMap<String, usize>,
    selector: &ProcessSelector,
    id: u32,
    line: &str,
    stream: &'static str,
    suppressed: bool,
) -> io::Result<()> {
    if !suppressed && selector_allows(selector, cache, id) {
        let name = resolved_name(cache, id);
        let instance = resolved_instance(cache, counts, id);
        write_line(streams, id, &name, instance, stream, line)?;
    }
    Ok(())
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
    counts: &HashMap<String, usize>,
    selector: &ProcessSelector,
    args: &BleatsArgs,
    event: BusEvent,
) -> io::Result<()> {
    match event {
        BusEvent::LogOut { id, line } => {
            handle_log_line(streams, cache, counts, selector, id, &line, "out", args.err)
        }
        BusEvent::LogErr { id, line } => {
            handle_log_line(streams, cache, counts, selector, id, &line, "err", args.out)
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
/// Delegates to [`bleats_armed`] with the interrupt listener installed here.
/// The shepherd is this machine's: see [`bleats_on_host`] for a host's.
pub async fn bleats(
    client: &Client,
    streams: &mut Streams<'_>,
    quiet: bool,
    args: &BleatsArgs,
) -> ExitCode {
    bleats_armed(client, streams, quiet, args, None).await
}

/// [`bleats`] for the shepherd on `host`, where `client` is the ssh link and
/// not a local socket.
///
/// The caller says so, rather than this reading it off `client`, because the
/// cost of getting it wrong is a tail opened from the remote's paths on this
/// machine. Every file is read by that shepherd.
pub async fn bleats_on_host(
    client: &Client,
    streams: &mut Streams<'_>,
    quiet: bool,
    host: &str,
    args: &BleatsArgs,
) -> ExitCode {
    bleats_armed(client, streams, quiet, args, Some(host)).await
}

/// [`bleats_over`] with the operator's Ctrl+C listened for from here.
///
/// Installed here rather than awaited through [`tokio::signal::ctrl_c`],
/// which registers its handler on its first poll: a verb reaches that poll
/// only after its listing, its backlog reads and its `Subscribe` round trip.
/// A `SIGINT` landing before a handler exists reaches the default disposition
/// and kills this process. Registering here means tokio queues one that lands
/// early, and [`raced`] gives every step of the verb the chance to read it.
///
/// # Errors
/// Renders and returns the exit code for a listener this OS refuses to
/// register, which leaves the verb with no way to end on Ctrl+C.
async fn bleats_armed(
    client: &Client,
    streams: &mut Streams<'_>,
    quiet: bool,
    args: &BleatsArgs,
    host: Option<&str>,
) -> ExitCode {
    let mut interrupt = match Interrupt::install() {
        Ok(interrupt) => interrupt,
        Err(err) => {
            let message = format!("listening for an interrupt: {err}");
            return streams.fail(ExitCode::Failure, &message);
        }
    };
    bleats_over(
        client,
        streams,
        quiet,
        args,
        host,
        interrupt.recv().map(|_| ()),
    )
    .await
}

/// [`bleats`] with the interrupt injected, so the Ctrl-C branch has a test
/// that does not need a real `SIGINT`.
///
/// One `Request::ListFlock` builds the id -> name cache both paths share.
/// `--no-follow` hands off to [`tail_log_files`], issuing no
/// `Request::Subscribe`. Every step before the loop is raced against
/// `interrupt` by [`raced`]. `streams.out` is flushed on every exit path.
pub async fn bleats_with_signal(
    client: &Client,
    streams: &mut Streams<'_>,
    quiet: bool,
    args: &BleatsArgs,
    interrupt: impl std::future::Future<Output = ()> + Send,
) -> ExitCode {
    bleats_over(client, streams, quiet, args, None, interrupt).await
}

/// What a request whose link closed mid-flight means.
///
/// Over a host the ssh child dies on the same Ctrl+C as this process, so a
/// closed connection is equally consistent with the operator ending the verb:
/// only [`RequestError::Closed`] gets the grace, and locally a Ctrl+C does.
///
/// `streams.out` is flushed before either way out.
async fn link_gone<F, R>(
    streams: &mut Streams<'_>,
    host: Option<&str>,
    err: &RequestError,
    interrupt: &mut F,
    report: R,
) -> ExitCode
where
    F: std::future::Future<Output = ()> + Unpin,
    R: FnOnce(&mut Streams<'_>, &RequestError) -> ExitCode,
{
    let _ = streams.out.flush();
    if host.is_some()
        && matches!(err, RequestError::Closed)
        && interrupt_within_grace(interrupt).await
    {
        return ExitCode::Success;
    }
    // The message `report` wrote is the reason the verb ended.
    let code = report(streams, err);
    let _ = streams.err.flush();
    code
}

/// One step of the verb, raced against the operator's Ctrl+C.
///
/// A step that never polled the interrupt would hold the operator's key
/// press until it finished. `None` means the interrupt landed while the
/// step was still waiting, and the caller owes only what it printed.
///
/// The work arm is polled first: a shepherd that answered answered.
async fn raced<F, T, W>(interrupt: &mut F, work: W) -> Option<T>
where
    F: std::future::Future<Output = ()> + Unpin,
    W: std::future::Future<Output = T>,
{
    tokio::select! {
        biased;
        value = work => Some(value),
        () = &mut *interrupt => None,
    }
}

/// What an interrupt the operator meant owes this verb: what it printed
/// already, and no error.
fn interrupt_exit(streams: &mut Streams<'_>) -> ExitCode {
    let _ = streams.out.flush();
    ExitCode::Success
}

/// [`bleats_with_signal`] with the host decided by the caller, so a test
/// needs no ssh.
///
/// A `host` changes two things. Every tail is read by that host's shepherd,
/// because `out_file`/`err_file` are the remote's paths and are not files
/// here. Every request whose link closed is decided by [`link_gone`], because
/// the ssh child dies on the same Ctrl+C.
async fn bleats_over(
    client: &Client,
    streams: &mut Streams<'_>,
    quiet: bool,
    args: &BleatsArgs,
    host: Option<&str>,
    interrupt: impl std::future::Future<Output = ()> + Send,
) -> ExitCode {
    tokio::pin!(interrupt);

    let selector = match parse_selector(streams, &args.selector) {
        Ok(selector) => selector,
        Err(code) => return code,
    };

    let source = match host {
        Some(name) => LogSource::Host { client, name },
        None => LogSource::Local,
    };

    // The id/name cache is built from one listing taken before subscribing.
    // Subscribing first would lose every line pushed while the listing is
    // still in flight.
    let cache = match raced(&mut interrupt, resolve_names(client)).await {
        None => return interrupt_exit(streams),
        Some(Ok(cache)) => cache,
        Some(Err(err)) => {
            return link_gone(streams, host, &err, &mut interrupt, typed_request_error).await;
        }
    };

    if args.no_follow {
        let tail = tail_log_files(streams, quiet, &cache, &selector, args, source);
        return match raced(&mut interrupt, tail).await {
            None => interrupt_exit(streams),
            Some(Ok(code)) => code,
            Some(Err(err)) => {
                link_gone(streams, host, &err, &mut interrupt, stop_a_read(source)).await
            }
        };
    }

    // A follow labels every line it receives, and `cache` cannot change
    // under it: taken once, before subscribing.
    let counts = instance_counts(&cache);

    // Backlog before subscribing, so a line in the gap is missed rather
    // than printed twice. The tail's exit code is discarded: an unreadable
    // log for one sheep must not stop the flock, but a host that answered
    // nothing to a read would answer nothing to the subscribe either.
    if args.lines > 0 {
        let backlog = tail_log_files(streams, quiet, &cache, &selector, args, source);
        match raced(&mut interrupt, backlog).await {
            None => return interrupt_exit(streams),
            Some(Err(err)) => {
                return link_gone(streams, host, &err, &mut interrupt, stop_a_read(source)).await;
            }
            Some(Ok(_)) => {}
        }
    }

    let mut stream = match raced(&mut interrupt, subscribe(client)).await {
        None => return interrupt_exit(streams),
        Some(Ok(stream)) => stream,
        Some(Err(err)) => {
            return link_gone(streams, host, &err, &mut interrupt, typed_request_error).await;
        }
    };

    loop {
        tokio::select! {
            biased;
            item = stream.next() => {
                match item {
                    Some(Ok(event)) => {
                        if let Err(write_err) =
                            handle_event(streams, quiet, &cache, &counts, &selector, args, event)
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
                        if host.is_some() && interrupt_within_grace(&mut interrupt).await {
                            return ExitCode::Success;
                        }
                        return ExitCode::DaemonUnreachable;
                    }
                }
            }
            () = &mut interrupt => {
                return interrupt_exit(streams);
            }
        }
    }
}

#[cfg(test)]
mod tests;
