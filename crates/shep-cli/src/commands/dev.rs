//! `shep dev`: the isolated, throwaway sibling of `shep runtime`. Forces
//! `watch = true` onto every app it resolves and hands the result to
//! `commands::foreground`'s engine with `tidy_up: true`, on a home
//! `--home`/`$SHEP_HOME` cannot reach.

use std::path::{Path, PathBuf};

use shep_core::config::AppConfig;
use shep_core::paths::{ShepPaths, user_home};

use crate::cli::DevArgs;
use crate::commands::foreground::{self, ForegroundOptions};
use crate::commands::lifecycle::{resolve_target, target_exit_code};
use crate::commands::runtime::discovered_target;
use crate::exit::ExitCode;
use crate::output::Streams;

/// What `shep dev` reports when nothing resolves a root for its own home.
#[cfg(not(windows))]
const UNRESOLVED_DEV_HOME: &str =
    "neither $SHEP_DEV_HOME nor $HOME resolves a root directory for shep dev";

/// What `shep dev` reports when nothing resolves a root for its own home.
///
/// Names `%USERPROFILE%` for the same reason the shared refusal in `lib.rs`
/// does: Windows sets no `HOME`.
#[cfg(windows)]
const UNRESOLVED_DEV_HOME: &str =
    "neither %SHEP_DEV_HOME% nor %USERPROFILE% resolves a root directory for shep dev";

/// How an operator on this platform spells the variable, for a refusal that
/// names it mid-sentence.
#[cfg(not(windows))]
const DEV_HOME_VAR: &str = "$SHEP_DEV_HOME";

/// How an operator on this platform spells the variable, for a refusal that
/// names it mid-sentence.
#[cfg(windows)]
const DEV_HOME_VAR: &str = "%SHEP_DEV_HOME%";

/// Why [`dev_home`] would not name a root for this session.
///
/// `shep dev`'s own refusals, not [`crate::HomeRefusal`]'s: both messages
/// have to name `$SHEP_DEV_HOME`, which is the one variable this verb reads
/// and the one an operator can act on.
#[derive(Debug)]
enum DevHomeRefusal {
    /// Neither `$SHEP_DEV_HOME` nor a home directory resolved a root.
    Unresolved,
    /// `$SHEP_DEV_HOME` named a path with no root.
    Relative {
        /// The path as the operator spelled it.
        given: PathBuf,
        /// The same path joined onto this process's directory, for the
        /// remedy line. `None` when that directory could not be read.
        absolute: Option<PathBuf>,
    },
}

impl core::fmt::Display for DevHomeRefusal {
    /// The operator-facing message, remedy included.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unresolved => f.write_str(UNRESOLVED_DEV_HOME),
            Self::Relative { given, absolute } => {
                crate::write_relative_refusal(f, DEV_HOME_VAR, given, absolute.as_deref())
            }
        }
    }
}

impl core::error::Error for DevHomeRefusal {}

/// Where a dev flock lives: `$SHEP_DEV_HOME`, else `~/.shep-dev`.
///
/// `--home` and `$SHEP_HOME` are ignored: sharing a real flock's home would
/// land the forced `watch = true` on production apps.
///
/// The home is injected as [`ShepPaths::resolve`]'s own answer for
/// `SHEP_HOME`, so every derived path matches the other verbs. `resolve`
/// therefore never reaches its own `home_dir` fallback, and is handed the
/// dev home rather than a second path that would go unread.
///
/// `home_dir` is read for the `~/.shep-dev` fallback alone, so `None` (no
/// passwd home, no `$HOME`) is answerable as long as `$SHEP_DEV_HOME` names
/// somewhere.
///
/// # Errors
///
/// - [`DevHomeRefusal::Relative`] if `$SHEP_DEV_HOME` named a path with no
///   root. Gated here rather than in [`dev`], so the rule travels with the
///   resolver that reads the variable.
/// - [`DevHomeRefusal::Unresolved`] if neither named a root.
fn dev_home(
    env: &impl Fn(&str) -> Option<String>,
    home_dir: Option<&Path>,
) -> Result<ShepPaths, DevHomeRefusal> {
    let home = match env("SHEP_DEV_HOME") {
        Some(dir) => {
            let named = PathBuf::from(dir);
            if named.is_relative() {
                return Err(DevHomeRefusal::Relative {
                    absolute: crate::absolute_form(&named),
                    given: named,
                });
            }
            named
        }
        None => home_dir
            .ok_or(DevHomeRefusal::Unresolved)?
            .join(".shep-dev"),
    };
    let inject = |key: &str| (key == "SHEP_HOME").then(|| home.to_string_lossy().into_owned());
    Ok(ShepPaths::resolve(&inject, &home))
}

/// Sets `watch = true` on every app, in place: rebuilding each [`AppConfig`]
/// would silently drop any field this function does not know to copy.
fn force_watch(apps: &mut [AppConfig]) {
    for app in apps {
        app.watch = true;
    }
}

/// Fills a missing `cwd` with the directory containing that app's own
/// `script`: the daemon refuses `watch = true` with no `cwd` to arm, and
/// [`force_watch`] has just turned `watch` on for every app. Only a bare
/// script target leaves that gap. A script with no directory component falls
/// back to this process's current directory.
fn default_watch_cwd(apps: &mut [AppConfig]) {
    for app in apps {
        if app.cwd.is_some() {
            continue;
        }
        let parent = Path::new(&app.script)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok());
        app.cwd = parent.map(|p| p.to_string_lossy().into_owned());
    }
}

/// Runs `shep dev`.
///
/// With no `args.target`, [`discovered_target`] looks in the current
/// directory for a conventional name.
///
/// `home_given` is true whenever `--home` or its aliased `$SHEP_HOME` named
/// anything; this then notices that `dev` uses [`dev_home`] instead.
pub async fn dev(
    streams: &mut Streams<'_>,
    quiet: bool,
    home_given: bool,
    args: &DevArgs,
) -> ExitCode {
    if home_given {
        streams.aside(
            "home_ignored",
            "shep dev ignores --home/$SHEP_HOME; isolation is the whole feature — set \
             $SHEP_DEV_HOME instead",
        );
    }

    let target = match &args.target {
        Some(target) => target.clone(),
        None => match discovered_target(streams) {
            Ok(target) => target,
            Err(code) => return code,
        },
    };

    let mut apps = match resolve_target(&target, args.name.as_deref(), &[], false) {
        Ok(apps) => apps,
        Err(err) => {
            let code = target_exit_code(&err);
            return streams.fail(code, &err.to_string());
        }
    };

    force_watch(&mut apps);
    default_watch_cwd(&mut apps);
    let names = apps
        .iter()
        .map(|app| app.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    streams.aside(
        "watch_forced",
        &format!(
            "shep dev: forcing watch on {names} — each app's own `watch` setting is ignored here"
        ),
    );

    // `env` only recognizes `SHEP_DEV_HOME`, so a real flock's env can't leak
    // in here. The home directory is read separately below, only for the
    // fallback parent.
    let env = |key: &str| {
        if key == "SHEP_DEV_HOME" {
            std::env::var("SHEP_DEV_HOME").ok()
        } else {
            None
        }
    };
    let home_dir = user_home(&|key| std::env::var_os(key));
    let paths = match dev_home(&env, home_dir.as_deref()) {
        Ok(paths) => paths,
        Err(refusal) => return streams.fail(ExitCode::Usage, &refusal.to_string()),
    };

    let options = ForegroundOptions {
        paths,
        apps,
        tidy_up: true,
    };
    foreground::run(streams, quiet, options).await
}

#[cfg(test)]
mod tests {
    use shep_core::values::MemSize;

    use super::*;

    #[test]
    fn the_dev_home_ignores_shep_home_and_prefers_its_own_variable() {
        let env = |key: &str| match key {
            "SHEP_HOME" => Some("/srv/production".to_string()),
            _ => None,
        };
        let paths = dev_home(&env, Some(Path::new("/home/ada"))).unwrap();
        assert_eq!(paths.home, Path::new("/home/ada/.shep-dev"));

        let env = |key: &str| match key {
            "SHEP_HOME" => Some("/srv/production".to_string()),
            "SHEP_DEV_HOME" => Some(ABSOLUTE_DEV_HOME.to_string()),
            _ => None,
        };
        assert_eq!(
            dev_home(&env, Some(Path::new("/home/ada"))).unwrap().home,
            Path::new(ABSOLUTE_DEV_HOME)
        );

        // No passwd home and no `$HOME`: `$SHEP_DEV_HOME` alone still
        // answers, and without it there is nowhere to put a dev flock.
        assert_eq!(
            dev_home(&env, None).unwrap().home,
            Path::new(ABSOLUTE_DEV_HOME),
            "`$SHEP_DEV_HOME` names the home outright, so no fallback is needed"
        );
        let no_dev_home = |key: &str| (key == "SHEP_HOME").then(|| "/srv/production".to_string());
        assert!(matches!(
            dev_home(&no_dev_home, None),
            Err(DevHomeRefusal::Unresolved)
        ));
    }

    /// The same rule `resolve_paths` holds for `$SHEP_HOME`: a home with no
    /// root would put the dev flock under whatever directory `shep dev` was
    /// run from, and its logs under a second one.
    #[test]
    fn a_relative_dev_home_is_refused_and_the_absolute_form_named() {
        let env = |key: &str| (key == "SHEP_DEV_HOME").then(|| "scratch/.shep-dev".to_string());
        let Err(refusal) = dev_home(&env, Some(Path::new("/home/ada"))) else {
            panic!("a relative $SHEP_DEV_HOME must not resolve a layout");
        };
        assert!(matches!(refusal, DevHomeRefusal::Relative { .. }));

        let rendered = refusal.to_string();
        assert!(
            rendered.contains("scratch/.shep-dev"),
            "the refusal must quote the path as typed: {rendered}"
        );
        let cwd = std::env::current_dir().expect("a current directory");
        assert!(
            rendered.contains(&cwd.join("scratch/.shep-dev").display().to_string()),
            "the remedy must name the absolute form of what was typed: {rendered}"
        );
    }

    /// A rooted path on the platform running the test: `Path::is_relative`
    /// answers yes to `/tmp/t1` on Windows, which has no drive prefix.
    #[cfg(not(windows))]
    const ABSOLUTE_DEV_HOME: &str = "/tmp/t1";

    /// A rooted path on the platform running the test.
    #[cfg(windows)]
    const ABSOLUTE_DEV_HOME: &str = r"C:\tmp\t1";

    #[test]
    fn every_app_gets_watch_and_keeps_everything_else() {
        let mut apps = vec![AppConfig::minimal("web", "./server.js")];
        apps[0].watch = false;
        apps[0].max_memory = Some(MemSize::from_bytes(1024));
        force_watch(&mut apps);
        assert!(apps[0].watch);
        assert_eq!(apps[0].max_memory, Some(MemSize::from_bytes(1024)));
        assert_eq!(apps[0].name, "web");
    }

    #[test]
    fn a_missing_cwd_defaults_to_the_scripts_own_directory() {
        let mut apps = vec![AppConfig::minimal("web", "/srv/app/server.js")];
        default_watch_cwd(&mut apps);
        assert_eq!(apps[0].cwd.as_deref(), Some("/srv/app"));
    }

    #[test]
    fn an_explicit_cwd_is_left_untouched() {
        let mut apps = vec![AppConfig::minimal("web", "./server.js")];
        apps[0].cwd = Some("/srv/explicit".to_string());
        default_watch_cwd(&mut apps);
        assert_eq!(apps[0].cwd.as_deref(), Some("/srv/explicit"));
    }
}
