//! Typed shorthands for the requests more than one caller makes.
//!
//! Each sends one [`Request`] and answers the one [`Response`] it expects,
//! so a caller never writes the match or the error for any other answer.
//! [`Client`] and [`ReconnectingClient`] carry the same set.

use core::future::Future;

use shep_core::config::DogTable;
use shep_core::protocol::{
    DogSectionToml, HostUsage, LogStream, LogTail, ProcessInfo, Request, Response, RpcError,
    RpcErrorCode, SelectorSpec,
};
use shep_core::secrets::references_in;

use crate::ReconnectingClient;
use crate::client::{Client, LOG_PLANE_DEADLINE, RequestError};

impl Client {
    /// The dog `name`'s own section of `dogs.toml` with every
    /// `{{secret:...}}` reference resolved, empty when it has none.
    ///
    /// Asks [`Request::DogConfigResolved`]. A shepherd too old to know it
    /// is asked [`Request::DogConfig`] instead, and a section that still
    /// holds a reference is refused rather than handed over unresolved.
    ///
    /// # Errors
    ///
    /// As [`Self::request`], and [`RequestError::UnexpectedReply`] for any
    /// answer but [`Response::DogSection`]. [`RequestError::Rpc`] with
    /// [`RpcErrorCode::Unsupported`] when an older shepherd's section names
    /// a secret reference it cannot resolve.
    pub async fn dog_config(&self, name: &str) -> Result<DogSectionToml, RequestError> {
        resolved_dog_section(name, |body| self.request(body)).await
    }

    /// The sheep `name`'s `[app.dogs.<dog>]` table, with every
    /// `{{secret:...}}` reference resolved in that sheep's own environment.
    /// `None` when it carries no such table.
    ///
    /// Asks [`Request::SheepDogSettingsResolved`]. A shepherd too old to know
    /// it is asked [`Request::DogSheepSettings`] instead, and a table still
    /// holding a reference is refused rather than handed over unresolved.
    ///
    /// # Errors
    ///
    /// As [`Self::request`]: [`RpcErrorCode::NotFound`] for a name no sheep
    /// holds, and [`RpcErrorCode::InvalidConfig`] for a reference that cannot
    /// resolve. A different answer gives [`RequestError::UnexpectedReply`].
    pub async fn sheep_dog_table(
        &self,
        name: &str,
        dog: &str,
    ) -> Result<Option<DogTable>, RequestError> {
        resolved_sheep_dog_table(name, dog, |body| self.request(body)).await
    }

    /// Every supervised entry, dogs included.
    ///
    /// # Errors
    ///
    /// As [`Self::request`], and [`RequestError::UnexpectedReply`] for any
    /// answer but [`Response::Flock`].
    pub async fn list_flock(&self) -> Result<Vec<ProcessInfo>, RequestError> {
        flock(self.request(Request::ListFlock).await?)
    }

    /// Reopens the log files of every sheep `selector` matches, under
    /// [`LOG_PLANE_DEADLINE`], answering each one it matched.
    ///
    /// # Errors
    ///
    /// As [`Self::request`], and [`RequestError::UnexpectedReply`] for any
    /// answer but [`Response::Reopened`].
    pub async fn reopen(&self, selector: SelectorSpec) -> Result<Vec<ProcessInfo>, RequestError> {
        let body = Request::Reopen { selector };
        reopened(
            self.request_with_deadline(body, Some(LOG_PLANE_DEADLINE))
                .await?,
        )
    }

    /// What the flock's machine is doing, or `None` where the shepherd
    /// cannot read its host at all.
    ///
    /// # Errors
    ///
    /// As [`Self::request`], and [`RequestError::UnexpectedReply`] for any
    /// answer but [`Response::HostUsage`].
    pub async fn host_usage(&self) -> Result<Option<HostUsage>, RequestError> {
        host(self.request(Request::HostUsage).await?)
    }

    /// Publishes `payload` as the event kind `event` of the dog on this
    /// connection: it reaches subscribers as `dog.<name>.<event>`.
    ///
    /// Asks [`Request::PublishEvent`], which only a dog's own connection may
    /// send. The shepherd checks that `event` is in the dog's manifest and
    /// nothing about `payload`.
    ///
    /// # Errors
    ///
    /// As [`Self::request`]: [`RpcErrorCode::Forbidden`] naming the kind when
    /// the dog did not declare it or this is not a dog's connection. A
    /// different answer gives [`RequestError::UnexpectedReply`].
    pub async fn publish_event(
        &self,
        event: &str,
        payload: serde_json::Value,
    ) -> Result<(), RequestError> {
        published(
            self.request(Request::PublishEvent {
                event: event.to_owned(),
                payload,
            })
            .await?,
        )
    }

    /// The last `lines` lines of one sheep's out or err log, read by the
    /// shepherd on its own host.
    ///
    /// A file that is absent, unreadable or not recorded is a [`LogTail`]
    /// variant, not an error.
    ///
    /// # Errors
    ///
    /// As [`Self::request`], including [`RpcErrorCode::NotFound`] for an id
    /// no sheep holds, and [`RequestError::UnexpectedReply`] for any answer
    /// but [`Response::LogTail`].
    pub async fn tail_log(
        &self,
        id: u32,
        stream: LogStream,
        lines: u32,
    ) -> Result<LogTail, RequestError> {
        log_tail(self.request(Request::TailLog { id, stream, lines }).await?)
    }
}

impl ReconnectingClient {
    /// Shorthand for [`Client::publish_event`] on the current generation.
    ///
    /// # Errors
    ///
    /// As [`Client::publish_event`].
    pub async fn publish_event(
        &self,
        event: &str,
        payload: serde_json::Value,
    ) -> Result<(), RequestError> {
        published(
            self.request(Request::PublishEvent {
                event: event.to_owned(),
                payload,
            })
            .await?,
        )
    }

    /// Shorthand for [`Client::dog_config`] on the current generation.
    ///
    /// # Errors
    ///
    /// As [`Client::dog_config`].
    pub async fn dog_config(&self, name: &str) -> Result<DogSectionToml, RequestError> {
        resolved_dog_section(name, |body| self.request(body)).await
    }

    /// Shorthand for [`Client::sheep_dog_table`] on the current generation.
    ///
    /// # Errors
    ///
    /// As [`Client::sheep_dog_table`].
    pub async fn sheep_dog_table(
        &self,
        name: &str,
        dog: &str,
    ) -> Result<Option<DogTable>, RequestError> {
        resolved_sheep_dog_table(name, dog, |body| self.request(body)).await
    }

    /// Shorthand for [`Client::list_flock`] on the current generation.
    ///
    /// # Errors
    ///
    /// As [`Client::list_flock`].
    pub async fn list_flock(&self) -> Result<Vec<ProcessInfo>, RequestError> {
        flock(self.request(Request::ListFlock).await?)
    }

    /// Shorthand for [`Client::reopen`] on the current generation.
    ///
    /// # Errors
    ///
    /// As [`Client::reopen`].
    pub async fn reopen(&self, selector: SelectorSpec) -> Result<Vec<ProcessInfo>, RequestError> {
        let body = Request::Reopen { selector };
        reopened(
            self.request_with_deadline(body, Some(LOG_PLANE_DEADLINE))
                .await?,
        )
    }

    /// Shorthand for [`Client::host_usage`] on the current generation.
    ///
    /// # Errors
    ///
    /// As [`Client::host_usage`].
    pub async fn host_usage(&self) -> Result<Option<HostUsage>, RequestError> {
        host(self.request(Request::HostUsage).await?)
    }

    /// Shorthand for [`Client::tail_log`] on the current generation.
    ///
    /// # Errors
    ///
    /// As [`Client::tail_log`].
    pub async fn tail_log(
        &self,
        id: u32,
        stream: LogStream,
        lines: u32,
    ) -> Result<LogTail, RequestError> {
        log_tail(self.request(Request::TailLog { id, stream, lines }).await?)
    }
}

fn dog_config_request(name: &str) -> Request {
    Request::DogConfigResolved {
        name: name.to_owned(),
    }
}

/// Asks `ask` for the resolved section, and for the plain one when the
/// shepherd does not know the request.
///
/// A shepherd built before the request refuses it with `Unsupported`, which
/// `Request::Unrecognized` earns on the daemon side. `Unrecognized` covers a
/// code this build was not taught.
async fn resolved_dog_section<F, Fut>(name: &str, ask: F) -> Result<DogSectionToml, RequestError>
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Result<Response, RequestError>>,
{
    match ask(dog_config_request(name)).await {
        Err(RequestError::Rpc(refusal)) if is_unknown_request(&refusal) => {
            let plain = Request::DogConfig {
                name: name.to_owned(),
            };
            let section = dog_section(ask(plain).await?)?;
            if !references_in(section.as_str()).is_empty() {
                return Err(RequestError::Rpc(RpcError {
                    code: RpcErrorCode::Unsupported,
                    message: format!(
                        "the shepherd is too old to resolve secret references in the \
                         `{name}` dog's section"
                    ),
                    daemon_version: refusal.daemon_version,
                }));
            }
            Ok(section)
        }
        answer => dog_section(answer?),
    }
}

/// Whether `refusal` is a shepherd saying it does not know the request.
fn is_unknown_request(refusal: &RpcError) -> bool {
    matches!(
        refusal.code,
        RpcErrorCode::Unsupported | RpcErrorCode::Unrecognized
    )
}

/// Asks `ask` for the sheep's resolved table, and for every sheep's plain
/// one when the shepherd does not know the request, as
/// [`resolved_dog_section`] does for a section.
async fn resolved_sheep_dog_table<F, Fut>(
    name: &str,
    dog: &str,
    ask: F,
) -> Result<Option<DogTable>, RequestError>
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Result<Response, RequestError>>,
{
    let resolved = Request::SheepDogSettingsResolved {
        name: name.to_owned(),
        dog: dog.to_owned(),
    };
    match ask(resolved).await {
        Err(RequestError::Rpc(refusal)) if is_unknown_request(&refusal) => {
            let plain = Request::DogSheepSettings {
                dog: dog.to_owned(),
            };
            let table = match ask(plain).await? {
                Response::DogSheepSettings { mut tables } => tables.remove(name),
                other => return Err(unexpected("DogSheepSettings", &other)),
            };
            if table.is_none() {
                ensure_a_sheep(name, &ask).await?;
            }
            if table
                .as_ref()
                .is_some_and(|table| !table.references().is_empty())
            {
                return Err(RequestError::Rpc(RpcError {
                    code: RpcErrorCode::Unsupported,
                    message: format!(
                        "the shepherd is too old to resolve secret references in the \
                         `{name}` sheep's `{dog}` table"
                    ),
                    daemon_version: refusal.daemon_version,
                }));
            }
            Ok(table)
        }
        answer => match answer? {
            Response::SheepDogTable { table } => Ok(table),
            other => Err(unexpected("SheepDogSettingsResolved", &other)),
        },
    }
}

/// Succeeds when the flock holds a sheep named `name`, and is `NotFound`
/// otherwise, a dog's own name included.
///
/// `DogSheepSettings` lists only the sheep that carry a table, so on its
/// own it answers an unknown name and a sheep without the table alike.
/// The request this stands in for tells them apart.
async fn ensure_a_sheep<F, Fut>(name: &str, ask: &F) -> Result<(), RequestError>
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Result<Response, RequestError>>,
{
    let sheep = flock(ask(Request::ListFlock).await?)?;
    if sheep
        .iter()
        .any(|entry| entry.name == name && entry.dog.is_none())
    {
        return Ok(());
    }
    Err(RequestError::Rpc(RpcError {
        code: RpcErrorCode::NotFound,
        message: format!("no sheep named {name}"),
        daemon_version: None,
    }))
}

fn dog_section(answer: Response) -> Result<DogSectionToml, RequestError> {
    match answer {
        Response::DogSection { toml } => Ok(toml),
        other => Err(unexpected("DogConfigResolved", &other)),
    }
}

fn flock(answer: Response) -> Result<Vec<ProcessInfo>, RequestError> {
    match answer {
        Response::Flock(flock) => Ok(flock),
        other => Err(unexpected("ListFlock", &other)),
    }
}

fn reopened(answer: Response) -> Result<Vec<ProcessInfo>, RequestError> {
    match answer {
        Response::Reopened(sheep) => Ok(sheep),
        other => Err(unexpected("Reopen", &other)),
    }
}

fn host(answer: Response) -> Result<Option<HostUsage>, RequestError> {
    match answer {
        Response::HostUsage(usage) => Ok(usage),
        other => Err(unexpected("HostUsage", &other)),
    }
}

fn published(answer: Response) -> Result<(), RequestError> {
    match answer {
        Response::EventPublished => Ok(()),
        other => Err(unexpected("PublishEvent", &other)),
    }
}

fn log_tail(answer: Response) -> Result<LogTail, RequestError> {
    match answer {
        Response::LogTail(tail) => Ok(tail),
        other => Err(unexpected("TailLog", &other)),
    }
}

fn unexpected(asked: &'static str, answer: &Response) -> RequestError {
    RequestError::UnexpectedReply {
        asked,
        answered: answer.name(),
    }
}

#[cfg(test)]
mod tests;
