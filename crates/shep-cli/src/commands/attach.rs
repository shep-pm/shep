//! `attach`: show one pty sheep's live terminal and type into it.
//!
//! The request flow is [`session`], which sees the terminal only as a
//! [`Tty`] of channels, so a test drives it with no terminal at all. Raw mode,
//! the stdin thread and `SIGWINCH` live in `tty`, and are the only part that
//! touches a real terminal.
// Only `attach` for unix reaches the session; the rest is unreachable on Windows.
#![cfg_attr(not(unix), allow(dead_code))]

use std::io;

use futures_util::FutureExt as _;
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
    match tty::open() {
        Ok(tty) => session(client, streams, &args.name, tty).await,
        Err(err) => streams.fail(
            ExitCode::Failure,
            &format!("could not put the terminal into raw mode: {err}"),
        ),
    }
}

/// Refuses `attach`, which needs a Unix terminal.
#[cfg(not(unix))]
pub(crate) async fn attach(
    _client: &Client,
    streams: &mut Streams<'_>,
    _args: &AttachArgs,
) -> ExitCode {
    streams.fail(
        ExitCode::Failure,
        "`shep attach` is not available on Windows",
    )
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

/// Moves the sheep's output to the terminal and the operator's keys and
/// resizes to the sheep, until one of them ends the session.
///
/// A ready event is taken before a key, so a sheep that has just exited is
/// read as that rather than as a refusal of the next key.
async fn pump(
    client: &Client,
    streams: &mut Streams<'_>,
    name: &str,
    events: &mut EventStream,
    mut keys: mpsc::Receiver<Vec<u8>>,
    mut resizes: mpsc::Receiver<Size>,
) -> End {
    let mut typed = keys::Keys::default();
    loop {
        tokio::select! {
            biased;
            event = events.next() => {
                if let Err(end) = show(streams, name, event) {
                    return end;
                }
            }
            chunk = keys.recv() => {
                let Some(chunk) = chunk else { return End::Detached };
                let keys = typed.push(&chunk);
                if !keys.text.is_empty() {
                    let body = Request::PtyInput { name: name.to_owned(), data: keys.text };
                    if let Err(end) = send(client, streams, name, events, body).await {
                        return end;
                    }
                }
                if keys.detach {
                    return End::Detached;
                }
            }
            Some(size) = resizes.recv() => {
                let body = Request::PtyResize {
                    name: name.to_owned(),
                    rows: size.rows,
                    cols: size.cols,
                };
                if let Err(end) = send(client, streams, name, events, body).await {
                    return end;
                }
            }
        }
    }
}

/// Sends `body` and waits for its reply, showing the sheep's output as it
/// arrives meanwhile, so a slow round trip cannot overflow the event buffer.
///
/// An event that is already waiting when the request fails is shown before
/// the failure is reported: the request is most likely refused because the
/// sheep just exited, and the waiting event says so.
async fn send(
    client: &Client,
    streams: &mut Streams<'_>,
    name: &str,
    events: &mut EventStream,
    body: Request,
) -> Result<(), End> {
    let reply = client.request(body);
    tokio::pin!(reply);
    let outcome = loop {
        tokio::select! {
            biased;
            event = events.next() => show(streams, name, event)?,
            outcome = &mut reply => break outcome,
        }
    };
    if let Err(err) = outcome {
        while let Some(event) = events.next().now_or_never() {
            show(streams, name, event)?;
        }
        return Err(End::Request(err));
    }
    Ok(())
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
