//! `attach`: show one pty sheep's live terminal and type into it.
//!
//! The request flow is [`session`], which sees the terminal only as a
//! [`Tty`] of channels, so a test drives it with no terminal at all. Raw mode,
//! the stdin thread and `SIGWINCH` live in `tty`, and are the only part that
//! touches a real terminal.
// Only `attach` for unix reaches the session; the rest is unreachable on Windows.
#![cfg_attr(not(unix), allow(dead_code))]

use std::io;
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use shep_client::{Client, EventStream, Lagged, RequestError};
use shep_core::protocol::{BusEvent, Request, Response};
use tokio::sync::mpsc;

use crate::cli::{AttachArgs, Format};
use crate::commands::rpc::{client_error, unexpected_response};
use crate::exit::ExitCode;
use crate::lookout::term::RestoreGuard;
use crate::output::Streams;

mod keys;
#[cfg(unix)]
mod tty;

#[cfg(test)]
mod tests;

/// The byte ctrl-] types. It detaches and is never sent to the sheep.
pub(crate) const DETACH_KEY: u8 = 0x1d;

/// The terminal's size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Size {
    pub rows: u16,
    pub cols: u16,
}

/// What [`session`] needs from the operator's terminal.
pub(crate) struct Tty {
    /// The size at the moment of attaching.
    pub size: Size,
    /// Each read of raw stdin, in order. Closing it detaches.
    pub keys: mpsc::Receiver<Vec<u8>>,
    /// The new size after each resize.
    pub resizes: mpsc::Receiver<Size>,
    /// Puts the terminal back when dropped, on every way out of [`session`],
    /// a panic included.
    pub restore: RestoreGuard,
}

/// Shows `args.name`'s terminal and sends the operator's keys to it.
///
/// Ends [`ExitCode::Success`] on a detach and when the sheep's terminal
/// closes. Anything the shepherd refuses, including a sheep without `pty`,
/// is its message and its code.
#[cfg(unix)]
pub(crate) async fn attach(
    client: &Client,
    streams: &mut Streams<'_>,
    args: &AttachArgs,
) -> ExitCode {
    use tokio::signal::unix::{SignalKind, signal};

    // Listening before raw mode goes on: with no listener a SIGTERM ends the
    // process without running the guard that puts the terminal back.
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(terminate) => terminate,
        Err(err) => {
            return streams.fail(
                ExitCode::Failure,
                &format!("could not listen for SIGTERM: {err}"),
            );
        }
    };
    let tty = match tty::open() {
        Ok(tty) => tty,
        Err(err) => {
            return streams.fail(
                ExitCode::Failure,
                &format!("could not put the terminal into raw mode: {err}"),
            );
        }
    };
    // Dropping the session on SIGTERM drops its restore guard.
    tokio::select! {
        code = session(client, streams, &args.name, tty) => code,
        _ = terminate.recv() => streams.fail(ExitCode::Failure, "terminated by SIGTERM"),
    }
}

/// The refusal for a build with no Unix terminal, or `None` where `attach`
/// can run. Asked before any connection is made, so an absent shepherd or an
/// unreachable host does not hide it.
#[cfg(unix)]
pub(crate) fn unsupported(_streams: &mut Streams<'_>) -> Option<ExitCode> {
    None
}

/// The refusal for a build with no Unix terminal, or `None` where `attach`
/// can run. Asked before any connection is made, so an absent shepherd or an
/// unreachable host does not hide it.
#[cfg(not(unix))]
pub(crate) fn unsupported(streams: &mut Streams<'_>) -> Option<ExitCode> {
    Some(streams.fail(
        ExitCode::Failure,
        "`shep attach` is not available on Windows",
    ))
}

/// Refuses `attach`, which needs a Unix terminal.
#[cfg(not(unix))]
pub(crate) async fn attach(
    _client: &Client,
    streams: &mut Streams<'_>,
    _args: &AttachArgs,
) -> ExitCode {
    unsupported(streams).unwrap_or(ExitCode::Failure)
}

/// How a [`session`] ended.
enum End {
    /// The operator pressed the detach key, or stdin closed.
    Detached,
    /// The sheep's terminal closed.
    SheepExited,
    /// A request failed, or the connection went.
    Request(RequestError),
    /// The terminal would not take the sheep's output.
    Write(io::Error),
}

/// Attaches to `name` and runs until the operator detaches, the sheep's
/// terminal closes, or the connection fails.
///
/// The terminal is restored before the closing line is written, so that line
/// is not drawn in raw mode.
pub(crate) async fn session(
    client: &Client,
    streams: &mut Streams<'_>,
    name: &str,
    tty: Tty,
) -> ExitCode {
    let Tty {
        size,
        keys,
        resizes,
        restore,
    } = tty;
    // Opened before the attach, since a receiver sees nothing sent before it
    // existed.
    let mut events = match client.events().await {
        Ok(events) => events,
        Err(err) => {
            drop(restore);
            return client_error(streams, &err);
        }
    };
    let attach = Request::Attach {
        name: name.to_owned(),
        rows: size.rows,
        cols: size.cols,
    };
    let end = match client.request(attach).await {
        Ok(Response::Attached { .. }) => {
            pump(client, streams, name, &mut events, keys, resizes).await
        }
        Ok(_) => {
            drop(restore);
            return unexpected_response(streams);
        }
        Err(err) => End::Request(err),
    };
    drop(restore);
    match end {
        End::Detached => closing_line(streams, "detached"),
        End::SheepExited => closing_line(streams, "the sheep exited"),
        End::Request(err) => return client_error(streams, &err),
        End::Write(err) => {
            let message = format!("writing the sheep's output: {err}");
            return streams.fail(ExitCode::Failure, &message);
        }
    }
    ExitCode::Success
}

/// What turns off every terminal mode a sheep can switch on through its output.
///
/// Mouse tracking, focus reports, application keys, the kitty keyboard stack
/// and `modifyOtherKeys`, then the colours. Left on, they feed the shell escape
/// sequences, and a pushed kitty protocol stops ctrl-] arriving as `0x1d`.
/// Written before the alternate screen is left: the kitty stack is per screen.
const RESET_MODES: &str = "\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\
\x1b[?1004l\x1b[?1l\x1b>\x1b[<99u\x1b[>4;0m\x1b[0m";

/// Writes [`RESET_MODES`] to the operator's terminal.
///
/// # Errors
///
/// Whatever the write refused. The caller is restoring a terminal and has
/// nothing better to do than go on.
pub(crate) fn reset_modes(out: &mut impl io::Write) -> io::Result<()> {
    out.write_all(RESET_MODES.as_bytes())?;
    out.flush()
}

/// How long a detach waits for the keys typed before it to reach the sheep.
const DETACH_FLUSH: Duration = Duration::from_millis(250);

/// Moves the sheep's output to the terminal and the operator's keys and
/// resizes to the sheep, until one of them ends the session.
///
/// One request is in flight at a time, a branch of the same `select!` as the
/// events and keys, so a slow reply holds up neither. Keys typed meanwhile are
/// queued. No branch is preferred, so output cannot starve the keys.
async fn pump<'a>(
    client: &'a Client,
    streams: &mut Streams<'_>,
    name: &str,
    events: &mut EventStream,
    mut keys: mpsc::Receiver<Vec<u8>>,
    mut resizes: mpsc::Receiver<Size>,
) -> End {
    let typed = keys::Keys;
    let mut queued: Vec<u8> = Vec::new();
    let mut resize: Option<Size> = None;
    let mut in_flight: Option<LocalBoxFuture<'a, Result<Response, RequestError>>> = None;
    loop {
        if in_flight.is_none() {
            let body = if queued.is_empty() {
                resize.take().map(|size| Request::PtyResize {
                    name: name.to_owned(),
                    rows: size.rows,
                    cols: size.cols,
                })
            } else {
                Some(input(name, &mut queued))
            };
            in_flight = body.map(|body| client.request(body).boxed_local());
        }
        tokio::select! {
            event = events.next() => {
                if let Err(end) = show(streams, name, event) {
                    return end;
                }
            }
            chunk = keys.recv() => {
                // Stdin closed: leave the way ctrl-] does, so what was read
                // before the end still reaches the sheep.
                let Some(chunk) = chunk else {
                    return detach(client, name, in_flight.take(), queued).await;
                };
                let typed_now = typed.push(&chunk);
                queued.extend_from_slice(&typed_now.bytes);
                if typed_now.detach {
                    return detach(client, name, in_flight.take(), queued).await;
                }
            }
            Some(size) = resizes.recv() => resize = Some(size),
            outcome = async {
                match in_flight.as_mut() {
                    Some(request) => request.await,
                    None => std::future::pending().await,
                }
            } => {
                in_flight = None;
                if let Err(err) = outcome {
                    // Most likely refused because the sheep just exited, and
                    // the event that says so is already waiting.
                    while let Some(event) = events.next().now_or_never() {
                        if let Err(end) = show(streams, name, event) {
                            return end;
                        }
                    }
                    return End::Request(err);
                }
            }
        }
    }
}

/// A `PtyInput` of everything queued, which it empties.
fn input(name: &str, queued: &mut Vec<u8>) -> Request {
    Request::PtyInput {
        name: name.to_owned(),
        data: std::mem::take(queued),
    }
}

/// Ends the session on the detach key, first letting what was typed before
/// it reach the sheep for as long as [`DETACH_FLUSH`], so a stalled write
/// cannot keep the operator from leaving.
async fn detach(
    client: &Client,
    name: &str,
    in_flight: Option<LocalBoxFuture<'_, Result<Response, RequestError>>>,
    mut queued: Vec<u8>,
) -> End {
    let flush = async {
        if let Some(request) = in_flight {
            let _ = request.await;
        }
        if !queued.is_empty() {
            let _ = client.request(input(name, &mut queued)).await;
        }
    };
    let _ = tokio::time::timeout(DETACH_FLUSH, flush).await;
    End::Detached
}

/// Acts on one item of the event stream.
///
/// # Errors
///
/// The [`End`] the event means: the sheep's terminal closed, the connection
/// closed, or the terminal could not be written.
fn show(
    streams: &mut Streams<'_>,
    name: &str,
    event: Option<Result<BusEvent, Lagged>>,
) -> Result<(), End> {
    match event {
        Some(Ok(BusEvent::PtyOut { name: from, data })) if from == name => streams
            .out
            .write_all(data.as_bytes())
            .and_then(|()| streams.out.flush())
            .map_err(End::Write),
        Some(Ok(BusEvent::PtyEnd { name: from })) if from == name => Err(End::SheepExited),
        Some(Ok(BusEvent::Dropped { count }) | Err(Lagged { count })) => {
            notice(
                streams,
                &format!("{count} events were dropped; the screen may be out of sync, redraw it"),
            );
            Ok(())
        }
        Some(_) => Ok(()),
        None => Err(End::Request(RequestError::Closed)),
    }
}

/// A line on stderr in the middle of a session, while the terminal is raw.
fn notice(streams: &mut Streams<'_>, message: &str) {
    match streams.fmt {
        Format::Table => {
            let _ = write!(streams.err, "\r\nshep: {message}\r\n");
        }
        Format::Json => streams.aside("attach", message),
    }
}

/// The one line a session ends with, on stderr.
fn closing_line(streams: &mut Streams<'_>, message: &str) {
    match streams.fmt {
        Format::Table => {
            let _ = writeln!(streams.err, "{message}");
        }
        Format::Json => streams.aside("attach", message),
    }
}
