//! [`DogRuntime`], the connection and configuration a dog starts with.

use serde::de::DeserializeOwned;
use shep_core::paths::ShepPaths;
use shep_core::protocol::DogSectionToml;

use super::{DogIdentity, SectionError, ShepherdError, parse_section};
use crate::ReconnectingClient;

/// A dog's connection to its shepherd, and its own section of `dogs.toml`.
///
/// `Debug` prints the section's length and never its text, which routinely
/// carries a webhook token.
///
/// # Example
///
/// ```no_run
/// use shep_client::dogs::{DogIdentity, DogRuntime};
/// # use shep_client::shep_core::paths::ShepPaths;
///
/// #[derive(Default, serde::Deserialize)]
/// struct Settings {
///     interval: Option<String>,
/// }
///
/// # async fn dog(paths: ShepPaths) -> Result<(), Box<dyn core::error::Error>> {
/// let env = |key: &str| std::env::var(key).ok();
/// let identity = DogIdentity::from_env(&env, "log-rotate");
/// let runtime = DogRuntime::start(identity, paths).await?;
/// let settings: Settings = runtime.config()?;
/// # let _ = settings.interval;
/// # Ok(())
/// # }
/// # let _ = dog;
/// ```
#[derive(Debug)]
pub struct DogRuntime {
    client: ReconnectingClient,
    section: DogSectionToml,
    paths: ShepPaths,
    identity: DogIdentity,
}

impl DogRuntime {
    /// Connects as `identity` through `paths`' socket and asks for its
    /// section.
    ///
    /// # Errors
    ///
    /// - [`ShepherdError::Connect`]: no shepherd answered, or it refused
    ///   the handshake.
    /// - [`ShepherdError::Request`]: the section request failed, or was
    ///   answered with something other than the section.
    pub async fn start(identity: DogIdentity, paths: ShepPaths) -> Result<Self, ShepherdError> {
        let client = ReconnectingClient::connect_as(&paths.socket, &identity).await?;
        let section = client.dog_config(identity.section()).await?;
        Ok(Self {
            client,
            section,
            paths,
            identity,
        })
    }

    /// This dog's section parsed into `T`, or `T::default()` when the
    /// shepherd had none for it.
    ///
    /// # Errors
    ///
    /// [`SectionError`] when the section does not fit `T`, naming the line.
    pub fn config<T>(&self) -> Result<T, SectionError>
    where
        T: DeserializeOwned + Default,
    {
        parse_section(self.identity.section(), self.section.as_str())
    }

    /// The connection, which re-establishes itself across a handover.
    #[must_use]
    pub fn client(&self) -> &ReconnectingClient {
        &self.client
    }

    /// The section's text as the shepherd served it, empty when there is
    /// none.
    #[must_use]
    pub fn section(&self) -> &DogSectionToml {
        &self.section
    }

    /// The `$SHEP_HOME` layout this dog was started with.
    #[must_use]
    pub fn paths(&self) -> &ShepPaths {
        &self.paths
    }

    /// Which dog this is.
    #[must_use]
    pub fn identity(&self) -> &DogIdentity {
        &self.identity
    }

    /// The connection alone, for a dog that hands it to a task of its own.
    #[must_use]
    pub fn into_client(self) -> ReconnectingClient {
        self.client
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use shep_core::protocol::{Request, Response};

    use super::*;
    use crate::testing::{control_address, fake_daemon, sample_ack, serve_one_request};

    /// A layout rooted at `dir`, with its socket where the fake daemon bound.
    fn paths_in(dir: &Path) -> ShepPaths {
        let mut paths = ShepPaths::resolve(&|_| None, dir);
        paths.socket = control_address(dir);
        paths
    }

    /// Starts a runtime against a fake that serves `section` once.
    async fn started(dir: &Path, section: &str) -> (DogRuntime, Request) {
        let paths = paths_in(dir);
        let served = serve_one_request(
            &paths.socket,
            sample_ack(),
            Response::DogSection {
                toml: section.to_owned().into(),
            },
        )
        .await;
        let runtime = tokio::time::timeout(
            Duration::from_secs(5),
            DogRuntime::start(DogIdentity::named("bark"), paths),
        )
        .await
        .expect("DogRuntime::start hung instead of connecting")
        .unwrap();
        let asked = tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("the request reached the fake daemon")
            .unwrap();
        (runtime, asked.body)
    }

    #[tokio::test]
    async fn a_dog_asks_for_its_own_section_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, asked) = started(dir.path(), "poll = \"1m\"\n").await;

        assert_eq!(
            asked,
            Request::DogConfig {
                name: "bark".to_owned()
            }
        );
        assert_eq!(runtime.section().as_str(), "poll = \"1m\"\n");
        assert_eq!(runtime.identity().section(), "bark");
    }

    /// An unnamed dog still reads its default section, and says nothing
    /// about who it is.
    #[tokio::test]
    async fn an_unnamed_dog_reads_its_default_section_anonymously() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let hello = fake_daemon(&paths.socket, Ok(sample_ack())).await;
        let identity = DogIdentity::from_env(&|_| None, "log-rotate");

        // The fake closes after the handshake, so the request fails; the
        // hello is the frame under test.
        let started =
            tokio::time::timeout(Duration::from_secs(5), DogRuntime::start(identity, paths))
                .await
                .expect("DogRuntime::start hung instead of connecting");

        assert!(matches!(started, Err(ShepherdError::Request(_))));
        let hello = tokio::time::timeout(Duration::from_secs(5), hello)
            .await
            .expect("the fake daemon read the hello")
            .unwrap();
        assert_eq!(hello.dog_name, None);
    }

    #[tokio::test]
    async fn a_section_that_does_not_fit_is_refused_rather_than_defaulted() {
        #[derive(Debug, Default, serde::Deserialize, PartialEq)]
        #[serde(deny_unknown_fields, default)]
        struct Settings {
            port: u16,
        }
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _) = started(dir.path(), "port = \"nine thousand\"\n").await;

        let err = runtime.config::<Settings>().unwrap_err();
        assert_eq!(err.section(), "bark");
        assert_eq!(err.line(), Some(1));
    }

    #[tokio::test]
    async fn no_shepherd_is_a_connect_error() {
        let dir = tempfile::tempdir().unwrap();
        let started = DogRuntime::start(DogIdentity::named("bark"), paths_in(dir.path())).await;
        let err = started.unwrap_err();
        assert!(matches!(err, ShepherdError::Connect(_)), "{err}");
        assert_eq!(err.exit_code(), shep_core::exit::DAEMON_UNREACHABLE);
    }

    /// `client`'s own `Debug` names this test's tempdir, so only the
    /// section's part is pinned exactly.
    #[tokio::test]
    async fn debug_never_prints_the_section() {
        let secret = "https://hooks.example.com/services/T00/B00/super-secret-token";
        let section = format!("webhook = \"{secret}\"\n");
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _) = started(dir.path(), &section).await;

        let debug = format!("{runtime:?}");
        assert!(!debug.contains(secret), "{debug}");
        assert!(
            debug.contains(&format!(
                "section: DogSectionToml(<{} bytes>)",
                section.len()
            )),
            "{debug}"
        );
    }
}
