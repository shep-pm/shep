//! `shep adopt`: vets a third-party binary shep has never seen, records it,
//! and starts it if a shepherd is running.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use shep_client::{Client, RequestError};
use shep_core::dogs::manifest::Manifest;
use shep_core::paths::{ShepPaths, user_home};
use shep_core::protocol::{DogSource, EnvValue, Request, Response, RpcErrorCode};

use crate::cli::AdoptArgs;
use crate::commands::dog_migration::{
    DefaultsOutcome, DogMigrationError, migrate_dog_sections, seed_dog_defaults,
};
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::shep_toml::{GrantRecord, ShepToml, ShepTomlError};
use crate::dog_index::IndexedPermission;
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
    DECLINED_STATUS, DISABLED_STATUS, NO_SHEPHERD_ENABLE_STATUS, NOT_ENABLED_STATUS,
    connect_or_absent, emit_dog_action, fail_config, recorded_events,
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

/// What the caller of [`adopt_with`] reports itself as, and whether the dog
/// ends up running.
#[derive(Debug, Clone, Copy)]
pub(super) struct Flow {
    /// The `command` of the reply: `adopt`, or `install` when `shep install`
    /// hands its build over.
    pub(super) verb: &'static str,
    /// Whether a dog the operator granted is enabled. `shep install
    /// --no-enable` stops after adopting, which leaves the grant recorded and
    /// the dog off.
    pub(super) enable: bool,
    /// Whether a dog already running is stopped and started again, so it runs
    /// the binary just recorded. `shep upgrade` sets it.
    pub(super) restart: bool,
}

impl Flow {
    /// `shep adopt`: adopt, then enable.
    pub(super) const ADOPT: Self = Self {
        verb: "adopt",
        enable: true,
        restart: false,
    };
}

/// [`adopt`], asking at `input` rather than at this process's stdin.
pub(super) async fn adopt_asking(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &AdoptArgs,
    input: &mut Input,
) -> ExitCode {
    adopt_with(streams, paths, args, input, Flow::ADOPT, None).await
}

/// [`adopt_asking`], reporting as `flow.verb` and enabling only if
/// `flow.enable`.
pub(super) async fn adopt_with(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &AdoptArgs,
    input: &mut Input,
    flow: Flow,
    note: Option<&str>,
) -> ExitCode {
    adopt_noting(
        streams,
        paths,
        args,
        input,
        flow,
        Note {
            text: note,
            listed: None,
        },
        &mut false,
    )
    .await
}

/// What ends the grant listing, directly above the question.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Note<'a> {
    /// A line the caller has to say, such as where the build came from.
    pub(super) text: Option<&'a str>,
    /// The permissions the index row lists. Each one the reviewed manifest
    /// declares beyond these adds a line. `None` when the row listed none or
    /// the caller has no row to compare with.
    pub(super) listed: Option<&'a [IndexedPermission]>,
}

impl Note<'_> {
    /// The note for `manifest`: [`Note::text`], then one line for each
    /// declared permission not in [`Note::listed`], by name.
    pub(super) fn lines(self, manifest: Option<&Manifest>) -> Option<String> {
        let unlisted = self
            .listed
            .zip(manifest)
            .into_iter()
            .flat_map(|(listed, manifest)| {
                manifest
                    .permissions
                    .iter()
                    .filter(|declared| listed.iter().all(|row| row.name != declared.name))
                    .map(|declared| format!("the index did not list {}", declared.name))
            });
        let lines = self
            .text
            .map(str::to_owned)
            .into_iter()
            .chain(unlisted)
            .collect::<Vec<_>>();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }
}

/// [`adopt_with`], setting `said_no` when the operator answered no at the
/// grant question. A refusal before the answer leaves it as it was.
pub(super) async fn adopt_noting(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &AdoptArgs,
    input: &mut Input,
    flow: Flow,
    note: Note<'_>,
    said_no: &mut bool,
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
    // From the manifest the question is asked of, so a read that failed
    // earlier cannot leave an unlisted permission unsaid.
    let note = note.lines(vetted.manifest.as_ref());
    let verdict = match ask_before_adopting(
        streams,
        paths,
        &name,
        &reviewed,
        note.as_deref(),
        args.yes,
        input,
    ) {
        Ok(verdict) => verdict,
        Err(code) => return code,
    };
    let (adopted, grants, declined, held_back, env, events) =
        match ShepToml::try_edit(&paths.daemon_config, |cfg| {
            cfg.adopt_dog(&name, &path, channel)?;
            // The answer was asked against a read taken before the question. A
            // grant another shep wrote since was decided by someone with the
            // same say, and stands.
            let decided_meanwhile = cfg.granted(&name)?.is_some();
            // Whether a no is what this write left: the daemon half follows the
            // file, not the raw answer, so a dog granted meanwhile is not stopped.
            let mut declined = false;
            match &verdict {
                Some(_) if decided_meanwhile => follow_declared_events(cfg, &name, &reviewed)?,
                Some(Verdict::Grant(Some(grants))) => cfg.set_grant_record(
                    &name,
                    &GrantRecord::reviewed(grants.clone()).with_events(reviewed.events()),
                )?,
                // `adopt_dog` enables; a no leaves the dog adopted and off.
                Some(Verdict::Declined) => {
                    cfg.disable_dog(&name);
                    declined = true;
                }
                Some(Verdict::Grant(None)) => {}
                // Already decided before this run: nothing to ask, but the
                // kinds belong to the build being adopted.
                None => follow_declared_events(cfg, &name, &reviewed)?,
            }
            // `adopt_dog` enables; `--no-enable` takes that back, and says so
            // only when it is the reason the dog is off.
            let held_back = !flow.enable && !declined;
            if held_back {
                cfg.disable_dog(&name);
            }
            Ok((
                cfg.adopted_dog_names(),
                cfg.granted(&name)?,
                declined,
                held_back,
                cfg.adopted_dog_env(&name),
                recorded_events(cfg, &name),
            ))
        }) {
            Ok(recorded) => recorded,
            Err(err) => return fail_config(streams, &err),
        };
    *said_no = declined;
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
        // then refuses to move for holding values in both files.
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
        warn_of_pane_without_channel(streams, &name, manifest);
    }
    let client = match connect_or_absent(paths, streams).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    if declined {
        let source = DogSource::Adopted {
            grants: None,
            path: path.display().to_string(),
            channel,
            events: Vec::new(),
        };
        return declined_after_config(streams, flow.verb, &name, source, client.as_ref()).await;
    }
    if held_back {
        let source = DogSource::Adopted {
            grants,
            path: path.display().to_string(),
            channel,
            events,
        };
        return held_back_after_config(streams, flow.verb, &name, source, client.as_ref()).await;
    }
    let source = DogSource::Adopted {
        grants,
        path: path.display().to_string(),
        channel,
        events,
    };
    adopt_after_config(
        streams,
        flow.verb,
        &name,
        source,
        &env,
        client.as_ref(),
        flow.restart,
    )
    .await
}

/// A declined adopt's daemon half: the config now has the dog disabled, so a
/// shepherd already running it (a re-adopt of a dog from before grants) is told
/// to stop it. With no shepherd there is nothing running to stop.
pub(super) async fn declined_after_config(
    streams: &mut Streams<'_>,
    verb: &str,
    name: &str,
    source: DogSource,
    client: Option<&Client>,
) -> ExitCode {
    off_after_config(streams, verb, name, source, client, DECLINED_STATUS).await
}

/// `--no-enable`'s daemon half: the config has the dog adopted and granted
/// but disabled, so a shepherd already running it is told to stop it.
async fn held_back_after_config(
    streams: &mut Streams<'_>,
    verb: &str,
    name: &str,
    source: DogSource,
    client: Option<&Client>,
) -> ExitCode {
    off_after_config(streams, verb, name, source, client, NOT_ENABLED_STATUS).await
}

/// The daemon half of an adopt that leaves the dog off, told in `idle` when
/// no shepherd was running or the shepherd had no such dog.
async fn off_after_config(
    streams: &mut Streams<'_>,
    verb: &str,
    name: &str,
    source: DogSource,
    client: Option<&Client>,
    idle: DogActionStatus,
) -> ExitCode {
    let Some(client) = client else {
        return emit_dog_action(streams, verb, name, source, idle);
    };
    match client
        .request(Request::DisableDog {
            name: name.to_string(),
        })
        .await
    {
        Ok(Response::Deleted(_ids)) => {
            emit_dog_action(streams, verb, name, source, DISABLED_STATUS)
        }
        // The shepherd holds no such dog: nothing was running to stop.
        Err(RequestError::Rpc(rpc)) if rpc.code == RpcErrorCode::NotFound => {
            emit_dog_action(streams, verb, name, source, idle)
        }
        Ok(_unrecognised) => unexpected_response(streams),
        Err(err) => client_error(streams, &err),
    }
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
    note: Option<&str>,
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
    consent_from(streams, name, "adopt", reviewed, note, yes, input).map(Some)
}

/// Makes the event kinds an existing record holds for `name` those `reviewed`
/// declares, leaving the grants as they are: kinds are no permission, so a
/// rebuilt dog's follow its manifest with no question. A binary with no
/// manifest declares none, so the kinds an earlier build held are cleared. A
/// dog with no record is left alone.
///
/// # Errors
/// As [`ShepToml::set_grant_record`].
fn follow_declared_events(
    cfg: &mut ShepToml,
    name: &str,
    reviewed: &Review,
) -> Result<(), ShepTomlError> {
    let events = reviewed.events();
    match cfg.grant_record(name)? {
        Some(record) if record.events != events => {
            cfg.set_grant_record(name, &GrantRecord { events, ..record })
        }
        _ => Ok(()),
    }
}

/// Resolves `raw`, `shep adopt`'s own path argument, before it reaches
/// [`vet_binary`]: as given, with a leading `~/` expanded against `home`,
/// then looked up on `path_var`. A step moves on only when its lookup
/// proves the path absent, so a failed lookup counts as a hit.
///
/// `home` and `path_var` are parameters, read once by [`adopt`], so this
/// stays a pure function of its inputs: a test cannot reach `std::env::set_var`.
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
pub(super) fn lookup_on_path(name: &Path, path_var: Option<&OsStr>) -> Option<PathBuf> {
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
/// writes `foo.exe`, never `foo`. The bare name is tried first.
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
/// a verb added later is refused automatically. The tree is built first:
/// clap generates its `help` subcommand at build time.
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

/// [`emit_notice`](crate::output::emit_notice) code for a dog that asked for a
/// pane with no channel to answer it on; not a failure.
const DOG_PANE_WITHOUT_CHANNEL_NOTICE: &str = "dog_pane_without_channel";

/// Tells the operator that `name` asked for a pane and will not have one.
///
/// Not a refusal the way `validate` without a channel is: a pane is a screen
/// the operator loses, and a dog that never asked for the channel runs exactly
/// as it did. The one who wrote the dog is the one who can fix it.
fn warn_of_pane_without_channel(streams: &mut Streams<'_>, name: &str, manifest: &Manifest) {
    if !manifest.pane_ignored {
        return;
    }
    let message = format!(
        "{name}'s manifest asks for a pane, but the dog never asked for the shepherd channel to \
         answer it on, so it has none"
    );
    streams.aside(DOG_PANE_WITHOUT_CHANNEL_NOTICE, &message);
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
/// what `client: None` means. `source` is the adopted source with the
/// binary's own channel ask and the grants `shep.toml` records, and `env` the
/// `env` that entry holds after the adopt wrote it, which a re-adopt keeps.
async fn adopt_after_config(
    streams: &mut Streams<'_>,
    verb: &str,
    name: &str,
    source: DogSource,
    env: &BTreeMap<String, String>,
    client: Option<&Client>,
    restart: bool,
) -> ExitCode {
    let Some(client) = client else {
        return emit_dog_action(streams, verb, name, source, NO_SHEPHERD_ENABLE_STATUS);
    };
    // `EnableDog` leaves a dog that is running as it is, so a new binary needs
    // the old process stopped first. The config already names the new one.
    if restart {
        match client
            .request(Request::DisableDog {
                name: name.to_string(),
            })
            .await
        {
            Ok(_) => {}
            // Nothing was running to stop.
            Err(RequestError::Rpc(rpc)) if rpc.code == RpcErrorCode::NotFound => {}
            Err(err) => return client_error(streams, &err),
        }
    }
    let request = Request::EnableDog {
        name: name.to_string(),
        source: source.clone(),
        env: env
            .iter()
            .map(|(key, value)| (key.clone(), EnvValue::from(value.clone())))
            .collect(),
    };
    match client.request(request).await {
        Ok(Response::DogStarted(info)) => {
            warn_if_unenforced(streams, name, &source, client.daemon().protocol);
            emit_dog_action(
                streams,
                verb,
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
mod tests;
