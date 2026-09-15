//! The verb-routing `match`: one arm per [`Request`] variant, wrapped by
//! [`budget`] so no reply outlives its deadline.
//!
//! `run` is the whole dispatch table; [`dispatch`] and `with_deadline`
//! are the timeout wrapper around it.

use core::future::Future;
use core::time::Duration;

use std::collections::BTreeMap;

use shep_core::config::{NormalizeError, ResolvedApp, normalize_all};
use shep_core::protocol::{
    Envelope, PROTOCOL_VERSION, Reply, Request, Response, RpcError, RpcErrorCode, SheepApplied,
};
use shep_core::selector::ProcessSelector;

use crate::bus::TopicFilter;
use crate::dogs::DogSpec;
use crate::supervisor::{BatchPolicy, ConnId};

use super::batch::{duplicate_name, persists, staged_plan};
use super::context::{Outcome, RpcContext, budget};
use super::enrichment::{dog_staleness, handover_refusal, with_dog_contact, with_lambs, with_live_stats};
use super::error::rpc_error;
use super::selector_verbs::{not_found, selector_call, selector_of, signal_request, trigger};
use super::walk::{reload_request, restart_request};

/// Dispatches one request envelope against `ctx`, returning what the
/// connection layer must do with the result.
///
/// The deadline [`budget`] computes bounds the reply, not the actor's work:
/// dropping the work future only stops the daemon waiting on the supervisor,
/// and a command already handed to a sheep-owning task runs to completion.
/// So a `DeadlineExceeded` reply to `Start` means no answer within the
/// budget, not that nothing happened; a client that retries must reconcile
/// with `ListFlock`.
pub(crate) async fn dispatch(envelope: Envelope, conn: ConnId, ctx: &RpcContext) -> Outcome {
    let id = envelope.id;
    with_deadline(
        id,
        budget(envelope.deadline_ms),
        run(id, conn, envelope.body, ctx),
    )
    .await
}

// `+ Send`: awaited inside the per-connection `tokio::spawn`, so the bound is
// stated rather than inferred.
pub(super) async fn with_deadline<F: Future<Output = Outcome> + Send>(
    id: u64,
    budget: Duration,
    work: F,
) -> Outcome {
    match tokio::time::timeout(budget, work).await {
        Ok(outcome) => outcome,
        Err(_) => Outcome::Reply(Reply {
            id,
            result: Err(RpcError {
                code: RpcErrorCode::DeadlineExceeded,
                message: format!(
                    "the request deadline of {} ms expired before the daemon finished",
                    budget.as_millis()
                ),
                daemon_version: None,
            }),
        }),
    }
}

async fn run(id: u64, conn: ConnId, request: Request, ctx: &RpcContext) -> Outcome {
    let reply = |result| Outcome::Reply(Reply { id, result });
    match request {
        Request::Ping => reply(Ok(Response::Pong)),
        // One of the two verbs that pays for a live reading; `with_live_stats`
        // says why every lifecycle verb below goes without.
        Request::ListFlock => match ctx.supervisor.list_checked().await {
            Ok(infos) => reply(Ok(Response::Flock(with_dog_contact(
                &ctx.dog_refusals,
                with_live_stats(&ctx.stats, infos).await,
            )))),
            Err(err) => reply(Err(rpc_error(&err))),
        },
        // Free, unlike the two below it: the reading was taken on this
        // daemon's own tick and this arm hands back what is already in
        // memory. Sampling here would divide a near-zero delta by a
        // near-zero window; `crate::host` is the argument.
        Request::HostUsage => reply(Ok(Response::HostUsage(ctx.host.latest()))),
        // The other one. Sampled after the selector has narrowed the
        // listing, so the join below runs over the matched rows alone.
        Request::Describe { selector } => match selector_of(selector) {
            Err(err) => reply(Err(err)),
            Ok(selector) => match ctx.supervisor.list_checked().await {
                Err(err) => reply(Err(rpc_error(&err))),
                Ok(infos) => {
                    // The rule `Actor::matching_ids` applies to every
                    // lifecycle verb, repeated here because this filter is
                    // over `ProcessInfo`s: a dog is not a flock member, so a
                    // sweep passes it by and an exact selector reaches it.
                    let exact = selector.is_exact();
                    let hits: Vec<_> = infos
                        .into_iter()
                        .filter(|i| exact || i.dog.is_none())
                        .filter(|i| selector.matches(&i.name, i.id, i.fold.as_deref(), i.instance))
                        .collect();
                    if hits.is_empty() {
                        reply(Err(not_found()))
                    } else {
                        let hits = with_live_stats(&ctx.stats, hits).await;
                        let hits = with_lambs(&ctx.stats, hits).await;
                        reply(Ok(Response::Described(with_dog_contact(
                            &ctx.dog_refusals,
                            hits,
                        ))))
                    }
                }
            },
        },
        // Peer input is untrusted: re-normalize before anything is registered.
        //
        // `AllOrNothing` no longer means the whole request is atomic: the
        // stages are one `Command::Start` each, so a refusal in stage 1 leaves
        // stage 0 running and nothing rolls it back. The refusal names those
        // apps; `start_in_stages` argues why they are left alone.
        Request::Start { apps } => match normalize_all(apps) {
            Err(err) => reply(Err(RpcError {
                code: RpcErrorCode::InvalidConfig,
                message: err.to_string(),
                daemon_version: None,
            })),
            Ok(resolved) => match staged_plan(ctx, &resolved) {
                Err(err) => reply(Err(err)),
                Ok(plan) => {
                    ctx.registry.record(&resolved);
                    match crate::boot_order::start_in_stages(
                        &plan,
                        &resolved,
                        &ctx.supervisor,
                        &ctx.events,
                        BatchPolicy::AllOrNothing,
                    )
                    .await
                    {
                        Ok(infos) => reply(Ok(Response::Started(infos))),
                        Err(err) => reply(Err(rpc_error(&err))),
                    }
                }
            },
        },
        // The membership half of `Start` with none of the spawning, and the
        // same untrusted-peer rule. Recorded in the registry as `Start` is:
        // an added app is a flock member that happens to be stopped, so a
        // `shep save` after a `shep add` has to write it.
        //
        // The cycle refusal is shared and the stages are not: a document that
        // cannot be started is one to refuse at the door an operator is
        // standing at, and `add` starts nothing to order.
        Request::Add { apps } => match normalize_all(apps) {
            Err(err) => reply(Err(RpcError {
                code: RpcErrorCode::InvalidConfig,
                message: err.to_string(),
                daemon_version: None,
            })),
            Ok(resolved) => match staged_plan(ctx, &resolved) {
                Err(err) => reply(Err(err)),
                Ok(_) => {
                    ctx.registry.record(&resolved);
                    match ctx.supervisor.register_at_rest(resolved).await {
                        Ok(infos) => reply(Ok(Response::Added(infos))),
                        Err(err) => reply(Err(rpc_error(&err))),
                    }
                }
            },
        },
        // Re-normalized for the reason `Start` is, plus one of its own: an
        // unnormalized config would report every default it did not spell out
        // as a difference. Nothing is recorded, since this answers a question
        // and must not change what the next `shep save` writes.
        Request::ConfigDrift { apps } => match normalize_all(apps) {
            Err(err) => reply(Err(RpcError {
                code: RpcErrorCode::InvalidConfig,
                message: err.to_string(),
                daemon_version: None,
            })),
            Ok(resolved) => match ctx.supervisor.config_drift(resolved).await {
                Ok(drifted) => reply(Ok(Response::Drifted(drifted))),
                Err(err) => reply(Err(rpc_error(&err))),
            },
        },
        Request::Stop { selector } => {
            selector_call(id, selector, |s| ctx.supervisor.stop(s), Response::Stopped).await
        }
        // Forward, dependencies first, when the selector matches more than
        // one sheep. Not reverse-stop then forward-start: the rolling version
        // puts the whole fold down at once in the middle, and forward-only
        // never does.
        Request::Restart { selector } => restart_request(id, selector, ctx).await,
        // Staged like `Restart` above and for its reason. Which of the two
        // reloads an app gets is still `ReloadMode::of`'s call: ordering
        // decides when a stage begins and nothing about the swap inside it.
        //
        // `Reloading` still names an acceptance, and a single-target reload
        // still answers before the first replacement is spawned. A walk of
        // several stages is what that costs: it holds for the swaps of every
        // app another matched app waits on, so the reply lands that much
        // later, and a fold deep enough outlives the request budget and is
        // abandoned by `with_deadline` with its last stages unreloaded.
        // `staged_start_deadline` is how `shep start` buys the room for the
        // same walk; `shep reload` sends `RELOAD_DEADLINE`, which is this
        // module's own 60s ceiling and so the most it can buy.
        Request::Reload { selector } => reload_request(id, selector, ctx).await,
        Request::Reopen { selector } => {
            selector_call(
                id,
                selector,
                |s| ctx.supervisor.reopen(s),
                Response::Reopened,
            )
            .await
        }
        Request::Flush { selector } => {
            selector_call(id, selector, |s| ctx.supervisor.flush(s), Response::Flushed).await
        }
        Request::Trigger {
            selector,
            action,
            params,
        } => trigger(id, selector, action, params, ctx).await,
        Request::Signal { selector, signal } => signal_request(id, selector, signal, ctx).await,
        Request::SendLine { selector, line } => {
            // Refused here, not silently split by the writer: a line
            // carrying a newline would be delivered as two commands where the
            // operator typed one. `\r` too, since CRLF reaches a shell as a
            // command with a stray carriage return in it.
            if line.contains(['\n', '\r']) {
                return reply(Err(RpcError {
                    code: RpcErrorCode::InvalidConfig,
                    message: "a line may not contain a newline or a carriage return; \
                              send one line per request"
                        .to_string(),
                    daemon_version: None,
                }));
            }
            match selector_of(selector) {
                Ok(selector) => match ctx.supervisor.send_line(selector, line).await {
                    Ok(rows) => reply(Ok(Response::SentLine(rows))),
                    Err(err) => reply(Err(rpc_error(&err))),
                },
                Err(err) => reply(Err(err)),
            }
        }
        Request::Delete { selector } => match selector_of(selector) {
            Err(err) => reply(Err(err)),
            Ok(selector) => match ctx.supervisor.delete(selector).await {
                Ok(ids) => reply(Ok(Response::Deleted(ids))),
                Err(err) => reply(Err(rpc_error(&err))),
            },
        },
        Request::Scale { name, count } => match ctx.supervisor.scale(&name, count).await {
            Ok(scaled) => {
                // Recorded unconditionally: without it `shep stock web 4`
                // then `shep save` writes `instances = 2` and the next reboot
                // undoes the scale. Unconditionally, since a partial scale-up
                // leaves real instances the roll has to know about too.
                let achieved = scaled.achieved();
                let requested = scaled.requested;
                ctx.registry.record(&[scaled.app]);
                match scaled.shortfall {
                    None => reply(Ok(Response::Scaled(scaled.instances))),
                    // Non-zero exit: the operator asked for four and has
                    // three. The sentence names both numbers, so a reader can
                    // tell a scale that achieved nothing from one that nearly
                    // finished.
                    Some(message) => reply(Err(RpcError {
                        code: RpcErrorCode::SpawnFailed,
                        message: format!(
                            "scaled {name} to {achieved} of {requested} requested; \
                             the next instance would not spawn: {message}"
                        ),
                        daemon_version: None,
                    })),
                }
            }
            Err(err) => reply(Err(rpc_error(&err))),
        },
        // Scoped to `conn`, which is what makes a smit ephemeral: the
        // connection layer forgets this one's marks in its own tail. `smit`
        // arrives already validated by `Smit`'s hand-written `Deserialize`,
        // so the only refusal left here is a name nothing holds.
        Request::SetSmit { sheep, smit } => {
            match ctx.supervisor.set_smit(conn, &sheep, smit).await {
                Ok(infos) => reply(Ok(Response::SmitPainted(infos))),
                Err(err) => reply(Err(rpc_error(&err))),
            }
        }
        Request::SaveRoll => match ctx.save_roll_now().await {
            Ok(Some(saved)) => reply(Ok(Response::RollSaved {
                // Lossy, as `to_info` treats log paths: a non-UTF-8 roll
                // path degrades one field rather than the whole reply.
                path: saved.path.to_string_lossy().into_owned(),
                apps: saved.apps,
            })),
            Ok(None) => reply(Err(RpcError {
                code: RpcErrorCode::Internal,
                message: "the supervisor engine has stopped; no roll was written".to_string(),
                daemon_version: None,
            })),
            Err(err) => reply(Err(RpcError {
                code: RpcErrorCode::Internal,
                message: err.to_string(),
                daemon_version: None,
            })),
        },
        // The same restore `boot` runs, called the same way
        // (`crate::snapshot::muster`).
        Request::Muster => {
            match crate::snapshot::muster(
                &ctx.snapshot_path,
                &ctx.registry,
                &ctx.supervisor,
                &ctx.events,
                &ctx.dog_names,
                &ctx.boot_first_dogs,
            )
            .await
            {
                Err(err) => reply(Err(RpcError {
                    code: RpcErrorCode::Internal,
                    message: err.to_string(),
                    daemon_version: None,
                })),
                Ok(names) => match ctx.supervisor.list_checked().await {
                    Err(err) => reply(Err(rpc_error(&err))),
                    // Every sheep of every app the roll restored, not only
                    // the ones this call spawned (`Response::Mustered`).
                    Ok(infos) => reply(Ok(Response::Mustered(
                        infos
                            .into_iter()
                            .filter(|info| names.contains(&info.name))
                            .collect(),
                    ))),
                },
            }
        }
        // Re-read per request, never cached: `shep disable X && shep enable
        // X` bounces a dog to reload its configuration, and a copy taken at
        // boot would answer with the section as it was. A dog subscribed to
        // `config.dog.<name>` reaches this same arm without going down.
        Request::DogConfig { name } => match crate::dogs::dog_section(&ctx.dogs_config, &name) {
            Ok(toml) => reply(Ok(Response::DogSection { toml: toml.into() })),
            Err(err) => reply(Err(RpcError {
                code: RpcErrorCode::InvalidConfig,
                message: err.to_string(),
                daemon_version: None,
            })),
        },
        Request::EnableDog { name, source } => {
            let spec = DogSpec { name, source };
            match crate::dogs::dog_app(&spec, &ctx.paths) {
                Err(err) => reply(Err(RpcError {
                    code: RpcErrorCode::InvalidConfig,
                    message: err.to_string(),
                    daemon_version: None,
                })),
                Ok(app) => {
                    // Read before `start_dog` takes the app. An operator
                    // reading the dog's log during an upgrade is usually
                    // asking which file the spawn resolved to.
                    let script = app.config().script.clone();
                    match ctx.supervisor.start_dog(app, spec.source).await {
                        // `start_dog` is idempotent by name, so what comes
                        // back is whatever already holds it. An unmarked entry
                        // means a sheep holds it: nothing was spawned, so the
                        // refusal has nothing to undo.
                        Ok(info) if info.dog.is_none() => reply(Err(RpcError {
                            code: RpcErrorCode::InvalidConfig,
                            message: format!(
                                "a sheep is already registered as `{}`; rename it or give the dog another name",
                                spec.name
                            ),
                            daemon_version: None,
                        })),
                        Ok(info) => {
                            // The one place this daemon learns of a dog it
                            // was not told about at boot. `shep adopt` and
                            // `shep enable` both arrive here, and both have
                            // just written the name into `shep.toml`, which
                            // this crate does not read. Recorded on the
                            // success arm only: the refusal above is a
                            // sheep holding the name, and that is not a dog
                            // to remember.
                            ctx.known_dogs.insert(&info.name);
                            // Wording is about the binary this shepherd
                            // resolved, not about a spawn having happened:
                            // `start_dog` is idempotent by name, so this may
                            // be a dog that was already running.
                            crate::dogs::narrate(
                                &ctx.events,
                                &info,
                                &format!(
                                    "shep has this dog enabled, running the binary at {script}"
                                ),
                            )
                            .await;
                            reply(Ok(Response::DogStarted(info)))
                        }
                        Err(err) => reply(Err(rpc_error(&err))),
                    }
                }
            }
        }
        // Through `delete` with an exact `Name` selector: disabling a dog
        // reuses the stop-then-deregister path every sheep takes rather than
        // opening a second way to end a supervised process.
        Request::DisableDog { name } => {
            match ctx.supervisor.delete(ProcessSelector::Name(name)).await {
                Ok(ids) => reply(Ok(Response::Deleted(ids))),
                Err(err) => reply(Err(rpc_error(&err))),
            }
        }
        Request::Subscribe { topics } => match TopicFilter::new(&topics) {
            Ok(filter) => Outcome::Subscribe {
                reply: Reply {
                    id,
                    result: Ok(Response::Subscribed),
                },
                filter,
            },
            Err(err) => reply(Err(RpcError {
                code: RpcErrorCode::InvalidConfig,
                message: err.to_string(),
                daemon_version: None,
            })),
        },
        Request::DogStaleness => {
            let (stale, pending) = dog_staleness(ctx).await;
            reply(Ok(Response::DogStaleness { stale, pending }))
        }
        Request::HandoverFitness => reply(Ok(Response::HandoverFitness {
            refusal: handover_refusal(ctx).await,
        })),
        Request::KillDaemon => Outcome::Shutdown(Reply {
            id,
            result: Ok(Response::ShuttingDown),
        }),
        // The acting half of `ConfigDrift` above, and the one arm here that
        // changes a running flock's config without replacing anything.
        Request::ApplyConfig { apps, reset } => match duplicate_name(&apps) {
            Some(name) => reply(Err(RpcError {
                code: RpcErrorCode::InvalidConfig,
                message: NormalizeError::DuplicateName(name).to_string(),
                daemon_version: None,
            })),
            None => match ctx.supervisor.apply_config(apps, reset).await {
                Ok(applied) => {
                    // Recorded unconditionally, as the `Scale` arm above is:
                    // an apply that reached the stored spec must reach the
                    // roll too. An app whose merge produced no honest config
                    // carries `None` and is skipped rather than invented.
                    let recorded: Vec<ResolvedApp> =
                        applied.iter().filter_map(|a| a.app.clone()).collect();
                    ctx.registry.record(&recorded);
                    reply(Ok(Response::Applied(
                        applied.into_iter().map(SheepApplied::from).collect(),
                    )))
                }
                Err(err) => reply(Err(rpc_error(&err))),
            },
        },
        // The two config-pane reads and writes. Neither takes a selector:
        // a pane edits one sheep, so an unknown name is `NotFound` here
        // rather than an empty match.
        Request::SheepConfig { name } => match ctx.supervisor.sheep_config(name.clone()).await {
            Ok(Some(view)) => reply(Ok(Response::SheepConfig(Box::new(view)))),
            Ok(None) => reply(Err(RpcError {
                code: RpcErrorCode::NotFound,
                message: format!("no sheep named {name}"),
                daemon_version: None,
            })),
            Err(err) => reply(Err(rpc_error(&err))),
        },
        Request::SetSheepEnv { name, key, value } => {
            match ctx
                .supervisor
                .set_sheep_env(name.clone(), key.clone(), value)
                .await
            {
                // Recorded exactly as `Start`, `Add`, `Scale` and
                // `ApplyConfig` record theirs, and for the reason
                // `Scale`'s arm gives: the muster roll is written from the
                // registry (`snapshot::muster`) and nothing on the restore
                // path reads the override store, so an env edit that
                // skipped this would survive a `shep daemon reload` (the
                // handover blob carries `pending`) and vanish on a cold
                // restart. The same field class behaving differently
                // depending on which request set it is the bug.
                Ok(Some(app)) => {
                    ctx.registry.record(&[app]);
                    reply(Ok(Response::SheepEnvSet { name, key }))
                }
                Ok(None) => reply(Err(RpcError {
                    code: RpcErrorCode::NotFound,
                    message: format!("no sheep named {name}"),
                    daemon_version: None,
                })),
                Err(err) => reply(Err(rpc_error(&err))),
            }
        }
        Request::SetSheepEnvBatch {
            name,
            entries,
            force,
            dry_run,
        } => {
            let values: BTreeMap<String, String> = entries
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().to_string()))
                .collect();
            match ctx
                .supervisor
                .set_sheep_env_batch(name.clone(), values, force, dry_run)
                .await
            {
                // Recorded for `SetSheepEnv`'s reason: the muster roll is
                // written from the registry and nothing on the restore path
                // reads the override store. `app` is `None` for a dry run,
                // for a refused collision, and for a batch every key of
                // which was already held, none of which wrote anything to
                // record.
                Ok(Some(batch)) => {
                    if let Some(app) = batch.app {
                        ctx.registry.record(&[app]);
                    }
                    reply(Ok(Response::SheepEnvBatch {
                        name,
                        set: batch.set,
                        unchanged: batch.unchanged,
                        collisions: batch.collisions,
                    }))
                }
                Ok(None) => reply(Err(RpcError {
                    code: RpcErrorCode::NotFound,
                    message: format!("no sheep named {name}"),
                    daemon_version: None,
                })),
                Err(err) => reply(Err(rpc_error(&err))),
            }
        }
        Request::SetSheepField { name, key, value } => {
            match ctx
                .supervisor
                .set_sheep_field(name.clone(), key.clone(), value)
                .await
            {
                // Recorded exactly as `SetSheepEnv` records its own, and
                // for that arm's reason: the muster roll is written from
                // the registry and nothing on the restore path reads the
                // override store, so a field edit that skipped this would
                // survive a `shep daemon reload` and vanish on a cold
                // restart.
                Ok(Some(set)) => {
                    let pending = set.pending;
                    let warning = set.warning;
                    ctx.registry.record(&[set.app]);
                    reply(Ok(Response::SheepFieldSet {
                        name,
                        key,
                        pending,
                        warning,
                    }))
                }
                Ok(None) => reply(Err(RpcError {
                    code: RpcErrorCode::NotFound,
                    message: format!("no sheep named {name}"),
                    daemon_version: None,
                })),
                Err(err) => reply(Err(rpc_error(&err))),
            }
        }
        // The inverse of every other config door's guard: `dogs.toml`
        // holds dogs' sections and nothing else, so what this one refuses
        // is a sheep's name, not merely a dog no one has heard of. Asked
        // before the file is opened, so a mistyped name leaves no stray
        // table behind for a dog that will never exist.
        //
        // Guarded on `known_dogs`, not on the running flock: a guard on the
        // flock alone would refuse the dog most in need of configuring, one
        // that is disabled or has never started. The running flock is still
        // consulted too, as a widening, because a dog adopted and enabled
        // since this shepherd booted is not yet in the list the CLI handed
        // over at boot.
        //
        // Written here rather than through the supervisor: `dogs.toml` is
        // not supervisor state, and the file's path and the bus are both
        // already in scope here. The daemon, not the client, writes it
        // because the daemon is the only publisher of `config.dog.<name>`,
        // and a section written with nothing publishing that topic leaves
        // a running dog reading the old one.
        Request::SetDogConfig { name, toml } => {
            // A stopped engine runs no dogs, so "not running" is the honest
            // answer and `known_dogs` is left carrying the guard alone.
            let running_dog = || async {
                ctx.supervisor
                    .list_checked()
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|info| info.name == name && info.dog.is_some())
            };
            if !ctx.known_dogs.contains(&name) && !running_dog().await {
                return reply(Err(RpcError {
                    code: RpcErrorCode::NotFound,
                    message: format!(
                        "no dog named {name}; `shep adopt` or `shep enable` makes one known \
                         to this shepherd"
                    ),
                    daemon_version: None,
                }));
            }
            match crate::dogs::set_dog_section(&ctx.dogs_config, &name, toml.as_str()) {
                Ok(()) => {
                    crate::bus::publish_dog_config_changed(
                        &ctx.events,
                        std::slice::from_ref(&name),
                    );
                    reply(Ok(Response::DogConfigSet { name }))
                }
                Err(err) => reply(Err(RpcError {
                    code: RpcErrorCode::InvalidConfig,
                    message: err.to_string(),
                    daemon_version: None,
                })),
            }
        }
        // A provider dog's push. Unguarded by design, for the reason the
        // variant's own doc gives.
        //
        // Both NAMES and every entry KEY are checked against the store's own
        // grammar: one outside it is a name no `{{secret:...}}` reference
        // could reach, so storing under it would answer `accepted` and
        // resolve nothing. Values are capped at the same `MAX_VALUE_BYTES`
        // the operator's own store enforces, since without it a socket peer
        // sets the shepherd's memory and, with `persist`, rewrites and
        // fsyncs whatever it sent on every later push.
        //
        // One offender refuses the whole push, as the namespace check
        // already does: a partial store is one a dog would report `accepted`
        // for and a spawn would refuse on, and the dog has the whole set to
        // send again.
        //
        // Handled here rather than through the supervisor: the registry is
        // not supervisor state, and this arm writes the cache file, which
        // the actor must never wait on. See `crate::secrets`.
        Request::PutSecrets {
            namespace,
            environment,
            entries,
        } => {
            let refused = [
                ("namespace", namespace.as_str()),
                ("environment", environment.as_str()),
            ]
            .into_iter()
            .chain(entries.keys().map(|key| ("key", key.as_str())))
            .find(|(_, value)| !shep_core::secrets::is_name(value));
            if let Some((field, value)) = refused {
                return reply(Err(RpcError {
                    code: RpcErrorCode::InvalidConfig,
                    message: format!(
                        "`{value}` is not a valid {field}: a name is 1 to \
                         {max} bytes of `[A-Za-z0-9._-]` and may not start with `.`",
                        max = shep_core::secrets::MAX_KEY_BYTES
                    ),
                    daemon_version: None,
                }));
            }
            // The key, never the value: an error message is the one place a
            // pushed value must not turn up (IR-41).
            let oversized = entries
                .iter()
                .map(|(key, value)| (key, value.as_str().len()))
                .find(|(_, len)| *len > shep_core::secrets::MAX_VALUE_BYTES);
            if let Some((key, len)) = oversized {
                return reply(Err(RpcError {
                    code: RpcErrorCode::InvalidConfig,
                    message: format!(
                        "the value for `{key}` is {len} bytes, over the {max}-byte limit",
                        max = shep_core::secrets::MAX_VALUE_BYTES
                    ),
                    daemon_version: None,
                }));
            }
            let values = entries
                .into_iter()
                .map(|(key, value)| (key, value.as_str().to_owned()))
                .collect();
            match ctx.provider_secrets.put(
                &namespace,
                &environment,
                values,
                persists(&ctx.dogs_config, &namespace),
            ) {
                Ok(accepted) => reply(Ok(Response::SecretsPut { accepted })),
                // The values are already in memory and already resolvable;
                // what failed is only their surviving a restart. Reported
                // rather than swallowed, so a dog whose operator asked for
                // a cache learns it is not getting one.
                Err(err) => reply(Err(RpcError {
                    code: RpcErrorCode::Internal,
                    message: format!(
                        "{namespace}/{environment} is stored, but the cache could not be \
                         written: {err}"
                    ),
                    daemon_version: None,
                })),
            }
        }
        // The catch-all `#[serde(other)]` lands on the wire, not a panic:
        // a client newer than this daemon gets a named refusal instead of
        // a dropped connection, and the connection keeps serving requests
        // this daemon does know. `daemon_version` stays `None` here: that
        // field is reserved for a `ProtocolMismatch` refusal, the only point
        // where `HelloAck::daemon_version` never reaches the client, and this
        // reply follows a successful handshake that already delivered it.
        Request::Unrecognized => reply(Err(RpcError {
            code: RpcErrorCode::Unsupported,
            message: format!(
                "this shepherd speaks protocol {PROTOCOL_VERSION} and does not \
                 implement the request the client sent"
            ),
            daemon_version: None,
        })),
        // A `Request` variant this crate added but this match forgot to wire
        // up: `Request::Unrecognized` above already answers a verb this
        // daemon has never heard of, so reaching this arm is our own bug,
        // not a hostile or newer client (IR-47).
        _ => reply(Err(RpcError {
            code: RpcErrorCode::Internal,
            message: "this daemon does not implement that request".to_string(),
            daemon_version: None,
        })),
    }
}
