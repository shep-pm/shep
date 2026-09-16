use super::privilege_check::is_rc_safe_user;
use super::step_execution::Refusal;
use super::unit::UnitSpec;
use super::unit_layout::{DEFAULT_HOME_DIR, current_init, unit_path_for};
use crate::cli::{Init, StartupArgs};
use crate::exit::ExitCode;
use shep_core::paths::ShepPaths;
use std::path::{Path, PathBuf};

/// Everything resolved before any privilege is needed: the unit to render,
/// where it goes, and the command to print if this process cannot install it
/// (built from `spec`'s own `exec`, `user` and `home`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartupPlan {
    /// Which init system this build writes for.
    pub init: Init,
    /// What the unit carries.
    pub spec: UnitSpec,
    /// Where the rendered unit goes.
    pub unit_path: PathBuf,
    /// The launchd label, unused on systemd.
    pub label: String,
    /// `$SUDO_USER`, resolved once in [`plan`]: a value [`install`](crate::commands::startup::step_execution::install) reads
    /// rather than an env lookup of its own, so a test can drive the
    /// sanitised-`PATH` warning without `std::env::set_var`, which is
    /// `unsafe` under `#![forbid(unsafe_code)]`.
    pub sudo_user: Option<String>,
    /// The layout to create before installing: `<passwd home>/.shep` of the
    /// user running shep, and only with no `--home`/`$SHEP_HOME`. A named
    /// home is never created, and another user's default is not this
    /// process's to make: under `sudo` root would own it at 0700 and the
    /// daemon started as that user could not open it. [`install`](crate::commands::startup::step_execution::install) refuses
    /// a missing one; [`remove`](crate::commands::startup::step_execution::remove) never reads this.
    pub own_default_home: Option<ShepPaths>,
}

/// The user a generated unit runs the daemon as: `--user` when given, else
/// `$SUDO_USER`, else the invoking user.
///
/// Under `sudo shep startup` the invoking user is root, so `$SUDO_USER`
/// beats it: otherwise the unit would supervise root's flock while the
/// operator's stayed down.
pub(crate) fn target_user(
    explicit: Option<&str>,
    sudo_user: Option<&str>,
    invoking: &str,
) -> String {
    explicit.or(sudo_user).unwrap_or(invoking).to_string()
}

/// The `$SHEP_HOME` a generated unit carries: an explicit `--home`/`$SHEP_HOME`
/// when given, else the target user's own `<passwd home>/.shep`.
///
/// `user_home` is the target user's passwd home, never this process's
/// `$HOME`: `sudo` resets that to root's, so a unit built from it would
/// carry `/root/.shep` and restore nothing after a reboot.
pub(crate) fn target_home(explicit: Option<&Path>, user_home: &Path) -> PathBuf {
    explicit.map_or_else(|| user_home.join(DEFAULT_HOME_DIR), Path::to_path_buf)
}

/// Whether the `$SHEP_HOME` a unit carries is this process's own default to
/// create: no `--home`/`$SHEP_HOME` was given, and the target user is the
/// user running shep. [`StartupPlan::own_default_home`] says why the other
/// two cases are nobody's.
pub(crate) fn default_home_is_own(explicit: Option<&Path>, target: &str, invoking: &str) -> bool {
    explicit.is_none() && target == invoking
}

/// Resolves everything either verb needs before privilege enters into it.
///
/// `$SUDO_USER` is read here rather than passed in, so [`target_user`] stays
/// a function three cases can be stated about; an empty one is treated as
/// unset, since that is what a shell that exported it without a value means.
pub(super) fn plan(
    explicit_home: Option<&Path>,
    args: &StartupArgs,
) -> Result<StartupPlan, Refusal> {
    let Some(init) = args.init.or_else(current_init) else {
        return Err(Refusal {
            code: ExitCode::Failure,
            message: "could not tell which init system is running: neither \
                      /run/systemd/system nor /run/openrc is present. Name one \
                      with --init (systemd, openrc, launchd, freebsd-rc, openbsd-rc)"
                .to_string(),
        });
    };
    let exec = std::env::current_exe().map_err(|err| Refusal {
        code: ExitCode::Failure,
        message: format!("could not resolve this binary's own path: {err}"),
    })?;
    let sudo_user = std::env::var("SUDO_USER")
        .ok()
        .filter(|name| !name.is_empty());
    let invoking = invoking_user()?;
    let user = target_user(args.user.as_deref(), sudo_user.as_deref(), &invoking);
    if matches!(init, Init::FreebsdRc | Init::OpenbsdRc) && !is_rc_safe_user(&user) {
        return Err(Refusal {
            code: ExitCode::Usage,
            message: format!(
                "a BSD rc.d script turns the user name into a shell variable, so {user} \
                 cannot be used: it must start with a letter or underscore and contain \
                 only letters, digits and underscores. Pass --user with a name that does."
            ),
        });
    }
    let passwd_home = passwd_home(&user)?;
    let unit_path = unit_path_for(init, &user);
    // Spelled by `ShepPaths` because creating it needs the whole layout;
    // `the_default_home_and_the_layout_created_for_it_agree` pins it to
    // `target_home`'s `.shep` below.
    let own_default_home = default_home_is_own(explicit_home, &user, &invoking)
        .then(|| ShepPaths::resolve(&|_| None, &passwd_home));
    Ok(StartupPlan {
        init,
        label: super::unit::launchd_label(&user),
        spec: UnitSpec {
            user,
            exec,
            home: target_home(explicit_home, &passwd_home),
            // Captured from this invocation so an interpreter under
            // `~/.bun` or `~/.cargo` stays findable after reboot. Left
            // empty rather than guessed at if unset.
            path: std::env::var_os("PATH").unwrap_or_default(),
            working_dir: passwd_home,
        },
        unit_path,
        sudo_user,
        own_default_home,
    })
}

/// This process's own user name, for [`target_user`]'s last fallback.
fn invoking_user() -> Result<String, Refusal> {
    let uid = nix::unistd::geteuid();
    match nix::unistd::User::from_uid(uid) {
        Ok(Some(user)) => Ok(user.name),
        Ok(None) => Err(Refusal {
            code: ExitCode::Failure,
            message: format!("no passwd entry for uid {uid}"),
        }),
        Err(errno) => Err(Refusal {
            code: ExitCode::Failure,
            message: format!("could not read this process's own passwd entry: {errno}"),
        }),
    }
}

/// The target user's passwd home: the unit's working directory, and the
/// root its `$SHEP_HOME` defaults under.
fn passwd_home(name: &str) -> Result<PathBuf, Refusal> {
    match nix::unistd::User::from_name(name) {
        Ok(Some(user)) => Ok(user.dir),
        Ok(None) => Err(Refusal {
            code: ExitCode::Usage,
            message: format!("no such user: {name}"),
        }),
        Err(errno) => Err(Refusal {
            code: ExitCode::Failure,
            message: format!("could not look up {name}: {errno}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::super::privilege_check::Privilege;
    use super::super::step_execution::{ABSENT, OK, install, remove};
    use super::super::unit_layout::{remove_unit, write_unit};
    use std::path::Path;

    use crate::exit::ExitCode;
    use crate::output::Streams;

    use super::super::testing::*;
    use super::*;
    use crate::cli::Format;

    /// Under `sudo`, the invoking user is root; ignoring `$SUDO_USER` would
    /// install a unit supervising root's flock instead of the operator's.
    #[test]
    fn the_target_user_prefers_an_explicit_name_then_sudo_user() {
        assert_eq!(target_user(Some("deploy"), Some("ada"), "root"), "deploy");
        assert_eq!(target_user(None, Some("ada"), "root"), "ada");
        assert_eq!(target_user(None, None, "ada"), "ada");
    }

    /// `sudo` resets `$HOME` to root's; falling back to it would carry
    /// `/root/.shep` and restore nothing after a reboot.
    #[test]
    fn the_target_home_comes_from_the_target_user_not_the_invoker() {
        assert_eq!(
            target_home(None, Path::new("/home/ada")),
            Path::new("/home/ada/.shep")
        );
        assert_eq!(
            target_home(Some(Path::new("/srv/shep")), Path::new("/home/ada")),
            Path::new("/srv/shep")
        );
    }

    #[test]
    fn a_default_home_is_created_only_for_the_user_running_shep() {
        assert!(default_home_is_own(None, "ada", "ada"));
        assert!(!default_home_is_own(None, "ada", "root"));
        assert!(
            !default_home_is_own(Some(Path::new("/srv/shep")), "ada", "ada"),
            "a named home is never created, by anyone"
        );
    }

    #[test]
    fn the_default_home_and_the_layout_created_for_it_agree() {
        let passwd_home = Path::new("/home/ada");
        assert_eq!(
            shep_core::paths::ShepPaths::resolve(&|_| None, passwd_home).home,
            target_home(None, passwd_home)
        );
    }

    /// The secure-`PATH` warning is checked after the privilege gate, not
    /// before: nothing is written on this path, so it must stay as silent
    /// as the plain refusal above.
    #[test]
    fn an_unprivileged_startup_under_sudo_still_prints_no_secure_path_warning() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        std::fs::create_dir_all(&home).unwrap();
        let plan = StartupPlan {
            sudo_user: Some("ada".to_string()),
            ..plan_for_test(&home)
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            install(&mut streams, &plan, Privilege::Unprivileged);
        }
        let printed = String::from_utf8(err).unwrap();
        assert!(!printed.contains("secure_path"), "{printed}");
    }

    /// Rewriting an existing unit's file would not change the service
    /// already loaded, leaving the file and the running unit disagreeing.
    ///
    /// `Privilege::Root`: unprivileged would refuse for the other reason
    /// first, never reaching this check.
    #[test]
    fn an_existing_unit_is_refused_rather_than_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        std::fs::create_dir_all(&home).unwrap();
        let plan = StartupPlan {
            sudo_user: Some("ada".to_string()),
            ..plan_for_test(&home)
        };
        std::fs::write(&plan.unit_path, "# hand-edited\n").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            install(&mut streams, &plan, Privilege::Root)
        };
        assert_eq!(code, ExitCode::Usage);
        let printed = String::from_utf8(err).unwrap();
        assert!(
            printed.contains(plan.unit_path.to_str().unwrap()),
            "{printed}"
        );
        assert!(printed.contains("unstartup"), "{printed}");
        assert!(
            !printed.contains("secure_path"),
            "a plan carrying $SUDO_USER still warns nothing on a refusal path \
                 that wrote no unit: {printed}"
        );
        assert_eq!(
            std::fs::read_to_string(&plan.unit_path).unwrap(),
            "# hand-edited\n",
            "a refused startup leaves the operator's own file alone"
        );
    }

    /// The removal check matters most: this verb's whole job is destructive.
    #[test]
    fn an_unprivileged_unstartup_prints_the_command_and_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        let plan = plan_for_test(&home);
        std::fs::write(&plan.unit_path, "# installed\n").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            remove(&mut streams, &plan, Privilege::Unprivileged)
        };
        assert_ne!(code, ExitCode::Success);
        let printed = String::from_utf8(err).unwrap();
        assert!(printed.contains("sudo"), "{printed}");
        assert!(printed.contains("unstartup"), "{printed}");
        assert!(
            plan.unit_path.exists(),
            "an unprivileged unstartup removes nothing"
        );
    }

    /// Drives `unit_layout::write_unit` and `unit_layout::remove_unit` directly rather than
    /// `step_execution::install`: `step_execution::install`'s privileged path runs `systemctl`, which no
    /// test here may reach.
    #[test]
    fn the_unit_is_written_at_0644_and_removed_again() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join(".shep");
        let plan = plan_for_test(&home);

        let step = write_unit(&plan);
        assert_eq!(step.result, OK, "{step:?}");
        assert_eq!(
            std::fs::read_to_string(&plan.unit_path).unwrap(),
            super::super::unit::systemd_unit(&plan.spec)
        );
        let mode = std::fs::metadata(&plan.unit_path)
            .unwrap()
            .permissions()
            .mode();
        // A literal, not `unit_mode(Init::Systemd)`: comparing the
        // function's own return value to itself would pass no matter what
        // it returned.
        assert_eq!(mode & 0o777, 0o644, "mode was {:o}", mode & 0o777);

        assert_eq!(remove_unit(&plan).result, OK);
        assert!(!plan.unit_path.exists());
        assert_eq!(
            remove_unit(&plan).result,
            ABSENT,
            "removing what is already gone is the state that was asked for"
        );
    }
}
