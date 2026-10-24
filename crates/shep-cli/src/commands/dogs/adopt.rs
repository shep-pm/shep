//! `shep adopt`: vets a third-party binary shep has never seen, records it,
//! and starts it if a shepherd is running.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use shep_client::Client;
use shep_core::paths::{ShepPaths, user_home};
use shep_core::permissions::Grants;
use shep_core::protocol::{DogSource, Request, Response};

use crate::cli::AdoptArgs;
use crate::commands::dog_migration::{
    DefaultsOutcome, DogMigrationError, migrate_dog_sections, seed_dog_defaults,
};
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;
use crate::output::{DogActionStatus, Streams};

use super::consent::{Input, Review, Verdict, consent_from, review};
use super::depends::warn_of_unmet_depends;
use super::grants::warn_if_unenforced;
use super::vet::{
    DogSchema, fail_adopt, report_channel_ask, report_dog_version, vet_binary, warn_group_writable,
    warn_unreadable_schema,
};
use super::{
    DECLINED_STATUS, NO_SHEPHERD_ENABLE_STATUS, connect_or_absent, emit_dog_action, fail_config,
};

/// `shep adopt <path> [--name <name>]`: vets a binary shep has never seen,
/// records it, and starts it if a shepherd is running.
///
/// `args.path` is resolved by [`resolve_adopt_path`], and `args.name`
/// defaults to the resolved binary's file stem ([`default_dog_name`]); a
/// defaulted name goes through the same [`collides_with_a_verb`] refusal an
/// explicit `--name` would.
pub async fn adopt(streams: &mut Streams<'_>, paths: &ShepPaths, args: &AdoptArgs) -> ExitCode {
    adopt_asking(streams, paths, args, &mut Input::stdin()).await
}

/// [`adopt`], asking at `input` rather than at this process's stdin.
pub(super) async fn adopt_asking(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &AdoptArgs,
    input: &mut Input,
) -> ExitCode {
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
    let path = vetted.path.clone();
    for writable in &vetted.group_writable {
        warn_group_writable(streams, writable);
    }
    if let Some(answer) = &vetted.answer {
        report_dog_version(streams, &name, answer);
    }
    let channel = vetted.answer.as_ref().is_some_and(|answer| answer.channel);
    if channel {
        report_channel_ask(streams, &name);
    }
    // The schema is stored by nothing and asked fresh where it is needed,
    // so the only thing to report is the one answer that is a bug.
    if vetted.schema == DogSchema::Unreadable {
        warn_unreadable_schema(streams, &name);
    }
    // Before anything is written: a refusal here, for a dog that requires
    // what this shep does not know or for want of a terminal, leaves
    // `shep.toml` as it was.
    let reviewed = match review(vetted.manifest.as_ref()) {
        Ok(reviewed) => reviewed,
        Err(refusal) => return refusal.report(streams, &name, "adopted"),
    };
    let verdict = match ask_before_adopting(streams, paths, &name, &reviewed, args.yes, input) {
        Ok(verdict) => verdict,
        Err(code) => return code,
    };
    let (adopted, grants) = match ShepToml::try_edit(&paths.daemon_config, |cfg| {
        cfg.adopt_dog(&name, &path, channel)?;
        // The answer was asked against a read taken before the question. A
        // grant another shep wrote since was decided by someone with the
        // same say, and stands.
        let decided_meanwhile = cfg.granted(&name)?.is_some();
        match &verdict {
            Some(_) if decided_meanwhile => {}
            Some(Verdict::Grant(Some(grants))) => cfg.set_granted(&name, grants)?,
            // `adopt_dog` enables; a no leaves the dog adopted and off.
            Some(Verdict::Declined) => cfg.disable_dog(&name),
            Some(Verdict::Grant(None)) | None => {}
        }
        Ok((cfg.adopted_dog_names(), cfg.granted(&name)?))
    }) {
        Ok(recorded) => recorded,
        Err(err) => return fail_config(streams, &err),
    };
    // The registration is in, so the defaults half is the last config write
    // `adopt` makes and it still precedes any socket: a dog adopted against
    // no shepherd gets its section too.
    if let Some(defaults) = vetted
        .manifest
        .as_ref()
        .and_then(|manifest| manifest.defaults.as_deref())
    {
        // A home that still holds `[dog.<name>]` values in `shep.toml` must
        // move them before a seeded section could sit beside them: seeding
        // first would write a second copy of a section the boot's migration
        // then refuses to move for holding values in both files. The
        // migration is what the next boot runs anyway, and it is idempotent,
        // so running it here only moves that boot's work earlier, where the
        // operator is watching. Same report as `shep reload` gives.
        match migrate_dog_sections(paths) {
            Ok(moved) if moved.is_empty() => {}
            Ok(moved) => streams.aside(
                "adopt",
                &format!(
                    "moved dog config out of shep.toml and into dogs.toml: {}",
                    moved.join(", ")
                ),
            ),
            Err(err) => return fail_dogs_config(streams, &err),
        }
        match seed_dog_defaults(paths, &name, defaults) {
            Ok(DefaultsOutcome::Written) => report_defaults_written(streams, &name),
            Ok(DefaultsOutcome::Kept | DefaultsOutcome::Empty) => {}
            Ok(DefaultsOutcome::Unreadable) => warn_unreadable_defaults(streams, &name),
            Err(err) => return fail_dogs_config(streams, &err),
        }
    }
    if let Some(manifest) = &vetted.manifest {
        warn_of_unmet_depends(streams, &name, manifest, &adopted);
    }
    if verdict == Some(Verdict::Declined) {
        // Nothing for a shepherd to start: the dog is adopted and disabled.
        let source = DogSource::Adopted {
            grants: None,
            path: path.display().to_string(),
            channel,
        };
        return emit_dog_action(streams, "adopt", &name, source, DECLINED_STATUS);
    }
    let client = match connect_or_absent(paths, streams).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    adopt_after_config(streams, &name, &path, channel, grants, client.as_ref()).await
}

/// Asks whether to grant `name` what it declares, unless `shep.toml` already
/// holds a decision for it.
///
/// `None` is that earlier decision standing: a re-adopt of a dog that was
/// granted something keeps it, and the answer to a changed declaration is a
/// later step's. `Some` is this run's answer.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
fn ask_before_adopting(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    reviewed: &Review,
    yes: bool,
    input: &mut Input,
) -> Result<Option<Verdict>, ExitCode> {
    let cfg =
        ShepToml::read_only(&paths.daemon_config).map_err(|err| fail_config(streams, &err))?;
    if cfg
        .granted(name)
        .map_err(|err| fail_config(streams, &err))?
        .is_some()
    {
        return Ok(None);
    }
    consent_from(streams, name, "adopt", reviewed, yes, input).map(Some)
}

/// Resolves `raw`, `shep adopt`'s own path argument, before it reaches
/// [`vet_binary`]: as given, with a leading `~/` expanded against `home`,
/// then looked up on `path_var`. First hit wins. A step moves on only when
/// its lookup proves the path absent: one that fails, an unsearchable
/// directory say, counts as a hit, so `vet_binary` reports it as
/// [`AdoptRefusal::Inaccessible`](super::vet::AdoptRefusal::Inaccessible).
/// If no step finds anything, `raw` comes back unchanged and `vet_binary`
/// reports the same [`AdoptRefusal::Missing`](super::vet::AdoptRefusal::Missing)
/// it always has.
///
/// `home` and `path_var` are parameters, read once by [`adopt`], so this
/// stays a pure function of its inputs: this crate forbids `unsafe`, and a
/// test cannot reach `std::env::set_var`. All three routes funnel into the
/// one [`vet_binary`] call, so this changes what `adopt` can find, never
/// what it vets.
fn resolve_adopt_path(raw: &Path, home: Option<&Path>, path_var: Option<&OsStr>) -> PathBuf {
    // Only a lookup that proves absence moves on: one that fails, an
    // unsearchable directory say, leaves `vet_binary` to report why.
    let not_absent = |path: &Path| !matches!(path.try_exists(), Ok(false));
    if not_absent(raw) {
        return raw.to_path_buf();
    }
    if let Some(expanded) = raw
        .to_str()
        .and_then(|value| expand_tilde_candidate(value, home))
        && not_absent(&expanded)
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
/// a verb added later is refused automatically. The tree is built first
/// because clap generates its `help` subcommand at build time: an unbuilt
/// `Cli::command()` holds no `help`, and `shep help` prints shep's own help
/// rather than reaching a dog.
pub(crate) fn collides_with_a_verb(name: &str) -> bool {
    use clap::CommandFactory as _;
    let mut command = crate::cli::Cli::command();
    command.build();
    command
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

/// [`emit_notice`](crate::output::emit_notice) code for the report that a
/// manifest's defaults became the dog's section; not a failure.
const DOG_DEFAULTS_WRITTEN_NOTICE: &str = "dog_defaults_written";

/// Tells the operator that the dog's own defaults are now its section in
/// `dogs.toml`.
///
/// The whole point of writing them is that the operator now has the knobs
/// where they can edit them, and this is the one line saying where that is.
fn report_defaults_written(streams: &mut Streams<'_>, name: &str) {
    let message = format!(
        "{name}'s manifest defaults are now its `[{name}]` section in dogs.toml, where \
         you can edit them"
    );
    streams.aside(DOG_DEFAULTS_WRITTEN_NOTICE, &message);
}

/// [`emit_notice`](crate::output::emit_notice) code for the unreadable-
/// defaults warning; not a failure.
const DOG_DEFAULTS_UNREADABLE_NOTICE: &str = "dog_defaults_unreadable";

/// Warns that `name` shipped a manifest whose `defaults` is not valid
/// TOML, and lets the adopt proceed.
///
/// The dog's bug, told the same way an unreadable schema is told: the dog
/// is adopted, its section is not written, and the one who wrote the dog
/// is the one to fix it.
fn warn_unreadable_defaults(streams: &mut Streams<'_>, name: &str) {
    let message = format!(
        "{name} shipped a manifest whose `defaults` is not valid TOML, so nothing was \
         added to dogs.toml for it. The dog is adopted and runs normally; this is a bug to \
         report to whoever wrote it"
    );
    streams.aside(DOG_DEFAULTS_UNREADABLE_NOTICE, &message);
}

/// Renders a `dogs.toml` or migration failure and returns its exit code.
///
/// A `dogs.toml` that is not valid TOML is the operator's broken config, so
/// [`ExitCode::InvalidConfig`] like a broken `shep.toml`, and so are the
/// migration's two refusals: a name holding values in both files, and a
/// `[dog]` whose names did not come back as sections. The I/O modes have no
/// more specific code than [`ExitCode::Failure`].
fn fail_dogs_config(streams: &mut Streams<'_>, err: &DogMigrationError) -> ExitCode {
    let code = match err {
        DogMigrationError::Parse(_)
        | DogMigrationError::WouldOverwrite { .. }
        | DogMigrationError::SectionsUnreadable { .. } => ExitCode::InvalidConfig,
        _ => ExitCode::Failure,
    };
    streams.fail(code, &err.to_string())
}

/// `adopt`'s daemon half; see enable_after_config for the split and for
/// what `client: None` means. `channel` is the binary's own ask and `grants`
/// what `shep.toml` records for the dog.
async fn adopt_after_config(
    streams: &mut Streams<'_>,
    name: &str,
    path: &Path,
    channel: bool,
    grants: Option<Grants>,
    client: Option<&Client>,
) -> ExitCode {
    let source = DogSource::Adopted {
        grants,
        path: path.display().to_string(),
        channel,
    };
    let Some(client) = client else {
        return emit_dog_action(streams, "adopt", name, source, NO_SHEPHERD_ENABLE_STATUS);
    };
    let request = Request::EnableDog {
        name: name.to_string(),
        source: source.clone(),
    };
    match client.request(request).await {
        Ok(Response::DogStarted(info)) => {
            warn_if_unenforced(streams, name, &source, client.daemon().protocol);
            emit_dog_action(
                streams,
                "adopt",
                name,
                source,
                DogActionStatus::Acted(info.status.into()),
            )
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
    use crate::commands::dogs::test_support::streams;

    /// `adopt`'s own report is the only place an operator learns which
    /// refusal mode it was.
    #[tokio::test]
    async fn adopt_of_a_missing_binary_reports_the_refusal_on_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let args = AdoptArgs {
            yes: true,
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
        for channel in [false, true] {
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
                channel,
                None,
                Some(&client),
            )
            .await;

            let sent = envelopes.recv().await.unwrap();
            assert_eq!(
                sent.body,
                Request::EnableDog {
                    name: "otel".to_string(),
                    source: DogSource::Adopted {
                        grants: None,
                        path: "/usr/local/bin/shep-otel".to_string(),
                        channel,
                    },
                }
            );
        }
    }

    /// Writes an executable `/bin/sh` dog that answers `--version` with
    /// `answer` and exits 0 on any other run.
    fn write_dog(dir: &Path, answer: &str) -> PathBuf {
        let binary = dir.join("shep-otel");
        let script =
            format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf '{answer}'; fi\nexit 0\n");
        std::fs::write(&binary, script).unwrap();
        let mut mode = std::fs::metadata(&binary).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        std::fs::set_permissions(&binary, mode).unwrap();
        binary
    }

    /// Through `adopt` itself, so the ask travels from a real probe answer
    /// to `shep.toml`. A dog that does not ask is the control.
    #[tokio::test]
    async fn adopt_records_a_channel_ask_only_when_the_dog_makes_one() {
        for (answer, asked) in [
            (r"shep-otel 0.1.0\nshep-channel: true\n", true),
            (r"shep-otel 0.1.0\n", false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let paths = ShepPaths::resolve(&|_| None, dir.path());
            let args = AdoptArgs {
                yes: true,
                name: Some("otel".to_string()),
                path: write_dog(dir.path(), answer),
            };
            let mut out = Vec::new();
            let mut err = Vec::new();

            let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

            assert_eq!(code, ExitCode::Success);
            let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
            let cfg = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
            assert_eq!(
                cfg.daemon.channel_dogs.contains(&"otel".to_string()),
                asked,
                "{written}"
            );
            let notices = String::from_utf8(err).unwrap();
            assert_eq!(
                notices.contains("asked for the shepherd channel"),
                asked,
                "the operator hears what the dog asked for: {notices}"
            );
        }
    }

    /// `shepherd_acted` and `status` together separate "only the config
    /// changed" from "a shepherd acted", and this verb's two branches differ
    /// in little else. The pair is the guard: either field alone passes while
    /// the other branch carries the wrong one.
    #[tokio::test]
    async fn adopt_reports_whether_a_shepherd_acted() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = shep_client::testing::fake_client_answering(&path, |_| {
            Response::DogStarted(shep_client::testing::sample_info())
        })
        .await;
        let binary = PathBuf::from("/usr/local/bin/shep-otel");

        for (client, expected) in [
            (Some(&client), DogActionStatus::Acted("online")),
            (None, NO_SHEPHERD_ENABLE_STATUS),
        ] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut streams = streams(&mut out, &mut err);
            streams.fmt = Format::Json;

            let code = adopt_after_config(&mut streams, "otel", &binary, false, None, client).await;

            assert_eq!(code, ExitCode::Success);
            let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(
                json["data"]["shepherd_acted"],
                serde_json::json!(expected.shepherd_acted()),
                "client present: {}, payload: {}",
                client.is_some(),
                String::from_utf8_lossy(&out)
            );
            assert_eq!(
                json["data"]["status"],
                expected.text(),
                "payload: {}",
                String::from_utf8_lossy(&out)
            );
        }
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
            yes: true,
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
            yes: true,
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
                yes: true,
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
            yes: true,
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

    /// `exists` answers `false` for a path it cannot search, which would hand
    /// `vet_binary` the unexpanded `~/…` and a "no file exists" for a file
    /// that is there.
    #[test]
    fn resolve_adopt_path_expands_a_tilde_path_it_cannot_search() {
        let home = tempfile::tempdir().unwrap();
        let sealed = home.path().join("sealed");
        std::fs::create_dir(&sealed).unwrap();
        std::fs::write(sealed.join("dog"), "").unwrap();
        let mut mode = std::fs::metadata(&sealed).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o000);
        std::fs::set_permissions(&sealed, mode.clone()).unwrap();

        // A process that bypasses mode bits (root) can still search the
        // directory, so the lookup never fails and nothing is under test.
        let hidden = std::fs::metadata(sealed.join("dog")).is_err();
        let raw = Path::new("~/sealed/dog");
        let resolved = resolve_adopt_path(raw, Some(home.path()), None);

        // Restored before any assert so the tempdir can be removed.
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o700);
        std::fs::set_permissions(&sealed, mode).unwrap();
        if !hidden {
            return;
        }
        assert_eq!(resolved, sealed.join("dog"));
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
        // clap generates the `help` subcommand when it builds a command, so
        // it is missing until `Cli::command()` is built. `shep help` prints
        // shep's own help and never reaches a dog.
        assert!(
            collides_with_a_verb("help"),
            "clap's generated help subcommand is a name a dog can never take"
        );
        assert!(
            !collides_with_a_verb("watchdog"),
            "an arbitrary name must not collide"
        );
    }
}
