//! `shep copy`: one setting, one dog's whole table, or a sheep's env, from one
//! sheep onto another.
//!
//! `Request::ListFlock` resolves the two names, `Request::SheepConfig` reads
//! each side, and the plan turns into [`Request::SetSheepField`] or
//! [`Request::SetSheepDogSettings`], so everything those refuse this refuses.
//! `env` is the exception: a client is never sent env values, so [`mod@env`]
//! plans and writes it through [`Request::CopySheepEnv`].
//!
//! Nothing is sent until the operator has seen every key the copy touches.

pub(crate) mod address;
pub(crate) mod confirm;
pub(crate) mod env;
pub(crate) mod plan;
pub(crate) mod report;

use std::fmt;
use std::io::IsTerminal as _;

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{Request, Response};

use crate::cli::{CopyArgs, Format};
use crate::commands::rpc::{client_error, typed_request_error, unexpected_response};
use crate::commands::settings_write;
use crate::dog_settings::{AskFailure, ask_dog};
use crate::exit::ExitCode;
use crate::output::Streams;

use address::{Address, ConfigPath, FlockNames};
use confirm::{Answer, Context, Go, confirm, decide};
use plan::{Plan, Write};
use report::{ReportState, finish, print_changes, report_for, shows_changes_first};

/// Why a copy stopped before it wrote anything.
///
/// Every variant carries the sentence the operator reads, because the reason
/// is the remedy: an instance count names `shep stock`, and a missing source names the path that is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CopyRefusal {
    /// An address or a flag is malformed, or the two sides name different
    /// paths, or the same sheep.
    Usage(String),
    /// No sheep in this flock answers that name.
    UnknownSheep(String),
    /// The source sheep has no table, or no key, at the path named.
    MissingSource(String),
    /// A key this verb never copies, in the reason the shepherd gives for it.
    Refused(String),
    /// The dog refused the table the copy would write, in its own words.
    ///
    /// Separate from [`Self::Refused`]: that is the shepherd refusing a key this
    /// verb never carries, and this is the dog refusing what it was asked about.
    ByDog(String),
}

impl CopyRefusal {
    /// The sentence the operator reads.
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Usage(message)
            | Self::UnknownSheep(message)
            | Self::MissingSource(message)
            | Self::Refused(message)
            | Self::ByDog(message) => message,
        }
    }

    /// The code that reports it.
    ///
    /// [`ExitCode::InvalidConfig`] for a refused key is the code the shepherd's
    /// own refusal of that key carries, so `copy` and the request it stands in
    /// for exit alike.
    ///
    /// `pub(crate)`: `shep set` refuses the same keys with the same reasons.
    pub(crate) const fn code(&self) -> ExitCode {
        match self {
            Self::Usage(_) => ExitCode::Usage,
            Self::UnknownSheep(_) | Self::MissingSource(_) => ExitCode::NotFound,
            Self::Refused(_) | Self::ByDog(_) => ExitCode::InvalidConfig,
        }
    }

    /// Writes the refusal and hands back the code it wrote.
    ///
    /// `pub(crate)`: see [`Self::code`].
    pub(crate) fn report(&self, streams: &mut Streams<'_>) -> ExitCode {
        streams.fail(self.code(), self.message())
    }
}

impl fmt::Display for CopyRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl core::error::Error for CopyRefusal {}

/// `shep copy <from> <to>`.
///
/// `paths` is this machine's `$SHEP_HOME`, which is where a dog's cached
/// manifest says whether the copy has to ask that dog first.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`: a malformed address, a name
/// no sheep answers, a key this verb refuses, a missing source, a refusal to
/// write with no terminal to ask on, a dog refusing its own table, or a write
/// the shepherd refused.
pub(crate) async fn copy(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &CopyArgs,
) -> ExitCode {
    let flock = match flock_names(client, streams).await {
        Ok(names) => names,
        Err(code) => return code,
    };
    // Before any sheep name, and against every dog the shepherd knows: a
    // defaults address belongs to a dog whether or not a sheep runs it.
    for raw in [&args.from, &args.to] {
        let segments: Vec<String> = raw.split('.').map(str::to_string).collect();
        if let Some(defaults) = settings_write::defaults_in(&segments, &flock, paths) {
            return address::defaults_refusal(raw, &defaults).report(streams);
        }
    }
    let from = match address::written(&args.from, &flock) {
        Ok(written) => written,
        Err(refusal) => return refusal.report(streams),
    };
    let to = match address::written(&args.to, &flock) {
        Ok(written) => written,
        Err(refusal) => return refusal.report(streams),
    };
    if let Some(refusal) = same_sheep(&from.sheep, &to.sheep) {
        return refusal.report(streams);
    }
    let except = match plan::except_paths(&args.except) {
        Ok(paths) => paths,
        Err(refusal) => return refusal.report(streams),
    };

    let source = match read_config(client, streams, &from.sheep).await {
        Ok(config) => config,
        Err(code) => return code,
    };
    let target = match read_config(client, streams, &to.sheep).await {
        Ok(config) => config,
        Err(code) => return code,
    };

    // The path half is read after the two configs, because a dog name and a
    // key inside a table may each contain a dot, and only the tables say where
    // one ends.
    let dogs = address::DogNames::new(&flock, &source, &target);
    let from = match address::path_of(&from, &dogs, &source, &target) {
        Ok(path) => Address {
            sheep: from.sheep.clone(),
            path,
        },
        Err(refusal) => return refusal.report(streams),
    };
    let to = match address::path_of(&to, &dogs, &source, &target) {
        Ok(path) => Address {
            sheep: to.sheep.clone(),
            path,
        },
        Err(refusal) => return refusal.report(streams),
    };
    if let Err(refusal) = same_pair(&from, &to) {
        return refusal.report(streams);
    }

    let plan = if from.path == ConfigPath::Env {
        let sides = plan::Sides {
            source: &from.sheep,
            target: &to.sheep,
        };
        match env::plan_env(client, streams, sides, &args.except, args.replace).await {
            Ok(plan) => plan,
            Err(code) => return code,
        }
    } else {
        match build_plan(&from, &to, &except, args.replace, &source, &target) {
            Ok(plan) => plan,
            Err(refusal) => return refusal.report(streams),
        }
    };

    let path = from.path.dotted();
    let mut report = report_for(&from, &to, &plan, ReportState::preview(args.dry_run));

    if plan.changes.is_empty() {
        let message = format!(
            "nothing to copy: `{}` already holds the same value at `{path}`",
            to.sheep
        );
        streams.note("copy", &message);
        return finish(streams, &report);
    }

    let go = decide(Context {
        dry_run: args.dry_run,
        yes: args.yes,
        stdin_is_terminal: std::io::stdin().is_terminal(),
        has_changes: !plan.changes.is_empty(),
    });

    // The change list prints before anything is asked about it, in every
    // form. Where it prints depends on what that form's stdout carries: the
    // table form's stdout is the change list, and the JSON form's is one
    // envelope that can only be written after the write the question gates.
    if shows_changes_first(streams.fmt, go)
        && let Err(code) = print_changes(streams, &report)
    {
        return code;
    }

    if go == Go::Refuse {
        return streams.fail(
            ExitCode::Usage,
            "nothing was written: `shep copy` asks before it writes, and this run has no \
             terminal to ask on. Pass --yes to write without asking, or --dry-run to see the \
             changes and write nothing",
        );
    }
    if go == Go::Ask {
        let mut reader = std::io::stdin().lock();
        match confirm(
            streams,
            &from.sheep,
            &to.sheep,
            &path,
            plan.changes.len(),
            &mut reader,
        ) {
            Answer::Yes => {}
            Answer::No => {
                streams.note("copy", "nothing was written");
                return finish(streams, &report);
            }
            Answer::Failed(reason) => {
                return streams.fail(
                    ExitCode::Failure,
                    &format!("nothing was written: the question could not be read ({reason})"),
                );
            }
        }
    }
    if go == Go::DryRun {
        streams.note("dry_run", "nothing was written");
        return finish(streams, &report);
    }

    let Some(write) = plan.write.clone() else {
        let message = format!("the plan named {} changes and no write", plan.changes.len());
        return streams.fail(ExitCode::Internal, &message);
    };
    let pending = match write_plan(client, streams, paths, &to.sheep, write).await {
        Ok(pending) => pending,
        Err(code) => return code,
    };

    report = report_for(&from, &to, &plan, ReportState::Written);
    if !pending.is_empty() {
        let message = format!(
            "{} waiting on the next spawn (`shep reload {}` promotes them)",
            pending.join(", "),
            to.sheep
        );
        streams.note("parked", &message);
    }
    report.pending = pending;
    if streams.fmt == Format::Table {
        let message = format!(
            "wrote {} change{} onto `{}`",
            plan.changes.len(),
            if plan.changes.len() == 1 { "" } else { "s" },
            to.sheep
        );
        streams.note("written", &message);
    }
    finish(streams, &report)
}

/// The flock's names, split into sheep and dogs.
///
/// `pub(crate)`: `shep get` resolves an address against the same two name
/// sets `shep copy` does, and one reader of `Request::ListFlock` is what
/// keeps the two verbs agreeing on what a name is.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
pub(crate) async fn flock_names(
    client: &Client,
    streams: &mut Streams<'_>,
) -> Result<FlockNames, ExitCode> {
    match client.list_flock().await {
        Ok(info) => Ok(FlockNames {
            sheep: info
                .iter()
                .filter(|info| info.dog.is_none())
                .map(|info| info.name.clone())
                .collect(),
            dogs: info
                .iter()
                .filter(|info| info.dog.is_some())
                .map(|info| info.name.clone())
                .collect(),
        }),
        Err(err) => Err(typed_request_error(streams, &err)),
    }
}

/// One sheep's config as JSON, which is the shape a plan reads.
///
/// `env` is emptied by the shepherd: the values of a copy's env never reach
/// this config, and [`mod@env`] asks the shepherd to do that copy.
///
/// `pub(crate)`: `shep get` reads the same answer for the sheep half of an
/// address.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
pub(crate) async fn read_config(
    client: &Client,
    streams: &mut Streams<'_>,
    name: &str,
) -> Result<serde_json::Value, ExitCode> {
    match client
        .request(Request::SheepConfig {
            name: name.to_string(),
        })
        .await
    {
        Ok(Response::SheepConfig(view)) => serde_json::to_value(&view.config).map_err(|_| {
            streams.fail(
                ExitCode::Internal,
                "the shepherd's config answer did not serialize, so nothing was copied",
            )
        }),
        Ok(_other) => Err(unexpected_response(streams)),
        Err(err) => Err(client_error(streams, &err)),
    }
}

/// The refusal for one sheep named on both sides.
fn same_sheep(from: &str, to: &str) -> Option<CopyRefusal> {
    (from == to).then(|| {
        CopyRefusal::Usage(format!(
            "`{from}` is on both sides: a copy needs two different sheep"
        ))
    })
}

/// The two sides of one copy: two different sheep, and one path between them.
///
/// # Errors
/// [`CopyRefusal::Usage`] for a path that differs, or for a sheep named on
/// both sides.
fn same_pair(from: &Address, to: &Address) -> Result<(), CopyRefusal> {
    if from.path != to.path {
        return Err(CopyRefusal::Usage(format!(
            "the path must be the same on both sides: `{}.{}` and `{}.{}`",
            from.sheep,
            from.path.dotted(),
            to.sheep,
            to.path.dotted()
        )));
    }
    if let Some(refusal) = same_sheep(&from.sheep, &to.sheep) {
        return Err(refusal);
    }
    Ok(())
}

/// The plan for one copy.
///
/// # Errors
/// [`CopyRefusal::Usage`] for `--except` on a field, and whatever
/// [`plan::plan_dog`] refuses about the source.
fn build_plan(
    from: &Address,
    to: &Address,
    except: &[Vec<String>],
    replace: bool,
    source: &serde_json::Value,
    target: &serde_json::Value,
) -> Result<Plan, CopyRefusal> {
    match &from.path {
        ConfigPath::Field { name } => {
            if !except.is_empty() {
                return Err(CopyRefusal::Usage(format!(
                    "--except names a key inside a dog's table: `{name}` is one field, and a \
                     copy takes a field whole"
                )));
            }
            Ok(plan::plan_field(name, source, target))
        }
        ConfigPath::Env => Err(CopyRefusal::Usage(
            "the env of a sheep is planned by the shepherd, not from the configs read here"
                .to_string(),
        )),
        ConfigPath::Dog { dog, keys } => plan::plan_dog(
            plan::Sides {
                source: &from.sheep,
                target: &to.sheep,
            },
            dog,
            keys,
            except,
            replace,
            source,
            target,
        ),
    }
}

/// Sends the plan's write, and reports which keys wait for a respawn.
///
/// A write that carries a dog's whole table is put past [`ask_dog`] first, the
/// way `shep set` puts one: the copy replaces a dog's table on the target, and a
/// dog that declared `validate` is shown the table it would end up with before
/// anything is written.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
async fn write_plan(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    target: &str,
    write: Write,
) -> Result<Vec<String>, ExitCode> {
    match write {
        Write::Field { name, value } => {
            let request = Request::SetSheepField {
                name: target.to_string(),
                key: name.clone(),
                value,
            };
            match client.request(request).await {
                Ok(Response::SheepFieldSet {
                    pending, warning, ..
                }) => {
                    if let Some(warning) = warning {
                        streams.aside("warning", &warning);
                    }
                    Ok(pending.then_some(name).into_iter().collect())
                }
                Ok(_other) => Err(unexpected_response(streams)),
                Err(err) => Err(client_error(streams, &err)),
            }
        }
        Write::Env {
            from,
            replace,
            except,
            expect,
        } => env::write_env(client, streams, target, &from, replace, &except, expect).await,
        Write::DogTable { dog, table } => {
            match ask_dog(client, paths, &dog, table.as_map(), Some(target)).await {
                Ok(()) => {}
                Err(AskFailure::Refused { reason }) => {
                    return Err(CopyRefusal::ByDog(format!(
                        "nothing was written: `{dog}` refused `{target}`'s table for `{dog}`: \
                         {reason}"
                    ))
                    .report(streams));
                }
                // No dog said this one: shep could not read whether the dog
                // wanted to be asked, or could not read what it answered.
                Err(AskFailure::Unknown { reason }) => {
                    return Err(
                        CopyRefusal::Refused(format!("nothing was written: {reason}"))
                            .report(streams),
                    );
                }
                Err(AskFailure::Transport(err)) => return Err(client_error(streams, &err)),
            }
            let request = Request::SetSheepDogSettings {
                name: target.to_string(),
                dog,
                table: Some(table),
            };
            match client.request(request).await {
                Ok(Response::SheepDogSettingsSet { .. }) => Ok(Vec::new()),
                Ok(_other) => Err(unexpected_response(streams)),
                Err(err) => Err(client_error(streams, &err)),
            }
        }
    }
}

#[cfg(test)]
mod validate_tests;
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::commands::copy::plan::ChangeKind;

    /// An address, resolved with no config in hand.
    fn address(raw: &str) -> Address {
        let written = address::written(raw, &flock()).unwrap();
        let dogs =
            address::DogNames::new(&flock(), &serde_json::Value::Null, &serde_json::Value::Null);
        Address {
            sheep: written.sheep.clone(),
            path: address::path_of(
                &written,
                &dogs,
                &serde_json::Value::Null,
                &serde_json::Value::Null,
            )
            .unwrap(),
        }
    }

    /// The flock every test here resolves against.
    fn flock() -> FlockNames {
        FlockNames {
            sheep: ["shep", "shep-kelpie", "web", "api", "we.b"]
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            dogs: ["metrics", "foo.bar"]
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        }
    }

    /// One sheep's config, holding one dog table built from `table`.
    fn config_with_dog(dog: &str, table: serde_json::Value) -> serde_json::Value {
        json!({ "name": "web", "dogs": { dog: table } })
    }

    /// The pure half of a copy run: resolve, check the pair, plan.
    fn resolve(
        from: &str,
        to: &str,
        source: serde_json::Value,
        target: serde_json::Value,
        except: &[String],
        replace: bool,
    ) -> Result<Plan, CopyRefusal> {
        let flock = flock();
        let from = address::written(from, &flock)?;
        let to = address::written(to, &flock)?;
        let dogs = address::DogNames::new(&flock, &source, &target);
        let from = Address {
            sheep: from.sheep.clone(),
            path: address::path_of(&from, &dogs, &source, &target)?,
        };
        let to = Address {
            sheep: to.sheep.clone(),
            path: address::path_of(&to, &dogs, &source, &target)?,
        };
        same_pair(&from, &to)?;
        let except = plan::except_paths(except)?;
        build_plan(&from, &to, &except, replace, &source, &target)
    }

    /// The table the plan would leave on the target, for a dog copy.
    fn written_table(plan: &Plan) -> serde_json::Map<String, serde_json::Value> {
        let Write::DogTable { table, .. } = plan.write.clone().expect("a dog copy writes") else {
            panic!("a dog copy writes a dog table");
        };
        table.into_map()
    }

    #[test]
    fn a_dog_sub_table_merges_onto_the_target_and_keeps_its_other_keys() {
        let source = config_with_dog(
            "kelpie",
            json!({ "rulings": { "decide": "panel", "report": "brief" }, "git": { "remote": "origin" } }),
        );
        let target = config_with_dog(
            "kelpie",
            json!({ "rulings": { "decide": "issue", "own": "keep" }, "watch": true }),
        );
        let plan = resolve(
            "shep.dogs.kelpie.rulings",
            "shep-kelpie.dogs.kelpie.rulings",
            source,
            target,
            &[],
            false,
        )
        .expect("the source has this table");

        let kinds: Vec<(&str, &str)> = plan
            .changes
            .iter()
            .map(|change| (change.key.as_str(), change.kind.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [("decide", "changed"), ("report", "added"),],
            "the copied sub-table is what is judged, and a key only the target has inside it stays"
        );

        let table = written_table(&plan);
        assert_eq!(
            table["rulings"],
            json!({ "decide": "panel", "own": "keep", "report": "brief" }),
            "the sub-table merges: the source's keys overwrite, the target's own key stays"
        );
        assert_eq!(table["watch"], json!(true), "a key outside the path stays");
        assert!(
            !table.contains_key("git"),
            "the source's `git` sits outside the copied path, so it is not copied"
        );
    }

    #[test]
    fn replace_makes_the_target_table_exactly_the_source() {
        let source = config_with_dog("kelpie", json!({ "rulings": { "decide": "panel" } }));
        let target = config_with_dog(
            "kelpie",
            json!({ "rulings": { "decide": "issue", "own": "gone" }, "watch": true }),
        );
        let plan = resolve(
            "shep.dogs.kelpie",
            "shep-kelpie.dogs.kelpie",
            source,
            target,
            &[],
            true,
        )
        .expect("the source has this table");

        let kinds: Vec<(&str, &str)> = plan
            .changes
            .iter()
            .map(|change| (change.key.as_str(), change.kind.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("rulings.decide", "changed"),
                ("rulings.own", "removed"),
                ("watch", "removed"),
            ]
        );
        assert_eq!(
            serde_json::Value::Object(written_table(&plan)),
            json!({ "rulings": { "decide": "panel" } })
        );
    }

    #[test]
    fn an_excepted_key_is_left_as_the_target_had_it() {
        let source = config_with_dog(
            "kelpie",
            json!({ "git": { "checkout": "./src", "remote": "upstream" }, "rulings": { "decide": "panel" } }),
        );
        let target = config_with_dog(
            "kelpie",
            json!({ "git": { "checkout": "./mine", "branch": "main" } }),
        );
        let plan = resolve(
            "shep.dogs.kelpie",
            "shep-kelpie.dogs.kelpie",
            source,
            target,
            &["git.checkout".to_string()],
            false,
        )
        .expect("the source has this table");

        let keys: Vec<&str> = plan.changes.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["git.remote", "rulings.decide"]);
        assert_eq!(
            written_table(&plan)["git"],
            json!({ "branch": "main", "checkout": "./mine", "remote": "upstream" }),
            "`git.checkout` is the target's own value, and `git.branch` was never in question"
        );
    }

    #[test]
    fn a_field_copies_and_the_refused_fields_say_why() {
        let source = json!({ "name": "web", "restart_delay": "3s", "env": {} });
        let target = json!({ "name": "api", "restart_delay": "1s", "env": {} });
        let plan = resolve(
            "web.restart_delay",
            "api.restart_delay",
            source.clone(),
            target.clone(),
            &[],
            false,
        )
        .expect("restart_delay is a field");
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].kind, ChangeKind::Changed);
        assert_eq!(plan.changes[0].old, Some(json!("1s")));
        assert_eq!(
            plan.write,
            Some(Write::Field {
                name: "restart_delay".to_string(),
                value: json!("3s"),
            }),
            "the write is the request the shepherd already answers"
        );

        for (path, expected) in [
            ("web.name", "different sheep"),
            ("web.instances", "shep stock web"),
            ("web.dogs", "web.dogs.<dog>"),
        ] {
            let err = resolve(
                path,
                &path.replace("web.", "api."),
                source.clone(),
                target.clone(),
                &[],
                false,
            )
            .expect_err("{path} is refused");
            assert!(err.message().contains(expected), "{path}: {err}");
        }
    }

    #[test]
    fn different_paths_on_the_two_sides_are_refused() {
        let source = config_with_dog("kelpie", json!({ "rulings": { "decide": "panel" } }));
        let target = config_with_dog("jobs", json!({ "rulings": { "decide": "issue" } }));
        let err = resolve(
            "web.dogs.kelpie.rulings",
            "api.dogs.jobs.rulings",
            source.clone(),
            target.clone(),
            &[],
            false,
        )
        .expect_err("a dog name is part of the path");
        assert_eq!(
            err,
            CopyRefusal::Usage(
                "the path must be the same on both sides: `web.dogs.kelpie.rulings` and \
                 `api.dogs.jobs.rulings`"
                    .to_string()
            )
        );

        let err = resolve(
            "web.restart_delay",
            "api.kill_timeout",
            source,
            target,
            &[],
            false,
        )
        .expect_err("two fields are two paths");
        assert!(err.message().contains("the path must be the same"), "{err}");
    }

    #[test]
    fn dry_run_writes_nothing_and_a_pipe_needs_yes() {
        assert_eq!(
            decide(Context {
                dry_run: true,
                yes: false,
                stdin_is_terminal: false,
                has_changes: true
            }),
            Go::DryRun
        );
        assert_eq!(
            decide(Context {
                dry_run: true,
                yes: true,
                stdin_is_terminal: true,
                has_changes: true
            }),
            Go::DryRun
        );
        assert_eq!(
            decide(Context {
                dry_run: false,
                yes: false,
                stdin_is_terminal: false,
                has_changes: true
            }),
            Go::Refuse
        );
        assert_eq!(
            decide(Context {
                dry_run: false,
                yes: true,
                stdin_is_terminal: false,
                has_changes: true
            }),
            Go::Write
        );
        assert_eq!(
            decide(Context {
                dry_run: false,
                yes: false,
                stdin_is_terminal: true,
                has_changes: true
            }),
            Go::Ask
        );
        assert_eq!(
            decide(Context {
                dry_run: false,
                yes: false,
                stdin_is_terminal: true,
                has_changes: false
            }),
            Go::Nothing
        );

        let source = config_with_dog("kelpie", json!({ "rulings": { "decide": "panel" } }));
        let target = config_with_dog("kelpie", json!({ "rulings": { "decide": "issue" } }));
        let plan = resolve(
            "web.dogs.kelpie",
            "api.dogs.kelpie",
            source,
            target,
            &[],
            false,
        )
        .expect("the source has this table");

        let dry = report_for(
            &address("web.dogs.kelpie"),
            &address("api.dogs.kelpie"),
            &plan,
            ReportState::DryRun,
        );
        assert!(dry.dry_run);
        assert!(
            !dry.written,
            "a dry run reports no write, whatever the plan holds"
        );
        assert_eq!(dry.changes.len(), 1);
        assert_ne!(
            decide(Context {
                dry_run: true,
                yes: false,
                stdin_is_terminal: false,
                has_changes: true
            }),
            Go::Write
        );
        assert_ne!(
            decide(Context {
                dry_run: false,
                yes: false,
                stdin_is_terminal: false,
                has_changes: true
            }),
            Go::Write
        );
    }

    #[test]
    fn a_missing_source_is_named() {
        let source = config_with_dog("kelpie", json!({ "rulings": { "decide": "panel" } }));
        let target = config_with_dog("kelpie", json!({ "rulings": { "decide": "issue" } }));

        let err = resolve(
            "web.dogs.jobs",
            "api.dogs.jobs",
            source.clone(),
            target.clone(),
            &[],
            false,
        )
        .expect_err("no table for `jobs`");
        assert_eq!(
            err,
            CopyRefusal::MissingSource(
                "no `web.dogs.jobs`: that sheep has no table for the dog `jobs`".to_string()
            )
        );

        let err = resolve(
            "web.dogs.kelpie.reporting",
            "api.dogs.kelpie.reporting",
            source.clone(),
            target.clone(),
            &[],
            false,
        )
        .expect_err("no key `reporting`");
        assert_eq!(
            err,
            CopyRefusal::MissingSource("no `reporting` under `web.dogs.kelpie`".to_string())
        );

        let err = resolve(
            "nope.dogs.kelpie",
            "api.dogs.kelpie",
            source,
            target,
            &[],
            false,
        )
        .expect_err("no sheep `nope`");
        assert_eq!(
            err,
            CopyRefusal::UnknownSheep(
                "no sheep named `nope`; `shep flock` lists the flock".to_string()
            )
        );
    }

    #[test]
    fn a_sheep_name_with_a_dot_resolves() {
        let source = config_with_dog("kelpie", json!({ "rulings": { "decide": "panel" } }));
        let target = config_with_dog("kelpie", json!({ "rulings": { "decide": "issue" } }));
        let plan = resolve(
            "we.b.dogs.kelpie.rulings",
            "api.dogs.kelpie.rulings",
            source,
            target,
            &[],
            false,
        )
        .expect("`we.b` is a sheep name, dots and all");
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].key, "decide");
    }

    /// A copy onto itself cannot differ, and saying so beats an empty table.
    #[test]
    fn the_same_sheep_on_both_sides_is_refused() {
        let err = same_pair(&address("web.dogs.kelpie"), &address("web.dogs.kelpie"))
            .expect_err("one sheep is not a copy");
        assert_eq!(
            err,
            CopyRefusal::Usage(
                "`web` is on both sides: a copy needs two different sheep".to_string()
            )
        );
    }

    /// `--except` names a key inside a dog's table, and a field is not one.
    #[test]
    fn except_on_a_field_is_refused() {
        let err = resolve(
            "web.restart_delay",
            "api.restart_delay",
            json!({ "restart_delay": "3s" }),
            json!({ "restart_delay": "1s" }),
            &["git.checkout".to_string()],
            false,
        )
        .expect_err("a field has no keys to leave out");
        assert!(err.message().contains("one field"), "{err}");
    }

    /// A flock running a dog named `foo.bar` does not stop a copy of the key
    /// `bar` under the dog `foo`, which is what the address was naming when no
    /// sheep has a table for the flock's dotted name.
    #[test]
    fn a_flock_dog_name_does_not_hide_a_key_under_a_dog_it_prefixes() {
        let source = config_with_dog("foo", json!({ "bar": { "enabled": true } }));
        let target = config_with_dog("foo", json!({ "keep": 1 }));
        let plan = resolve(
            "web.dogs.foo.bar",
            "api.dogs.foo.bar",
            source,
            target,
            &[],
            false,
        )
        .expect("the table reading wins over the flock's dog name");
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(
            plan.changes[0].key, "enabled",
            "a sub-table copy names its keys under the path it copied"
        );
        let table = written_table(&plan);
        assert_eq!(table["bar"], json!({ "enabled": true }));
        assert_eq!(
            table["keep"],
            json!(1),
            "the target's other key under `foo` survives"
        );
    }
}
