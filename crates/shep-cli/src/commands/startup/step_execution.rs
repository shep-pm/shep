use super::privilege_check::{Privilege, privilege};
use super::target_resolution::{StartupPlan, plan};
use super::unit_layout::{remove_unit, secure_path_warning, unit_file_name, write_unit};
use crate::cli::{Init, StartupArgs};
use crate::exit::ExitCode;
use crate::output::{StartupStep, StartupSteps, Streams, emit, write_outcome};
use std::path::Path;

/// A step that did what it was asked.
pub(super) const OK: &str = "ok";

/// A step that found nothing to do, and that being the state it was asked to
/// produce. Only `unstartup` reaches it.
pub(super) const ABSENT: &str = "absent";

/// A refusal that never got as far as a step: the exit code to return, and
/// the one line explaining it.
#[derive(Debug)]
pub(super) struct Refusal {
    pub(super) code: ExitCode,
    pub(super) message: String,
}

/// Installs the init unit that starts the shepherd at boot.
///
/// `explicit_home` is `--home`/`$SHEP_HOME` as clap already folded it, and
/// `run` has already refused one that is not there. When it names nothing
/// the unit carries the target user's own `<passwd home>/.shep`, never
/// this process's `$HOME`: under `sudo` that is root's, and a unit built
/// from it restores nothing after a reboot. A default that is this
/// process's own is created first; see [`StartupPlan::own_default_home`].
pub fn startup(
    streams: &mut Streams<'_>,
    explicit_home: Option<&Path>,
    args: &StartupArgs,
) -> ExitCode {
    let plan = match plan(explicit_home, args) {
        Ok(plan) => plan,
        Err(refusal) => return refuse(streams, refusal.code, &refusal.message),
    };
    // Created before the privilege and existing-unit checks on purpose.
    // An unprivileged run prints `sudo ... --home <default>`, which
    // `ensure_home` refuses unless the default exists. Every other verb
    // creates it before doing anything, too.
    if let Some(paths) = plan.own_default_home.clone()
        && let Err(refusal) = crate::home::create_default_home(streams, paths)
    {
        return refuse(streams, refusal.code(), &refusal.to_string());
    }
    install(streams, &plan, privilege())
}

/// Disables and removes the unit [`startup`] installed.
///
/// Resolves its plan with no explicit home: a removal is addressed by the
/// unit's path and label, both of which come from the target user alone, and
/// nothing here reads the `$SHEP_HOME` the unit happens to carry.
pub fn unstartup(streams: &mut Streams<'_>, args: &StartupArgs) -> ExitCode {
    match plan(None, args) {
        Ok(plan) => remove(streams, &plan, privilege()),
        Err(refusal) => refuse(streams, refusal.code, &refusal.message),
    }
}

/// Writes and enables the unit, or prints the command that would.
///
/// Refused, in order, before anything is written: a `$SHEP_HOME` that is
/// not a directory ([`ExitCode::Usage`]); a unit that already exists
/// ([`ExitCode::Usage`], naming `unstartup`, since rewriting a loaded
/// unit's file would leave it disagreeing with the running service); then
/// [`Privilege::Unprivileged`], which prints the resolved `sudo` command
/// and exits [`ExitCode::Failure`].
///
/// If [`plan`] saw `$SUDO_USER` set, [`secure_path_warning`] also warns:
/// `sudo` typically replaces `PATH` with its own `secure_path`, and shep
/// has no login `PATH` left to compare against.
pub(crate) fn install(
    streams: &mut Streams<'_>,
    plan: &StartupPlan,
    privilege: Privilege,
) -> ExitCode {
    if !plan.spec.home.is_dir() {
        return refuse(
            streams,
            ExitCode::Usage,
            &format!(
                "no directory at {}; create it first (any shep verb run as {} creates that \
                 user's own ~/.shep), or pass --home with the $SHEP_HOME this unit should carry",
                plan.spec.home.display(),
                plan.spec.user,
            ),
        );
    }
    if plan.unit_path.exists() {
        return refuse(
            streams,
            ExitCode::Usage,
            &format!(
                "{} already exists; shep unstartup removes it first",
                plan.unit_path.display()
            ),
        );
    }
    if privilege == Privilege::Unprivileged {
        return refuse(
            streams,
            ExitCode::Failure,
            &format!(
                "installing the unit needs root; run: sudo {} startup --user {} --home {}",
                shell_quote(&plan.spec.exec.display().to_string()),
                shell_quote(&plan.spec.user),
                shell_quote(&plan.spec.home.display().to_string()),
            ),
        );
    }
    if let Some(message) = secure_path_warning(plan.sudo_user.as_deref(), &plan.spec) {
        streams.aside("secure_path", &message);
    }

    let mut steps = vec![write_unit(plan)];
    match plan.init {
        Init::Systemd => {
            steps.push(run_step("systemctl", &["daemon-reload"]));
            steps.push(run_step(
                "systemctl",
                &["enable", "--now", &unit_file_name(plan)],
            ));
        }
        Init::Launchd => steps.push(run_step(
            "launchctl",
            &["bootstrap", "system", &plan.unit_path.display().to_string()],
        )),
        Init::Openrc => {
            steps.push(run_step(
                "rc-update",
                &["add", &unit_file_name(plan), "default"],
            ));
            steps.push(run_step("rc-service", &[&unit_file_name(plan), "start"]));
        }
        Init::FreebsdRc => {
            steps.push(run_step(
                "sysrc",
                &[&format!("{}_enable=YES", unit_file_name(plan))],
            ));
            steps.push(run_step("service", &[&unit_file_name(plan), "start"]));
        }
        Init::OpenbsdRc => {
            steps.push(run_step("rcctl", &["enable", &unit_file_name(plan)]));
            steps.push(run_step("rcctl", &["start", &unit_file_name(plan)]));
        }
    }
    report(streams, "startup", steps)
}

/// Disables and removes the unit, or prints the command that would.
///
/// A missing unit is a success carrying one `absent` row; that check runs
/// before the privilege gate, so `shep unstartup` on a host that never ran
/// `startup` answers without demanding root. Otherwise this needs root:
/// without it, prints `sudo <exec> unstartup --user <user>` (no `--home`,
/// since removal is addressed by path and label alone) and exits
/// [`ExitCode::Failure`].
pub(crate) fn remove(
    streams: &mut Streams<'_>,
    plan: &StartupPlan,
    privilege: Privilege,
) -> ExitCode {
    if !plan.unit_path.exists() {
        return report(
            streams,
            "unstartup",
            vec![StartupStep {
                action: "removed",
                target: plan.unit_path.display().to_string(),
                result: ABSENT.to_string(),
            }],
        );
    }
    if privilege == Privilege::Unprivileged {
        return refuse(
            streams,
            ExitCode::Failure,
            &format!(
                "removing the unit needs root; run: sudo {} unstartup --user {}",
                shell_quote(&plan.spec.exec.display().to_string()),
                shell_quote(&plan.spec.user),
            ),
        );
    }

    let mut steps = Vec::new();
    match plan.init {
        Init::Systemd => {
            steps.push(run_step(
                "systemctl",
                &["disable", "--now", &unit_file_name(plan)],
            ));
            steps.push(remove_unit(plan));
            steps.push(run_step("systemctl", &["daemon-reload"]));
        }
        Init::Launchd => {
            steps.push(run_step(
                "launchctl",
                &["bootout", &format!("system/{}", plan.label)],
            ));
            steps.push(remove_unit(plan));
        }
        Init::Openrc => {
            steps.push(run_step("rc-service", &[&unit_file_name(plan), "stop"]));
            steps.push(run_step(
                "rc-update",
                &["del", &unit_file_name(plan), "default"],
            ));
            steps.push(remove_unit(plan));
        }
        Init::FreebsdRc => {
            steps.push(run_step("service", &[&unit_file_name(plan), "stop"]));
            steps.push(run_step(
                "sysrc",
                &["-x", &format!("{}_enable", unit_file_name(plan))],
            ));
            steps.push(remove_unit(plan));
        }
        Init::OpenbsdRc => {
            steps.push(run_step("rcctl", &["stop", &unit_file_name(plan)]));
            steps.push(run_step("rcctl", &["disable", &unit_file_name(plan)]));
            steps.push(remove_unit(plan));
        }
    }
    report(streams, "unstartup", steps)
}

/// Runs one init-system command and reports it as a step.
///
/// Never short-circuits the caller: a command that failed is a row like any
/// other, and [`report`] is what turns a failed row into a non-zero exit
/// after every remaining step has still been attempted.
fn run_step(program: &str, args: &[&str]) -> StartupStep {
    let target = format!("{program} {}", args.join(" "));
    let result = match std::process::Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => OK.to_string(),
        Ok(output) => failure_line(&output),
        Err(err) => err.to_string(),
    };
    StartupStep {
        action: "ran",
        target,
        result,
    }
}

/// Emits the steps and returns the code they earned.
///
/// A step that failed fails the verb, but only after every remaining step has
/// run: a half-installed unit is worse than a fully-attempted one, and the
/// operator needs every row to know which half.
fn report(streams: &mut Streams<'_>, command: &str, steps: Vec<StartupStep>) -> ExitCode {
    let failed = steps
        .iter()
        .any(|step| step.result != OK && step.result != ABSENT);
    let written = write_outcome(emit(
        &mut *streams.out,
        streams.fmt,
        command,
        StartupSteps(steps),
        streams.style,
    ));
    if failed { ExitCode::Failure } else { written }
}

/// Quotes one word of a printed command so the line can be pasted rather
/// than read and repaired.
///
/// A `$SHEP_HOME` with a space is a legal path; unquoted, it would become
/// two arguments and paste a command carrying half the path.
///
/// `pub(crate)`: [`super::unit::freebsd_rc_script`] and [`super::unit::openbsd_rc_script`]
/// reuse it as the single-quote former for a value re-evaluated by a
/// nested shell, distinct from `unit`'s own double-quote escaper.
pub(crate) fn shell_quote(word: &str) -> String {
    let safe = |b: &u8| b.is_ascii_alphanumeric() || b"_./:@%+=-".contains(b);
    if !word.is_empty() && word.as_bytes().iter().all(safe) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The one line a failed command is reported by: the first non-blank line of
/// its stderr, or its exit status when it failed without saying anything.
///
/// One line because a row is one line, and systemd answers a refusal with
/// several of its own advice.
fn failure_line(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(|| format!("failed: {}", output.status), ToString::to_string)
}

/// Writes one refusal to stderr and returns the code it earned.
pub(super) fn refuse(streams: &mut Streams<'_>, code: ExitCode, message: &str) -> ExitCode {
    streams.fail(code, message)
}

#[cfg(test)]
mod tests {
    use super::super::privilege_check::Privilege;

    use crate::cli::Init;
    use crate::exit::ExitCode;
    use crate::output::{StartupStep, Streams};

    use super::super::testing::*;
    use super::*;
    use crate::cli::Format;

    /// Exit 0 would make a script believe a unit was installed; a command
    /// missing `--home` re-runs the sudo trap [`target_home`] describes.
    #[test]
    fn an_unprivileged_startup_prints_the_command_and_exits_non_zero() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        std::fs::create_dir_all(&home).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            install(&mut streams, &plan_for_test(&home), Privilege::Unprivileged)
        };
        assert_ne!(code, ExitCode::Success);
        let printed = String::from_utf8(err).unwrap();
        assert!(printed.contains("sudo"), "{printed}");
        assert!(printed.contains("--home"), "{printed}");
        assert!(printed.contains(home.to_str().unwrap()), "{printed}");
        assert!(
            !plan_for_test(&home).unit_path.exists(),
            "an unprivileged startup writes no unit"
        );
    }

    /// Accepting a missing `$SHEP_HOME` would yield a unit that boots
    /// cleanly and restores an empty flock.
    #[test]
    fn a_shep_home_that_does_not_exist_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            // Root: an unprivileged run would refuse for the other reason
            // first, never exercising the home check.
            install(&mut streams, &plan_for_test(&missing), Privilege::Root)
        };
        assert_eq!(code, ExitCode::Usage);
        let printed = String::from_utf8(err).unwrap();
        assert!(printed.contains(missing.to_str().unwrap()), "{printed}");
        assert!(
            !plan_for_test(&missing).unit_path.exists(),
            "a refused startup writes no unit"
        );
    }

    /// Drives `report` with hand-built rows rather than a real `install`:
    /// producing a genuinely failing step would need a real `systemctl`,
    /// which no test in this crate may run.
    #[test]
    fn a_failed_step_fails_the_verb_and_still_prints_every_row() {
        let step = |action, target: &str, result: &str| StartupStep {
            action,
            target: target.to_string(),
            result: result.to_string(),
        };
        let steps = vec![
            step("wrote", "/etc/systemd/system/shep-deploy.service", OK),
            step(
                "ran",
                "systemctl daemon-reload",
                "Failed to reload: read-only file system",
            ),
            step("ran", "systemctl enable --now shep-deploy.service", OK),
        ];

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            report(&mut streams, "startup", steps)
        };
        assert_eq!(code, ExitCode::Failure);
        let printed = String::from_utf8(out).unwrap();
        for expected in [
            "daemon-reload",
            "read-only file system",
            "enable --now shep-deploy.service",
        ] {
            assert!(
                printed.contains(expected),
                "every step is reported, not only the ones before the failure: {printed}"
            );
        }
    }

    /// A `$SHEP_HOME` with a space is legal; unquoted it would split into
    /// two arguments.
    #[test]
    fn a_printed_command_quotes_what_a_shell_would_split() {
        assert_eq!(shell_quote("/home/ada/.shep"), "/home/ada/.shep");
        assert_eq!(shell_quote("/opt/my shep"), "'/opt/my shep'");
        assert_eq!(shell_quote("ada's"), r"'ada'\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    /// systemd answers a refusal in several lines; a row is one line.
    #[test]
    fn a_failed_step_reports_one_line_and_never_an_empty_one() {
        use std::os::unix::process::ExitStatusExt as _;

        let failed = std::process::ExitStatus::from_raw(256);
        let output = |stderr: &str| std::process::Output {
            status: failed,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        assert_eq!(
            failure_line(&output(
                "\nFailed to enable unit: Unit is masked.\nSee `journalctl`.\n"
            )),
            "Failed to enable unit: Unit is masked."
        );
        assert!(
            !failure_line(&output("")).is_empty(),
            "a command that failed silently still has to say so"
        );
    }

    /// The escape hatch for a container with no `/run/systemd/system`, and
    /// the only way a macOS host renders a systemd unit.
    #[test]
    fn an_explicit_init_beats_detection() {
        use clap::Parser as _;

        use crate::cli::{Cli, Commands};

        let cli = Cli::try_parse_from(["shep", "startup", "--init", "openrc"]).unwrap();
        match cli.command {
            Commands::Startup(args) => assert_eq!(args.init, Some(Init::Openrc)),
            other => panic!("expected Startup, got {other:?}"),
        }
    }
}
