//! Protocol version safety: refusing a shepherd this build cannot write to,
//! and `shep flock`'s own dispatch, which applies the guard by hand around
//! its roll fallback.

use std::io::IsTerminal;
use std::time::Duration;

use cli::{Commands, Format};
use commands::query;
use exit::ExitCode;
use output::Streams;
use shep_client::Client;
use shep_core::protocol::PROTOCOL_VERSION;

use crate::client::unreachable_message;
use crate::remote::Link;
use crate::{cli, commands, exit, output};

/// The verbs a version skew must never refuse, spelled the way an operator
/// types them.
///
/// A verb belongs here only if it is a way out of a skew: a guard whose
/// remedy is itself guarded leaves nothing that can touch the flock.
/// [`VERSION_SKEW_REMEDY`] reads `shep daemon reload` out of this list.
const RECOVERY_VERBS: [&str; 3] = ["kill", "daemon reload", "ping"];

/// Which [`RECOVERY_VERBS`] entry `command` is, or `None` for an ordinary
/// verb the version guard applies to.
///
/// Returns the name rather than a bool so a test can hold this mapping and
/// that list against each other.
fn recovery_verb(command: &Commands) -> Option<&'static str> {
    match command {
        Commands::Kill => Some("kill"),
        Commands::Ping => Some("ping"),
        // A bare `shep daemon` is the hidden boot re-exec, which reaches no
        // shepherd and so has nothing to be exempt from.
        Commands::Daemon(args) => match args.cmd {
            Some(cli::DaemonCmd::Reload) => Some("daemon reload"),
            None => None,
        },
        _ => None,
    }
}

/// The verbs that only read the flock, spelled the way an operator types
/// them, which a version skew warns about and does not refuse.
///
/// `bleats` is the whole verb: both its forms send one `ListFlock` and then
/// read. A pane or tool that can write (`lookout`, `whistle`) stays out.
///
/// Read only by the test that holds it against [`read_verb`].
#[allow(dead_code)]
const READ_VERBS: [&str; 3] = ["flock", "describe", "bleats"];

/// Which [`READ_VERBS`] entry `command` is, or `None` for a verb that
/// changes the flock or is a recovery verb.
///
/// Returns the name rather than a bool so a test can hold this mapping and
/// that list against each other.
fn read_verb(command: &Commands) -> Option<&'static str> {
    match command {
        Commands::Flock(_) => Some("flock"),
        Commands::Describe(_) => Some("describe"),
        Commands::Bleats(_) => Some("bleats"),
        _ => None,
    }
}

/// Whether a shepherd this build cannot write to refuses this invocation.
///
/// What a shepherd cannot be written to is the link's question, not the
/// verb's: see [`skew_of`].
///
/// `pub(crate)`: `lookout`, `whistle` and `foreground` name [`Self::Enforce`]
/// at their own connect sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VersionGuard {
    /// Refuse: this verb needs a shepherd that agrees with this binary.
    Enforce,
    /// Never refuse, whatever the shepherd answers: one of
    /// [`RECOVERY_VERBS`].
    Exempt,
    /// Proceed and say so on stderr: one of [`READ_VERBS`], which change
    /// nothing, so a shepherd on another version can still be listed.
    Warn,
}

impl VersionGuard {
    /// The guard that applies to `command`.
    pub(crate) fn for_command(command: &Commands) -> Self {
        match (recovery_verb(command), read_verb(command)) {
            (Some(_), _) => Self::Exempt,
            (None, Some(_)) => Self::Warn,
            (None, None) => Self::Enforce,
        }
    }
}

/// Why a skew happens, as the two lines the table form prints.
///
/// Held as rendered lines rather than one string: the JSON form joins them
/// with a space and the table form with a newline.
const VERSION_SKEW_CAUSE: [&str; 2] = [
    "`cargo install shep` replaced the binary. It did not restart the",
    "shepherd, which is still running the old code.",
];

/// Why a host refuses when its install is the older side of the protocol
/// gap, as the two lines the table form prints.
///
/// Not [`VERSION_SKEW_CAUSE`]: `cargo install` on this machine says nothing
/// about a shepherd that lives elsewhere. And it names the wire, not the
/// version, because over `--host` a version difference alone is a notice and
/// never reaches a cause: see [`skew_of`].
const HOST_BEHIND_CAUSE: [&str; 2] = [
    "The shep installed on that host speaks an older wire protocol.",
    "A reload runs the shep installed there; install this version there if they still differ.",
];

/// Why a host refuses when its install is the newer side of the gap: the fix
/// runs here, and a reload there changes nothing, since that host already
/// runs the newer install. Sending the operator after a reload instead
/// downgrades the host that is up to date, which is the state a fleet is in
/// while it upgrades one host at a time.
const HOST_AHEAD_CAUSE: [&str; 2] = [
    "The shep installed on that host speaks a newer wire protocol.",
    "That host runs its own install, and it is this machine that is behind.",
];

/// What a tolerated skew adds after the two builds are named, one per kind of
/// difference.
///
/// Held apart from [`skew_of`] because each sentence states what the operator
/// may still do, which is the part that changes when a link stops refusing.
const LOCAL_CRATE_NOTICE: &str =
    "Listing works, but verbs that change the flock refuse until it is reloaded.";
const HOST_CRATE_NOTICE: &str = concat!(
    "The wire protocol matches, so verbs go ahead; install the same version ",
    "there if you want them to match.",
);
const PROTOCOL_NOTICE: &str = "Read verbs go ahead, but verbs that change the flock refuse.";

/// The one command that fixes a skew.
///
/// Read out of [`RECOVERY_VERBS`] because the two have to agree: a remedy the
/// guard itself refused would be a dead end.
pub(crate) const VERSION_SKEW_REMEDY: &str = RECOVERY_VERBS[1];

/// The command that replaces this machine's shep, spelled as the help's
/// Upgrading line and [`VERSION_SKEW_CAUSE`] spell it.
const UPGRADE_COMMAND: &str = "cargo install shep";

/// The command that fixes a skew, spelled for where the shepherd is.
///
/// With a `host` it runs over ssh: the bare [`VERSION_SKEW_REMEDY`] would
/// reload this machine's shepherd, which is not the one in skew.
fn version_skew_command(host: Option<&str>) -> String {
    match host {
        Some(dest) => format!("ssh {dest} shep {VERSION_SKEW_REMEDY}"),
        None => format!("shep {VERSION_SKEW_REMEDY}"),
    }
}

/// The command that fixes a protocol gap, spelled for the side of the gap it
/// runs on.
///
/// Which side is behind decides it. A host behind this build is fixed there,
/// over ssh. A host ahead of it is fixed here: reloading a host that already
/// runs the newer install changes nothing, and installing this build there
/// takes that host down with it.
fn protocol_fix(host: Option<&str>, host_ahead: bool) -> String {
    if host_ahead {
        UPGRADE_COMMAND.to_owned()
    } else {
        version_skew_command(host)
    }
}

/// The imperative naming a fix's command, in the two shapes the formats need.
///
/// A `--format json` consumer gets a single-line message with the command
/// inside it. The table form has layout, and its label carries no blank line
/// after it: it sits directly on the command so the two read as one thing.
fn version_skew_instruction(fmt: Format, command: &str) -> String {
    match fmt {
        Format::Json => format!("Run `{command}`."),
        // Two spaces of its own, on top of the indent `safe_message` gives
        // every continuation line, so the command sits a level under its
        // label rather than beside it.
        Format::Table => format!("Run:\n  {command}"),
    }
}

/// Where the shepherd is, as a skew message names it.
fn shepherd_at(host: Option<&str>) -> String {
    match host {
        Some(dest) => format!("the shepherd on {dest}"),
        None => "the running shepherd".to_owned(),
    }
}

/// The opening clause of a skew message: both versions, and where the
/// shepherd runs.
fn version_skew_summary(running: &str, host: Option<&str>) -> String {
    format!(
        "this shep is {}, {} is {running}",
        env!("CARGO_PKG_VERSION"),
        shepherd_at(host)
    )
}

/// The opening clause of a refusal for a shepherd on another protocol: both
/// protocol numbers, and both crate versions.
///
/// The versions travel with the numbers because the fix runs on whichever
/// side of the gap is behind, and an operator who has both numbers can tell
/// which side that is before upgrading anything.
fn protocol_skew_summary(running: &str, peer: u32, host: Option<&str>) -> String {
    format!(
        "this shep is {} speaking protocol {PROTOCOL_VERSION}, {} is {running} speaking protocol {peer}",
        env!("CARGO_PKG_VERSION"),
        shepherd_at(host)
    )
}

/// The one line a tolerated skew prints: the two builds, what the operator
/// may still do, and where the fix runs.
fn notice_line(summary: &str, tail: &str, command: &str) -> String {
    let sentence = format!("{summary}. {tail} Run `{command}`.");
    // `daemon_version` arrives over the socket, so the sentence is
    // collapsed as the table refusal's is.
    crate::terminal_safe::sanitise(&sentence).0
}

/// The refusal [`refuse_version_skew`] lays out, or [`host_skew`] flattens.
struct Refusal {
    /// The opening clause naming both builds.
    summary: String,
    /// The command that fixes it, spelled for the machine it runs on.
    command: String,
    /// Why it happened, as the two lines the table form prints.
    cause: [&'static str; 2],
}

/// The refusal as the one line a caller that lays out no table needs.
fn one_line_refusal(summary: &str, command: &str) -> String {
    crate::terminal_safe::sanitise(&format!("{summary}. Run `{command}`.")).0
}

/// What the handshake this client completed says about versions, in the two
/// forms a guard needs.
///
/// The local socket asks whether this machine is half-replaced: `cargo install
/// shep` leaves the shepherd running old code. A link over `--host` asks only
/// whether the two agree on the wire. Equal protocols, not
/// `peer >= MIN_SUPPORTED`; `docs/decisions.md` turned the floor down.
fn skew_of(client: &Client) -> Skew {
    let ack = client.daemon();
    let host = client.host();
    let running = ack.daemon_version.as_str();
    if host.is_some() {
        if ack.protocol != PROTOCOL_VERSION {
            // The direction of the gap is the direction of the fix: a host
            // behind this build is reloaded, a host ahead of it is not.
            let ahead = ack.protocol > PROTOCOL_VERSION;
            let summary = protocol_skew_summary(running, ack.protocol, host);
            let command = protocol_fix(host, ahead);
            return Skew {
                notice: Some(notice_line(&summary, PROTOCOL_NOTICE, &command)),
                refusal: Some(Refusal {
                    summary,
                    command,
                    cause: if ahead {
                        HOST_AHEAD_CAUSE
                    } else {
                        HOST_BEHIND_CAUSE
                    },
                }),
            };
        }
        if running != env!("CARGO_PKG_VERSION") {
            let summary = version_skew_summary(running, host);
            let command = version_skew_command(host);
            return Skew {
                notice: Some(notice_line(&summary, HOST_CRATE_NOTICE, &command)),
                refusal: None,
            };
        }
    } else if running != env!("CARGO_PKG_VERSION") {
        let summary = version_skew_summary(running, None);
        let command = version_skew_command(None);
        return Skew {
            notice: Some(notice_line(&summary, LOCAL_CRATE_NOTICE, &command)),
            refusal: Some(Refusal {
                summary,
                command,
                cause: VERSION_SKEW_CAUSE,
            }),
        };
    }
    Skew {
        notice: None,
        refusal: None,
    }
}

/// A difference between this build and the shepherd it reached, in the two
/// forms a guard needs.
struct Skew {
    /// The one line for stderr when the difference is tolerated, `None` when
    /// there is nothing to say.
    notice: Option<String>,
    /// The message a verb that changes the flock gets when this link refuses
    /// the difference, `None` when it tolerates it.
    refusal: Option<Refusal>,
}

/// What `guard` makes of a skew on the host this client reached, for a caller
/// that asks several hosts at once and so cannot write to a [`Streams`] as
/// each answers.
///
/// # Errors
/// The one-line refusal, with the fix spelled for the side of the gap that is
/// behind, when `guard` is [`VersionGuard::Enforce`] and that host's shepherd
/// speaks a protocol other than [`PROTOCOL_VERSION`]. Otherwise the notice to
/// print, if there is one: none for [`VersionGuard::Exempt`].
pub(crate) fn host_skew(client: &Client, guard: VersionGuard) -> Result<Option<String>, String> {
    let skew = skew_of(client);
    match (guard, skew.refusal) {
        (VersionGuard::Exempt, _) => Ok(None),
        (VersionGuard::Warn, _) => Ok(skew.notice),
        (VersionGuard::Enforce, None) => Ok(skew.notice),
        (VersionGuard::Enforce, Some(refusal)) => {
            Err(one_line_refusal(&refusal.summary, &refusal.command))
        }
    }
}

/// Refuses a shepherd this link cannot write to, or notices the difference
/// and goes ahead.
///
/// Which difference refuses is the link's, and [`skew_of`] states it. A
/// tolerated difference writes [`Skew::notice`] to stderr, so stdout stays
/// whatever the verb prints. Over ssh the refusal names its host.
///
/// # Errors
/// [`ExitCode::VersionSkew`], after writing the refusal to `streams`, when
/// `guard` is [`VersionGuard::Enforce`] and this link refuses the difference.
/// A [`VersionGuard::Exempt`] or [`VersionGuard::Warn`] verb is always `Ok`.
pub(crate) fn refuse_version_skew(
    streams: &mut Streams<'_>,
    client: &Client,
    guard: VersionGuard,
) -> Result<(), ExitCode> {
    let skew = skew_of(client);
    let refusal = match (guard, skew.refusal) {
        (VersionGuard::Exempt, _) => return Ok(()),
        (VersionGuard::Warn, _) | (VersionGuard::Enforce, None) => {
            if let Some(notice) = skew.notice {
                streams.aside("version_skew", &notice);
            }
            return Ok(());
        }
        (VersionGuard::Enforce, Some(refusal)) => refusal,
    };
    let code = ExitCode::VersionSkew;
    let Refusal {
        summary,
        command,
        cause,
    } = refusal;
    match streams.fmt {
        // One line, one envelope. A `--format json` consumer has no use for
        // the layout below, and it still gets every fact in `error.message`.
        Format::Json => {
            let cause = cause.join(" ");
            let instruction = version_skew_instruction(Format::Json, &command);
            streams.fail(code, &format!("{summary}. {cause} {instruction}"));
        }
        // The fix has to sit on a line of its own to be copied.
        Format::Table => {
            let cause = cause.join("\n");
            // `daemon_version` arrives over the socket, and the table
            // emitter keeps line breaks, so this collapses the one fragment
            // a peer worded before it joins prose that does not.
            let summary = crate::terminal_safe::sanitise(&summary).0;
            let instruction = version_skew_instruction(Format::Table, &command);
            streams.fail(code, &format!("{summary}\n\n{cause}\n\n{instruction}"));
        }
    }
    Err(code)
}

/// Connects to the shepherd `link` names: the socket at `paths.socket`, or a
/// host over ssh. Never autostarts.
///
/// The one seam every verb needing a [`Client`] passes through, so a shepherd
/// of a different version is refused here and no verb has to remember to ask.
pub(crate) async fn connect_client(
    streams: &mut Streams<'_>,
    link: &Link<'_>,
    guard: VersionGuard,
) -> Result<Client, ExitCode> {
    let dialled = match link {
        Link::Local(paths) => Client::connect(&paths.socket).await,
        Link::Host { dest, options } => Client::connect_ssh_with(dest, options).await,
    };
    match dialled {
        Ok(client) => {
            refuse_version_skew(streams, &client, guard)?;
            Ok(client)
        }
        Err(err) => {
            let code = ExitCode::from(&err);
            Err(streams.fail(code, &unreachable_message(&err)))
        }
    }
}

/// `shep flock`'s own dispatch, split out of [`run`](crate::dispatch::run) so
/// a test can drive it against a real fixture socket.
///
/// Uses its own `Client::connect`, not [`connect_client`]: this arm applies
/// the version guard by hand and falls back to the roll on
/// [`shep_client::ConnectError::Connect`] alone. Over a host there is no roll
/// fallback.
pub(crate) async fn flock_command(
    streams: &mut Streams<'_>,
    link: &Link<'_>,
    guard: VersionGuard,
    args: &cli::FlockArgs,
) -> ExitCode {
    flock_command_on(streams, link, guard, args, std::io::stdout().is_terminal()).await
}

/// [`flock_command`] with whether stdout is a terminal decided by the caller,
/// so a test of the terminal refusal holds under `--nocapture` too.
pub(crate) async fn flock_command_on(
    streams: &mut Streams<'_>,
    link: &Link<'_>,
    guard: VersionGuard,
    args: &cli::FlockArgs,
    stdout_is_terminal: bool,
) -> ExitCode {
    let paths = match link {
        Link::Local(paths) => *paths,
        Link::Host { .. } if args.follow => {
            return follow_flock_command(streams, link, guard, args, stdout_is_terminal).await;
        }
        Link::Host { dest, .. } => {
            return match connect_client(streams, link, guard).await {
                Ok(client) => query::flock_from(&client, streams, Some(dest), None).await,
                Err(code) => code,
            };
        }
    };
    if args.follow {
        return follow_flock_command(streams, link, guard, args, stdout_is_terminal).await;
    }
    let hosts = query::fleet::listed_hosts(streams, paths);
    if !hosts.is_empty() {
        return query::fleet::flock_all(streams, paths, guard, &hosts).await;
    }
    match Client::connect(&paths.socket).await {
        Ok(client) => match refuse_version_skew(streams, &client, guard) {
            Ok(()) => query::flock_local(&client, streams, paths).await,
            // A skew is not an absence either: the roll fallback below would
            // print a listing while hiding why every other verb is refusing.
            Err(code) => code,
        },
        // The roll fallback is a table-format affordance: under `--format
        // json` a failed invocation leaves stdout empty and puts an error
        // envelope on stderr, absence included.
        Err(err) if streams.fmt == Format::Json => {
            let code = ExitCode::from(&err);
            streams.fail(code, &unreachable_message(&err))
        }
        Err(shep_client::ConnectError::Connect { .. }) => query::flock_from_roll(streams, paths),
        Err(err) => {
            let code = ExitCode::from(&err);
            streams.fail(code, &flock_connect_refusal_message(&err))
        }
    }
}

/// `shep flock --follow`'s own dispatch: two refusals, then
/// [`connect_client`] and the redraw loop.
///
/// [`connect_client`], not [`flock_command`]'s hand-rolled connect, because
/// a follow has no roll to fall back to. Both refusals are usage errors: a
/// follow written into a file would be a file of escape sequences.
async fn follow_flock_command(
    streams: &mut Streams<'_>,
    link: &Link<'_>,
    guard: VersionGuard,
    args: &cli::FlockArgs,
    stdout_is_terminal: bool,
) -> ExitCode {
    if streams.fmt == Format::Json {
        return streams.fail(
            ExitCode::Usage,
            "`--follow` redraws a table; `--format json` has no follow form",
        );
    }
    if !stdout_is_terminal {
        return streams.fail(
            ExitCode::Usage,
            "`--follow` needs a terminal; stdout is not one",
        );
    }
    match connect_client(streams, link, guard).await {
        Ok(client) => {
            query::flock_follow(&client, streams, Duration::from_secs(args.interval)).await
        }
        Err(code) => code,
    }
}

/// Renders `err` for [`flock_command`]'s refusal arm: the shepherd is there,
/// so this reports what it did and names the fix rather than the roll's "no
/// shepherd running".
pub(crate) fn flock_connect_refusal_message(err: &shep_client::ConnectError) -> String {
    format!("{err}; run `shep {VERSION_SKEW_REMEDY}`")
}

#[cfg(test)]
mod tests;
