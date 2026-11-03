//! The check on a dog's own connection: whether the dog was granted what
//! the request takes.
//!
//! [`check`] asks `shep_core::permissions::need` what a request takes and
//! looks up what only the shepherd can: which sheep or dog a selector
//! reaches, and which environment a sheep resolves its secrets in. The
//! control socket never comes through here; it acts as the operator.

use bytes::Bytes;
use shep_core::permissions::{Grants, Need, Permission, need};
use shep_core::protocol::{Request, RpcError, RpcErrorCode};
use shep_core::selector::ProcessSelector;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::context::RpcContext;
use super::error::rpc_error;
use super::selector_verbs::matching_rows;
use crate::bus::{TopicFilter, spawn_forwarder, spawn_guarded_forwarder};

/// Which link a request arrives on, when it is not the control socket
#[derive(Debug, Clone, Copy)]
pub(crate) enum Link<'a> {
    /// A dog's own connection, bounded by the grants recorded for it.
    Dog(&'a str),
    /// A launch link: the connection `sheep` holds because `dog` launched it.
    /// Bounded by the sheep's own table and nothing else.
    Launch {
        /// The dog launching the sheep.
        dog: &'a str,
        /// The sheep the link was opened for.
        sheep: &'a str,
    },
}

/// Whether the launch link of `sheep`, opened for `dog`, may send `request`.
///
/// Admits `SheepDogSettingsResolved` for this sheep and dog, and a
/// `Subscribe` to `config.sheep.<dog>` alone. No grant widens it.
///
/// # Errors
/// [`RpcErrorCode::Forbidden`] naming the sheep and the request.
pub(super) fn check_launch(dog: &str, sheep: &str, request: &Request) -> Result<(), RpcError> {
    let own_topic = format!("config.sheep.{dog}");
    let allowed = match request {
        Request::SheepDogSettingsResolved { name, dog: table } => name == sheep && table == dog,
        Request::Subscribe { topics } => topics.iter().all(|topic| *topic == own_topic),
        _ => false,
    };
    if allowed {
        return Ok(());
    }
    Err(RpcError {
        code: RpcErrorCode::Forbidden,
        message: format!(
            "the launch link of sheep `{sheep}` was refused `{}`: it reads only the sheep's own `{dog}` table and hears `{own_topic}`",
            kind_of(request)
        ),
        daemon_version: None,
    })
}

/// Whether the dog `dog` may send `request`.
///
/// A dog with no grants recorded is undeclared and keeps full access.
///
/// # Errors
/// - [`RpcErrorCode::Forbidden`] naming the dog, the request and the first
///   permission it lacks.
/// - Whatever the supervisor reports, when it cannot answer the lookup.
pub(super) async fn check(ctx: &RpcContext, dog: &str, request: &Request) -> Result<(), RpcError> {
    let Some(grants) = ctx.known_dogs.grants(dog) else {
        return Ok(());
    };
    let refuse = |needed: &str| forbidden(dog, request, needed);
    match need(request, dog) {
        Need::Always => Ok(()),
        Need::Permission(permission) => require(&grants, &permission).map_err(|p| refuse(&p)),
        Need::Selector {
            selector,
            sheep,
            dog: for_dog,
        } => {
            // An invalid selector is the handler's to refuse, with its own
            // reason.
            let Ok(selector) = ProcessSelector::try_from(selector.clone()) else {
                return Ok(());
            };
            let infos = ctx
                .supervisor
                .list_checked()
                .await
                .map_err(|err| rpc_error(&err))?;
            let mut reached = matching_rows(infos, &selector)
                .into_iter()
                .map(|i| {
                    if i.dog.is_some() {
                        for_dog(&i.name)
                    } else {
                        sheep.clone()
                    }
                })
                .peekable();
            // Reaching nothing takes the sheep permission, so a dog cannot
            // probe for what exists.
            if reached.peek().is_none() {
                return require(&grants, &sheep).map_err(|p| refuse(&p));
            }
            reached.try_for_each(|p| require(&grants, &p).map_err(|p| refuse(&p)))
        }
        Need::Topics(topics) => topics
            .iter()
            .find(|topic| !grants.allows_topic(topic) && !is_own_config_topic(topic, dog))
            .map_or(Ok(()), |topic| {
                Err(refuse(&format!("events:{}", literal_head(topic))))
            }),
        Need::ResolvedRead { sheep, dog: table } => {
            let read = ctx
                .supervisor
                .sheep_dog_read(sheep.to_string(), table.to_string())
                .await
                .map_err(|err| rpc_error(&err))?;
            // No such sheep: the handler answers it, and there is no
            // environment to have refused.
            let Some(read) = read else { return Ok(()) };
            require(&grants, &Permission::SecretsRead(read.environment)).map_err(|p| refuse(&p))
        }
        // `Need` is `#[non_exhaustive]`: a need this build cannot read is
        // refused rather than waved through.
        _ => Err(refuse("a permission this shepherd does not know")),
    }
}

/// `request` as the operator's word on grants, on a dog's own connection.
///
/// An `EnableDog` carries the grants the CLI read from `shep.toml`, and
/// recording them is the one way a dog's grants change. A dog holding
/// `dogs.control` may send one for itself or for another dog, but the grants
/// in it are replaced by what is already recorded for the name. A dog never
/// grants anything, and that holds for the dogs it enables too.
pub(super) fn pin_grants(ctx: &RpcContext, request: Request) -> Request {
    match request {
        Request::EnableDog { name, source, env } => {
            let recorded = ctx
                .known_dogs
                .recorded(&name)
                .unwrap_or_else(|| Some(Grants::default()));
            let events = ctx.known_dogs.events(&name);
            Request::EnableDog {
                source: source.with_grants(recorded).with_events(events),
                name,
                env,
            }
        }
        other => other,
    }
}

/// Whether `link` may hold the subscription `topics`, for a successor
/// re-arming one a predecessor carried over.
///
/// The same check a fresh `Subscribe` meets, against the grants this
/// shepherd read, which may be narrower than the ones the dog subscribed under.
///
/// # Errors
/// [`RpcErrorCode::Forbidden`] naming the first topic outside the grants, or
/// for a launch link the request.
pub(crate) async fn check_carried(
    ctx: &RpcContext,
    link: Link<'_>,
    topics: &[String],
) -> Result<(), RpcError> {
    let request = Request::Subscribe {
        topics: topics.to_vec(),
    };
    match link {
        Link::Dog(dog) => check(ctx, dog, &request).await,
        Link::Launch { dog, sheep } => check_launch(dog, sheep, &request),
    }
}

/// Forwards the events `filter` selects to the connection `out`, and for a
/// dog's own connection stops when its grants no longer cover every topic.
/// A launch link hears only the events that name its sheep.
///
/// A `Subscribe` is checked once, by [`check`]; this is the recheck an
/// `EnableDog` that narrows the dog's grants needs, since the forwarder
/// otherwise keeps the filter it was armed with.
pub(crate) fn spawn_checked_forwarder(
    ctx: &RpcContext,
    link: Option<Link<'_>>,
    filter: TopicFilter,
    out: mpsc::Sender<Bytes>,
) -> JoinHandle<()> {
    let dog = match link {
        None => return spawn_forwarder(&ctx.events, filter, out),
        Some(Link::Launch { sheep, .. }) => {
            return spawn_forwarder(&ctx.events, filter.only_sheep(sheep), out);
        }
        Some(Link::Dog(dog)) => dog,
    };
    let (known_dogs, dog) = (ctx.known_dogs.clone(), dog.to_owned());
    let topics = filter.patterns().to_vec();
    spawn_guarded_forwarder(&ctx.events, filter, out, move || {
        known_dogs.grants(&dog).is_none_or(|grants| {
            topics
                .iter()
                .all(|topic| grants.allows_topic(topic) || is_own_config_topic(topic, &dog))
        })
    })
}

/// `Ok` when `grants` holds `permission`, and its name otherwise.
fn require(grants: &Grants, permission: &Permission) -> Result<(), String> {
    if grants.allows(permission) {
        Ok(())
    } else {
        Err(permission.to_string())
    }
}

/// The two topics that announce a change to a dog's own section and tables,
/// which the dog may always read, as it may always read the section.
fn is_own_config_topic(topic: &str, dog: &str) -> bool {
    topic
        .strip_prefix("config.dog.")
        .or_else(|| topic.strip_prefix("config.sheep."))
        == Some(dog)
}

/// The wire name of `request`'s variant, and nothing of what it carries.
///
/// A request that serializes with no `kind` is still narrated, as `request`.
pub(crate) fn kind_of(request: &Request) -> String {
    serde_json::to_value(request)
        .ok()
        .and_then(|value| value.get("kind")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| "request".to_owned())
}

/// The part of a topic glob before its first glob character: the `events:`
/// prefix a grant must cover to allow it.
fn literal_head(pattern: &str) -> &str {
    pattern
        .find(['*', '?', '[', '{', '\\'])
        .map_or(pattern, |at| &pattern[..at])
}

fn forbidden(dog: &str, request: &Request, needed: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Forbidden,
        message: format!(
            "dog `{dog}` was refused `{}`: it needs `{needed}`, which was not granted to it",
            kind_of(request)
        ),
        daemon_version: None,
    }
}
