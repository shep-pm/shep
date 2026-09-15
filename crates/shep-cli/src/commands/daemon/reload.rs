//! Reload arm selection and orchestration: choosing between the `execve`
//! handover and stopping and starting the shepherd, and driving whichever
//! arm the running daemon's version supports.

use shep_client::{Client, ConnectError};
use shep_core::config::DaemonConfig;
use shep_core::paths::ShepPaths;
use shep_daemon::boot::{self, BootError, Shepherd};

use super::lifecycle::{DaemonRunError, daemon_exit_code, read_daemon_config_source};
use super::reload_report::report_reload;
use crate::commands::{admin, dog_migration};
use crate::exit::ExitCode;
use crate::output::Streams;

/// Which mechanism a reload uses to give the flock a shepherd running this
/// binary's code.
///
/// [`Self::StopAndStart`] is permanent: it serves the three cases the handover
/// cannot, Windows, a shepherd predating the handover, and a handover that
/// fails to rehydrate.
#[derive(Debug, PartialEq, Eq)]
enum Arm {
    /// The `execve` handover: the shepherd replaces its own image and the
    /// flock never stops.
    ///
    /// Unix only: Windows has no `execve`.
    #[cfg(unix)]
    Handover,
    /// Stop the shepherd the way `kill` does, wait out its teardown, start a
    /// successor, and muster the roll back.
    StopAndStart,
}

/// The first shep release whose shepherd can hand its flock to a successor.
///
/// A floor on the version the CLI finds running, never on its own: the
/// handover has to exist in the shepherd being replaced. `0.1.17` is the last
/// release that shipped without it. It also keeps `PROTOCOL_VERSION` where it
/// is, since `Request::HandoverFitness` is a variant a shepherd below the
/// floor would end the connection on.
///
/// Only the unix arm of [`Arm::for_daemon`] reads it, hence the `allow` on
/// Windows.
#[cfg_attr(windows, allow(dead_code))]
const HANDOVER_SINCE: &str = "0.1.18";

/// `major.minor.patch` as three numbers, or `None` for anything this cannot
/// read.
///
/// Any pre-release or build suffix is dropped with the patch component it
/// hangs off, so `0.1.18-rc.1` reads as `0.1.18`. Every caller treats `None`
/// as the safe arm.
#[cfg_attr(windows, allow(dead_code))]
fn version_parts(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?;
    let patch = patch
        .split_once(['-', '+'])
        .map_or(patch, |(number, _suffix)| number);
    Some((major, minor, patch.parse().ok()?))
}

impl Arm {
    /// The arm that reloads a shepherd reporting `daemon_version`.
    ///
    /// [`Self::Handover`] needs the shepherd being replaced to carry the
    /// mechanism, so this compares its version against [`HANDOVER_SINCE`]; a
    /// shepherd reporting this binary's own version always counts as
    /// [`Self::Handover`], whatever the floor says. Whether that shepherd's
    /// flock can be carried is a separate question, asked over the socket.
    /// Unknown takes the safe arm: a `None` version, and one this CLI cannot
    /// parse.
    fn for_daemon(daemon_version: Option<&str>) -> Self {
        #[cfg(unix)]
        {
            let Some(running) = daemon_version.and_then(version_parts) else {
                return Self::StopAndStart;
            };
            let floor = version_parts(HANDOVER_SINCE)
                .expect("HANDOVER_SINCE is a literal three-number version");
            let own = version_parts(env!("CARGO_PKG_VERSION"))
                .expect("this crate's own version is a three-number version");
            if running >= floor || running == own {
                Self::Handover
            } else {
                Self::StopAndStart
            }
        }
        // No `execve`, so no handover, whatever the shepherd's version says.
        #[cfg(windows)]
        {
            let _ = daemon_version;
            Self::StopAndStart
        }
    }
}

/// The running shepherd's own crate version, as far as a failed connect can
/// report it.
///
/// Only a protocol refusal names one: every other connect failure happened
/// before the shepherd said who it is.
fn version_from_refusal(err: &ConnectError) -> Option<&str> {
    match err {
        ConnectError::ProtocolMismatch { daemon_version, .. } => daemon_version.as_deref(),
        _ => None,
    }
}

/// Replaces the running shepherd with one running this binary's code, and
/// brings the flock back.
///
/// One of the three recovery verbs the version guard exempts, so it must work
/// against a shepherd that refuses the handshake and never needs the socket to
/// succeed. `guard` is threaded from `crate::run`.
pub async fn reload(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    guard: crate::version_guard::VersionGuard,
) -> ExitCode {
    reload_with_wait(streams, paths, guard, admin::KILL_TEARDOWN_WAIT).await
}

/// As [`reload`], but with a caller-chosen teardown wait.
async fn reload_with_wait(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    guard: crate::version_guard::VersionGuard,
    wait: std::time::Duration,
) -> ExitCode {
    // Before the connection and before either arm: the handover arm execs a
    // successor that re-reads this file, and a value that fails to load there
    // exits it with the predecessor already gone. File only, since the
    // successor inherits the daemon's own env and flags through `execve`.
    if let Err(err) = read_daemon_config_source(paths).and_then(|source| {
        DaemonConfig::load(source.as_deref(), &|_| None).map_err(DaemonRunError::from)
    }) {
        return streams.fail(daemon_exit_code(&err), &err.to_string());
    }

    // The same argument, aimed at the other file the successor reads: nothing
    // below has signalled the predecessor yet, so a refusal here ends the verb
    // with the running shepherd untouched. Run, not dry-run: the migration is
    // idempotent, and leaves no window between a check and the act it checked.
    match dog_migration::migrate_dog_sections(paths) {
        Ok(moved) if moved.is_empty() => {}
        Ok(moved) => {
            streams.aside(
                "reload",
                &format!(
                    "moved dog config out of shep.toml and into dogs.toml: {}",
                    moved.join(", ")
                ),
            );
        }
        Err(err) => {
            let err = DaemonRunError::DogMigration(err);
            return streams.fail(daemon_exit_code(&err), &err.to_string());
        }
    }

    // Connected to ask who is there, and, on the handover arm, whether this
    // flock can be carried. Dropped before anything is signalled.
    let connected = match Client::connect(&paths.socket).await {
        Ok(client) => Ok(client),
        Err(err) => Err(version_from_refusal(&err).map(str::to_owned)),
    };
    // `cfg(unix)`, like its only reader: on Windows the arm is never in
    // question and the binding would warn as unused.
    #[cfg(unix)]
    let running_version = match &connected {
        Ok(client) => Some(client.daemon().daemon_version.clone()),
        Err(from_refusal) => from_refusal.clone(),
    };
    #[cfg(unix)]
    if Arm::for_daemon(running_version.as_deref()) == Arm::Handover
        // A version learned from a protocol refusal cannot reach this arm: the
        // decision needs a fitness answer, and a refused handshake has no
        // connection to ask over.
        && let Ok(client) = &connected
    {
        match ask_fitness(client).await {
            Fitness::Carryable => {
                // Before the signal: the shepherd is about to replace its own
                // image, and this connection would not survive it.
                drop(connected);
                return hand_over(streams, paths, guard, wait).await;
            }
            // Not a failure: the flock carries something that cannot move in
            // place, so the reload happens the other way and says why.
            Fitness::Refused(reason) => {
                streams.aside("reload", &reason);
            }
        }
    }
    // Before the stop arm too: on Windows the control address is a named pipe,
    // and a handle held open here keeps the pipe instance alive past the
    // daemon's exit, so `stop_and_start`'s wait would run out.
    drop(connected);
    stop_and_start(streams, paths, guard, wait).await
}

/// What a shepherd says about carrying its own flock.
///
/// The daemon owns both the gate and the wording; the refusal reaches the
/// operator verbatim.
#[cfg(unix)]
#[derive(Debug)]
enum Fitness {
    /// Every sheep can be carried across the exec.
    Carryable,
    /// At least one cannot, and this sentence says which and why.
    Refused(String),
}

/// Asks `client`'s shepherd whether its flock can be handed over in place.
///
/// Every failure is a refusal rather than an error: a shepherd that answers
/// the wrong reply, refuses the request, or drops the connection has said
/// nothing this CLI can act on, and the stop arm works against all three. A
/// signal carries no reply, so the question is settled before one is sent.
#[cfg(unix)]
async fn ask_fitness(client: &Client) -> Fitness {
    match client
        .request(shep_core::protocol::Request::HandoverFitness)
        .await
    {
        Ok(shep_core::protocol::Response::HandoverFitness { refusal: None }) => Fitness::Carryable,
        Ok(shep_core::protocol::Response::HandoverFitness {
            refusal: Some(reason),
        }) => Fitness::Refused(reason),
        Ok(other) => Fitness::Refused(format!(
            "this shepherd answered a handover question with {other:?}, so its flock is being \
             stopped and started instead"
        )),
        Err(err) => Fitness::Refused(format!(
            "this shepherd could not say whether its flock can be handed over ({err}), so it is \
             being stopped and started instead"
        )),
    }
}

/// Signals the shepherd to replace its own image in place, then waits for
/// the successor to serve and reports the flock.
///
/// The flock never stops: a carried sheep keeps its pid, its log file handles
/// and its place in the shepherd's registry. The signal is the trigger, not
/// the decision, which already happened over the socket. A handover can still
/// fail after the signal, so this waits for a shepherd of this binary's
/// version and takes the stop arm's own tail when none answers.
#[cfg(unix)]
async fn hand_over(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    guard: crate::version_guard::VersionGuard,
    wait: std::time::Duration,
) -> ExitCode {
    let pid = match proven_shepherd(streams, paths) {
        Ok(pid) => pid,
        Err(code) => return code,
    };
    // Held across the signal: accepted connections are not carried across the
    // `execve`, so this one closing is the proof the old image is gone.
    // Predecessor and successor otherwise answer on the same pid and version.
    let Ok(witness) = Client::connect(&paths.socket).await else {
        // No witness, no handover: `await_successor` with nothing to outlive
        // would take the predecessor's own answer as the successor's.
        let message = "could not hold a connection across the handover signal; \
                       stopping and starting instead";
        streams.aside("reload", message);
        return stop_and_start(streams, paths, guard, wait).await;
    };
    if let Err((code, message)) = signal_handover(pid) {
        return streams.fail(code, &message);
    }
    match await_successor(paths, &witness, wait).await {
        // The successor carried the flock; nothing to restore.
        Some(client) => report_reload(&client, streams, false).await,
        None => {
            // The flock is about to be started rather than carried, so the
            // pids change and the operator is told.
            let message = "the shepherd did not come back on this version after the handover \
                           signal; starting one instead";
            streams.aside("reload", message);
            let client = match crate::client::connect_or_spawn_client(streams, paths, guard).await {
                Ok(client) => client,
                Err(code) => return code,
            };
            report_reload(&client, streams, true).await
        }
    }
}

/// Asks the shepherd at `pid` to hand its flock to a successor.
///
/// SIGHUP, since SIGUSR2 is already the log-reopen signal. A shepherd too old
/// to hand over installs SIGHUP as a graceful stop, so the arm selection keeps
/// this away from one.
///
/// # Errors
/// The exit code and the sentence to report, when the pid is not one this
/// platform can name or the signal itself failed. The caller prints it.
#[cfg(unix)]
fn signal_handover(pid: u32) -> Result<(), (ExitCode, String)> {
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    let Ok(target) = i32::try_from(pid) else {
        let message = format!("the recorded pid {pid} is not one this platform can signal");
        return Err((ExitCode::Internal, message));
    };
    signal::kill(Pid::from_raw(target), Signal::SIGHUP).map_err(|errno| {
        let message = format!("could not signal the shepherd at pid {pid}: {errno}");
        (ExitCode::Failure, message)
    })
}

/// Polls the control socket until a shepherd running this binary's version
/// answers, or `wait` expires.
///
/// The version is what tells the two images apart: the predecessor answers on
/// the same socket right up until it execs. A connect failure inside the
/// window is expected, not a fault.
#[cfg(unix)]
async fn await_successor(
    paths: &ShepPaths,
    witness: &Client,
    wait: std::time::Duration,
) -> Option<Client> {
    let deadline = tokio::time::Instant::now() + wait;

    // Stage one: wait out the predecessor. A request answered on `witness`
    // says the old image is still serving, since that connection cannot
    // survive its exec. Never skipped: a caller without a witness must not
    // reach here.
    while witness
        .request(shep_core::protocol::Request::ListFlock)
        .await
        .is_ok()
    {
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(SUCCESSOR_POLL_INTERVAL).await;
    }

    // Stage two: the old image is gone, so an answered request can only have
    // come from the successor.
    loop {
        // A handshake proves a daemon answered, not that the successor did:
        // `execve` keeps the pid, and this arm can be selected against a
        // shepherd of this very version. Only a served request separates them,
        // since the outgoing image stops serving the moment it execs.
        if let Ok(client) = Client::connect(&paths.socket).await
            && client.daemon().daemon_version == env!("CARGO_PKG_VERSION")
            && client
                .request(shep_core::protocol::Request::ListFlock)
                .await
                .is_ok()
        {
            return Some(client);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(SUCCESSOR_POLL_INTERVAL).await;
    }
}

/// Gap between [`await_successor`]'s probes. Short: an `execve` plus a
/// rehydrate is milliseconds of work.
#[cfg(unix)]
const SUCCESSOR_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// The pid of the shepherd owning this home, or the code and sentence
/// saying why there is none to act on.
///
/// Reads the lock the daemon holds for its whole life, not the pidfile: a
/// stale pidfile from a crash still exists and its pid may have been reused.
fn proven_shepherd(streams: &mut Streams<'_>, paths: &ShepPaths) -> Result<u32, ExitCode> {
    match boot::daemon_liveness(paths) {
        Ok(Shepherd::Running(pid)) => Ok(pid),
        // Alive and owns the home, but has not recorded a pid yet.
        Ok(Shepherd::Booting) => {
            let message = "a shepherd is starting up and has not recorded its pid yet; try again";
            Err(streams.fail(ExitCode::DaemonUnreachable, message))
        }
        // Nothing to replace: `reload` matches a running system to the binary,
        // so it never starts one unasked.
        Ok(Shepherd::Absent) => {
            let message = format!(
                "no shepherd is running, so there is nothing to reload (nothing holds the lock \
                 on `{}`). `shep muster` brings the flock up from the roll",
                boot::pidfile(paths).display()
            );
            Err(streams.fail(ExitCode::DaemonUnreachable, &message))
        }
        Err(err) => Err(streams.fail(ExitCode::Failure, &err.to_string())),
    }
}

/// What a reload does when [`admin::KILL_TEARDOWN_WAIT`] elapses with the
/// control address still answering: the refusal to report, or `None` to go on
/// and start the successor anyway.
///
/// The elapsed clock is not the answer, the pidfile lock is. A staged
/// teardown costs a sum over stages rather than one kill ladder, so no
/// constant is large enough for every flock, and the one state this verb must
/// never leave behind is a signalled predecessor with no successor. So:
///
/// - [`Shepherd::Absent`] means the predecessor is gone and only its socket
///   file outlived it. Starting the successor is the whole point of the verb,
///   and refusing here is what would strand the flock.
/// - [`Shepherd::Running`] and [`Shepherd::Booting`] mean the predecessor
///   still owns the home, so it is still the shepherd supervising the flock
///   and nothing has been lost by not replacing it yet.
/// - An unreadable pidfile answers neither, so it refuses rather than start a
///   second shepherd over a first that may still be running.
fn refusal_after_teardown_budget(
    liveness: Result<Shepherd, BootError>,
) -> Option<(ExitCode, String)> {
    match liveness {
        Ok(Shepherd::Absent) => None,
        Ok(Shepherd::Running(_) | Shepherd::Booting) => Some((
            ExitCode::DeadlineExceeded,
            "a shepherd still owns this home and still supervises the flock; nothing has been \
             started in its place, and `shep daemon reload` can be run again once it has \
             stopped"
                .to_string(),
        )),
        Err(err) => Some((
            ExitCode::Failure,
            format!(
                "the shepherd was signalled, teardown is still in progress, and this home's \
                 pidfile could not be read to find out whether it stopped ({err}); nothing has \
                 been started in its place"
            ),
        )),
    }
}

/// Stops the shepherd, waits it out, starts a successor, and musters.
///
/// [`boot::daemon_liveness`] proves the pid, `commands::admin` owns the signal
/// and the teardown wait, and `crate::client::connect_or_spawn_client` is the
/// autostart `shep start` already uses.
///
/// A budget that elapses is a question rather than an answer, and
/// [`refusal_after_teardown_budget`] asks the lock: the predecessor has been
/// signalled by then, so reading the clock as a failure is how this verb
/// would leave a home with no shepherd at all.
async fn stop_and_start(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    guard: crate::version_guard::VersionGuard,
    wait: std::time::Duration,
) -> ExitCode {
    let pid = match proven_shepherd(streams, paths) {
        Ok(pid) => pid,
        Err(code) => return code,
    };
    if let Err((code, message)) = admin::signal_graceful_stop(pid) {
        return streams.fail(code, &message);
    }
    if !admin::wait_for_socket_to_disappear(&paths.socket, wait).await
        && let Some((code, message)) = refusal_after_teardown_budget(boot::daemon_liveness(paths))
    {
        return streams.fail(code, &message);
    }
    let client = match crate::client::connect_or_spawn_client(streams, paths, guard).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    report_reload(&client, streams, true).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Format;
    use crate::version_guard::VersionGuard;
    use shep_core::protocol::{Request, Response};

    #[test]
    fn reload_picks_the_stop_arm_against_a_daemon_too_old_to_hand_over() {
        assert_eq!(Arm::for_daemon(Some("0.1.8")), Arm::StopAndStart);
    }

    /// Component-wise rather than lexical: `0.1.9` against `0.1.18` is the
    /// case that tells the two apart.
    #[test]
    fn reload_picks_the_stop_arm_at_every_version_below_the_floor() {
        assert_eq!(Arm::for_daemon(Some("0.1.9")), Arm::StopAndStart);
        assert_eq!(Arm::for_daemon(Some("0.1.16")), Arm::StopAndStart);
        assert_eq!(
            Arm::for_daemon(Some("not a version")),
            Arm::StopAndStart,
            "a version this CLI cannot read is unknown, and unknown is the safe arm"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_shepherd_of_this_binarys_own_version_answers_for_itself() {
        assert_eq!(
            Arm::for_daemon(Some(env!("CARGO_PKG_VERSION"))),
            Arm::Handover
        );
    }

    #[test]
    fn reload_picks_the_stop_arm_when_the_handshake_is_refused_without_a_version() {
        let refusal = ConnectError::ProtocolMismatch {
            client: shep_core::protocol::PROTOCOL_VERSION,
            daemon_version: None,
            message: "this daemon speaks protocol 1".to_string(),
        };
        assert_eq!(version_from_refusal(&refusal), None);
        assert_eq!(
            Arm::for_daemon(version_from_refusal(&refusal)),
            Arm::StopAndStart
        );
    }

    #[test]
    fn a_refusal_that_names_a_version_yields_it_for_the_arm_choice() {
        let refusal = ConnectError::ProtocolMismatch {
            client: shep_core::protocol::PROTOCOL_VERSION,
            daemon_version: Some("0.1.8".to_string()),
            message: "this daemon speaks protocol 1".to_string(),
        };
        assert_eq!(version_from_refusal(&refusal), Some("0.1.8"));
    }

    #[test]
    fn a_connect_failure_that_is_not_a_refusal_names_no_version() {
        let err = ConnectError::HandshakeClosed;
        assert_eq!(version_from_refusal(&err), None);
    }

    #[tokio::test]
    async fn reload_reports_each_sheep_rather_than_announcing_the_flock_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let addr = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = shep_client::testing::fake_client_answering(&addr, |_req| {
            Response::Mustered(vec![shep_client::testing::sample_info()])
        })
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            report_reload(&client, &mut streams, true).await
        };

        assert_eq!(code, ExitCode::Success);
        let text = format!(
            "{}{}",
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap()
        );
        assert!(text.contains("web"), "{text}");
        assert!(!text.to_lowercase().contains("flock stopped"), "{text}");
    }

    /// Drives `reload` end to end, so the connect, the arm choice and the
    /// liveness proof are all in the path.
    #[tokio::test]
    async fn reload_refuses_a_home_no_shepherd_owns() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.run).unwrap();
        std::fs::create_dir_all(&paths.pids).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            reload(&mut streams, &paths, VersionGuard::Exempt).await
        };

        assert_eq!(code, ExitCode::DaemonUnreachable);
        let text = String::from_utf8(err).unwrap();
        assert!(text.contains("no shepherd"), "{text}");
    }

    /// Valid TOML holding an invalid level, the gap `toml_edit`'s own parse
    /// check cannot close. No daemon runs here, so without the pre-flight
    /// `reload` would return `DaemonUnreachable`.
    #[tokio::test]
    async fn reload_refuses_a_shep_toml_that_will_not_load() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(paths.daemon_config.parent().unwrap()).unwrap();
        std::fs::write(&paths.daemon_config, "[daemon]\nlog_level = \"verbose\"\n").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            reload(&mut streams, &paths, VersionGuard::Exempt).await
        };

        assert_eq!(code, ExitCode::InvalidConfig);
        let rendered = String::from_utf8(err).unwrap();
        assert!(
            rendered.contains("verbose"),
            "the refusal must name the bad value: {rendered}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reload_picks_the_handover_against_a_daemon_new_enough_to_carry_its_flock() {
        assert_eq!(Arm::for_daemon(Some("9.9.9")), Arm::Handover);
        assert_eq!(Arm::for_daemon(Some(HANDOVER_SINCE)), Arm::Handover);
    }

    /// `Request::HandoverFitness` is a variant an older daemon cannot parse,
    /// so the version gate has to keep it unsent.
    #[tokio::test]
    async fn a_reload_against_an_older_daemon_never_sends_the_query() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.run).unwrap();
        std::fs::create_dir_all(&paths.pids).unwrap();
        let mut sent = shep_client::testing::fake_daemon_answering_with_ack(
            &paths.socket,
            ack_naming("0.1.8"),
            |_| Response::Pong,
        )
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            reload(&mut streams, &paths, VersionGuard::Exempt).await;
        }

        let asked: Vec<Request> = std::iter::from_fn(|| sent.try_recv().ok())
            .map(|envelope| envelope.body)
            .collect();
        assert!(
            !asked
                .iter()
                .any(|req| matches!(req, Request::HandoverFitness)),
            "a daemon that cannot parse the query must never be asked it: {asked:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_reload_against_a_newer_daemon_asks_before_it_signals() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.run).unwrap();
        std::fs::create_dir_all(&paths.pids).unwrap();
        let mut sent = shep_client::testing::fake_daemon_answering_with_ack(
            &paths.socket,
            ack_naming("9.9.9"),
            |_| Response::HandoverFitness { refusal: None },
        )
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            reload(&mut streams, &paths, VersionGuard::Exempt).await;
        }

        let asked: Vec<Request> = std::iter::from_fn(|| sent.try_recv().ok())
            .map(|envelope| envelope.body)
            .collect();
        assert_eq!(
            asked
                .iter()
                .filter(|req| matches!(req, Request::HandoverFitness))
                .count(),
            1,
            "exactly one fitness query, and it is the first thing asked: {asked:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_flock_prints_the_reason_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.run).unwrap();
        std::fs::create_dir_all(&paths.pids).unwrap();
        let _sent = shep_client::testing::fake_daemon_answering_with_ack(
            &paths.socket,
            ack_naming("9.9.9"),
            |_| Response::HandoverFitness {
                refusal: Some("sheep 'clustered' has more than one instance".to_string()),
            },
        )
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            reload(&mut streams, &paths, VersionGuard::Exempt).await
        };

        let text = format!(
            "{}{}",
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap()
        );
        assert!(text.contains("more than one instance"), "{text}");
        // No shepherd owns this home, so the stop arm has nothing to stop,
        // which is what proves it took that arm.
        assert_eq!(code, ExitCode::DaemonUnreachable, "{text}");
    }

    #[test]
    fn a_predecessor_that_is_still_tearing_down_gets_no_successor_started_over_it() {
        // fails if an elapsed teardown budget is read as "the predecessor is
        // gone". It still holds the lock, so it is still the shepherd, and a
        // second one over the top of it would be two supervisors on one home.
        let (code, message) =
            refusal_after_teardown_budget(Ok(Shepherd::Running(4242))).expect("a refusal");
        assert_eq!(code, ExitCode::DeadlineExceeded);
        assert!(
            message.contains("still supervises the flock")
                && message.contains("nothing has been started in its place"),
            "the refusal must say the flock is still supervised: {message}"
        );

        let (booting, _) = refusal_after_teardown_budget(Ok(Shepherd::Booting)).expect("a refusal");
        assert_eq!(booting, ExitCode::DeadlineExceeded);
    }

    #[test]
    fn a_predecessor_already_gone_when_the_budget_elapsed_still_gets_a_successor() {
        // fails if a slow teardown that finished just past the budget is
        // reported as a timeout. That is the one outcome this verb may never
        // leave behind: a signalled predecessor and no successor.
        assert!(
            refusal_after_teardown_budget(Ok(Shepherd::Absent)).is_none(),
            "an absent predecessor is the state the successor is started in"
        );
    }

    #[test]
    fn a_pidfile_that_cannot_be_read_after_the_budget_refuses_rather_than_guess() {
        // fails if an unreadable pidfile is treated as an absent shepherd,
        // which would start a second one over a first that may still run.
        let (code, message) = refusal_after_teardown_budget(Err(BootError::Io {
            path: std::path::PathBuf::from("run/shepd.pid"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        }))
        .expect("a refusal");
        assert_eq!(code, ExitCode::Failure);
        assert!(
            message.contains("pidfile could not be read"),
            "the refusal must name what it could not read: {message}"
        );
    }

    #[test]
    fn the_teardown_budget_covers_a_staged_shutdown_and_not_one_kill_ladder() {
        // fails if the budget goes back to a single flock-wide ladder. A
        // reverse-order teardown pays each stage's longest ladder in turn, so
        // four stages holding one `kill_timeout = "5s"` member each need
        // twenty seconds of a shutdown that is working correctly.
        assert!(
            admin::KILL_TEARDOWN_WAIT >= std::time::Duration::from_secs(60),
            "the budget is the daemon's own 60s ceiling, not one ladder"
        );
    }

    /// A `HelloAck` naming `version`, for the arm-selection tests.
    fn ack_naming(version: &str) -> shep_core::protocol::HelloAck {
        shep_core::protocol::HelloAck {
            daemon_version: version.to_string(),
            protocol: shep_core::protocol::PROTOCOL_VERSION,
            pid: 4242,
            min_supported: None,
        }
    }
}
