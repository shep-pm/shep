//! Folding a Flockfile declaration into a running spec.
//!
//! These are pure functions over config values, with no actor state and no
//! IO. `merge_declared` is the centre: it decides, field by field, what a
//! declared app changes on a sheep that is already registered, and what has
//! to wait for a restart. The rest support it or report what it decided.

use super::*;

/// The `AppConfig` fields a lifecycle extra reads when it is armed, rather
/// than at each decision.
///
/// All eight are [`ApplyGroup::Live`], but a worker already armed against the
/// old value keeps it, so changing one needs [`ExtrasRegistry::rearm_name`] on
/// top of the spec write. Not derivable from [`apply_group`]: `fold` and
/// `action_timeout` are Live too and need no re-arm.
pub(super) const EXTRAS_FIELDS: &[&str] = &[
    "max_memory",
    "watch",
    "ignore_watch",
    "watch_delay",
    "watch_options",
    "cron_restart",
    "cron_timezone",
    "liveness_probe",
];

/// Records what a load's `env` branch just merged: the file's env keys are
/// established from here, and an override of one is spent, unless it is a
/// tombstone, which is spent by nothing a load can do.
///
/// A tombstone means the absence of a key, and a file that declares that key
/// is where it still has work to do. Known gap: an env key skipped because an
/// override holds it is established anyway, so clearing it leaves the key out.
fn establish_env(next: &mut AppOverrides, incoming: &DeclaredApp) {
    if let Some(serde_json::Value::Object(env)) = next.fields.get_mut("env") {
        for key in &incoming.declared_env {
            if env.get(key).is_some_and(serde_json::Value::is_null) {
                continue;
            }
            env.remove(key);
        }
        if env.is_empty() {
            next.fields.remove("env");
        }
    }
    next.declared_env
        .extend(incoming.declared_env.iter().cloned());
}

/// The JSON object an `AppConfig` serializes to, the shape every merge here
/// edits field by field.
///
/// Through serde rather than field by field: a hand-written list of
/// assignments goes stale when a field is added to the struct.
///
/// # Errors
///
/// A description of the failure, for [`Applied::refused`], if the config
/// fails to travel through serde, or serializes as something other than an
/// object.
fn app_config_object(
    config: &AppConfig,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match serde_json::to_value(config) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(_) => Err("an app config must serialize as an object".to_string()),
        Err(err) => Err(err.to_string()),
    }
}

/// An `AppConfig` read back out of an object built by [`app_config_object`].
///
/// # Errors
///
/// A description of the failure, for [`Applied::refused`], if the object is
/// not a valid `AppConfig`. Only the shape is checked here: `normalize` is
/// what checks fields against each other, and each caller runs it later.
fn app_config_from_object(
    map: serde_json::Map<String, serde_json::Value>,
) -> Result<AppConfig, String> {
    serde_json::from_value(serde_json::Value::Object(map)).map_err(|err| err.to_string())
}

/// Merges what a Flockfile declares into what a sheep is running, returning
/// the merged config and the override record to write back.
///
/// A key in scope takes the file's value and gives up its override, and
/// `reset` decides scope: `None` only a declared key nobody has established
/// and never `instances`, `File` every declared key, `Policy` every key but
/// `env`, `Env` none (it resets `env` alone), `All` every key and `env` too.
///
/// # Errors
///
/// A description of the failure, for [`Applied::refused`], if either config
/// fails to travel through serde.
pub(super) fn merge_declared(
    stored: &AppConfig,
    incoming: &DeclaredApp,
    overrides: &AppOverrides,
    reset: ResetDepth,
) -> Result<(AppConfig, AppOverrides), String> {
    let mut merged = app_config_object(stored)?;
    let file = app_config_object(&incoming.config)?;
    let defaults = app_config_object(&AppConfig::default())?;

    let mut next = overrides.clone();
    // What the file established on an earlier load, plus what the operator
    // has set since. Either one makes a key established.
    let established: BTreeSet<&String> = overrides
        .declared
        .iter()
        .chain(overrides.fields.keys())
        .collect();

    for key in defaults.keys() {
        if key == "env" {
            continue;
        }
        let in_scope = match reset {
            // Every key goes back, declared or not; one the file is silent
            // about goes back to what a fresh start off that file would give.
            ResetDepth::Policy | ResetDepth::All => true,
            // A key the template never named has no template value to go back
            // to, so the operator's stands. `instances` gets no exception: a
            // template declaring it is an operator asking for that count.
            ResetDepth::File => incoming.declared.contains(key),
            // `ResetDepth::Env` and `ResetDepth::None`, append-only: a key the
            // template declares that nobody has established. `instances` is
            // held out of both, since the store cannot tell a stocked count
            // from an untouched one, so appending would delete instances.
            _ => {
                key != "instances" && incoming.declared.contains(key) && !established.contains(key)
            }
        };
        if !in_scope {
            // An override the file still declares: the file's value is what
            // `ResetSheepField` returns to, so it is kept current here even
            // though this load leaves the sheep alone.
            if next.fields.contains_key(key)
                && let Some(value) = incoming
                    .declared
                    .contains(key)
                    .then(|| file.get(key))
                    .flatten()
            {
                next.template.insert(key.clone(), value.clone());
            }
            continue;
        }
        // The override is spent: one left in `fields` would keep
        // `ProcessEntry::overridden` reporting a field that matches the file.
        next.fields.remove(key);
        next.template.remove(key);
        // `declared` grows only where the file really spoke: marking an
        // undeclared key would lock a later append out of it.
        if incoming.declared.contains(key) {
            next.declared.insert(key.clone());
        }
        // `file` is `incoming.config`, the CLI-resolved app, so an undeclared
        // key reads back what a fresh `shep start` of this file would give it,
        // not the compiled default.
        if let Some(value) = file.get(key) {
            merged.insert(key.clone(), value.clone());
        }
    }

    match reset {
        // The two depths that keep `env`: it is operator-supplied data where
        // the rest is operator-tuned policy.
        ResetDepth::Policy | ResetDepth::File => {}
        // The two that reset it. They agree about nothing else: `All` resets
        // every setting and `Env` resets none.
        ResetDepth::All | ResetDepth::Env => {
            next.fields.remove("env");
            merged.insert(
                "env".to_string(),
                file.get("env")
                    .cloned()
                    .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
            );
            establish_env(&mut next, incoming);
        }
        _ => {
            let overridden_env: BTreeSet<&String> = overrides
                .fields
                .get("env")
                .and_then(serde_json::Value::as_object)
                .map(|env| env.keys().collect())
                .unwrap_or_default();
            let mut env = stored.env.clone();
            for key in &incoming.declared_env {
                if overrides.declared_env.contains(key) || overridden_env.contains(key) {
                    continue;
                }
                if let Some(value) = incoming.config.env.get(key) {
                    env.insert(key.clone(), value.clone());
                }
            }
            merged.insert(
                "env".to_string(),
                serde_json::to_value(env).map_err(|err| err.to_string())?,
            );
            establish_env(&mut next, incoming);
        }
    }

    let merged = app_config_from_object(merged)?;
    Ok((merged, next))
}

/// The spec a load leaves on a running instance: what it was spawned from,
/// plus every merged field that can reach it without a replacement, plus the
/// instance count actually achieved.
///
/// `reaching` is the drifted [`ApplyGroup::Live`] and
/// [`ApplyGroup::NextSpawn`] field names; a [`ApplyGroup::NeedsRespawn`] one
/// is left in [`ProcessEntry::pending`] until the instance is replaced.
///
/// # Errors
///
/// A description of the failure if the rebuilt config does not travel through
/// serde or does not normalize, which a subset of a valid merge can.
pub(super) fn reached_spec<'a>(
    stored: &AppConfig,
    merged: &AppConfig,
    reaching: impl IntoIterator<Item = &'a String>,
    instances: u32,
) -> Result<ResolvedApp, String> {
    let mut next = app_config_object(stored)?;
    let source = app_config_object(merged)?;
    for field in reaching {
        if let Some(value) = source.get(field) {
            next.insert(field.clone(), value.clone());
        }
    }
    let mut config = app_config_from_object(next)?;
    config.instances = instances;
    normalize(config).map_err(|err| err.to_string())
}

/// The field names `entry`'s parked config differs from its running spec on,
/// or empty when nothing is parked.
///
/// Two readers ask this and must never disagree about the same sheep:
/// [`ProcessInfo::pending`], which every listing renders, and
/// [`Actor::handle_sheep_config`], which a config pane renders. An empty
/// answer covers both nothing parked and a parked config equal to the spec.
pub(super) fn pending_fields(entry: &ProcessEntry) -> Vec<String> {
    entry.pending.as_ref().map_or_else(Vec::new, |parked| {
        entry.spec.config().drifted_fields(parked.config())
    })
}

/// The one sentence every door that refuses to touch a dog's config says.
///
/// Four doors say it: [`Actor::apply_one`], where a Flockfile named a dog;
/// [`Actor::handle_sheep_config`], where a pane asked to read one; and
/// [`Actor::handle_set_sheep_env`] and [`Actor::handle_set_sheep_field`],
/// where a pane asked to write one.
pub(super) fn dog_config_refusal(name: &str) -> String {
    format!(
        "{name} is a dog, and a dog's config comes from `shep adopt` rather than \
         from a Flockfile"
    )
}

/// `record`'s stored `env` override, created empty when it has none.
///
/// A flat JSON object under the `env` key, which is the shape
/// [`merge_declared`] reads to decide which env keys an operator has
/// established. Anything else there is a store this build cannot act on, and
/// overwriting it would silently discard whatever a later shep wrote
/// ([`AppOverrides::fields`]' own doc argues the rule).
///
/// # Errors
///
/// [`SupervisorError::Overrides`] - the `env` key holds something other than
/// an object. Both callers refuse on it, so they say it once here.
pub(super) fn env_override_map<'a>(
    record: &'a mut AppOverrides,
    name: &str,
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>, SupervisorError> {
    record
        .fields
        .entry("env".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| {
            SupervisorError::Overrides(format!("{name}'s stored `env` override is not an object"))
        })
}

/// `app` with its instance count set to `instances`.
///
/// Re-normalized rather than mutated in place: a [`ResolvedApp`] is a proof
/// token that its config passed `normalize`.
///
/// # Errors
///
/// Whatever `normalize` refused. Only `instances` moves here, so a config
/// that normalized before can fail this on that count alone.
pub(super) fn try_with_count(
    app: &ResolvedApp,
    instances: u32,
) -> Result<ResolvedApp, NormalizeError> {
    let mut config = app.config().clone();
    config.instances = instances;
    normalize(config)
}

/// [`try_with_count`], with the refusal collapsed to `None`.
pub(super) fn with_count(app: &ResolvedApp, instances: u32) -> Option<ResolvedApp> {
    try_with_count(app, instances).ok()
}
