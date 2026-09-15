//! Which sheep a selector names, and how to send a request and render it.
//!
//! Shared plumbing for every verb that acts on a [`ProcessSelector`]: parsing
//! the CLI tokens, matching them against a fetched flock, and rendering
//! whatever the daemon answers.

use std::time::Duration;

use shep_client::Client;
use shep_core::protocol::{ProcessInfo, Request, Response, SelectorSpec};
use shep_core::selector::ProcessSelector;

use crate::cli::Format;
use crate::commands::lifecycle::resolve::{TargetError, target_exit_code};
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::selector::parse_selector_spec;
use crate::exit::ExitCode;
use crate::output::{Render, Streams, emit, emit_flock, write_outcome};

/// Parses every selector the invocation named, refusing on the first bad one
///
/// All-or-nothing: a typo in the third target must not be discovered after
/// the first two were acted on.
pub(crate) fn parse_selectors(
    streams: &mut Streams<'_>,
    raw: &[String],
) -> Result<Vec<SelectorSpec>, ExitCode> {
    let mut parsed = Vec::with_capacity(raw.len());
    for one in raw {
        parsed.push(parse_selector_spec(streams, one)?);
    }
    Ok(parsed)
}

/// Sends one request per selector and collects what each returned
///
/// Not atomic: `shep stop a b c` where `b` matches nothing still stops `a`
/// and `c`. Every selector is attempted, errors are rendered as they
/// happen, and the returned code is the first failure.
pub(crate) async fn request_each<I, B, F>(
    client: &Client,
    streams: &mut Streams<'_>,
    selectors: &[SelectorSpec],
    deadline: Option<Duration>,
    body: B,
    mut extract: F,
) -> (Vec<I>, Option<ExitCode>)
where
    B: Fn(SelectorSpec) -> Request,
    // `FnMut` so a caller can keep what the reply carried beside the rows:
    // `reload` collects the refused apps out of a response whose row half is
    // all this returns.
    F: FnMut(Response) -> Option<Vec<I>>,
{
    let mut collected = Vec::new();
    let mut failure: Option<ExitCode> = None;

    for selector in selectors {
        match client
            .request_with_deadline(body(selector.clone()), deadline)
            .await
        {
            Ok(response) => match extract(response) {
                Some(mut rows) => collected.append(&mut rows),
                None => failure = failure.or(Some(unexpected_response(streams))),
            },
            Err(err) => failure = failure.or(Some(client_error(streams, &err))),
        }
    }
    (collected, failure)
}

/// Which sheep in `flock` a `start` target names
///
/// A `Name` token is tried as an exact sheep name first and as a fold name
/// second, so `shep start backed` reaches a fold called `backed`.
///
/// A wildcard passes a dog by and an exact selector reaches it, keyed off
/// [`ProcessSelector::is_exact`] so a form like `name:slot` cannot land on
/// the wrong side. The fold fallback counts as a wildcard.
pub(crate) fn flock_matches(selector: &ProcessSelector, flock: &[ProcessInfo]) -> Vec<ProcessInfo> {
    let sheep_only = |flock: &[ProcessInfo], keep: &dyn Fn(&ProcessInfo) -> bool| {
        flock
            .iter()
            .filter(|info| info.dog.is_none())
            .filter(|info| keep(info))
            .cloned()
            .collect::<Vec<ProcessInfo>>()
    };
    match selector {
        ProcessSelector::Name(wanted) => {
            let named: Vec<ProcessInfo> = flock
                .iter()
                .filter(|info| &info.name == wanted)
                .cloned()
                .collect();
            if !named.is_empty() {
                return named;
            }
            sheep_only(flock, &|info| info.fold.as_deref() == Some(wanted.as_str()))
        }
        exact if exact.is_exact() => flock
            .iter()
            .filter(|info| exact.matches(&info.name, info.id, info.fold.as_deref(), info.instance))
            .cloned()
            .collect(),
        wildcard => sheep_only(flock, &|info| {
            wildcard.matches(&info.name, info.id, info.fold.as_deref(), info.instance)
        }),
    }
}

/// Whether `target` carries a marker that makes it unmistakably a selector,
/// and so what to say when it matched nothing
///
/// Only the message differs. `start` still falls through to the Flockfile
/// and path tiers for every token: `/srv/app/` parses as a `/regex/` and is
/// also a directory somebody might have.
pub(crate) fn selector_miss(
    target: &str,
    selector: &ProcessSelector,
    flock: &[ProcessInfo],
) -> Option<String> {
    match selector {
        // Phrased off the sheep count, not `flock.is_empty()`: a wildcard
        // passes dogs by, so `all` matching nothing means no sheep.
        ProcessSelector::All if flock.iter().any(|info| info.dog.is_some()) => Some(
            "no sheep in the flock; there is nothing to start. The dogs listed \
             by `shep dogs` are not sheep and `all` never reaches them"
                .to_string(),
        ),
        ProcessSelector::All => Some("the flock is empty; there is nothing to start".to_string()),
        ProcessSelector::Fold(fold) => Some(format!("no sheep is in a fold called {fold}")),
        ProcessSelector::Regex(_) => Some(format!("no sheep matched {target}")),
        // A colon is not a path character, so `name:slot` is a marker the
        // same way `fold:` is, not a filename that could exist instead.
        ProcessSelector::Instance { name, slot } => {
            Some(format!("no instance {slot} of {name} is registered"))
        }
        // A bare name or id carries no marker and may have meant a
        // filename, so the unresolvable message naming every tier stands.
        ProcessSelector::Name(_) | ProcessSelector::Id(_) => None,
    }
}

/// Whether `target` could name a sheep or a fold at all
///
/// A sheep name may not contain a path separator and may not be `.` or `..`
/// (`shep_core::config::normalize`), so `./backed` is always the file.
/// Applied to the `Name` form only: `/web/` is a regex full of slashes.
pub(crate) fn is_reachable_as_a_name(selector: &ProcessSelector) -> bool {
    match selector {
        ProcessSelector::Name(name) => !name.contains(['/', '\\']) && name != "." && name != "..",
        _ => true,
    }
}

/// Renders what a lifecycle verb leaves on screen: the rows it touched
/// under `--format json`, the whole flock as a table otherwise
///
/// The table costs one extra `ListFlock` round trip. `--format json` keeps
/// the narrow payload, so a script reads `data[0]` to learn what it touched.
///
/// In table form a dog goes through [`emit_flock`], not [`emit`], so it
/// renders in the dogs table with its `SOURCE` column.
///
/// # Errors
/// Never returns a listing failure as this verb's failure: an unreachable or
/// unrecognised listing prints nothing extra and reports success.
pub(crate) async fn render_outcome<T: Render>(
    client: &Client,
    streams: &mut Streams<'_>,
    command: &str,
    narrow: T,
) -> ExitCode {
    if streams.fmt == Format::Json {
        return write_outcome(emit(
            &mut *streams.out,
            streams.fmt,
            command,
            narrow,
            streams.style,
        ));
    }
    let listing = flock_now(client).await;
    write_outcome(emit_flock(
        &mut *streams.out,
        streams.fmt,
        command,
        listing,
        // A dog listing says nothing about the machine, and a `host` key on
        // it would claim this verb answers a question it was never asked.
        None,
        streams.style,
    ))
}

/// Renders `err` and returns the exit code `start` reports it as.
pub(crate) fn fail_target(streams: &mut Streams<'_>, err: &TargetError) -> ExitCode {
    let code = target_exit_code(err);
    streams.fail(code, &err.to_string())
}

/// The flock as it stands, for deciding whether a target names a sheep that
/// already exists
///
/// A name is unique across a flock, so a target naming one can never have
/// meant "add another". Fetched once per invocation and matched locally.
/// An unreachable or unexpected answer yields an empty flock; the `Start`
/// that follows reports its own failures.
pub(crate) async fn flock_now(client: &Client) -> Vec<shep_core::protocol::ProcessInfo> {
    match client.request(Request::ListFlock).await {
        Ok(Response::Flock(procs)) => procs,
        _ => Vec::new(),
    }
}
