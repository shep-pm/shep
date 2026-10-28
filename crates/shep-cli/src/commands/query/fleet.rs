//! `shep flock` with a `hosts` list: every shepherd's flock in one run, this
//! machine's first and then each listed host's in list order.
//!
//! The hosts are asked at once and nothing is printed until all of them have
//! answered, so the order of the output is the list's and never the
//! network's. A host that fails prints one line on stderr instead of a table
//! and does not hide the others. Under `--format json` the answers are one
//! envelope instead of one table apiece.

use std::future::Future;
use std::io;
use std::time::Duration;

use futures_util::future::join_all;
use shep_client::{Client, ConnectError, RequestError, SshOptions};
use shep_core::destination::Destination;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{HostUsage, ProcessInfo};
use tokio::time::error::Elapsed;

use super::flock_display::{flock_from_roll, host_usage, print_listing};
use crate::cli::Format;
use crate::client::unreachable_message;
use crate::commands::hosts::{HOST_CONNECT_TIMEOUT, unattended};
use crate::commands::rpc::UNRECOGNISED;
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;
use crate::output::{HostListing, Streams, emit_fleet_flock, write_outcome};
use crate::version_guard::{
    VersionGuard, flock_connect_refusal_message, host_skew, refuse_version_skew,
};

/// What the table form heads this machine's listing with.
const LOCAL_HEADING: &str = "local";

/// One shepherd's answer to a listing.
struct Answer {
    procs: Vec<ProcessInfo>,
    usage: Option<Option<HostUsage>>,
}

/// A host's answer, and the version warning it earned.
struct HostAnswer {
    answer: Answer,
    skew: Option<String>,
}

/// Why one shepherd gave no listing.
struct Failure {
    code: ExitCode,
    message: String,
}

impl Failure {
    fn of_connect(err: &ConnectError) -> Self {
        Self {
            code: ExitCode::from(err),
            message: unreachable_message(err),
        }
    }

    /// A shepherd that did not answer within `timeout`.
    fn abandoned(timeout: Duration) -> Self {
        Self {
            code: ExitCode::DaemonUnreachable,
            message: format!("did not answer within {timeout:?}"),
        }
    }

    fn of_request(err: &RequestError) -> Self {
        match err {
            RequestError::UnexpectedReply { .. } => Self {
                code: ExitCode::Internal,
                message: UNRECOGNISED.to_owned(),
            },
            other => Self {
                code: ExitCode::from(other),
                message: other.to_string(),
            },
        }
    }
}

/// The hosts `paths`' `shep.toml` lists, or none.
///
/// A file that will not read or parse is none too, with one aside saying so:
/// a broken list must not stop a local `shep flock`. The error's own text is
/// left out because a parse error quotes the line it stopped on; `shep host
/// list` reports it in full.
pub(crate) fn listed_hosts(streams: &mut Streams<'_>, paths: &ShepPaths) -> Vec<String> {
    let names = ShepToml::read_only(&paths.daemon_config).and_then(|doc| doc.host_names());
    names.unwrap_or_else(|_| {
        let message = format!(
            "the hosts list in {} was not read, so only this machine's flock is listed; \
             `shep host list` says why",
            paths.daemon_config.display()
        );
        streams.aside("hosts_unread", &message);
        Vec::new()
    })
}

/// `shep flock` over `hosts`, dialled without prompts under the bound every
/// unattended dial shares.
///
/// # Errors
/// As [`fleet`].
pub(crate) async fn flock_all(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    guard: VersionGuard,
    hosts: &[String],
) -> ExitCode {
    let ssh = unattended(HOST_CONNECT_TIMEOUT);
    fleet(streams, paths, guard, hosts, &ssh, HOST_CONNECT_TIMEOUT).await
}

/// This machine's flock and then each of `hosts`, one table apiece, or under
/// `--format json` one envelope of every row.
///
/// Every host is dialled through `ssh` and given `timeout` for the dial and
/// both requests together: a box that says nothing is abandoned.
///
/// Returns [`ExitCode::Success`] only when every shepherd answered.
pub(super) async fn fleet(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    guard: VersionGuard,
    hosts: &[String],
    ssh: &SshOptions,
    timeout: Duration,
) -> ExitCode {
    let (local, remote) = tokio::join!(
        tokio::time::timeout(timeout, Client::connect(&paths.socket)),
        join_all(hosts.iter().map(|host| ask_host(host, ssh, timeout, guard))),
    );

    let mut tables = Tables::new(streams.fmt);
    let mut first_failure = None;
    let mut note = |code: ExitCode| {
        first_failure.get_or_insert(code);
    };

    match ask_local(streams, guard, local, timeout).await {
        Local::Listed(answer) => {
            if let Err(done) = tables.print(streams, None, answer, None) {
                return done;
            }
        }
        Local::Roll if streams.fmt == Format::Json => {
            let message = format!("{LOCAL_HEADING}: no shepherd running");
            note(streams.fail(ExitCode::DaemonUnreachable, &message));
        }
        Local::Roll => match print_roll(streams, &mut tables, paths) {
            Ok(code) => note(code),
            Err(done) => return done,
        },
        Local::Failed(code) => note(code),
    }
    for (host, result) in hosts.iter().zip(remote) {
        match result {
            Ok(HostAnswer { answer, skew }) => {
                if let Err(done) = tables.print(streams, Some(host), answer, skew) {
                    return done;
                }
            }
            Err(Failure { code, message }) => {
                note(streams.fail(code, &format!("{host}: {message}")));
            }
        }
    }
    if let Err(done) = tables.finish(streams) {
        return done;
    }
    first_failure.unwrap_or(ExitCode::Success)
}

/// `ask`, or [`Failure::abandoned`] if it takes longer than `timeout`.
async fn within<T>(
    timeout: Duration,
    ask: impl Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    tokio::time::timeout(timeout, ask)
        .await
        .unwrap_or_else(|_elapsed| Err(Failure::abandoned(timeout)))
}

/// What a fleet listing has written so far: under `--format table`, whether
/// a table has been printed yet, so the next one is set apart from it; under
/// `--format json`, the answers held back for [`Tables::finish`] to write as
/// one envelope.
struct Tables {
    printed: bool,
    held: Option<Vec<HostListing>>,
}

impl Tables {
    fn new(fmt: Format) -> Self {
        Self {
            printed: false,
            held: (fmt == Format::Json).then(Vec::new),
        }
    }

    /// Writes the blank line that separates this table from the last one, and
    /// the heading that names whose it is.
    fn open(&mut self, streams: &mut Streams<'_>, heading: &str) -> io::Result<()> {
        if self.printed {
            writeln!(streams.out)?;
        }
        self.printed = true;
        writeln!(streams.out, "{heading}")
    }

    /// Prints `answer` under `host`'s name, or this machine's under `local`,
    /// after the warning `skew` carries. Under `--format json` the answer is
    /// held for [`Self::finish`] and only the warning is written.
    ///
    /// # Errors
    /// The exit code to end the run with when stdout is closed.
    fn print(
        &mut self,
        streams: &mut Streams<'_>,
        host: Option<&str>,
        answer: Answer,
        skew: Option<String>,
    ) -> Result<(), ExitCode> {
        if let Some(warning) = skew {
            streams.aside("version_skew", &warning);
        }
        if let Some(held) = &mut self.held {
            held.push(HostListing {
                host: host.map(str::to_owned),
                procs: answer.procs,
                usage: answer.usage,
            });
            return Ok(());
        }
        self.open(streams, host.unwrap_or(LOCAL_HEADING))
            .and_then(|()| print_listing(streams, answer.procs, answer.usage, None))
            .map_err(|err| write_outcome(Err(err)))
    }

    /// Writes the held answers as one envelope, when any are held.
    ///
    /// # Errors
    /// The exit code to end the run with when stdout is closed.
    fn finish(self, streams: &mut Streams<'_>) -> Result<(), ExitCode> {
        match self.held {
            Some(held) => emit_fleet_flock(&mut *streams.out, "flock", held)
                .map_err(|err| write_outcome(Err(err))),
            None => Ok(()),
        }
    }
}

/// How this machine's shepherd answered.
enum Local {
    Listed(Answer),
    /// Nothing is listening; [`print_roll`] speaks for it.
    Roll,
    /// Already reported on stderr.
    Failed(ExitCode),
}

/// Reads this machine's listing off `connected`, applying `guard` as
/// `shep flock` does.
async fn ask_local(
    streams: &mut Streams<'_>,
    guard: VersionGuard,
    connected: Result<Result<Client, ConnectError>, Elapsed>,
    timeout: Duration,
) -> Local {
    let client = match connected {
        Ok(Ok(client)) => client,
        Ok(Err(ConnectError::Connect { .. })) => return Local::Roll,
        Err(_elapsed) => {
            let message = format!("{LOCAL_HEADING}: {}", Failure::abandoned(timeout).message);
            return Local::Failed(streams.fail(ExitCode::DaemonUnreachable, &message));
        }
        Ok(Err(err)) => {
            let message = format!("{LOCAL_HEADING}: {}", flock_connect_refusal_message(&err));
            return Local::Failed(streams.fail(ExitCode::from(&err), &message));
        }
    };
    if let Err(code) = refuse_version_skew(streams, &client, guard) {
        return Local::Failed(code);
    }
    match within(timeout, fetch(&client)).await {
        Ok(answer) => Local::Listed(answer),
        Err(Failure { code, message }) => {
            Local::Failed(streams.fail(code, &format!("{LOCAL_HEADING}: {message}")))
        }
    }
}

/// The saved roll of the shepherd that is not running, under the `local`
/// heading, and the code it counts as.
///
/// The roll is printed into a buffer first so an empty one, which prints
/// nothing, leaves no heading over nothing.
///
/// # Errors
/// The exit code to end the run with when stdout is closed.
fn print_roll(
    streams: &mut Streams<'_>,
    tables: &mut Tables,
    paths: &ShepPaths,
) -> Result<ExitCode, ExitCode> {
    let mut rolled = Vec::new();
    let code = flock_from_roll(
        &mut Streams {
            out: &mut rolled,
            err: &mut *streams.err,
            style: streams.style,
            fmt: streams.fmt,
        },
        paths,
    );
    if !rolled.is_empty() {
        tables
            .open(streams, LOCAL_HEADING)
            .and_then(|()| streams.out.write_all(&rolled))
            .map_err(|err| write_outcome(Err(err)))?;
    }
    Ok(code)
}

/// `Request::ListFlock` and then the host usage request, as `shep flock`
/// asks them.
async fn fetch(client: &Client) -> Result<Answer, Failure> {
    let procs = client
        .list_flock()
        .await
        .map_err(|e| Failure::of_request(&e))?;
    let usage = host_usage(client).await;
    Ok(Answer { procs, usage })
}

/// One host's listing, or why there is none.
///
/// `host` is parsed as a [`Destination`] before ssh sees it, so a hand-edited
/// entry that would read as an ssh option is that host's failure alone. A
/// skew is treated as `guard` treats it for a single `--host`: a notice for a
/// read verb, and for a verb that writes whatever that link refuses, which
/// over ssh is only another protocol. See `crate::version_guard::skew_of`.
async fn ask_host(
    host: &str,
    ssh: &SshOptions,
    timeout: Duration,
    guard: VersionGuard,
) -> Result<HostAnswer, Failure> {
    let destination = Destination::parse(host).map_err(|err| Failure {
        code: ExitCode::InvalidConfig,
        message: format!("not a destination: {err}"),
    })?;
    within(timeout, async {
        let client = Client::connect_ssh_with(destination.as_str(), ssh)
            .await
            .map_err(|e| Failure::of_connect(&e))?;
        let answer = match host_skew(&client, guard) {
            Ok(skew) => fetch(&client)
                .await
                .map(|answer| HostAnswer { answer, skew }),
            Err(message) => Err(Failure {
                code: ExitCode::VersionSkew,
                message,
            }),
        };
        let _ = client.close().await;
        answer
    })
    .await
}

#[cfg(all(test, unix))]
mod tests;
