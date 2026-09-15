use core::fmt;
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

/// Paths ignored by every watch, before `ignore_watch` is even consulted.
///
/// Dot-entries cover editor swap files and `.git`'s own churn. The
/// `logs`/`pids` entries match a root-relative path, so they cover only a
/// `logs/` or `pids/` directory the user keeps inside the watched tree;
/// they do not cover shep's own log writes, which is what
/// `own_log_ignores` is for.
pub(super) const DEFAULT_IGNORE_GLOBS: &[&str] = &[
    "**/.*",
    "**/.*/**",
    "**/node_modules/**",
    "**/logs/**",
    "**/pids/**",
];

/// Pattern standing in for "no `watch_options` configured": matches every
/// relative path, so an app that names none is filtered by the default
/// ignores alone.
pub(super) const MATCH_EVERYTHING: &str = "**";

/// Ignore patterns covering a sheep's own log files: one per path in `logs`
/// that lies under `root`, nothing for the ones that don't.
///
/// [`DEFAULT_IGNORE_GLOBS`] cannot cover an explicit `out_file`/`err_file`
/// under an app's own `cwd`: unignored, it loops forever, since an
/// automatic restart resets `max_restarts` rather than spending it.
///
/// Each path is canonicalized through its parent before stripping `root`,
/// since an app's `cwd` need not already be canonical (macOS resolves
/// `/var/…` to `/private/var/…`).
pub(crate) fn own_log_ignores<'a>(
    root: &Path,
    logs: impl IntoIterator<Item = &'a Path>,
) -> Vec<String> {
    logs.into_iter()
        .filter_map(|log| literal_glob_under(root, log))
        .collect()
}

/// One path's root-relative form as a glob matching it and nothing else, or
/// `None` when it does not lie under `root` (the ordinary case, since the
/// default log paths live in `$SHEP_HOME`) or cannot be spelled as a pattern.
pub(super) fn literal_glob_under(root: &Path, path: &Path) -> Option<String> {
    let relative = canonical_parent_of(path);
    let relative = relative.strip_prefix(root).ok()?;
    // Assembled component by component rather than from `to_str`, because a
    // glob's separator is `/` on every platform while a Windows path spells it
    // `\`. `escape` then makes each component match LITERALLY: a log file whose
    // name contains `[` or `*` is a filename, not a pattern.
    let mut pattern = String::new();
    for component in relative.iter() {
        if !pattern.is_empty() {
            pattern.push('/');
        }
        pattern.push_str(&globset::escape(component.to_str()?));
    }
    (!pattern.is_empty()).then_some(pattern)
}

/// `path` with its PARENT canonicalized and its file name left alone.
///
/// The file itself may not exist yet: a re-arm happens before the
/// respawned child has written a byte, while its directory does by the
/// time a spawn succeeds. Falls back to `path` untouched when even the
/// parent will not resolve.
pub(super) fn canonical_parent_of(path: &Path) -> PathBuf {
    let (Some(parent), Some(file)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    std::fs::canonicalize(parent).map_or_else(|_| path.to_path_buf(), |dir| dir.join(file))
}

/// Decides whether a changed path should trigger a restart.
#[derive(Debug)]
pub struct WatchFilter {
    include: GlobSet,
    ignore: GlobSet,
}

/// Compiles `patterns` into one [`GlobSet`], attributing a rejected pattern
/// to itself rather than reporting globset's own aggregate failure.
pub(super) fn build_glob_set(patterns: &[String]) -> Result<GlobSet, WatchFilterError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|err| WatchFilterError::Glob {
            pattern: pattern.clone(),
            reason: err.to_string(),
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|err| {
        // Unreachable in practice: every `Glob` above already parsed, and
        // globset's set-compilation step has no further way to reject an
        // already-valid pattern list. Returned as an error rather than
        // unwrapped, so a future violation fails loudly instead of panicking.
        WatchFilterError::Glob {
            pattern: patterns.join(", "),
            reason: err.to_string(),
        }
    })
}

/// Why a watch filter could not be built.
///
/// One variant, no `#[non_exhaustive]`: the only way construction fails is
/// a pattern globset rejects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchFilterError {
    /// A `watch_options` or `ignore_watch` pattern globset rejected.
    /// Carries the pattern as the user wrote it and globset's rendered
    /// reason.
    Glob {
        /// The pattern as written in the Flockfile.
        pattern: String,
        /// globset's own rendered reason.
        reason: String,
    },
}

impl fmt::Display for WatchFilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Glob { pattern, reason } => {
                write!(f, "invalid watch pattern `{pattern}`: {reason}")
            }
        }
    }
}

impl core::error::Error for WatchFilterError {}

/// A [`WatchFilter`] paired with the root its `triggers` calls are
/// relative to.
#[derive(Debug)]
pub(super) struct RootedFilter {
    pub(super) root: PathBuf,
    pub(super) filter: WatchFilter,
}

impl RootedFilter {
    /// Whether `path`, an absolute path exactly as notify delivered it,
    /// triggers a restart: strips `root`, then asks `filter`.
    ///
    /// A path outside `root` never triggers, rather than falling back to
    /// matching the untouched absolute form. `root` itself never triggers
    /// either, since it strips to an empty relative path; this matters
    /// because macOS can deliver a spurious `Create(Folder)` for the root
    /// the instant a watch arms.
    pub(super) fn triggers(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        if relative.as_os_str().is_empty() {
            return false;
        }
        self.filter.triggers(relative)
    }
}

impl WatchFilter {
    /// Builds the filter from an app's `watch_options` and `ignore_watch`.
    ///
    /// An empty `watch_options` matches every path; the default ignores
    /// always apply on top of `ignore_watch`.
    ///
    /// # Errors
    ///
    /// - [`WatchFilterError::Glob`]: a pattern the globset crate rejected,
    ///   carrying the pattern and the reason.
    pub fn new(
        watch_options: &[String],
        ignore_watch: &[String],
    ) -> Result<Self, WatchFilterError> {
        let include_patterns: Vec<String> = if watch_options.is_empty() {
            vec![MATCH_EVERYTHING.to_string()]
        } else {
            watch_options.to_vec()
        };
        let ignore_patterns: Vec<String> = DEFAULT_IGNORE_GLOBS
            .iter()
            .map(|pattern| (*pattern).to_string())
            .chain(ignore_watch.iter().cloned())
            .collect();

        Ok(Self {
            include: build_glob_set(&include_patterns)?,
            ignore: build_glob_set(&ignore_patterns)?,
        })
    }

    /// Whether `path`, relative to the watch root, triggers a restart.
    #[must_use]
    pub fn triggers(&self, path: &Path) -> bool {
        self.include.is_match(path) && !self.ignore.is_match(path)
    }
}

#[cfg(test)]
mod tests {

    use core::time::Duration;
    use std::path::{Path, PathBuf};

    use super::*;

    use crate::fake::ProcScript;

    use super::super::testing::*;

    // ------------------------------------------------------------------
    // `WatchFilter` and the root-relative boundary: pure, no tokio, no
    // filesystem.
    // ------------------------------------------------------------------
    #[test]
    fn empty_watch_options_matches_every_path() {
        let filter = WatchFilter::new(&[], &[]).unwrap();
        assert!(filter.triggers(Path::new("top.txt")));
        assert!(filter.triggers(Path::new("src/a/b.rs")));
    }

    #[test]
    fn an_explicit_pattern_matches_its_own_tree_and_nothing_else() {
        let filter = WatchFilter::new(&["src/**/*.rs".to_string()], &[]).unwrap();
        assert!(filter.triggers(Path::new("src/a/b.rs")));
        assert!(!filter.triggers(Path::new("src/a/b.txt")));
        assert!(!filter.triggers(Path::new("other/a.rs")));
    }

    // Uses the literal glob a user would write, to stay distinguishable
    // from the empty-`watch_options` case.
    #[test]
    fn default_ignores_beat_an_explicit_include() {
        let filter = WatchFilter::new(&["**".to_string()], &[]).unwrap();
        assert!(!filter.triggers(Path::new(".git/index")));
        assert!(!filter.triggers(Path::new("node_modules/x/y.js")));
    }

    #[test]
    fn an_ignore_watch_entry_beats_an_include() {
        let filter = WatchFilter::new(&["**".to_string()], &["dist/**".to_string()]).unwrap();
        assert!(!filter.triggers(Path::new("dist/bundle.js")));
        // Control: only the `dist` tree is excluded, not everything.
        assert!(filter.triggers(Path::new("src/main.rs")));
    }

    #[test]
    fn a_pattern_matching_nothing_never_triggers() {
        let filter = WatchFilter::new(&["nomatch/**/*.foo".to_string()], &[]).unwrap();
        assert!(!filter.triggers(Path::new("src/main.rs")));
    }

    #[test]
    fn an_invalid_glob_is_rejected_with_its_pattern() {
        let err = WatchFilter::new(&["[".to_string()], &[]).unwrap_err();
        let WatchFilterError::Glob { pattern, reason } = err;
        assert_eq!(pattern, "[");
        assert!(!reason.is_empty());

        let _: &dyn core::error::Error = &WatchFilterError::Glob {
            pattern: "[".to_string(),
            reason: "boom".to_string(),
        };
    }

    // Pins the rendered text: the case above destructures the variant and
    // never renders it.
    #[test]
    fn watch_filter_error_display_names_the_pattern_and_its_reason() {
        // A fabricated reason rather than globset's own, so the assertion is
        // an exact string and not a re-statement of whatever that crate
        // happens to render this release.
        let err = WatchFilterError::Glob {
            pattern: "[".to_string(),
            reason: "unclosed character class".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "invalid watch pattern `[`: unclosed character class"
        );
    }

    #[test]
    fn a_path_outside_the_root_never_triggers() {
        let filter = RootedFilter {
            root: PathBuf::from("/watched"),
            filter: WatchFilter::new(&[], &[]).unwrap(),
        };
        assert!(!filter.triggers(Path::new("/elsewhere/file.rs")));
        // Control: the same filter, under the root, does trigger.
        assert!(filter.triggers(Path::new("/watched/file.rs")));
    }

    // The root triggering ahead of both glob sets cannot be right on macOS,
    // which delivers a spurious `Create(Folder)` the instant a watch arms.
    #[test]
    fn the_root_itself_never_triggers_however_wide_the_watch_options() {
        // The widest include there is, so a failure here is about the root
        // and not about patterns that happened not to match it.
        let filter = matches_everything(PathBuf::from("/watched"));
        assert!(!filter.triggers(Path::new("/watched")));
        // Control: the same filter, one level in, does trigger, so the
        // case above is about the root itself rather than about a filter
        // that matches nothing.
        assert!(filter.triggers(Path::new("/watched/other/a.txt")));
    }

    // fails if the path reaches globset unescaped: `app[0].log` would
    // become a character class matching `app0.log`.
    #[test]
    fn own_log_ignores_covers_only_the_paths_under_the_root() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        // Canonical, as `arm_watch` hands it over: the raw tempdir path is
        // `/var/…` on macOS where its resolved form is `/private/var/…`.
        let canonical = std::fs::canonicalize(root.path()).unwrap();

        let inside = root.path().join("app[0].log");
        let outside = elsewhere.path().join("web-0-out.log");
        let ignores = own_log_ignores(&canonical, [inside.as_path(), outside.as_path()]);

        assert_eq!(ignores, vec!["app[[]0[]].log".to_string()]);
        let filter = WatchFilter::new(&[], &ignores).unwrap();
        assert!(!filter.triggers(Path::new("app[0].log")));
        // Controls: the escape matches that name and not the class it would
        // otherwise have spelled, and an unrelated sibling still triggers.
        assert!(filter.triggers(Path::new("app0.log")));
        assert!(filter.triggers(Path::new("src/main.rs")));
    }

    #[tokio::test(start_paused = true)]
    async fn a_batch_of_only_ignored_paths_produces_no_restart() {
        // Two scripts, not one: `start_app` consumes the first, so a
        // filter-bypassing implementation needs a second for its respawn.
        // With one, that respawn would report `Errored`, invisible to
        // `assert_no_restart_within`, and the mutation would pass by accident.
        let (handle, mut rx, _dir) = spawn_test_fixture(vec![ProcScript::never_exits(); 2]);
        let name = "web";
        start_app(&handle, name, 1).await;
        let root = PathBuf::from("/watched");
        let (tx, group) = spawn_group_matching_everything(&root, name, &handle);

        tx.send(changed(vec![root.join(".git/index")])).unwrap();
        assert_no_restart_within(&mut rx, name, Duration::from_secs(5)).await;

        group.abort();
    }
}
