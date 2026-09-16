use super::step_execution::{ABSENT, OK};
use super::target_resolution::StartupPlan;
use super::unit::UnitSpec;
use crate::cli::Init;
use crate::output::StartupStep;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;

/// `$SHEP_HOME`'s own directory name under a user's home, mirroring
/// `ShepPaths::resolve`'s `home_dir.join(".shep")`. A literal there and a
/// literal here: shep-core exports the default as behaviour rather than as a
/// constant, and inventing a public one to share would widen that crate's
/// surface for one call site.
pub(super) const DEFAULT_HOME_DIR: &str = ".shep";

/// The mode a generated unit is created with.
///
/// systemd and launchd units are read, not executed: 0644. openrc and BSD
/// rc.d scripts are executed: 0755.
pub(crate) const fn unit_mode(init: Init) -> u32 {
    match init {
        Init::Systemd | Init::Launchd => 0o644,
        Init::Openrc | Init::FreebsdRc | Init::OpenbsdRc => 0o755,
    }
}

/// Where a generated unit for `init` is written, for `user`.
///
/// Systemd and launchd delegate to `super::unit::systemd_unit_path`/
/// `super::unit::launchd_plist_path`. Takes `Init` explicitly, not just the
/// detected one, so `step_execution::unstartup` can find the file under whatever `--init`
/// names.
pub(crate) fn unit_path_for(init: Init, user: &str) -> PathBuf {
    match init {
        Init::Systemd => super::unit::systemd_unit_path(user),
        Init::Launchd => super::unit::launchd_plist_path(user),
        Init::Openrc => PathBuf::from(format!("/etc/init.d/shep-{user}")),
        Init::FreebsdRc => PathBuf::from(format!("/usr/local/etc/rc.d/shep_{user}")),
        Init::OpenbsdRc => PathBuf::from(format!("/etc/rc.d/shep_{user}")),
    }
}

/// The notice [`install`](crate::commands::startup::step_execution::install) prints when `$SUDO_USER` was set: `None` if it
/// was not, else a message naming `sudo_user` and showing `spec.path` in
/// full, so the operator can check it against that user's login `PATH`.
///
/// A pure function of the two values, not an environment read of its own:
/// this crate is `#![forbid(unsafe_code)]`, so a test cannot call
/// `std::env::set_var` to establish an ambient `$SUDO_USER` for [`plan`](crate::commands::startup::target_resolution::plan)
/// to read.
pub(super) fn secure_path_warning(sudo_user: Option<&str>, spec: &UnitSpec) -> Option<String> {
    let sudo_user = sudo_user?;
    Some(format!(
        "sudo may have replaced PATH with its own secure_path before shep ever saw it \
         (SUDO_USER={sudo_user}); the unit now carries PATH={}; compare it against \
         {sudo_user}'s login PATH, and if a directory such as ~/.bun/bin or ~/.cargo/bin \
         is missing, run shep unstartup then sudo --preserve-env=PATH shep startup to \
         carry it through instead",
        spec.path.to_string_lossy(),
    ))
}

/// Renders the unit and writes it at [`unit_mode`], as one step.
///
/// Mode set at `open` time, not via a later chmod: a create-then-chmod
/// sequence would leave the file readable at the ambient umask until the
/// chmod lands, and this file lands in a directory every user can reach.
pub(super) fn write_unit(plan: &StartupPlan) -> StartupStep {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let rendered = match plan.init {
        Init::Systemd => super::unit::systemd_unit(&plan.spec),
        Init::Launchd => super::unit::launchd_plist(&plan.spec),
        Init::Openrc => super::unit::openrc_script(&plan.spec),
        Init::FreebsdRc => super::unit::freebsd_rc_script(&plan.spec),
        Init::OpenbsdRc => super::unit::openbsd_rc_script(&plan.spec),
    };
    let mode = unit_mode(plan.init);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&plan.unit_path)
        .and_then(|mut file| {
            file.write_all(rendered.as_bytes())?;
            // `open`'s mode is masked by the umask; this chmod makes it
            // deterministic. Acts on the open fd, so there is no race, and
            // it only ever widens a mode already no wider than this one.
            file.set_permissions(std::fs::Permissions::from_mode(mode))
        });
    StartupStep {
        action: "wrote",
        target: plan.unit_path.display().to_string(),
        result: match written {
            Ok(()) => OK.to_string(),
            Err(err) => err.to_string(),
        },
    }
}

/// Removes the unit file, as one step. A file that is already gone is the
/// state this was asked to produce, so it reports [`ABSENT`] rather than the
/// `NotFound` it saw.
pub(super) fn remove_unit(plan: &StartupPlan) -> StartupStep {
    StartupStep {
        action: "removed",
        target: plan.unit_path.display().to_string(),
        result: match std::fs::remove_file(&plan.unit_path) {
            Ok(()) => OK.to_string(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => ABSENT.to_string(),
            Err(err) => err.to_string(),
        },
    }
}

/// `shep-<user>.service`, read back off the path the plan already
/// resolved rather than formatted a second time: `systemctl enable` wants
/// the unit's name, and two spellings could drift apart.
pub(super) fn unit_file_name(plan: &StartupPlan) -> String {
    plan.unit_path
        .file_name()
        .unwrap_or(plan.unit_path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Which init a Linux host running these two probes is on.
///
/// A pure function so the order is testable off Linux. systemd wins a
/// tie: `/run/systemd/system` is what `sd_booted(3)` checks, so both
/// present means openrc leftovers on a systemd host, not the reverse.
///
/// [`current_init`]'s Linux arm is the only non-test caller; `#[cfg]`-ed
/// away on every other target rather than blanket-`#[allow(dead_code)]`ed.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const fn linux_init(systemd: bool, openrc: bool) -> Option<Init> {
    if systemd {
        Some(Init::Systemd)
    } else if openrc {
        Some(Init::Openrc)
    } else {
        None
    }
}

/// The init system this host is actually running, or `None` when it is one
/// shep has no renderer for.
///
/// Linux is a runtime probe: `target_os` cannot tell systemd and openrc
/// apart. The ordering lives in [`linux_init`]; this function is the two
/// filesystem reads that feed it. Every other target is a compile-time
/// fact.
///
/// A Linux container with no `/run/systemd/system` and no openrc is
/// refused rather than guessed at; `--init` overrides this entirely.
pub(super) fn current_init() -> Option<Init> {
    #[cfg(target_os = "linux")]
    {
        linux_init(
            Path::new("/run/systemd/system").is_dir(),
            Path::new("/run/openrc/softlevel").exists() || Path::new("/run/openrc").is_dir(),
        )
    }
    #[cfg(target_os = "macos")]
    {
        Some(Init::Launchd)
    }
    #[cfg(target_os = "freebsd")]
    {
        Some(Init::FreebsdRc)
    }
    #[cfg(target_os = "openbsd")]
    {
        Some(Init::OpenbsdRc)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd"
    )))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::unit::UnitSpec;
    use crate::cli::Init;

    use super::*;
    use std::ffi::OsString;

    #[test]
    fn secure_path_warning_names_the_sudo_user_and_the_full_path_only_under_sudo() {
        let spec = UnitSpec {
            user: "deploy".to_string(),
            exec: PathBuf::from("/usr/local/bin/shep"),
            home: PathBuf::from("/home/deploy/.shep"),
            path: OsString::from("/usr/local/sbin:/usr/local/bin:/usr/bin:/bin"),
            working_dir: PathBuf::from("/home/deploy"),
        };
        assert_eq!(
            secure_path_warning(None, &spec),
            None,
            "no $SUDO_USER means shep was never run through sudo at all"
        );

        let message =
            secure_path_warning(Some("ada"), &spec).expect("$SUDO_USER was set, so this warns");
        assert!(message.contains("SUDO_USER=ada"), "{message}");
        assert!(
            message.contains("/usr/local/sbin:/usr/local/bin:/usr/bin:/bin"),
            "the full captured PATH must be readable without a second lookup: {message}"
        );
        assert!(
            message.contains("--preserve-env=PATH"),
            "the operator needs a way to get the PATH they meant: {message}"
        );
    }

    #[test]
    fn the_mode_is_read_only_for_units_and_executable_for_scripts() {
        assert_eq!(unit_mode(Init::Systemd), 0o644);
        assert_eq!(unit_mode(Init::Launchd), 0o644);
        assert_eq!(unit_mode(Init::Openrc), 0o755);
        assert_eq!(unit_mode(Init::FreebsdRc), 0o755);
        assert_eq!(unit_mode(Init::OpenbsdRc), 0o755);
    }

    /// systemd wins a tie: `/run/systemd/system` is what `sd_booted(3)`
    /// checks, so both present means openrc leftovers on a systemd host.
    #[test]
    fn systemd_wins_when_both_linux_probes_are_true() {
        assert_eq!(linux_init(true, true), Some(Init::Systemd));
        assert_eq!(linux_init(true, false), Some(Init::Systemd));
        assert_eq!(linux_init(false, true), Some(Init::Openrc));
        assert_eq!(linux_init(false, false), None);
    }

    /// A unit installed under one init must be removable after the host
    /// changes to another, so this is about which file gets removed, not
    /// which struct the two verbs share.
    #[test]
    fn each_init_names_its_own_unit_path() {
        assert_eq!(
            unit_path_for(Init::Openrc, "deploy"),
            PathBuf::from("/etc/init.d/shep-deploy")
        );
        assert_eq!(
            unit_path_for(Init::FreebsdRc, "deploy"),
            PathBuf::from("/usr/local/etc/rc.d/shep_deploy")
        );
        assert_eq!(
            unit_path_for(Init::OpenbsdRc, "deploy"),
            PathBuf::from("/etc/rc.d/shep_deploy")
        );
        // systemd and launchd keep the paths they already had
        assert_eq!(
            unit_path_for(Init::Systemd, "deploy"),
            super::super::unit::systemd_unit_path("deploy")
        );
        assert_eq!(
            unit_path_for(Init::Launchd, "deploy"),
            super::super::unit::launchd_plist_path("deploy")
        );
    }
}
