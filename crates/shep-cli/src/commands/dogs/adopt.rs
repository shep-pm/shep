//! `shep adopt`: vets a third-party binary shep has never seen, records it,
//! and starts it if a shepherd is running.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use shep_client::Client;
use shep_core::paths::{ShepPaths, user_home};
use shep_core::protocol::{DogSource, Request, Response};

use crate::cli::AdoptArgs;
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;
use crate::output::{DogRow, Streams, emit, write_outcome};

use super::vet::{
    DogSchema, fail_adopt, report_dog_version, vet_binary, warn_group_writable,
    warn_unreadable_schema,
};
use super::{NO_SHEPHERD_ENABLE_STATUS, connect_or_absent, fail_config};

/// `shep adopt <path> [--name <name>]`: vets a binary shep has never seen,
/// records it, and starts it if a shepherd is running.
///
/// `args.path` is resolved by [`resolve_adopt_path`], and `args.name`
/// defaults to the resolved binary's file stem ([`default_dog_name`]); a
/// defaulted name goes through the same [`collides_with_a_verb`] refusal an
/// explicit `--name` would.
pub async fn adopt(streams: &mut Streams<'_>, paths: &ShepPaths, args: &AdoptArgs) -> ExitCode {
    let home = user_home(&|key| std::env::var_os(key));
    let path_var = std::env::var_os("PATH");
    let candidate = resolve_adopt_path(&args.path, home.as_deref(), path_var.as_deref());
    // Before vetting: `vet_binary` spawns the candidate, and a refusal
    // that runs after that spawn has already run what it refuses.
    let name = match &args.name {
        Some(name) => name.clone(),
        None => default_dog_name(&candidate),
    };
    if collides_with_a_verb(&name) {
        return fail_adopt_name_collision(streams, &name);
    }
    let vetted = match vet_binary(&candidate, &paths.home, &name) {
        Ok(vetted) => vetted,
        Err(refusal) => return fail_adopt(streams, &candidate, &refusal),
    };
    let path = vetted.path;
    for writable in &vetted.group_writable {
        warn_group_writable(streams, writable);
    }
    if let Some(answer) = &vetted.answer {
        report_dog_version(streams, &name, answer);
    }
    // The schema is stored by nothing and asked fresh where it is needed,
    // so the only thing to report is the one answer that is a bug.
    if vetted.schema == DogSchema::Unreadable {
        warn_unreadable_schema(streams, &name);
    }
    if let Err(err) = ShepToml::try_edit(&paths.daemon_config, |cfg| cfg.adopt_dog(&name, &path)) {
        return fail_config(streams, &err);
    }
    let client = match connect_or_absent(paths, streams).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    adopt_after_config(streams, &name, &path, client.as_ref()).await
}

/// Resolves `raw`, `shep adopt`'s own path argument, before it reaches
/// [`vet_binary`]: as given, with a leading `~/` expanded against `home`,
/// then looked up on `path_var`. First hit wins; if none finds anything,
/// `raw` comes back unchanged so `vet_binary` reports the same
/// [`AdoptRefusal::Missing`](super::vet::AdoptRefusal::Missing) it always has.
///
/// `home` and `path_var` are parameters, read once by [`adopt`], so this
/// stays a pure function of its inputs: this crate forbids `unsafe`, and a
/// test cannot reach `std::env::set_var`. All three routes funnel into the
/// one [`vet_binary`] call, so this changes what `adopt` can find, never
/// what it vets.
fn resolve_adopt_path(raw: &Path, home: Option<&Path>, path_var: Option<&OsStr>) -> PathBuf {
    if raw.exists() {
        return raw.to_path_buf();
    }
    if let Some(expanded) = raw
        .to_str()
        .and_then(|value| expand_tilde_candidate(value, home))
        && expanded.exists()
    {
        return expanded;
    }
    if let Some(found) = lookup_on_path(raw, path_var) {
        return found;
    }
    raw.to_path_buf()
}

/// `~/`-expands `value` against `home`, for [`resolve_adopt_path`]'s second
/// step. `None` for anything [`shep_core::config::expand_home_tilde`]
/// refuses, or that does not start with `~` at all: [`resolve_adopt_path`]
/// moves on rather than surfacing a tilde-specific error for a path that may
/// never have been a tilde path.
fn expand_tilde_candidate(value: &str, home: Option<&Path>) -> Option<PathBuf> {
    if !value.starts_with('~') {
        return None;
    }
    shep_core::config::expand_home_tilde(value, home)
        .ok()
        .map(PathBuf::from)
}

/// Looks `name` up on `path_var` the way a shell would, and only that way:
/// a bare name with no directory component of its own (`shep-log-rotate`,
/// not `./shep-log-rotate`), and only a hit with an execute bit set for
/// someone, so a same-named non-executable file earlier on `$PATH` does not
/// block the real binary further down it.
fn lookup_on_path(name: &Path, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let is_bare = name
        .parent()
        .is_some_and(|parent| parent.as_os_str().is_empty());
    if !is_bare {
        return None;
    }
    let dirs = path_var?;
    std::env::split_paths(dirs)
        .flat_map(|dir| {
            candidate_file_names(name)
                .into_iter()
                .map(move |file| dir.join(file))
        })
        .find(|candidate| {
            #[cfg(unix)]
            use std::os::unix::fs::PermissionsExt as _;
            // Windows has no execute bit and `CreateProcess` is the only
            // authority on `%PATHEXT%`, so being a file is the test there;
            // the spawn refuses a non-executable one with the OS's message.
            std::fs::metadata(candidate).is_ok_and(|meta| {
                #[cfg(unix)]
                {
                    meta.is_file() && meta.permissions().mode() & 0o111 != 0
                }
                #[cfg(windows)]
                {
                    meta.is_file()
                }
            })
        })
}

/// The file names a bare command could resolve to in one `$PATH` directory.
///
/// On unix that is the name itself and nothing else: a file is runnable if
/// its execute bit is set, whatever it is called.
///
/// Windows resolves a bare command through `%PATHEXT%`, and `cargo install`
/// writes `foo.exe`, never `foo`. The bare name is tried first, so an
/// extensionless file is still found; the extensions follow in `%PATHEXT%`
/// order. The fallback list is the documented default for a system where
/// the variable is unset.
fn candidate_file_names(name: &Path) -> Vec<std::ffi::OsString> {
    #[cfg(unix)]
    {
        vec![name.as_os_str().to_os_string()]
    }
    #[cfg(windows)]
    {
        let mut names = vec![name.as_os_str().to_os_string()];
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        for ext in pathext.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            let mut with_ext = name.as_os_str().to_os_string();
            with_ext.push(ext);
            names.push(with_ext);
        }
        names
    }
}

/// The dog name `shep adopt` defaults to when `--name` is omitted: `path`'s
/// file stem with one leading `shep-` stripped, the way `cargo` strips
/// `cargo-`. A stem that would strip to an empty name is kept whole.
///
/// Derived from `path` as resolved, before canonicalization: a symlink's own
/// name is what an operator typed.
fn default_dog_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("dog"));
    stem.strip_prefix("shep-")
        .filter(|rest| !rest.is_empty())
        .unwrap_or(stem)
        .to_string()
}

/// Whether `name` already names a built-in verb or one of its visible
/// aliases. A dog adopted under such a name could never be reached: `shep
/// <name>` dispatches to the built-in verb first.
///
/// Read off the real `clap::Command` tree rather than a hand-copied list, so
/// a verb added later is refused automatically.
fn collides_with_a_verb(name: &str) -> bool {
    use clap::CommandFactory as _;
    crate::cli::Cli::command()
        .get_subcommands()
        .any(|sub| sub.get_name() == name || sub.get_all_aliases().any(|alias| alias == name))
}

/// Renders the refusal for a name `shep adopt` will not accept because it
/// already names a built-in verb or alias.
fn fail_adopt_name_collision(streams: &mut Streams<'_>, name: &str) -> ExitCode {
    let code = ExitCode::InvalidConfig;
    let message = format!(
        "`{name}` is already a shep verb or alias, so an adopted dog by that name could never \
         be reached -- pick another name with --name"
    );
    streams.fail(code, &message)
}

/// `adopt`'s daemon half; see enable_after_config for the split and for
/// what `client: None` means.
async fn adopt_after_config(
    streams: &mut Streams<'_>,
    name: &str,
    path: &Path,
    client: Option<&Client>,
) -> ExitCode {
    let source = DogSource::Adopted {
        path: path.display().to_string(),
    };
    let Some(client) = client else {
        let row = DogRow::new(name, source, NO_SHEPHERD_ENABLE_STATUS, false);
        return write_outcome(emit(
            &mut *streams.out,
            streams.fmt,
            "adopt",
            row,
            streams.style,
        ));
    };
    let request = Request::EnableDog {
        name: name.to_string(),
        source: source.clone(),
    };
    match client.request(request).await {
        Ok(Response::DogStarted(info)) => {
            let row = DogRow::new(name, source, info.status, true);
            write_outcome(emit(
                &mut *streams.out,
                streams.fmt,
                "adopt",
                row,
                streams.style,
            ))
        }
        Ok(_unrecognised) => unexpected_response(streams),
        Err(err) => client_error(streams, &err),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use shep_client::testing::fake_client_capturing_envelopes;

    use super::*;
    use crate::cli::Format;

    /// Every test here drives a dog verb under `--format table`.
    fn streams<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>) -> Streams<'a> {
        Streams {
            out,
            err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        }
    }

    /// `adopt`'s own report is the only place an operator learns which
    /// refusal mode it was.
    #[tokio::test]
    async fn adopt_of_a_missing_binary_reports_the_refusal_on_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let args = AdoptArgs {
            name: Some("otel".to_string()),
            path: dir.path().join("nope"),
        };
        let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

        assert_eq!(code, ExitCode::InvalidConfig);
        let text = String::from_utf8(err).unwrap();
        assert!(
            text.contains("no file exists at that path"),
            "the refusal must reach the operator: {text}"
        );
    }

    #[tokio::test]
    async fn adopt_asks_the_shepherd_to_start_that_dog_with_its_adopted_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let binary = PathBuf::from("/usr/local/bin/shep-otel");
        let _ = adopt_after_config(
            &mut streams(&mut out, &mut err),
            "otel",
            &binary,
            Some(&client),
        )
        .await;

        let sent = envelopes.recv().await.unwrap();
        assert_eq!(
            sent.body,
            Request::EnableDog {
                name: "otel".to_string(),
                source: DogSource::Adopted {
                    path: "/usr/local/bin/shep-otel".to_string(),
                },
            }
        );
    }

    #[tokio::test]
    async fn adopt_with_no_shepherd_writes_the_config_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let binary = dir.path().join("shep-otel");
        std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
        let mut mode = std::fs::metadata(&binary).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        std::fs::set_permissions(&binary, mode).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let args = AdoptArgs {
            name: Some("otel".to_string()),
            path: binary,
        };
        let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(
            written.contains("otel"),
            "the config edit must still land: {written}"
        );
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("next shepherd"),
            "the operator needs to know the dog is not running yet: {text}"
        );
    }

    #[tokio::test]
    async fn adopt_with_no_name_flag_defaults_from_the_stripped_stem() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let binary = dir.path().join("shep-otel");
        std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
        let mut mode = std::fs::metadata(&binary).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        std::fs::set_permissions(&binary, mode).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let args = AdoptArgs {
            path: binary,
            name: None,
        };
        let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        let cfg = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
        assert!(
            cfg.daemon.adopted_dogs.contains_key("otel"),
            "the defaulted name must be `otel`, not `shep-otel`: {written}"
        );
    }

    /// `dispatch_adopted_dog` (`lib.rs`) runs only once clap has failed to
    /// match the name against a real subcommand, so such a dog is
    /// unreachable. The refusal must precede any `shep.toml` write.
    #[tokio::test]
    async fn adopt_refuses_a_name_that_collides_with_a_built_in_verb() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let binary = dir.path().join("watchdog");
        std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
        let mut mode = std::fs::metadata(&binary).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        std::fs::set_permissions(&binary, mode).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        // "stop" is a real verb; "ls" is `flock`'s own visible alias.
        for reserved in ["stop", "ls"] {
            let args = AdoptArgs {
                path: binary.clone(),
                name: Some(reserved.to_string()),
            };
            let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;
            assert_eq!(
                code,
                ExitCode::InvalidConfig,
                "`{reserved}` must be refused"
            );
        }
        assert!(
            !paths.daemon_config.exists(),
            "a name collision must never touch shep.toml: {}",
            paths.daemon_config.display()
        );
    }

    /// The outcome is the same whichever order runs, so the two are told
    /// apart by the refusal: `args.path` names nothing on disk, so vet-first
    /// would give "no file exists at that path".
    #[tokio::test]
    async fn a_name_collision_is_refused_before_vet_binary_ever_runs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        let mut out = Vec::new();
        let mut err = Vec::new();
        let args = AdoptArgs {
            path: dir.path().join("nope"),
            name: Some("stop".to_string()),
        };
        let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

        assert_eq!(code, ExitCode::InvalidConfig);
        let text = String::from_utf8(err).unwrap();
        assert!(
            text.contains("already a shep verb or alias"),
            "the collision must be the reported reason, not vet_binary's own refusal: {text}"
        );
        assert!(
            !text.contains("no file exists at that path"),
            "vet_binary must never run on a name that was always going to be refused: {text}"
        );
    }

    #[test]
    fn resolve_adopt_path_prefers_a_literal_path_that_exists() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("thing");
        std::fs::write(&binary, "").unwrap();

        let resolved = resolve_adopt_path(&binary, None, None);
        assert_eq!(resolved, binary);
    }

    #[test]
    fn resolve_adopt_path_expands_a_leading_tilde_against_the_given_home() {
        let home = tempfile::tempdir().unwrap();
        let binary_dir = home.path().join(".cargo/bin");
        std::fs::create_dir_all(&binary_dir).unwrap();
        let binary = binary_dir.join("shep-log-rotate");
        std::fs::write(&binary, "").unwrap();

        let raw = Path::new("~/.cargo/bin/shep-log-rotate");
        let resolved = resolve_adopt_path(raw, Some(home.path()), None);
        assert_eq!(resolved, binary);
    }

    /// `cargo install shep-log-rotate` puts the binary on `$PATH` under its
    /// own name, and nowhere else.
    #[test]
    fn resolve_adopt_path_falls_back_to_a_path_lookup_for_a_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("shep-log-rotate");
        std::fs::write(&binary, "").unwrap();
        let mut mode = std::fs::metadata(&binary).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        std::fs::set_permissions(&binary, mode).unwrap();
        let path_var = std::ffi::OsString::from(dir.path());

        let raw = Path::new("shep-log-rotate");
        let resolved = resolve_adopt_path(raw, None, Some(&path_var));
        assert_eq!(resolved, binary);
    }

    /// A same-named file elsewhere on `$PATH` would silently adopt the
    /// wrong binary.
    #[test]
    fn resolve_adopt_path_does_not_path_search_a_name_with_a_directory_component() {
        let path_dir = tempfile::tempdir().unwrap();
        // A file that would match if `$PATH` were searched, so the guard is
        // what this proves rather than an absent file.
        let decoy = path_dir.path().join("thing");
        std::fs::write(&decoy, "").unwrap();
        let mut mode = std::fs::metadata(&decoy).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        std::fs::set_permissions(&decoy, mode).unwrap();
        let path_var = std::ffi::OsString::from(path_dir.path());

        let raw = Path::new("./thing");
        let resolved = resolve_adopt_path(raw, None, Some(&path_var));
        assert_eq!(
            resolved, raw,
            "a name with its own directory must never be searched on $PATH"
        );
    }

    /// Keeps a plain missing path reporting [`AdoptRefusal::Missing`]
    /// rather than a resolution-specific error.
    #[test]
    fn resolve_adopt_path_returns_raw_unchanged_when_nothing_resolves() {
        let raw = Path::new("/nonexistent/shep-nothing");
        assert_eq!(resolve_adopt_path(raw, None, None), raw);
    }

    #[test]
    fn default_dog_name_strips_one_leading_shep_prefix_and_no_further() {
        assert_eq!(
            default_dog_name(Path::new("/opt/bin/shep-log-rotate")),
            "log-rotate"
        );
        assert_eq!(default_dog_name(Path::new("/opt/bin/otel")), "otel");
        // Stripping the prefix here would leave "", an unreachable name,
        // so the whole stem is kept.
        assert_eq!(default_dog_name(Path::new("/opt/bin/shep-")), "shep-");
    }

    #[test]
    fn collides_with_a_verb_covers_names_and_visible_aliases() {
        assert!(collides_with_a_verb("stop"), "a real verb must collide");
        assert!(collides_with_a_verb("ls"), "flock's own alias must collide");
        assert!(
            !collides_with_a_verb("watchdog"),
            "an arbitrary name must not collide"
        );
    }
}
