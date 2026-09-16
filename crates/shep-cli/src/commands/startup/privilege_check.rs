/// Whether `user` can appear in a BSD rc script's variable names.
///
/// `rcvar` and `rcctl` turn the service name into shell variable names
/// (`shep_<user>_enable`, `shep_<user>_flags`). A `-` or `.` in `user`
/// produces an invalid `sh` identifier, and the script then fails at
/// `load_rc_config` with a syntax error naming a line number, not a user.
///
/// systemd and openrc name files, not variables, and are unaffected.
pub(crate) fn is_rc_safe_user(user: &str) -> bool {
    let mut chars = user.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && user.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether this process can install a system unit.
///
/// A value rather than a `geteuid()` call inside [`install`](crate::commands::startup::step_execution::install), because a test
/// cannot become root and one that skipped when unprivileged would never run
/// anywhere. [`startup`](crate::commands::startup::step_execution::startup) reads `geteuid()` once and passes the answer down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Privilege {
    /// `geteuid() == 0`: a write under `/etc` or `/Library` will be allowed.
    Root,
    /// Anything else, which for this verb means: explain, do not attempt.
    Unprivileged,
}

/// This process's own privilege, read once.
pub(super) fn privilege() -> Privilege {
    if nix::unistd::geteuid().is_root() {
        Privilege::Root
    } else {
        Privilege::Unprivileged
    }
}

#[cfg(test)]
mod tests {
    use super::super::step_execution::{ABSENT, remove};

    use crate::exit::ExitCode;
    use crate::output::Streams;

    use super::super::testing::*;
    use super::*;
    use crate::cli::Format;

    /// `Privilege::Unprivileged`: the absence check runs before the
    /// privilege gate, so a `Privilege::Root` plan here would reach a real
    /// `systemctl`.
    #[test]
    fn an_absent_unit_is_an_absent_row_and_a_success() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            remove(&mut streams, &plan_for_test(&home), Privilege::Unprivileged)
        };
        assert_eq!(code, ExitCode::Success);
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains(ABSENT), "{printed}");
    }

    /// `web-app` and `deploy.svc` are legal usernames and illegal shell
    /// variable fragments; a script built from one fails at
    /// `load_rc_config` naming a line number, not the user.
    #[test]
    fn a_user_name_that_cannot_be_a_shell_variable_is_refused() {
        for ok in ["deploy", "www", "_shep", "app2"] {
            assert!(is_rc_safe_user(ok), "{ok} should be accepted");
        }
        for bad in ["web-app", "deploy.svc", "2fast", "", "ünicode"] {
            assert!(!is_rc_safe_user(bad), "{bad} should be refused");
        }
    }
}
