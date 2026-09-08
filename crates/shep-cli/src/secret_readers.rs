//! Which sheep name which secret, read from the muster roll.
//!
//! The wire cannot answer this: `SheepConfigView::new` clears `env` before
//! the struct is built, and a `{{secret:...}}` reference almost always
//! lives in an env value. The roll keeps `env` verbatim while keeping the
//! reference rather than the value it resolves to.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use shep_core::paths::ShepPaths;
use shep_core::protocol::ProcessInfo;
use shep_core::secrets;
use shep_core::status::ProcStatus;

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

/// One sheep that names a secret, and whether it is running now.
///
/// `online` is deliberately not "holds the current value". Nothing records
/// when a value was set, so a running sheep was given *a* value at spawn
/// and may have been given an older one. The pane's caption says exactly
/// that and no more.
///
/// `Debug` is derived: a name and an environment, no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reader {
    /// The sheep's name.
    pub name: String,
    /// The environment its reference resolves against.
    pub environment: String,
    /// Whether the shepherd currently reports it `Online`.
    pub online: bool,
}

/// Every secret reference the roll names, mapped to the sheep that name
/// it, in name order.
///
/// Keys are references exactly as an operator wrote them, so a namespaced
/// one arrives as `namespace/KEY` and matches the pane's own row key for a
/// provider row.
pub(crate) fn by_reference(
    paths: &ShepPaths,
    procs: &[ProcessInfo],
) -> BTreeMap<String, Vec<Reader>> {
    let online: BTreeSet<&str> = procs
        .iter()
        .filter(|proc| proc.status == ProcStatus::Online)
        .map(|proc| proc.name.as_str())
        .collect();
    let mut map: BTreeMap<String, Vec<Reader>> = BTreeMap::new();
    for namer in namers(paths, procs) {
        for reference in &namer.references {
            map.entry(reference.clone()).or_default().push(Reader {
                name: namer.name.clone(),
                environment: namer.environment.clone(),
                online: online.contains(namer.name.as_str()),
            });
        }
    }
    for readers in map.values_mut() {
        readers.sort_by(|a, b| a.name.cmp(&b.name));
    }
    map
}

/// How long ago the muster roll was written, or `None` when it is missing
/// or unreadable.
///
/// The pane states this because a failed roll write only warns, so a stale
/// roll is otherwise silent. A roll from the future reads as zero rather
/// than as an error: a clock that moved is not the operator's problem to
/// solve from this screen.
pub(crate) fn roll_age(paths: &ShepPaths) -> Option<Duration> {
    let roll = read_roll(paths)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().try_into().unwrap_or(u64::MAX));
    Some(Duration::from_millis(now.saturating_sub(roll.saved_at_ms)))
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

    fn online(name: &str) -> ProcessInfo {
        ProcessInfo::builder(1, name, ProcStatus::Online).build()
    }

    fn stopped(name: &str) -> ProcessInfo {
        ProcessInfo::builder(1, name, ProcStatus::Stopped).build()
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

        let found = namers(&paths, &[online("plain"), online("secretive")]);

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

        let found = namers(&paths, &[online("pinned"), online("floating")]);

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

        let found = namers(&paths, &[online("web"), online("web"), online("web")]);

        assert_eq!(found.len(), 1, "three instances, one config: {found:?}");
    }

    #[test]
    fn two_apps_naming_one_key_both_appear_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        let mut catcher = AppConfig::minimal("catcher", "./srv");
        catcher
            .env
            .insert("A".into(), "{{secret:SENTRY_DSN}}".into());
        let mut web = AppConfig::minimal("web", "./srv");
        web.env.insert("B".into(), "{{secret:SENTRY_DSN}}".into());
        write_roll(&paths, &[catcher, web]);

        let map = by_reference(&paths, &[online("catcher"), stopped("web")]);

        let readers = map.get("SENTRY_DSN").expect("the key has readers");
        assert_eq!(readers.len(), 2);
        assert!(readers.iter().any(|r| r.name == "catcher" && r.online));
        assert!(readers.iter().any(|r| r.name == "web" && !r.online));
    }

    #[test]
    fn readers_are_in_name_order_so_the_panel_does_not_reshuffle() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        let mut zeta = AppConfig::minimal("zeta", "./srv");
        zeta.env.insert("A".into(), "{{secret:K}}".into());
        let mut alpha = AppConfig::minimal("alpha", "./srv");
        alpha.env.insert("A".into(), "{{secret:K}}".into());
        write_roll(&paths, &[zeta, alpha]);

        let map = by_reference(&paths, &[online("zeta"), online("alpha")]);

        let names: Vec<&str> = map["K"].iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["alpha", "zeta"]);
    }

    #[test]
    fn each_readers_environment_is_its_own_apps_not_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        let mut pinned = AppConfig::minimal("pinned", "./srv");
        pinned.environment = Some("staging".into());
        pinned.env.insert("A".into(), "{{secret:K}}".into());
        let mut floating = AppConfig::minimal("floating", "./srv");
        floating.env.insert("A".into(), "{{secret:K}}".into());
        write_roll(&paths, &[pinned, floating]);

        let map = by_reference(&paths, &[online("pinned"), online("floating")]);

        let readers = &map["K"];
        let pinned = readers.iter().find(|r| r.name == "pinned").unwrap();
        let floating = readers.iter().find(|r| r.name == "floating").unwrap();
        assert_eq!(pinned.environment, "staging");
        assert_eq!(
            floating.environment,
            daemon_config(&paths).daemon.environment,
            "each reader keeps its own environment, not its neighbor's"
        );
    }
}
