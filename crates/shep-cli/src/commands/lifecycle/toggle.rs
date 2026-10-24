//! `enable` and `disable`: one verb pair for a sheep or a dog.
//!
//! A dog's name edits `shep.toml` and needs no shepherd. Anything else is a
//! sheep, which only a running shepherd can mark, so with none running the
//! verb refuses and does not start one.

use shep_client::{Client, RequestError};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{Request, Response, RpcErrorCode, SelectorSpec};
use shep_core::selector::ProcessSelector;

use crate::commands::dogs;
use crate::commands::lifecycle::selectors::{flock_now, render_outcome};
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::selector::parse_selector;
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;
use crate::output::{FlockRows, Streams};
use crate::remote::Link;
use crate::version_guard::{VersionGuard, connect_client};

/// Which of the two verbs ran
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Toggle {
    /// `shep enable`
    Enable,
    /// `shep disable`
    Disable,
}

impl Toggle {
    /// The verb as the operator types it
    fn verb(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }

    fn request(self, selector: SelectorSpec) -> Request {
        match self {
            Self::Enable => Request::Enable { selector },
            Self::Disable => Request::Disable { selector },
        }
    }
}

/// `shep enable <name>` or `shep disable <name>`
///
/// A name the daemon config knows as a dog goes through the dog verbs. Any
/// other token is a sheep selector and is sent to the shepherd.
///
/// `yes` is `--yes`, which only a dog's first enable reads: it grants the
/// dog everything it declares without asking.
pub(crate) async fn toggle(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    verb: Toggle,
    name: &str,
    yes: bool,
) -> ExitCode {
    toggle_with(
        streams,
        Some(paths),
        async |streams| match dogs::connect_or_absent(paths, streams).await {
            Ok(Some(client)) => Ok(client),
            Ok(None) => {
                let message = format!(
                    "no shepherd is running; `shep {} {name}` needs one to mark a sheep. \
                     start one with `shep start <target>`",
                    verb.verb()
                );
                Err(streams.fail(ExitCode::DaemonUnreachable, &message))
            }
            Err(code) => Err(code),
        },
        verb,
        name,
        yes,
    )
    .await
}

/// [`toggle`] over a [`Link::Host`]
///
/// There is no dog half: the `shep.toml` here is this machine's, not the
/// host's, so every name goes to the host's shepherd as a sheep selector and
/// is answered there.
pub(crate) async fn toggle_on_host(
    streams: &mut Streams<'_>,
    link: &Link<'_>,
    guard: VersionGuard,
    verb: Toggle,
    name: &str,
) -> ExitCode {
    toggle_with(
        streams,
        None,
        async |streams| connect_client(streams, link, guard).await,
        verb,
        name,
        false,
    )
    .await
}

/// [`toggle`] with the dog half's home and the connection decided by the
/// caller: `local_dogs` is `None` when there is no dog half, and `connect`
/// is asked only if `name` is a sheep selector.
pub(crate) async fn toggle_with(
    streams: &mut Streams<'_>,
    local_dogs: Option<&ShepPaths>,
    connect: impl AsyncFnOnce(&mut Streams<'_>) -> Result<Client, ExitCode>,
    verb: Toggle,
    name: &str,
    yes: bool,
) -> ExitCode {
    if let Some(paths) = local_dogs
        && names_a_dog(paths, name)
    {
        return match verb {
            Toggle::Enable => dogs::enable(streams, paths, name, yes).await,
            Toggle::Disable => dogs::disable(streams, paths, name).await,
        };
    }
    let client = match connect(streams).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    let known = local_dogs.map(known_dogs).unwrap_or_default();
    toggle_sheep(&client, streams, verb, name, &known).await
}

/// Every dog the daemon config can resolve: the built-ins, then the adopted
///
/// An unreadable config lists the built-ins alone, as [`names_a_dog`] reads
/// it as no adoptions.
fn known_dogs(paths: &ShepPaths) -> Vec<String> {
    let adopted = ShepToml::read_only(&paths.daemon_config)
        .map(|cfg| cfg.adopted_dog_names())
        .unwrap_or_default();
    crate::dog::BUILT_IN_DOGS
        .iter()
        .map(|dog| (*dog).to_string())
        .chain(adopted)
        .collect()
}

/// Whether `name` is a built-in dog, an adopted one or one already enabled
///
/// An unreadable config answers `false`: the sheep path then reports what
/// the shepherd says about the name.
fn names_a_dog(paths: &ShepPaths, name: &str) -> bool {
    if crate::dog::BUILT_IN_DOGS.contains(&name) {
        return true;
    }
    ShepToml::read_only(&paths.daemon_config).is_ok_and(|cfg| {
        cfg.adopted_dog_names().iter().any(|dog| dog == name)
            || cfg.enabled_dog_names().iter().any(|dog| dog == name)
    })
}

/// The sheep half of [`toggle`], against a connected shepherd
///
/// An app name, a fold, a regex or `all` is sent as typed: the shepherd owns
/// what a sweep reaches. An id or `name:slot` is refused here, naming the
/// app, so an operator never marks a whole app by naming one instance of it.
///
/// `dogs` is every name a dog answers to. A name that matches nothing is
/// refused with them listed, since a mistyped dog is the usual cause.
pub(crate) async fn toggle_sheep(
    client: &Client,
    streams: &mut Streams<'_>,
    verb: Toggle,
    name: &str,
    dogs: &[String],
) -> ExitCode {
    let selector = match parse_selector(streams, name) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let sweep = !selector.is_exact();
    let spec = match selector {
        ProcessSelector::Instance { name: app, .. } => {
            return refuse_instance(streams, verb, name, &app);
        }
        ProcessSelector::Id(id) => {
            let flock = flock_now(client).await;
            return match flock.iter().find(|info| info.id == id) {
                Some(info) => refuse_instance(streams, verb, name, &info.name),
                None => streams.fail(ExitCode::NotFound, &format!("no sheep has id {id}")),
            };
        }
        sweep_or_name => SelectorSpec::from(&sweep_or_name),
    };
    let names_one_app = matches!(spec, SelectorSpec::Name(_));
    let response = match client.request(verb.request(spec)).await {
        Ok(response) => response,
        Err(RequestError::Rpc(err)) if err.code == RpcErrorCode::Unsupported => {
            let message = format!(
                "this shepherd is too old to {} a sheep; upgrade it with `shep {}`",
                verb.verb(),
                crate::version_guard::VERSION_SKEW_REMEDY
            );
            return streams.fail(ExitCode::Unsupported, &message);
        }
        Err(RequestError::Rpc(err))
            if names_one_app && !dogs.is_empty() && err.code == RpcErrorCode::NotFound =>
        {
            return streams.fail(ExitCode::NotFound, &naming_the_dogs(&err.message, dogs));
        }
        Err(err) => return client_error(streams, &err),
    };
    match (verb, response) {
        (Toggle::Disable, Response::Disabled(procs)) if procs.is_empty() => {
            let said =
                format!("every sheep matching `{name}` is already disabled; nothing changed");
            streams.note("already_disabled", &said);
            ExitCode::Success
        }
        (Toggle::Disable, Response::Disabled(procs)) => {
            render_outcome(client, streams, "disable", FlockRows(procs)).await
        }
        (Toggle::Enable, Response::Enabled(procs)) if procs.is_empty() => {
            let said = if sweep {
                format!("no sheep matching `{name}` is disabled; nothing changed")
            } else {
                format!("`{name}` is not disabled; nothing changed")
            };
            streams.note("not_disabled", &said);
            ExitCode::Success
        }
        (Toggle::Enable, Response::Enabled(procs)) => {
            render_outcome(client, streams, "enable", FlockRows(procs)).await
        }
        _ => unexpected_response(streams),
    }
}

/// The shepherd's `NotFound` words, followed by the dogs a mistyped name
/// could have meant
pub(super) fn naming_the_dogs(said: &str, dogs: &[String]) -> String {
    let quoted: Vec<String> = dogs.iter().map(|dog| format!("{dog:?}")).collect();
    format!("{said}; the dogs are {}", dogs::join_with_and(&quoted))
}

/// Refuses an instance selector with the command that would work
fn refuse_instance(streams: &mut Streams<'_>, verb: Toggle, given: &str, app: &str) -> ExitCode {
    let message = format!(
        "`{given}` is one instance of `{app}`, and `{}` acts on a whole app; \
         run `shep {} {app}`",
        verb.verb(),
        verb.verb()
    );
    streams.fail(ExitCode::Usage, &message)
}
