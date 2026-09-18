//! One request to the daemon, the answer rendered, and every way that can go
//! wrong mapped to an exit code.
//!
//! A verb that sends one request and renders one payload calls
//! [`request_and_render`]. A verb whose success arm renders something no
//! single [`Render`] impl expresses — two tables out of one
//! `Vec<ProcessInfo>`, a flourish above the table, a notice beside an empty
//! one — keeps its own `match` on the answer and shares only
//! [`unexpected_response`] and [`client_error`], the two arms every verb
//! handles alike.

use std::time::Duration;

use shep_client::{Client, RequestError};
use shep_core::protocol::{Request, Response};

use crate::exit::ExitCode;
use crate::output::{Render, Streams, emit, write_outcome};

/// What a reply this build does not recognise is reported as.
///
/// `pub(crate)` for the one caller that appends to it rather than reporting
/// it alone: `import::dotenv` says which store it already wrote.
pub(crate) const UNRECOGNISED: &str =
    "the daemon answered with a response this client does not understand";

/// Reports an answer this client cannot render, and hands back its code.
///
/// `Response` is `#[non_exhaustive]`, so a variant a newer daemon added
/// reaches here rather than being guessed at.
pub(crate) fn unexpected_response(streams: &mut Streams<'_>) -> ExitCode {
    streams.fail(ExitCode::Internal, UNRECOGNISED)
}

/// Reports a request that never produced an answer, and hands back its code.
pub(crate) fn client_error(streams: &mut Streams<'_>, err: &RequestError) -> ExitCode {
    streams.fail(ExitCode::from(err), &err.to_string())
}

/// Sends `body` with `deadline` (`None` defers to the client's own default)
/// and pulls the verb's own payload out of the answer.
///
/// For a verb that renders its payload itself; [`request_and_render`] is the
/// shorter road for one that does not.
///
/// # Errors
///
/// The [`ExitCode`] already reported to `streams.err`: the request failed, or
/// `extract` did not recognise the answer.
pub(crate) async fn request_payload<T, F>(
    client: &Client,
    streams: &mut Streams<'_>,
    body: Request,
    deadline: Option<Duration>,
    extract: F,
) -> Result<T, ExitCode>
where
    F: FnOnce(Response) -> Option<T>,
{
    match client.request_with_deadline(body, deadline).await {
        Ok(response) => extract(response).ok_or_else(|| unexpected_response(streams)),
        Err(err) => Err(client_error(streams, &err)),
    }
}

/// Sends `body`, renders whatever the daemon answers through [`emit`] under
/// `command`, and maps every way that can go wrong to its exit code.
pub(crate) async fn request_and_render<T, F>(
    client: &Client,
    streams: &mut Streams<'_>,
    command: &str,
    body: Request,
    deadline: Option<Duration>,
    extract: F,
) -> ExitCode
where
    T: Render,
    F: FnOnce(Response) -> Option<T>,
{
    match request_payload(client, streams, body, deadline, extract).await {
        Ok(payload) => write_outcome(emit(
            &mut *streams.out,
            streams.fmt,
            command,
            payload,
            streams.style,
        )),
        Err(code) => code,
    }
}
