//! The table: which permission each [`Request`] takes.

use crate::protocol::{Request, SelectorSpec};

use super::Permission;

/// What a request needs of the dog that sent it.
///
/// Most requests take one fixed permission. The rest depend on something only
/// the shepherd can look up, and say what to look up rather than guessing.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Need<'r> {
    /// Nothing: the handshake, and a dog's own section and tables.
    Always,
    /// This permission.
    Permission(Permission),
    /// A request aimed by a selector: `sheep` for each sheep it reaches, and
    /// `dog` of the name for each dog it reaches. A selector that reaches
    /// nothing needs `sheep`.
    Selector {
        /// Who the request is aimed at.
        selector: &'r SelectorSpec,
        /// What reaching a sheep takes.
        sheep: Permission,
        /// What reaching the dog of this name takes.
        dog: fn(&str) -> Permission,
    },
    /// A subscription: each topic pattern is checked against the granted
    /// `events:` prefixes.
    Topics(&'r [String]),
    /// A sheep's table for a dog, read with its secrets resolved. Takes
    /// `secrets.read` scoped to the environment the sheep resolves in, which
    /// only the sheep knows.
    ResolvedRead {
        /// The sheep's name.
        sheep: &'r str,
        /// Whose table.
        dog: &'r str,
    },
}

/// The permission `request` takes when sent by the dog named `caller`.
///
/// An exhaustive match with no wildcard: a new [`Request`] does not compile
/// until it is placed. A request about the caller's own section or tables
/// takes [`Need::Always`]; the same request about another dog takes a group.
#[must_use]
pub fn need<'r>(request: &'r Request, caller: &str) -> Need<'r> {
    use Permission as P;
    let one = Need::Permission;
    // A dog's name is also the key of its section and of its tables.
    let own_or = |dog: &str, group: Permission| {
        if dog == caller {
            Need::Always
        } else {
            Need::Permission(group)
        }
    };
    match request {
        Request::Ping | Request::Unrecognized => Need::Always,
        Request::ListFlock
        | Request::HostUsage
        | Request::Describe { .. }
        | Request::ConfigDrift { .. }
        | Request::SheepConfig { .. }
        | Request::DogStaleness
        | Request::HandoverFitness => one(P::FlockRead),
        Request::Start { .. }
        | Request::Stop { .. }
        | Request::Restart { .. }
        | Request::Reset { .. }
        | Request::Reload { .. }
        | Request::Signal { .. }
        | Request::SendLine { .. }
        | Request::Enable { .. }
        | Request::Disable { .. }
        | Request::Muster
        | Request::SaveRoll => one(P::FlockControl),
        Request::Add { .. }
        | Request::ApplyConfig { .. }
        | Request::Scale { .. }
        | Request::SetSheepField { .. }
        | Request::ResetSheepField { .. }
        | Request::SetSheepEnv { .. }
        | Request::SetSheepEnvBatch { .. }
        | Request::CopySheepEnv { .. }
        | Request::SetSmit { .. } => one(P::FlockEdit),
        Request::TailLog { .. } => one(P::LogsRead),
        Request::Reopen { .. } | Request::Flush { .. } => one(P::LogsManage),
        Request::Subscribe { topics } => Need::Topics(topics),
        Request::PutSecrets { namespace, .. } => one(P::SecretsWrite(namespace.clone())),
        Request::SheepDogSettingsResolved { name, dog } => Need::ResolvedRead { sheep: name, dog },
        Request::EnableDog { .. } | Request::DisableDog { .. } => one(P::DogsControl),
        Request::DogConfig { name }
        | Request::DogConfigResolved { name }
        | Request::SetDogConfig { name, .. } => own_or(name, P::DogsControl),
        Request::DogSheepSettings { dog } => own_or(dog, P::FlockRead),
        Request::SetSheepDogSettings { dog, .. } => own_or(dog, P::FlockEdit),
        Request::DogDefaults { dog } => own_or(dog, P::FlockRead),
        Request::SetDogDefaults { .. } => one(P::FlockEdit),
        Request::Trigger { selector, .. } => Need::Selector {
            selector,
            sheep: P::FlockControl,
            dog: |name| P::DogsTrigger(name.to_string()),
        },
        Request::Delete { selector } => Need::Selector {
            selector,
            sheep: P::FlockEdit,
            dog: |_| P::DogsControl,
        },
        Request::KillDaemon => one(P::ShepherdControl),
    }
}
