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
    super::dispatch(envelope, ConnId::next(), ctx).await
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

/// Starts `web` through `h` and returns nothing: no case below asserts
/// on the start reply itself.
async fn start_web(h: &Harness) {
    reply_of(
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
                    source: DogSource::BuiltIn,
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
mod dog_env;
mod dog_fields;
mod dog_lifecycle;
mod lambs_staleness;
mod misc_verbs;
mod push;
mod start_add;
mod stats;
mod trigger_signal;
mod walk;
