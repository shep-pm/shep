//! [`DogRuntime`], the connection and configuration every dog starts with.

use core::fmt;

use shep_client::{ConnectError, ReconnectingClient, RequestError};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{Request, Response};

use crate::exit::ExitCode;

/// A dog's connection to the shepherd, and its own configuration.
///
/// Locate the socket from `$SHEP_HOME`, connect, handshake, ask for
/// `[dog.<name>]`, parse it.
pub struct DogRuntime {
    /// The connected client. A dog IS a client; there is no second protocol.
    ///
    /// A [`ReconnectingClient`] rather than a bare
    /// [`Client`](shep_client::Client): a dog's process survives the
    /// shepherd's `execve` for free, but only the listening socket crosses
    /// that exec, so the accepted connection underneath this field dies on
    /// every reload.
    pub client: ReconnectingClient,
    /// This dog's `[dog.<name>]` section, exactly as the shepherd rendered
    /// it, for the dog to parse into its own shape. Empty when the file has
    /// no such section.
    pub section: String,
    /// `$SHEP_HOME` as this dog resolved it.
    pub paths: ShepPaths,
    /// The dog's own name, kept so [`Self::config`] can name it in a
    /// [`DogRunError::Section`] without every caller threading it through
    /// again.
    pub(super) name: String,
}

/// Manual: [`Self::section`] is a dog's raw `[dog.<name>]` config text,
/// which routinely carries a webhook URL with a bearer token in its query
/// string. A derived `Debug` would print it in full. `client` and `paths`
/// carry nothing sensitive and print unchanged.
impl fmt::Debug for DogRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DogRuntime")
            .field("client", &self.client)
            .field("section", &format!("<{} bytes>", self.section.len()))
            .field("paths", &self.paths)
            .field("name", &self.name)
            .finish()
    }
}

/// Why [`DogRuntime::start`] or [`DogRuntime::config`] failed.
pub enum DogRunError {
    /// No shepherd answered at the socket.
    Connect(ConnectError),
    /// The shepherd refused the config request.
    Request(RequestError),
    /// The shepherd answered `Request::DogConfig` with something other
    /// than `Response::DogSection`. Never returned by a daemon on the same
    /// protocol version; kept reportable rather than `unreachable!()`, so
    /// a dog exits cleanly instead of panicking.
    UnexpectedReply,
    /// The section does not fit the shape [`DogRuntime::config`] was asked
    /// to parse it as.
    Section {
        /// The dog's own name.
        name: String,
        /// The parser's full complaint, which can quote the offending
        /// line.
        message: String,
    },
}

/// Manual: [`DogRunError::Section`]'s `message` is the TOML parser's own
/// complaint, which can quote a `[dog.<name>]` webhook URL verbatim.
/// Redacted to the dog's name and a fixed description. `Connect`/`Request`
/// wrap types with their own non-leaking `Debug` already.
impl fmt::Debug for DogRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => f.debug_tuple("Connect").field(err).finish(),
            Self::Request(err) => f.debug_tuple("Request").field(err).finish(),
            Self::UnexpectedReply => f.write_str("UnexpectedReply"),
            Self::Section { name, .. } => f
                .debug_struct("Section")
                .field("name", name)
                .field("message", &"<redacted: may quote the section>")
                .finish(),
        }
    }
}

impl fmt::Display for DogRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "no shepherd answered at the socket: {err}"),
            Self::Request(err) => write!(f, "the shepherd refused the config request: {err}"),
            Self::UnexpectedReply => {
                f.write_str("the shepherd answered with a response this client does not understand")
            }
            Self::Section { name, message } => {
                write!(f, "dog {name}'s own configuration does not fit: {message}")
            }
        }
    }
}

impl core::error::Error for DogRunError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Connect(err) => Some(err),
            Self::Request(err) => Some(err),
            Self::UnexpectedReply | Self::Section { .. } => None,
        }
    }
}

impl From<ConnectError> for DogRunError {
    fn from(source: ConnectError) -> Self {
        Self::Connect(source)
    }
}

impl From<RequestError> for DogRunError {
    fn from(source: RequestError) -> Self {
        Self::Request(source)
    }
}

impl DogRuntime {
    /// Connects and fetches `name`'s section.
    ///
    /// Announces itself as the dog registered under `name`, so a daemon
    /// that refuses this handshake on protocol skew knows which dog it
    /// just refused and can restart it from disk.
    ///
    /// # Errors
    /// - [`DogRunError::Connect`]: no shepherd answered at the socket.
    /// - [`DogRunError::Request`]: the shepherd refused the config request.
    /// - [`DogRunError::UnexpectedReply`]: the shepherd answered
    ///   `Request::DogConfig` with something other than
    ///   `Response::DogSection`.
    pub async fn start(name: &str, paths: ShepPaths) -> Result<Self, DogRunError> {
        let client = ReconnectingClient::connect_as_dog(&paths.socket, name).await?;
        let response = client
            .request(Request::DogConfig {
                name: name.to_string(),
            })
            .await?;
        let Response::DogSection { toml } = response else {
            return Err(DogRunError::UnexpectedReply);
        };
        Ok(Self {
            section: toml.as_str().to_string(),
            client,
            paths,
            name: name.to_string(),
        })
    }

    /// This dog's section parsed into `T`, or `T::default()` when the
    /// shepherd had no section for it.
    ///
    /// # Errors
    /// - [`DogRunError::Section`]: the section does not fit `T`, naming
    ///   the dog and the parser's own message.
    pub fn config<T>(&self) -> Result<T, DogRunError>
    where
        T: serde::de::DeserializeOwned + Default,
    {
        parse_section(&self.section).map_err(|err| DogRunError::Section {
            name: self.name.clone(),
            message: err.to_string(),
        })
    }
}

/// A dog's section parsed into `T`, or `T::default()` when it is empty,
/// which is how the shepherd answers for a dog with no section.
///
/// # Errors
/// The parser's own error. It can quote the section, webhook URLs
/// included, so report the fact rather than the message.
pub(super) fn parse_section<T>(section: &str) -> Result<T, toml::de::Error>
where
    T: serde::de::DeserializeOwned + Default,
{
    if section.is_empty() {
        return Ok(T::default());
    }
    toml::from_str(section)
}

/// Maps a failed [`DogRuntime::start`] to the exit code that reports it.
///
/// `Connect`/`Request` defer to the same `ExitCode` conversions every
/// other verb's client-connect/request failure goes through. `Section` is
/// [`ExitCode::InvalidConfig`]; `UnexpectedReply` is [`ExitCode::Internal`].
pub(super) fn exit_code_for(err: &DogRunError) -> ExitCode {
    match err {
        DogRunError::Connect(inner) => ExitCode::from(inner),
        DogRunError::Request(inner) => ExitCode::from(inner),
        DogRunError::Section { .. } => ExitCode::InvalidConfig,
        DogRunError::UnexpectedReply => ExitCode::Internal,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_client::testing::{fake_reconnecting_client_on, sample_ack, serve_one_request};

    use super::*;
    use crate::dog::tests::test_paths;

    /// Builds a [`DogRuntime`] carrying `section`, backed by a real (if
    /// otherwise unused) connection: the field has to hold one, even
    /// though [`DogRuntime::config`] never touches it. Bridges into its
    /// own fresh Tokio runtime, so call sites stay plain `#[test]`s.
    fn runtime_with_section(section: &str) -> DogRuntime {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (client, _daemon) = fake_reconnecting_client_on(&socket).await;
            DogRuntime {
                client,
                section: section.to_string(),
                paths: test_paths(dir.path(), socket),
                name: "testdog".to_string(),
            }
        })
    }

    #[test]
    fn a_section_that_does_not_fit_is_refused_rather_than_defaulted() {
        #[derive(Debug, Default, serde::Deserialize, PartialEq)]
        #[serde(deny_unknown_fields, default)]
        struct Cfg {
            port: u16,
        }
        let runtime = runtime_with_section("port = \"nine thousand\"\n");
        let err = runtime.config::<Cfg>().unwrap_err();
        assert!(matches!(err, DogRunError::Section { .. }));
        assert!(err.to_string().contains("port"));

        let empty = runtime_with_section("");
        assert_eq!(empty.config::<Cfg>().unwrap(), Cfg::default());
    }

    #[test]
    fn an_empty_section_parses_to_the_defaults_even_with_a_required_field() {
        #[derive(Debug, Default, serde::Deserialize, PartialEq)]
        struct Required {
            port: u16,
        }
        assert!(
            toml::from_str::<Required>("").is_err(),
            "the fixture must refuse an empty document, or this proves nothing"
        );
        assert_eq!(parse_section::<Required>("").unwrap(), Required::default());
    }

    #[tokio::test]
    async fn a_dog_asks_for_its_own_section_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let response = Response::DogSection {
            toml: "webhook = \"https://example.invalid/hook\"\n"
                .to_string()
                .into(),
        };
        let handle = serve_one_request(&socket, sample_ack(), response).await;
        let paths = test_paths(dir.path(), socket);

        let runtime = DogRuntime::start("bark", paths).await.unwrap();

        let envelope = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("DogRuntime::start must reach the wire; it hung instead of connecting")
            .unwrap();
        assert_eq!(
            envelope.body,
            Request::DogConfig {
                name: "bark".to_string()
            }
        );
        assert_eq!(
            runtime.section,
            "webhook = \"https://example.invalid/hook\"\n"
        );
    }

    /// The fake closes right after acking, so the `DogConfig` request that
    /// follows fails and `start` returns an error; the handshake has
    /// already happened by then, and it is the frame under test.
    #[tokio::test]
    async fn a_dog_announces_its_own_name_at_the_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let socket = shep_client::testing::control_address(dir.path());
        let served = shep_client::testing::fake_daemon(&socket, Ok(sample_ack())).await;
        let paths = test_paths(dir.path(), socket);

        let _started = DogRuntime::start("bark", paths).await;

        let hello = tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("DogRuntime::start must reach the wire; it hung instead of connecting")
            .unwrap();
        assert_eq!(
            hello.dog_name.as_deref(),
            Some("bark"),
            "a dog must announce the name it was registered under"
        );
    }

    /// `Debug` on a section mismatch carries the dog's name and a fixed
    /// description, never the parser's message, which can quote a webhook
    /// URL.
    #[test]
    fn dog_run_error_section_debug_never_prints_the_message() {
        let secret = "https://hooks.example.com/services/T00/B00/super-secret-token";
        let err = DogRunError::Section {
            name: "bark".to_string(),
            message: format!("invalid type: string \"{secret}\", expected u16\nin `webhook`"),
        };
        let debug = format!("{err:?}");
        assert!(!debug.contains(secret), "{debug}");
        assert!(!debug.contains("webhook"), "{debug}");
        assert_eq!(
            debug,
            "Section { name: \"bark\", message: \"<redacted: may quote the section>\" }"
        );
    }

    /// `client`'s own `Debug` embeds this test's tempdir socket path, so
    /// the whole struct cannot be one hardcoded exact string; the redacted
    /// `section` field alone gets that pin.
    #[test]
    fn dog_runtime_debug_never_prints_the_section() {
        let secret = "https://hooks.example.com/services/T00/B00/super-secret-token";
        let section = format!("webhook = \"{secret}\"\n");
        let byte_len = section.len();
        let runtime = runtime_with_section(&section);
        let debug = format!("{runtime:?}");
        assert!(!debug.contains(secret), "{debug}");
        assert!(!debug.contains("webhook"), "{debug}");
        assert!(
            debug.contains(&format!("section: \"<{byte_len} bytes>\"")),
            "{debug}"
        );
    }
}
