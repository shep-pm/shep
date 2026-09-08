//! Which sheep name which secret, read from the muster roll.
//!
//! The wire cannot answer this: `SheepConfigView::new` clears `env` before
//! the struct is built, and a `{{secret:...}}` reference almost always
//! lives in an env value. The roll keeps `env` verbatim while keeping the
//! reference rather than the value it resolves to.

use std::collections::BTreeSet;

use shep_core::paths::ShepPaths;
use shep_core::protocol::ProcessInfo;
use shep_core::secrets;

use crate::commands::query::read_roll;
use crate::commands::secret::daemon_config;

/// One app from the roll that names at least one secret.
///
/// `Debug` is derived: a reference is a key name, and no value reaches
/// this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretNamer {
    /// The app's name, as the roll spells it.
    pub name: String,
    /// The environment its references resolve against: its own when it has
    /// one, the daemon's default otherwise.
    pub environment: String,
    /// Every reference it names, as the operator wrote it (`KEY` or
    /// `namespace/KEY`, no braces).
    pub references: BTreeSet<String>,
}

/// Every app in `procs` that the roll has a config for and that names at
/// least one secret, deduplicated by name.
///
/// One entry per app rather than per instance: every instance of an app
/// shares one config, so three copies would say the same thing three
/// times.
///
/// Best-effort by construction. An app missing from the roll contributes
/// nothing, which is what a sheep registered since the last roll write
/// looks like.
pub(crate) fn namers(paths: &ShepPaths, procs: &[ProcessInfo]) -> Vec<SecretNamer> {
    let Some(roll) = read_roll(paths) else {
        return Vec::new();
    };
    let host_environment = daemon_config(paths).daemon.environment;
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut found = Vec::new();
    for proc in procs {
        if !seen.insert(proc.name.as_str()) {
            continue;
        }
        let Some(config) = roll
            .apps
            .iter()
            .find(|app| app.app.name == proc.name)
            .map(|app| &app.app)
        else {
            continue;
        };
        let references = secrets::references(config);
        if references.is_empty() {
            continue;
        }
        found.push(SecretNamer {
            name: proc.name.clone(),
            environment: config
                .environment
                .clone()
                .unwrap_or_else(|| host_environment.clone()),
            references,
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use shep_core::config::AppConfig;
    use shep_core::status::ProcStatus;
    use shep_daemon::snapshot::{FlockSnapshot, SavedApp};

    use super::*;

    /// `$SHEP_HOME` pinned to `dir` itself, matching the pattern
    /// `describe`'s own tests use (`crate::commands::query`): these tests
    /// write `paths.snapshot` directly, and the default `.shep`
    /// subdirectory is never created outside a real boot.
    fn paths_under(dir: &Path) -> ShepPaths {
        let home = dir.display().to_string();
        ShepPaths::resolve(&move |key| (key == "SHEP_HOME").then(|| home.clone()), dir)
    }

    fn write_roll(paths: &ShepPaths, apps: &[AppConfig]) {
        let roll = FlockSnapshot {
            version: 1,
            saved_at_ms: 0,
            apps: apps
                .iter()
                .cloned()
                .map(|app| SavedApp {
                    app,
                    instances_running: 1,
                })
                .collect(),
        };
        std::fs::write(&paths.snapshot, serde_json::to_vec(&roll).unwrap()).unwrap();
    }

    fn info(name: &str) -> ProcessInfo {
        ProcessInfo::builder(1, name, ProcStatus::Online).build()
    }

    #[test]
    fn an_app_naming_no_reference_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        let mut plain = AppConfig::minimal("plain", "./srv");
        plain.env.insert("PORT".into(), "8080".into());
        let mut secretive = AppConfig::minimal("secretive", "./srv");
        secretive
            .env
            .insert("PW".into(), "{{secret:DB_PASSWORD}}".into());
        write_roll(&paths, &[plain, secretive]);

        let found = namers(&paths, &[info("plain"), info("secretive")]);

        assert_eq!(found.len(), 1, "only the app with a reference: {found:?}");
        assert_eq!(found[0].name, "secretive");
        assert!(found[0].references.contains("DB_PASSWORD"));
    }

    #[test]
    fn an_apps_own_environment_beats_the_daemon_default() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        let mut pinned = AppConfig::minimal("pinned", "./srv");
        pinned.environment = Some("staging".into());
        pinned.env.insert("PW".into(), "{{secret:K}}".into());
        let mut floating = AppConfig::minimal("floating", "./srv");
        floating.env.insert("PW".into(), "{{secret:K}}".into());
        write_roll(&paths, &[pinned, floating]);

        let found = namers(&paths, &[info("pinned"), info("floating")]);

        let pinned = found.iter().find(|n| n.name == "pinned").unwrap();
        let floating = found.iter().find(|n| n.name == "floating").unwrap();
        assert_eq!(pinned.environment, "staging");
        assert_eq!(
            floating.environment,
            daemon_config(&paths).daemon.environment,
            "no environment of its own falls back to the daemon's"
        );
    }

    #[test]
    fn one_entry_per_app_however_many_instances_are_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        let mut web = AppConfig::minimal("web", "./srv");
        web.env.insert("PW".into(), "{{secret:K}}".into());
        write_roll(&paths, &[web]);

        let found = namers(&paths, &[info("web"), info("web"), info("web")]);

        assert_eq!(found.len(), 1, "three instances, one config: {found:?}");
    }
}
