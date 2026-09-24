//! Typed shorthands for the requests more than one caller makes.
//!
//! Each sends one [`Request`] and answers the one [`Response`] it expects,
//! so a caller never writes the match or the error for any other answer.
//! [`Client`] and [`ReconnectingClient`] carry the same set.

use shep_core::protocol::{
    DogSectionToml, HostUsage, ProcessInfo, Request, Response, SelectorSpec,
};

use crate::ReconnectingClient;
use crate::client::{Client, LOG_PLANE_DEADLINE, RequestError};

impl Client {
    /// The dog `name`'s own section of `dogs.toml`, empty when it has none.
    ///
    /// # Errors
    ///
    /// As [`Self::request`], and [`RequestError::UnexpectedReply`] for any
    /// answer but [`Response::DogSection`].
    pub async fn dog_config(&self, name: &str) -> Result<DogSectionToml, RequestError> {
        dog_section(self.request(dog_config_request(name)).await?)
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
}

impl ReconnectingClient {
    /// Shorthand for [`Client::dog_config`] on the current generation.
    ///
    /// # Errors
    ///
    /// As [`Client::dog_config`].
    pub async fn dog_config(&self, name: &str) -> Result<DogSectionToml, RequestError> {
        dog_section(self.request(dog_config_request(name)).await?)
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
}

fn dog_config_request(name: &str) -> Request {
    Request::DogConfig {
        name: name.to_owned(),
    }
}

fn dog_section(answer: Response) -> Result<DogSectionToml, RequestError> {
    match answer {
        Response::DogSection { toml } => Ok(toml),
        other => Err(unexpected("DogConfig", &other)),
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

fn unexpected(asked: &'static str, answer: &Response) -> RequestError {
    RequestError::UnexpectedReply {
        asked,
        answered: answer.name(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_core::protocol::{Envelope, HostUsage};
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::*;
    use crate::testing::{
        control_address, fake_client_answering, sample_ack, sample_info, serve_one_request,
    };

    /// The one envelope the client under test sent.
    async fn sent(envelopes: &mut UnboundedReceiver<Envelope>) -> Envelope {
        tokio::time::timeout(Duration::from_secs(5), envelopes.recv())
            .await
            .expect("the request reached the fake daemon")
            .expect("the fake daemon is still up")
    }

    #[tokio::test]
    async fn dog_config_asks_by_name_and_answers_the_section() {
        let dir = tempfile::tempdir().unwrap();
        let (client, mut envelopes) =
            fake_client_answering(&control_address(dir.path()), |_| Response::DogSection {
                toml: "poll = \"1m\"\n".to_owned().into(),
            })
            .await;

        let section = client.dog_config("bark").await.unwrap();

        assert_eq!(section.as_str(), "poll = \"1m\"\n");
        assert_eq!(
            sent(&mut envelopes).await.body,
            Request::DogConfig {
                name: "bark".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn list_flock_answers_the_listing() {
        let dir = tempfile::tempdir().unwrap();
        let (client, _envelopes) = fake_client_answering(&control_address(dir.path()), |_| {
            Response::Flock(vec![sample_info()])
        })
        .await;

        assert_eq!(client.list_flock().await.unwrap(), vec![sample_info()]);
    }

    /// The log plane walks the flock file by file, so the default budget
    /// can run out on a slow disk.
    #[tokio::test]
    async fn reopen_names_the_selector_and_asks_for_the_log_plane_budget() {
        let dir = tempfile::tempdir().unwrap();
        let (client, mut envelopes) = fake_client_answering(&control_address(dir.path()), |_| {
            Response::Reopened(vec![sample_info()])
        })
        .await;

        let reopened = client
            .reopen(SelectorSpec::Name("web".to_owned()))
            .await
            .unwrap();

        assert_eq!(reopened, vec![sample_info()]);
        let envelope = sent(&mut envelopes).await;
        assert_eq!(
            envelope.body,
            Request::Reopen {
                selector: SelectorSpec::Name("web".to_owned())
            }
        );
        let budget = u64::try_from(LOG_PLANE_DEADLINE.as_millis()).unwrap();
        assert_eq!(envelope.deadline_ms, Some(budget));
    }

    #[tokio::test]
    async fn host_usage_answers_the_reading() {
        let dir = tempfile::tempdir().unwrap();
        let reading = HostUsage {
            cpu_percent: None,
            memory_used_bytes: 1,
            memory_total_bytes: 2,
            disk_bytes_per_second: None,
            network_bytes_per_second: None,
        };
        let (client, _envelopes) = fake_client_answering(&control_address(dir.path()), move |_| {
            Response::HostUsage(Some(reading))
        })
        .await;

        assert_eq!(client.host_usage().await.unwrap(), Some(reading));
    }

    /// The error names both variants and never the section's body.
    #[tokio::test]
    async fn another_answer_is_named_by_variant_alone() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "https://example.invalid/hook/super-secret-token";
        let (client, _envelopes) = fake_client_answering(&control_address(dir.path()), move |_| {
            Response::DogSection {
                toml: format!("webhook = \"{secret}\"\n").into(),
            }
        })
        .await;

        let err = client.list_flock().await.unwrap_err();

        assert_eq!(
            err,
            RequestError::UnexpectedReply {
                asked: "ListFlock",
                answered: "DogSection",
            }
        );
        assert_eq!(
            err.to_string(),
            "the daemon answered ListFlock with DogSection"
        );
    }

    #[tokio::test]
    async fn a_reconnecting_client_asks_the_same_way() {
        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let served = serve_one_request(
            &socket,
            sample_ack(),
            Response::DogSection {
                toml: String::new().into(),
            },
        )
        .await;
        let client = ReconnectingClient::connect(&socket).await.unwrap();

        let section = client.dog_config("metrics").await.unwrap();

        assert_eq!(section.as_str(), "");
        let envelope = tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("the request reached the fake daemon")
            .unwrap();
        assert_eq!(
            envelope.body,
            Request::DogConfig {
                name: "metrics".to_owned()
            }
        );
    }
}
