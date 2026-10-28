//! Shared test harness for `rpc`: a `dispatch` that hands out a fresh
//! [`ConnId`] per call, the `envelope`/`reply_of` wire-shape helpers,
//! and everything the test submodules pull in with `use super::*`.

use super::*;
use crate::fake::{FIRST_SCRIPTED_PID, ProcScript};
use crate::limits::MEMORY_POLL_INTERVAL;
use crate::testing::{
    Harness, SCRIPTED_TREE_BYTES, harness, harness_identifying, harness_with_stats, identity,
};
use shep_core::config::{AppConfig, ApplyGroup, DeclaredApp, ResetDepth, apply_group};
use shep_core::protocol::{
    ActionOutcome, ActionReply, DogSource, HostUsage, ProcessEventKind, Request, Response,
    RpcErrorCode, SelectorSpec,
};
use shep_core::values::UpDuration;
use std::collections::{BTreeMap, BTreeSet};
use tokio::time::Instant;

/// Dispatches on a connection of its own, shadowing [`super::dispatch`]
/// so no case here has to name a [`ConnId`]. One fresh id per call:
/// nothing here spans two requests on the same connection.
async fn dispatch(envelope: Envelope, ctx: &RpcContext) -> Outcome {
    super::dispatch(envelope, ConnId::next(), None, ctx).await
}

fn envelope(id: u64, body: Request) -> Envelope {
    Envelope {
        id,
        deadline_ms: None,
        body,
    }
}

fn reply_of(outcome: Outcome) -> Reply {
    match outcome {
        Outcome::Reply(reply) | Outcome::Subscribe { reply, .. } | Outcome::Shutdown(reply) => {
            reply
        }
    }
}

/// Starts `web` through `h`, asserting the start landed: a fixture that
/// never came up has to fail here, not at whatever assertion reads the
/// flock afterwards.
///
/// # Panics
///
/// If the start reply is an error.
async fn start_web(h: &Harness) {
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal("web", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(
        started.result.is_ok(),
        "web must start: {:?}",
        started.result
    );
}

/// The flock a `Describe` of `selector` on `ctx` answers with.
///
/// # Panics
///
/// If the reply is anything but `Described`, which is a fixture bug.
async fn describe(ctx: &RpcContext, id: u64, selector: SelectorSpec) -> Vec<ProcessInfo> {
    let reply = reply_of(dispatch(envelope(id, Request::Describe { selector }), ctx).await);
    let Ok(Response::Described(infos)) = reply.result else {
        panic!("expected Described, got {:?}", reply.result)
    };
    infos
}

/// Sends `entries` for `namespace` in `environment` as a `PutSecrets`,
/// and hands back the whole reply: every case here reads either its
/// `Ok` or its `Err`, so neither half is thrown away.
async fn put_secrets(
    ctx: &RpcContext,
    id: u64,
    namespace: &str,
    environment: &str,
    entries: &[(&str, &str)],
) -> Reply {
    reply_of(
        dispatch(
            envelope(
                id,
                Request::PutSecrets {
                    namespace: namespace.to_string(),
                    environment: environment.to_string(),
                    entries: entries
                        .iter()
                        .map(|(key, value)| ((*key).to_string(), (*value).to_string().into()))
                        .collect(),
                },
            ),
            ctx,
        )
        .await,
    )
}

/// The flock a `ListFlock` on `ctx` answers with.
///
/// # Panics
///
/// If the reply is anything but `Flock`, which is a fixture bug.
async fn list_flock(ctx: &RpcContext, id: u64) -> Vec<ProcessInfo> {
    let reply = reply_of(dispatch(envelope(id, Request::ListFlock), ctx).await);
    let Ok(Response::Flock(infos)) = reply.result else {
        panic!("expected Flock, got {:?}", reply.result)
    };
    infos
}

/// Enables `name` as a built-in dog through the real dispatch path,
/// returning the entry it registered.
async fn enable_dog(ctx: &RpcContext, id: u64, name: &str) -> ProcessInfo {
    let reply = reply_of(
        dispatch(
            envelope(
                id,
                Request::EnableDog {
                    name: name.to_string(),
                    source: DogSource::BuiltIn { grants: None },
                },
            ),
            ctx,
        )
        .await,
    );
    let Ok(Response::DogStarted(info)) = reply.result else {
        panic!("expected DogStarted, got {:?}", reply.result)
    };
    info
}

/// Starts one sheep named `web` carrying a secret env value, which is
/// the fixture the config-pane cases below all want.
async fn start_web_with_a_secret(ctx: &RpcContext) {
    let mut config = AppConfig::minimal("web", "./srv");
    config
        .env
        .insert("DB_PASS".to_string(), "hunter2".to_string());
    let started = reply_of(dispatch(envelope(1, Request::Start { apps: vec![config] }), ctx).await);
    assert!(started.result.is_ok(), "{:?}", started.result);
}

/// Reads one sheep's config view, for the cases that assert on it.
async fn sheep_config_view(
    ctx: &RpcContext,
    id: u64,
    name: &str,
) -> shep_core::protocol::SheepConfigView {
    let reply = reply_of(
        dispatch(
            envelope(
                id,
                Request::SheepConfig {
                    name: name.to_string(),
                },
            ),
            ctx,
        )
        .await,
    );
    match reply.result {
        Ok(Response::SheepConfig(view)) => *view,
        other => panic!("expected SheepConfig, got {other:?}"),
    }
}

mod apply_config;
mod disable_enable;
mod disable_roll;
mod disabled_reload;
mod disabled_restarts;
mod disabled_wildcards;
mod dog_at_rest;
mod dog_delete;
mod dog_disable;
mod dog_enable_grants;
mod dog_env;
mod dog_fields;
mod dog_lifecycle;
mod dog_secrets;
mod dog_secrets_rotation;
mod dog_table_events;
mod dog_tables;
mod env_copy;
mod lambs_staleness;
mod log_tail;
mod misc_verbs;
mod push;
mod reset_field;
mod sheep_dog_names;
mod sheep_dog_secrets;
mod start_add;
mod stats;
mod trigger_signal;
mod vcs;
mod walk;
