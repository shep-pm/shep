//! Registers and removes the logon task through `schtasks.exe`.
//!
//! `schtasks /XML` rather than the Task Scheduler COM API: no `unsafe` and no
//! new crate. Needs no elevation, since the task is the signed-in user's own.

use super::render::{TaskTarget, refuse, run_args, task_name, task_path, task_xml};
use crate::cli::StartupArgs;
use crate::commands::startup_shared::{ABSENT, OK, Refusal, failure_line, report};
use crate::exit::ExitCode;
use crate::output::{StartupStep, Streams};
use shep_core::paths::ShepPaths;
use shep_daemon::boot::{self, Shepherd};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

/// Registers the logon task for the signed-in user.
///
/// `home` is the `$SHEP_HOME` the task carries, already through `ensure_home`.
/// Refused before anything is written, in order: `--init` or another `--user`
/// ([`ExitCode::Usage`]); a task that already exists ([`ExitCode::Usage`]).
/// A shepherd already holding `home` stays, and the task is not run.
pub(crate) fn startup(streams: &mut Streams<'_>, home: &Path, args: &StartupArgs) -> ExitCode {
    let (user, exec) = match signed_in_user().and_then(|user| {
        let exec = std::env::current_exe().map_err(|err| Refusal {
            code: ExitCode::Failure,
            message: format!("cannot find this executable to register: {err}"),
        })?;
        Ok((user, exec))
    }) {
        Ok(found) => found,
        Err(refusal) => return streams.fail(refusal.code, &refusal.message),
    };
    let target = TaskTarget {
        user,
        exec,
        home: home.to_path_buf(),
    };
    register(streams, &target, args)
}

/// Removes the logon task. A running shepherd keeps running.
pub(crate) fn unstartup(streams: &mut Streams<'_>, args: &StartupArgs) -> ExitCode {
    match signed_in_user() {
        Ok(user) => remove(streams, &user, args),
        Err(refusal) => streams.fail(refusal.code, &refusal.message),
    }
}

fn register(streams: &mut Streams<'_>, target: &TaskTarget, args: &StartupArgs) -> ExitCode {
    if let Err(refusal) = refuse(args, &target.user) {
        return streams.fail(refusal.code, &refusal.message);
    }
    let name = task_name(&target.user);
    match task_exists(&name) {
        Ok(true) => {
            return streams.fail(
                ExitCode::Usage,
                &format!(
                    "task {} already exists; shep unstartup removes it first",
                    task_path(&target.user)
                ),
            );
        }
        Ok(false) => {}
        Err(err) => return streams.fail(ExitCode::Failure, &err),
    }

    let shepherd = shepherd_on(&target.home).unwrap_or_else(|err| {
        streams.aside(
            "liveness",
            &format!("could not tell whether a shepherd already runs; running the task: {err}"),
        );
        Shepherd::Absent
    });
    let start = shepherd == Shepherd::Absent;
    if !start {
        streams.aside(
            "already_running",
            &format!(
                "a shepherd already runs on {}; it stays, and the task brings it back at the \
                 next sign-in",
                target.home.display()
            ),
        );
    }

    let mut steps = vec![StartupStep {
        action: "wrote",
        target: task_path(&target.user),
        result: create_task(&name, &task_xml(target)).map_or_else(|line| line, |()| OK.to_string()),
    }];
    if let Some(run) = run_args(&target.user, start) {
        steps.push(StartupStep {
            action: "ran",
            target: format!("schtasks {}", run.join(" ")),
            result: schtasks(&run).map_or_else(|err| err, |()| OK.to_string()),
        });
    }
    report(streams, "startup", steps)
}

fn remove(streams: &mut Streams<'_>, user: &str, args: &StartupArgs) -> ExitCode {
    if let Err(refusal) = refuse(args, user) {
        return streams.fail(refusal.code, &refusal.message);
    }
    let name = task_name(user);
    let result = match task_exists(&name) {
        Ok(false) => ABSENT.to_string(),
        Ok(true) => {
            schtasks(&["/Delete", "/TN", &name, "/F"]).map_or_else(|line| line, |()| OK.to_string())
        }
        Err(err) => return streams.fail(ExitCode::Failure, &err),
    };
    let steps = vec![StartupStep {
        action: "removed",
        target: task_path(user),
        result,
    }];
    report(streams, "unstartup", steps)
}

/// `%USERNAME%`: the account the task is registered for and runs as.
fn signed_in_user() -> Result<String, Refusal> {
    match std::env::var("USERNAME") {
        Ok(user) if !user.is_empty() => Ok(user),
        _ => Err(Refusal {
            code: ExitCode::Failure,
            message: "%USERNAME% is not set, so there is no user to register the task for"
                .to_string(),
        }),
    }
}

/// What holds `home`'s pidfile lock, asked without creating the pidfile.
fn shepherd_on(home: &Path) -> Result<Shepherd, boot::BootError> {
    let paths = ShepPaths::at(home.to_path_buf());
    if !boot::pidfile(&paths).exists() {
        return Ok(Shepherd::Absent);
    }
    boot::daemon_liveness(&paths)
}

/// Runs `schtasks.exe`; `Err` is the one line a failure is reported by.
fn run(args: &[&str]) -> Result<Output, String> {
    Command::new("schtasks.exe")
        .args(args)
        .output()
        .map_err(|err| format!("could not run schtasks.exe: {err}"))
}

fn schtasks<S: AsRef<str>>(args: &[S]) -> Result<(), String> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let output = run(&args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(failure_line(&output))
    }
}

/// Whether Task Scheduler has a task named `name`. A query that exits
/// non-zero is a task that is not there.
fn task_exists(name: &str) -> Result<bool, String> {
    Ok(run(&["/Query", "/TN", name])?.status.success())
}

/// Registers `xml` as `name`. The XML goes through a temporary file, removed
/// whether or not `schtasks` accepted it.
fn create_task(name: &str, xml: &str) -> Result<(), String> {
    let mut file = tempfile::Builder::new()
        .prefix("shep-task-")
        .suffix(".xml")
        .tempfile()
        .map_err(|err| format!("could not write the task file: {err}"))?;
    // UTF-8 with no BOM and no `<?xml?>` declaration: `schtasks` refuses a
    // file that declares an encoding it did not read.
    file.write_all(xml.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|err| format!("could not write the task file: {err}"))?;
    let path = file.into_temp_path();
    let path_arg = path.display().to_string();
    schtasks(&["/Create", "/TN", name, "/XML", &path_arg])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Format;
    use std::path::PathBuf;

    fn no_args() -> StartupArgs {
        StartupArgs {
            user: None,
            init: None,
        }
    }

    /// The rendered task with its action swapped for one that does nothing,
    /// so a logon between create and delete cannot start a shepherd.
    fn no_op_task_xml() -> String {
        let target = TaskTarget {
            user: signed_in_user().unwrap(),
            exec: PathBuf::from("shep.exe"),
            home: PathBuf::from(r"C:\"),
        };
        let xml = task_xml(&target);
        let (start, end) = (xml.find("<Exec>").unwrap(), xml.find("</Exec>").unwrap());
        format!(
            "{}<Exec><Command>cmd.exe</Command><Arguments>/c exit 0</Arguments>{}",
            &xml[..start],
            &xml[end..]
        )
    }

    #[test]
    fn a_task_registers_queries_and_deletes() {
        let name = format!("shep-test-{}", std::process::id());
        assert!(!task_exists(&name).unwrap());
        create_task(&name, &no_op_task_xml()).unwrap();
        let found = task_exists(&name);
        let deleted = schtasks(&["/Delete", "/TN", &name, "/F"]);
        assert!(found.unwrap(), "the created task was not found");
        deleted.unwrap();
        assert!(!task_exists(&name).unwrap(), "the task survived its delete");
    }

    #[test]
    fn unstartup_with_no_task_reports_absent() {
        let user = format!("no-such-user-{}", std::process::id());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Json);
            remove(&mut streams, &user, &no_args())
        };
        assert_eq!(code, ExitCode::Success);
        let rows = String::from_utf8(out).unwrap();
        assert!(rows.contains("\"absent\""), "{rows}");
        assert!(rows.contains(&format!(r"\\shep-{user}")), "{rows}");
    }
}
