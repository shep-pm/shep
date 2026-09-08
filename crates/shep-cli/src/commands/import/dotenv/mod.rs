//! `shep import env`: reading a `.env` into shep's stores.
//!
//! [`parse`] is the grammar. [`plan`] decides which store each key is bound
//! for and whether shep can hold it. This module does the I/O, in one order:
//!
//! 1. read and parse, then plan. Nothing written.
//! 2. `Request::SheepConfig`, for the environment the secrets go in. Skipped
//!    when `--env` names one, so this step is not what proves the sheep
//!    exists.
//! 3. `SetSheepEnvBatch` with `dry_run`, so the daemon names env collisions
//!    against the values it holds and this process never sees them. This is
//!    the step that refuses an unknown sheep and a dog, on either path into
//!    it, before either store is written.
//! 4. secret-store collisions, decided here, since this process holds those
//!    values.
//! 5. any collision without `--force`: name them all and exit, writing
//!    nothing.
//! 6. write `secrets.json`, then send the batch for real. The daemon decides
//!    collisions afresh, so the real send can still refuse what step 3 said
//!    it would take; that refusal is reported like step 5's, and says the
//!    secret store is already written.
//!
//! Secrets first and the batch second on purpose. A failure at the last step
//! leaves values in the store that nothing references, which are inert, and
//! the re-run is clean because an identical value is not a collision. The
//! reverse order leaves `{{secret:KEY}}` pointing at nothing and the re-run
//! then needs `--force`.

pub(crate) mod parse;
pub(crate) mod plan;

use std::collections::BTreeMap;

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{EnvValue, Request, Response};
use shep_core::secrets;

use plan::{Class, ImportPlan, Planned};

use crate::cli::ImportEnvArgs;
use crate::commands::secret::{daemon_config, exit_code_for};
use crate::exit::ExitCode;
use crate::output::{ImportEnvRow, ImportEnvRows, Streams, emit, write_outcome};

/// The `store` cell, and the word each collision line names.
const SECRET_STORE: &str = "secret";
/// The other one. An env key has no environment slot, so its `slot` is `-`.
const ENV_STORE: &str = "env";

/// Reads `args.file` into the secret store and `args.app`'s own env.
///
/// The module doc has the order and why it is that order. Nothing here
/// prints a value: a row carries a byte length, and no refusal names
/// anything but a key, a store and a path.
///
/// `"import"` is the envelope's command for this verb as much as for
/// `shep import pm2`: the noun names the command and the `data` shape
/// varies, which is the rule `shep secret`'s four subcommands already
/// follow.
pub async fn import_env(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &ImportEnvArgs,
) -> ExitCode {
    let text = match std::fs::read_to_string(&args.file) {
        Ok(text) => text,
        Err(err) => {
            let message = format!("could not read {}: {err}", args.file.display());
            return streams.fail(ExitCode::Usage, &message);
        }
    };
    let entries = match parse::parse(&text) {
        Ok(entries) => entries,
        // The file on disk is the problem, not what the operator typed.
        Err(err) => return streams.fail(ExitCode::InvalidConfig, &err.to_string()),
    };
    let plan = match plan::build(entries, &args.only, &args.secret) {
        Ok(plan) => plan,
        // The pattern is the operator's, so this one is theirs to fix.
        Err(err) => return streams.fail(ExitCode::Usage, &err.to_string()),
    };

    let environment = match resolve_environment(client, streams, paths, args).await {
        Ok(environment) => environment,
        Err(code) => return code,
    };

    let wire = wire_entries(&plan);
    let env_collisions = match probe_env(client, streams, args, &wire).await {
        Ok(collisions) => collisions,
        Err(code) => return code,
    };
    let secret_collisions = match probe_secrets(streams, paths, &plan, &environment) {
        Ok(collisions) => collisions,
        Err(code) => return code,
    };

    if !args.force {
        let refused = report_collisions(streams, &env_collisions, &secret_collisions);
        if refused != ExitCode::Success {
            return refused;
        }
    }

    if !plan.unnamed.is_empty() {
        let message = format!(
            "stored in the clear because no --secret named them: {}",
            plan.unnamed.join(", ")
        );
        streams.aside("secretish", &message);
    }

    if args.dry_run {
        return emit_rows(streams, &plan, &environment);
    }

    let mut written = 0_usize;
    for planned in &plan.entries {
        if planned.class == Class::Secret {
            if let Err(err) =
                secrets::set(&paths.secrets, &planned.key, &environment, &planned.value)
            {
                let message = format!("{err}{}", already_written(written));
                return streams.fail(exit_code_for(&err), &message);
            }
            written += 1;
        }
    }

    let request = Request::SetSheepEnvBatch {
        name: args.app.clone(),
        entries: wire,
        force: args.force,
        dry_run: false,
    };
    match client.request(request).await {
        Ok(Response::SheepEnvBatch { collisions, .. })
            if is_late_refusal(args.force, &collisions) =>
        {
            return report_late_collisions(streams, &collisions, written);
        }
        Ok(Response::SheepEnvBatch { .. }) => {}
        // Both of these land after the secret store is written, so both say
        // so: the operator is otherwise told a batch failed and left with no
        // account of the other store.
        Ok(_) => {
            let message = format!("{UNDERSTOOD_NOTHING}{}", already_written(written));
            return streams.fail(ExitCode::Internal, &message);
        }
        Err(err) => {
            let message = format!("{err}{}", already_written(written));
            return streams.fail(ExitCode::from(&err), &message);
        }
    }

    let message = format!(
        "{}'s env is parked for its next spawn; `shep reload {}` promotes it",
        args.app, args.app
    );
    streams.aside("parked", &message);
    emit_rows(streams, &plan, &environment)
}

/// What a reply this build does not understand is reported as.
const UNDERSTOOD_NOTHING: &str =
    "the daemon answered with a response this client does not understand";

/// The environment whose slot the secrets go in: `--env`, else the sheep's
/// own `environment`, else `[daemon] environment`.
///
/// `--env` returns before the request, so `Request::SheepConfig` is not what
/// proves the sheep exists: on that path an unknown name and a dog are both
/// refused by the dry run at step 3, with the same exit codes and still
/// before either store is touched.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams.err`.
async fn resolve_environment(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &ImportEnvArgs,
) -> Result<String, ExitCode> {
    if let Some(environment) = &args.env {
        return Ok(environment.clone());
    }
    let request = Request::SheepConfig {
        name: args.app.clone(),
    };
    match client.request(request).await {
        Ok(Response::SheepConfig(view)) => Ok(view
            .config
            .environment
            .clone()
            .unwrap_or_else(|| daemon_config(paths).daemon.environment)),
        Ok(_) => Err(streams.fail(ExitCode::Internal, UNDERSTOOD_NOTHING)),
        Err(err) => Err(streams.fail(ExitCode::from(&err), &err.to_string())),
    }
}

/// What each key puts in the sheep's env: the value itself for a plain key,
/// a `{{secret:KEY}}` reference for a secret one.
///
/// The plain value is escaped and the reference is not: the reference is
/// shep's own token and is meant to resolve.
fn wire_entries(plan: &ImportPlan) -> BTreeMap<String, EnvValue> {
    plan.entries
        .iter()
        .map(|planned| {
            let value = match planned.class {
                Class::Secret => format!("{{{{secret:{}}}}}", planned.key),
                Class::Plain => escape_template(&planned.value),
            };
            (planned.key.clone(), EnvValue::from(value))
        })
        .collect()
}

/// A `.env` value written as the shep template that renders back to itself.
///
/// Every env value a sheep holds is read as a template
/// ([`shep_core::config::template`]): `{{world}}` is refused at config time
/// and `{{name}}` substitutes. A `.env` promises neither, so each brace pair
/// takes that grammar's own escape, doubling.
fn escape_template(value: &str) -> String {
    value.replace("{{", "{{{{").replace("}}", "}}}}")
}

/// The env keys already holding a different value, as the daemon sees them.
///
/// A dry run, so nothing is written whatever the answer is. The daemon
/// compares against values this process never receives.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams.err`.
async fn probe_env(
    client: &Client,
    streams: &mut Streams<'_>,
    args: &ImportEnvArgs,
    entries: &BTreeMap<String, EnvValue>,
) -> Result<Vec<String>, ExitCode> {
    let request = Request::SetSheepEnvBatch {
        name: args.app.clone(),
        entries: entries.clone(),
        force: args.force,
        dry_run: true,
    };
    match client.request(request).await {
        Ok(Response::SheepEnvBatch { collisions, .. }) => Ok(collisions),
        Ok(_) => Err(streams.fail(ExitCode::Internal, UNDERSTOOD_NOTHING)),
        Err(err) => Err(streams.fail(ExitCode::from(&err), &err.to_string())),
    }
}

/// The secret keys whose slot already holds a different value.
///
/// Decided here rather than by the daemon, because this process is the one
/// holding the file's values and the store is the CLI's own to read.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams.err`, for a store that
/// could not be read.
fn probe_secrets(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    plan: &ImportPlan,
    environment: &str,
) -> Result<Vec<String>, ExitCode> {
    let mut collisions = Vec::new();
    for planned in plan.entries.iter().filter(|e| e.class == Class::Secret) {
        match secrets::get(&paths.secrets, &planned.key, environment) {
            Ok(Some(existing)) if existing != planned.value => collisions.push(planned.key.clone()),
            Ok(_) => {}
            Err(err) => return Err(streams.fail(exit_code_for(&err), &err.to_string())),
        }
    }
    Ok(collisions)
}

/// Names every collision from both stores, one line each, and refuses.
///
/// [`ExitCode::Success`] when there is nothing to refuse. No value: a line
/// names the key and which store already holds something else.
fn report_collisions(
    streams: &mut Streams<'_>,
    env_collisions: &[String],
    secret_collisions: &[String],
) -> ExitCode {
    if env_collisions.is_empty() && secret_collisions.is_empty() {
        return ExitCode::Success;
    }
    name_collisions(streams, env_collisions, secret_collisions);
    streams.fail(
        ExitCode::Usage,
        "nothing was imported; pass --force to overwrite the keys named above",
    )
}

/// One `collision` aside per key, naming the key and the store that already
/// holds something else. Never a value.
fn name_collisions(
    streams: &mut Streams<'_>,
    env_collisions: &[String],
    secret_collisions: &[String],
) {
    for (keys, store) in [
        (env_collisions, ENV_STORE),
        (secret_collisions, SECRET_STORE),
    ] {
        for key in keys {
            let message = format!("`{key}` already holds a different value in the {store} store");
            streams.aside("collision", &message);
        }
    }
}

/// The env store's refusal of the real batch, after the dry run at step 3
/// said it would be taken.
///
/// The daemon decides collisions afresh on every call, so anything that
/// touched this sheep's env between the two requests lands here. By then the
/// secret store is written, which the message says: those values are inert
/// until the import is re-run, and a re-run is clean.
fn report_late_collisions(
    streams: &mut Streams<'_>,
    collisions: &[String],
    written: usize,
) -> ExitCode {
    name_collisions(streams, collisions, &[]);
    let message = format!(
        "the sheep's env changed since this run's dry run, so nothing was written to it{}. \
         Re-run the import, with --force to overwrite the keys named above",
        already_written(written)
    );
    streams.fail(ExitCode::Usage, &message)
}

/// Whether the real batch's answer is a refusal.
///
/// The daemon names collisions in the same field whether it refused them or
/// overwrote them, so `force` is what tells the two apart.
fn is_late_refusal(force: bool, collisions: &[String]) -> bool {
    !force && !collisions.is_empty()
}

/// The clause every failure after the secret write appends to its message.
///
/// Empty when the write had reached no key yet, since a note counting zero
/// keys reads as though it counted some. Counts and key names only, so it is
/// safe on any stream (IR-41).
fn already_written(written: usize) -> String {
    if written == 0 {
        return String::new();
    }
    let (keys, verb) = if written == 1 {
        ("key", "was")
    } else {
        ("keys", "were")
    };
    format!(
        "; {written} secret {keys} {verb} already written to the secret store and sit \
         unreferenced until the import is re-run. A re-run is clean, since an identical \
         value is not a collision"
    )
}

/// One row per key the plan holds, in the file's own order.
///
/// Every planned key gets a row, including one the store already held: the
/// row says where a key belongs, not whether this run was the one that put
/// it there. `bytes` is the `.env`'s value, so a secret's row reports the
/// stored value's length rather than the reference's.
fn emit_rows(streams: &mut Streams<'_>, plan: &ImportPlan, environment: &str) -> ExitCode {
    let rows = ImportEnvRows(
        plan.entries
            .iter()
            .map(|p| row_for(p, environment))
            .collect(),
    );
    write_outcome(emit(
        &mut *streams.out,
        streams.fmt,
        "import",
        rows,
        streams.style,
    ))
}

/// One [`Planned`] as its row. Never the value, only its length (IR-41).
fn row_for(planned: &Planned, environment: &str) -> ImportEnvRow {
    let (store, slot) = match planned.class {
        Class::Secret => (SECRET_STORE, environment),
        Class::Plain => (ENV_STORE, "-"),
    };
    ImportEnvRow {
        key: planned.key.clone(),
        store: store.to_string(),
        slot: slot.to_string(),
        bytes: planned.value.len(),
    }
}

#[cfg(test)]
mod tests {
    use shep_core::config::template::render_positional;

    use super::*;

    const SAMPLE: &str = "PORT=8080\nDB_PASSWORD=hunter2\n";

    fn sample_plan() -> ImportPlan {
        let entries = parse::parse(SAMPLE).expect("this fixture parses");
        plan::build(entries, &[], &["DB_PASSWORD".to_string()]).expect("this fixture plans")
    }

    /// fails if a secret's value ever goes on the wire in the clear: the
    /// sheep's env gets the reference, and the value goes to `secrets.json`
    /// down the other path.
    #[test]
    fn a_secret_reaches_the_sheep_as_a_reference_and_a_plain_key_as_itself() {
        let wire = wire_entries(&sample_plan());
        assert_eq!(wire["PORT"].as_str(), "8080");
        assert_eq!(wire["DB_PASSWORD"].as_str(), "{{secret:DB_PASSWORD}}");
    }

    /// fails if a `.env` value is ever read as a shep template: `{{world}}`
    /// is refused by `normalize` and `{{name}}` substitutes silently, and
    /// both must reach the child as the file wrote them. The secret arm's
    /// reference is shep's own token and stays live.
    #[test]
    fn a_plain_value_escapes_its_braces_and_a_secret_reference_stays_live() {
        const BRACES: &str = "MOTD=hello {{world}}\nGREETING={{name}}-prod\n                              JSON={\"a\":{\"b\":1}}\nDB_PASSWORD=hunter2\n";
        let entries = parse::parse(BRACES).expect("this fixture parses");
        let plan =
            plan::build(entries, &[], &["DB_PASSWORD".to_string()]).expect("this fixture plans");
        let wire = wire_entries(&plan);

        assert_eq!(wire["MOTD"].as_str(), "hello {{{{world}}}}");
        assert_eq!(wire["GREETING"].as_str(), "{{{{name}}}}-prod");
        assert_eq!(wire["DB_PASSWORD"].as_str(), "{{secret:DB_PASSWORD}}");

        // What the child gets, through the renderer the daemon runs.
        for planned in plan.entries.iter().filter(|p| p.class == Class::Plain) {
            assert_eq!(
                render_positional(wire[&planned.key].as_str(), "echoer", 0),
                planned.value,
                "{} was rewritten on its way to the child",
                planned.key
            );
        }
    }

    /// fails if a row ever grows a value. `bytes` is the `.env`'s own value
    /// for both stores, so a secret reports what was stored rather than the
    /// reference that replaced it.
    #[test]
    fn a_row_carries_a_length_and_never_a_value() {
        let plan = sample_plan();
        let rows: Vec<ImportEnvRow> = plan
            .entries
            .iter()
            .map(|planned| row_for(planned, "production"))
            .collect();
        assert_eq!(rows[0].store, "env");
        assert_eq!(rows[0].slot, "-");
        assert_eq!(rows[1].store, "secret");
        assert_eq!(rows[1].slot, "production");
        assert_eq!(rows[1].bytes, "hunter2".len());
        assert!(!format!("{rows:?}").contains("hunter2"), "{rows:?}");
    }

    /// fails if the note every failure after the secret write shares stops
    /// saying what it left behind, or stops counting it.
    #[test]
    fn the_already_written_note_counts_keys_and_says_a_re_run_is_clean() {
        assert!(already_written(1).starts_with("; 1 secret key was already written"));
        assert!(already_written(4).starts_with("; 4 secret keys were already written"));
        assert!(already_written(1).contains("A re-run is clean"));
    }

    /// fails if a failure before the first `secrets::set` ever claims the
    /// store holds something: "0 secret keys were already written" describes
    /// nothing and reads as if it described something.
    #[test]
    fn the_already_written_note_is_empty_when_nothing_was_written() {
        assert_eq!(already_written(0), "");
    }

    /// fails if the late guard is ever inverted, which would refuse every
    /// `--force` and take every collision.
    #[test]
    fn only_an_unforced_answer_naming_collisions_is_a_late_refusal() {
        let none: &[String] = &[];
        let some = &["PORT".to_string()];
        assert!(is_late_refusal(false, some));
        assert!(!is_late_refusal(true, some));
        assert!(!is_late_refusal(false, none));
        assert!(!is_late_refusal(true, none));
    }
}
