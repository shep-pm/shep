//! The logon task `shep startup` registers on Windows, rendered as Task
//! Scheduler XML, and the rows `startup` and `unstartup` report.
//!
//! Pure: no environment reads, no `schtasks`. `install` runs what this
//! renders, and this compiles everywhere so macOS and Linux test it.

use crate::cli::StartupArgs;
use crate::commands::startup_shared::{Refusal, xml_text};
use crate::exit::ExitCode;
use std::path::PathBuf;

/// What the task needs to know about the machine, all passed in as values.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TaskTarget {
    /// `%USERNAME%`: the bare account name, with no domain.
    pub(super) user: String,
    /// The `shep.exe` the task runs.
    pub(super) exec: PathBuf,
    /// The `$SHEP_HOME` the task carries.
    pub(super) home: PathBuf,
}

/// The task's name, the unix unit name for `user`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn task_name(user: &str) -> String {
    format!("shep-{user}")
}

/// The Task Scheduler XML for `target`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn task_xml(target: &TaskTarget) -> String {
    let user = xml_text(&target.user);
    let arguments = xml_text(&format!(
        "--headless {} --home {} muster",
        windows_quote(&target.exec.display().to_string()),
        windows_quote(&target.home.display().to_string()),
    ));
    format!(
        "<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\
         <RegistrationInfo><Description>Brings the shep flock back at sign-in</Description></RegistrationInfo>\
         <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>\
         <Principals><Principal id=\"Author\"><UserId>{user}</UserId>\
         <LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\
         <Settings>\
         <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\
         <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\
         <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\
         <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\
         <Priority>4</Priority>\
         </Settings>\
         <Actions Context=\"Author\"><Exec><Command>conhost.exe</Command>\
         <Arguments>{arguments}</Arguments></Exec></Actions>\
         </Task>"
    )
}

/// Wraps `word` in double quotes for a Windows command line.
///
/// Backslashes are literal except before a `"`, so a run before a quote, or
/// before the closing one, is doubled.
fn windows_quote(word: &str) -> String {
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for c in word.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

/// The `\<name>` the rows call the task: where Task Scheduler files it.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn task_path(user: &str) -> String {
    format!("\\{}", task_name(user))
}

/// The `schtasks` arguments that start the task now, or `None` when a
/// shepherd already holds the home and `start` is false.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn run_args(user: &str, start: bool) -> Option<Vec<String>> {
    start.then(|| vec!["/Run".to_string(), "/TN".to_string(), task_name(user)])
}

/// Refuses what Windows has no meaning for.
///
/// # Errors
///
/// - [`ExitCode::Usage`] if `--init` is given, or `--user` names anyone but
///   `user`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn refuse(args: &StartupArgs, user: &str) -> Result<(), Refusal> {
    if args.init.is_some() {
        return Err(Refusal {
            code: ExitCode::Usage,
            message:
                "--init names a unix init system; on Windows shep startup registers a logon task"
                    .to_string(),
        });
    }
    if let Some(named) = &args.user
        && !named.eq_ignore_ascii_case(user)
    {
        return Err(Refusal {
            code: ExitCode::Usage,
            message: format!(
                "--user {named}: the logon task is registered by the user it runs as ({user}); \
                 Windows refuses a logon trigger for another user from an unelevated session"
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Init;

    fn target(home: &str) -> TaskTarget {
        TaskTarget {
            user: "ada".to_string(),
            exec: PathBuf::from(r"C:\Program Files\shep\shep.exe"),
            home: PathBuf::from(home),
        }
    }

    fn args(user: Option<&str>, init: Option<Init>) -> StartupArgs {
        StartupArgs {
            user: user.map(str::to_string),
            init,
        }
    }

    #[test]
    fn a_running_shepherd_leaves_out_the_run_row() {
        assert_eq!(task_path("ada"), r"\shep-ada");
        assert_eq!(run_args("ada", false), None);
        assert_eq!(
            run_args("ada", true).unwrap(),
            ["/Run", "/TN", "shep-ada"].map(str::to_string)
        );
    }

    #[test]
    fn the_task_is_named_after_the_user() {
        assert_eq!(task_name("ada"), "shep-ada");
    }

    #[test]
    fn the_trigger_and_principal_name_the_user() {
        let xml = task_xml(&target(r"C:\Users\ada\.shep"));
        assert!(
            xml.contains(
                "<LogonTrigger><Enabled>true</Enabled><UserId>ada</UserId></LogonTrigger>"
            ),
            "{xml}"
        );
        assert!(
            xml.contains(
                "<Principal id=\"Author\"><UserId>ada</UserId>\
                 <LogonType>InteractiveToken</LogonType>\
                 <RunLevel>LeastPrivilege</RunLevel></Principal>"
            ),
            "{xml}"
        );
        assert_eq!(xml.matches("<UserId>").count(), 2, "{xml}");
    }

    #[test]
    fn the_action_runs_muster_through_headless_conhost() {
        let xml = task_xml(&target(r"C:\Users\ada\.shep"));
        assert!(
            xml.contains(
                "<Exec><Command>conhost.exe</Command><Arguments>--headless \
                 \"C:\\Program Files\\shep\\shep.exe\" --home \"C:\\Users\\ada\\.shep\" muster\
                 </Arguments></Exec>"
            ),
            "{xml}"
        );
        assert_eq!(xml.matches("<Exec>").count(), 1, "{xml}");
    }

    #[test]
    fn a_home_with_a_space_stays_one_argument() {
        let xml = task_xml(&target(r"C:\Users\Jane Doe\.shep"));
        assert!(
            xml.contains(r#"--home "C:\Users\Jane Doe\.shep" muster"#),
            "{xml}"
        );
    }

    #[test]
    fn a_home_ending_in_a_backslash_keeps_its_closing_quote() {
        let xml = task_xml(&target(r"C:\"));
        assert!(xml.contains(r#"--home "C:\\" muster"#), "{xml}");
    }

    #[test]
    fn xml_special_characters_in_the_home_are_escaped() {
        let xml = task_xml(&target(r"C:\R&D <x>\.shep"));
        assert!(
            xml.contains(r#"--home "C:\R&amp;D &lt;x&gt;\.shep" muster"#),
            "{xml}"
        );
        assert!(!xml.contains("R&D"), "{xml}");
    }

    #[test]
    fn the_task_runs_at_normal_priority_with_no_time_limit() {
        let xml = task_xml(&target(r"C:\Users\ada\.shep"));
        assert!(xml.contains("<Priority>4</Priority>"), "{xml}");
        assert!(
            xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"),
            "{xml}"
        );
        assert!(
            xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"),
            "{xml}"
        );
        assert!(
            xml.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"),
            "{xml}"
        );
        assert!(
            xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"),
            "{xml}"
        );
    }

    #[test]
    fn the_task_has_no_restart_on_failure() {
        let xml = task_xml(&target(r"C:\Users\ada\.shep"));
        assert!(!xml.contains("RestartOnFailure"), "{xml}");
    }

    #[test]
    fn the_xml_has_no_declaration() {
        let xml = task_xml(&target(r"C:\Users\ada\.shep"));
        assert!(xml.starts_with("<Task "), "{xml}");
        assert!(!xml.contains("<?xml"), "{xml}");
    }

    #[test]
    fn init_is_refused_with_usage() {
        let refusal = refuse(&args(None, Some(Init::Systemd)), "ada").unwrap_err();
        assert_eq!(refusal.code, ExitCode::Usage);
        assert!(refusal.message.contains("--init"), "{}", refusal.message);
        assert!(
            refusal.message.contains("logon task"),
            "{}",
            refusal.message
        );
    }

    #[test]
    fn another_user_is_refused_with_usage() {
        let refusal = refuse(&args(Some("root"), None), "ada").unwrap_err();
        assert_eq!(refusal.code, ExitCode::Usage);
        assert!(refusal.message.contains("--user"), "{}", refusal.message);
        assert!(refuse(&args(Some("ADA"), None), "ada").is_ok());
        assert!(refuse(&args(None, None), "ada").is_ok());
    }
}
