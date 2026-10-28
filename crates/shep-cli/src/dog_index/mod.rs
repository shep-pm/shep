//! The community dog index: fetching it, validating it, and treating every
//! string in it as hostile input.
//!
//! `shep dogs --available` prints this document to a terminal, so every
//! surviving string passes through [`sanitise`]. Response headers are
//! sanitised a layer below, in [`crate::fetch`].
//!
//! A malformed entry is skipped and counted, never fatal. Only
//! [`IndexError::InsecureUrl`] names the URL. [`parse`] owns the document
//! grammar; this file owns the types, the URL policy and the fetch.
use core::fmt;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::fetch::{self, FetchError};
use crate::terminal_safe::sanitise;

mod parse;

pub use parse::parse_index;

/// Where the index lives when nothing overrides it. An exact file path, so
/// it answers 200 rather than redirecting: [`crate::fetch::get`] refuses
/// redirects.
pub const DEFAULT_INDEX_URL: &str = "https://shep-pm.com/dogs.json";

/// The environment variable that overrides [`DEFAULT_INDEX_URL`], for
/// self-hosting and for the integration tests. Trusted input: whoever can
/// set it can already run `shep`.
pub const INDEX_URL_ENV: &str = "SHEP_DOG_INDEX";

/// Where an entry naming a category this build does not know is filed.
///
/// A category is a grouping heading, so an unknown one means the index is
/// ahead of this binary rather than that the entry is bad. Filing it keeps
/// the dog listed and adoptable. Itself one of [`CATEGORIES`].
const UNKNOWN_CATEGORY_FALLBACK: &str = "other";

/// The seven categories a dog can be filed under, in the docs site's order.
///
/// `the_categories_match_the_docs_site_list` and
/// `the_schema_agrees_with_the_categories_and_source_kinds` hold this equal
/// to `web/src/data/dogs.ts` and `web/public/dogs.schema.json`. This list is
/// compiled in while the index is served live, so an installed binary can be
/// older than the index it reads: see [`UNKNOWN_CATEGORY_FALLBACK`].
const CATEGORIES: [&str; 7] = [
    "logs",
    "metrics",
    "alerts",
    "health",
    "deploy",
    "interactive",
    UNKNOWN_CATEGORY_FALLBACK,
];

/// The only `version` this build's [`parse_index`] accepts. Bump this and
/// the published `dogs.json` together when the wrapper's shape changes. An
/// entry's own shape may grow independently: a bad entry is skipped, not
/// refused.
const SUPPORTED_INDEX_VERSION: u64 = 1;

/// The response cap. A megabyte is roughly two thousand entries at the size
/// the live index's own entries run.
const SIZE_LIMIT: usize = 1 << 20;

/// End-to-end budget for the fetch, connect and TLS handshake included.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Hosts that may serve the index over plain `http://`. See
/// [`require_secure_url`].
const LOOPBACK_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "::1", "[::1]"];

/// One dog an operator could adopt, with every string already sanitised.
///
/// `Debug` needs no redaction: every field came out of a public JSON
/// document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AvailableDog {
    /// The dog's own name, displayed rather than typed.
    pub name: String,
    /// The crate or repository name: the dog's real identity.
    pub package: String,
    /// The name this dog expects to be adopted under. A dog is given no
    /// argv and cannot learn its own adopted name, so `shep adopt <path>
    /// --name <name>` with the wrong `<name>` silently discards its entire
    /// `[dog.<name>]` section. Build an adopt line from this field, never
    /// from [`Self::name`] or [`Self::package`].
    pub adopt_as: String,
    /// One line describing what the dog does.
    pub description: String,
    /// HTTPS URL of the dog's repository.
    pub repo: String,
    /// SPDX license string.
    pub license: String,
    /// Always one of [`CATEGORIES`]: an entry naming anything else is
    /// filed under [`UNKNOWN_CATEGORY_FALLBACK`] rather than dropped.
    pub category: String,
    /// How the dog is built.
    pub source: DogSourceKind,
}

/// How a dog is installed, tagged by `kind` exactly as the index tags it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DogSourceKind {
    /// Installable with `cargo install <package>` from crates.io. The
    /// package name is [`AvailableDog::package`], not a field here.
    Cargo {
        /// The exact version to name, for a dog that needs one. cargo
        /// resolves an absent version as `*`, and `*` never matches a
        /// pre-release. Absent for a dog on a normal release.
        version: Option<String>,
    },
    /// Installable with `cargo install --git <url>`, for a dog that is not
    /// on crates.io.
    CargoGit {
        /// The repository to install from, always `https://`.
        url: String,
    },
    /// Installable with `go install <module>@latest`.
    GoInstall {
        /// The Go module path. Not a URL, and not checked as one.
        module: String,
    },
    /// No one-line installer; `instructions` is prose, never a command to
    /// run.
    Manual {
        /// What the entry says to do instead, sanitised like every other
        /// string here.
        instructions: String,
    },
}

/// A parsed index, with the two counts a caller prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    /// The entries that validated, in the order the document listed them.
    pub dogs: Vec<AvailableDog>,
    /// How many entries were dropped for failing validation.
    pub skipped: usize,
    /// How many surviving entries had something stripped. Once per entry,
    /// never once per field, and never for a skipped entry.
    pub sanitised: usize,
}

/// Why reading the index failed outright, as opposed to the per-entry
/// problems [`Index::skipped`] counts.
///
/// Not `#[non_exhaustive]`: every module in this crate is private, so no
/// out-of-tree consumer can match on it. `Debug` needs no redaction, a dog
/// index URL being a public location rather than a credential.
#[derive(Debug)]
pub enum IndexError {
    /// The index URL was not `https://` and its host was not a loopback
    /// literal. Carries the URL, which is public by construction.
    InsecureUrl(String),
    /// The request itself failed, was refused, or came back malformed.
    Fetch(FetchError),
    /// The bytes were not JSON at all. Carries the parser's complaint,
    /// never the offending bytes.
    Malformed(String),
    /// The bytes were JSON, but the top level was not an object. Distinct
    /// from [`Self::Malformed`]: a bare array is a wrong document, not a
    /// broken one, and an empty listing would be the wrong answer for it.
    NotAnObject,
    /// The document's `version` was missing, or named a version this build
    /// does not understand. `found` is the value exactly as the document
    /// had it, `None` when the key was absent.
    UnsupportedVersion {
        /// The `version` value the document carried, if any.
        found: Option<Value>,
    },
    /// The document named a `version` this build understands, but its
    /// `dogs` field was missing or was not itself an array.
    MissingDogsArray,
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsecureUrl(url) => write!(
                f,
                "dog index url {url} is not https://; the index is read over TLS \
                 unless it is served from loopback"
            ),
            Self::Fetch(source) => write!(f, "{source}"),
            Self::Malformed(reason) => write!(f, "the dog index was not valid json: {reason}"),
            Self::NotAnObject => write!(
                f,
                "the dog index was not a json object -- a bare array is the shape shep 0.1.0 \
                 read, and is no longer accepted"
            ),
            Self::UnsupportedVersion { found } => {
                let found = found
                    .as_ref()
                    .map_or_else(|| "unspecified".to_string(), Value::to_string);
                write!(
                    f,
                    "the dog index is version {found}, which this shep does not understand \
                     (this build reads version {SUPPORTED_INDEX_VERSION}); upgrade shep to read it"
                )
            }
            Self::MissingDogsArray => write!(f, "the dog index has no \"dogs\" array"),
        }
    }
}

impl core::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Fetch(source) => Some(source),
            Self::InsecureUrl(_)
            | Self::Malformed(_)
            | Self::NotAnObject
            | Self::UnsupportedVersion { .. }
            | Self::MissingDogsArray => None,
        }
    }
}

impl From<FetchError> for IndexError {
    fn from(source: FetchError) -> Self {
        Self::Fetch(source)
    }
}

/// Where to read the index from: `SHEP_DOG_INDEX` when it is set,
/// [`DEFAULT_INDEX_URL`] when it is not.
///
/// Set but empty comes back as the empty string rather than falling back,
/// so a script whose variable failed to expand gets a refusal.
pub fn index_url() -> String {
    std::env::var(INDEX_URL_ENV).unwrap_or_else(|_err| DEFAULT_INDEX_URL.to_owned())
}

/// Fetches the index at `url` and parses it.
///
/// The `https://` policy lives here rather than in [`crate::fetch::get`],
/// which speaks either scheme so a test can bind a plain-HTTP port. The
/// refusal happens before a packet is sent.
///
/// # Errors
/// - [`IndexError::InsecureUrl`] if `url` is `http://` and not loopback.
/// - [`IndexError::Fetch`] if the request failed; see [`crate::fetch`].
/// - [`IndexError::Malformed`], [`IndexError::NotAnObject`],
///   [`IndexError::UnsupportedVersion`], [`IndexError::MissingDogsArray`]
///   as [`parse_index`].
pub async fn fetch_index(url: &str) -> Result<Index, IndexError> {
    let target = fetch::parse_url(url)?;
    require_secure_url(url, &target)?;
    let bytes = fetch::get(&target, SIZE_LIMIT, TIMEOUT).await?;
    parse_index(&bytes)
}

/// Refuses a plaintext index URL, unless it points at this machine.
///
/// HTTPS only, with a carve-out for a loopback literal: `SHEP_DOG_INDEX` is
/// an override for testing and self-hosting, and there is no wire between
/// two processes on one host. Exact equality against [`LOOPBACK_HOSTS`], so
/// `http://127.0.0.1.example.com/` cannot talk its way through a prefix or
/// suffix match. The sibling case, `http://evil.com@127.0.0.1/`, no longer
/// reaches here at all: [`fetch::parse_url`] runs first and refuses a
/// `user@` authority outright.
///
/// # Errors
/// - [`IndexError::InsecureUrl`] if `target` is `http://` and its host is
///   not one of [`LOOPBACK_HOSTS`].
fn require_secure_url(url: &str, target: &fetch::Target) -> Result<(), IndexError> {
    if target.https || LOOPBACK_HOSTS.contains(&target.host.as_str()) {
        Ok(())
    } else {
        Err(IndexError::InsecureUrl(url.to_owned()))
    }
}

/// Whether `url` is one this module will print as a link an operator might
/// copy.
fn is_https(url: &str) -> bool {
    url.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::dog_index::parse::tests::{SOURCE_KINDS, one_entry_with_description};

    /// `web/`, three directories above this file, only when it actually
    /// exists.
    ///
    /// It is absent once this crate is extracted on its own, so the drift
    /// guards below skip rather than fail there.
    fn workspace_web_dir() -> Option<PathBuf> {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web"));
        dir.is_dir().then(|| dir.to_path_buf())
    }

    /// Reads `web/{relative}`, or `None` outside the workspace checkout
    /// (see [`workspace_web_dir`]).
    ///
    /// # Panics
    /// Inside the workspace, if `relative` cannot be read: that is real
    /// drift the guard exists to catch.
    #[track_caller]
    fn read_workspace_web_file(relative: &str) -> Option<String> {
        let dir = workspace_web_dir()?;
        Some(
            std::fs::read_to_string(dir.join(relative)).unwrap_or_else(|err| {
                panic!("web/{relative} exists in the workspace but could not be read: {err}")
            }),
        )
    }

    /// The string literals of the array `declaration` names in `dogs.ts`.
    ///
    /// Stops at the array's closing `]` rather than at `];`, since a
    /// declaration may close with `] as const;`. Splits past the `=` first.
    ///
    /// # Panics
    /// If `declaration` is absent, has no initialiser, or never closes.
    #[track_caller]
    fn docs_site_array<'a>(dogs_ts: &'a str, declaration: &str) -> Vec<&'a str> {
        let after = dogs_ts
            .split_once(declaration)
            .unwrap_or_else(|| panic!("web/src/data/dogs.ts declares {declaration}"))
            .1
            .split_once('=')
            .unwrap_or_else(|| panic!("the {declaration} declaration has an initialiser"))
            .1;
        let literal = after
            .split_once(']')
            .unwrap_or_else(|| panic!("the {declaration} array is closed"))
            .0;
        literal.split('"').skip(1).step_by(2).collect()
    }

    /// Serves `body` once as a 200 on an ephemeral loopback port, and
    /// returns the URL to read it from.
    async fn serve_index(body: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            let (mut stream, _peer) = listener.accept().await.unwrap();
            // Drains the request so the client's write never stalls on a
            // full socket buffer.
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        format!("http://127.0.0.1:{}/dogs.json", addr.port())
    }

    #[test]
    fn the_unknown_category_fallback_is_itself_a_known_category() {
        assert!(
            CATEGORIES.contains(&UNKNOWN_CATEGORY_FALLBACK),
            "the fallback must be a category the index groups under"
        );
    }

    /// Skips outside the workspace checkout, see
    /// [`read_workspace_web_file`].
    #[test]
    fn the_source_kinds_match_the_docs_site_list() {
        let Some(dogs_ts) = read_workspace_web_file("src/data/dogs.ts") else {
            return;
        };

        let site = docs_site_array(&dogs_ts, "SOURCE_KINDS");

        let ours: Vec<&str> = SOURCE_KINDS.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            site, ours,
            "web/src/data/dogs.ts and dog_index.rs disagree about the source kinds"
        );
    }

    /// Two independent lists in two languages, and nothing but
    /// this test holds them equal. Only the runtime array is read: the
    /// `DogCategory` union above it is what the array is typed against, so
    /// TypeScript fails the site's own build if those two disagree.
    ///
    /// Skips outside the workspace checkout, see
    /// [`read_workspace_web_file`].
    #[test]
    fn the_categories_match_the_docs_site_list() {
        let Some(dogs_ts) = read_workspace_web_file("src/data/dogs.ts") else {
            return;
        };

        let site = docs_site_array(&dogs_ts, "CATEGORIES");

        assert_eq!(
            site,
            CATEGORIES.to_vec(),
            "web/src/data/dogs.ts and dog_index.rs disagree about the categories"
        );
    }

    /// The checked-in editor schema (`web/public/dogs.schema.json`)
    /// against both lists this file enforces.
    ///
    /// Skips outside the workspace checkout, see
    /// [`read_workspace_web_file`].
    #[test]
    fn the_schema_agrees_with_the_categories_and_source_kinds() {
        let Some(schema) = read_workspace_web_file("public/dogs.schema.json") else {
            return;
        };
        let schema: Value = serde_json::from_str(&schema).expect("dogs.schema.json is valid json");
        let entry_schema = &schema["properties"]["dogs"]["items"];

        let schema_categories: Vec<&str> = entry_schema["properties"]["category"]["enum"]
            .as_array()
            .expect("category.enum is an array")
            .iter()
            .map(|v| v.as_str().expect("each category is a string"))
            .collect();
        assert_eq!(
            schema_categories,
            CATEGORIES.to_vec(),
            "dogs.schema.json and dog_index.rs disagree about the categories"
        );

        let schema_kinds: Vec<&str> = entry_schema["properties"]["source"]["oneOf"]
            .as_array()
            .expect("source.oneOf is an array")
            .iter()
            .map(|variant| {
                variant["properties"]["kind"]["const"]
                    .as_str()
                    .expect("each source variant names a const kind")
            })
            .collect();
        let ours: Vec<&str> = SOURCE_KINDS.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            schema_kinds, ours,
            "dogs.schema.json and dog_index.rs disagree about the source kinds"
        );
    }

    /// It is the one URL an operator never types, so nothing else would
    /// notice.
    #[test]
    fn the_default_index_url_is_https() {
        assert!(
            DEFAULT_INDEX_URL.starts_with("https://"),
            "{DEFAULT_INDEX_URL}"
        );
    }

    /// The refusal lands before any connection is attempted, so this can
    /// name a host it never reaches.
    #[tokio::test]
    async fn a_plain_http_index_url_is_refused_before_it_connects() {
        let err = fetch_index("http://example.com/dogs.json")
            .await
            .expect_err("refused");
        let IndexError::InsecureUrl(url) = err else {
            panic!("wrong variant: {err:?}")
        };
        assert_eq!(url, "http://example.com/dogs.json");
    }

    #[tokio::test]
    async fn a_host_that_merely_contains_a_loopback_literal_is_still_refused() {
        for url in [
            "http://127.0.0.1.example.com/dogs.json",
            "http://localhost.example.com/dogs.json",
        ] {
            assert!(
                matches!(fetch_index(url).await, Err(IndexError::InsecureUrl(_))),
                "{url} was not refused"
            );
        }
    }

    /// The third URL that used to sit in the loop above. It is still
    /// refused, but a step earlier and by a different rule:
    /// [`fetch::parse_url`] refuses a `user@` authority before
    /// [`require_secure_url`] ever sees a host to compare.
    #[tokio::test]
    async fn a_loopback_literal_behind_a_userinfo_prefix_is_refused_at_parse_time() {
        let err = fetch_index("http://evil.com@127.0.0.1/dogs.json")
            .await
            .expect_err("refused");
        assert!(
            matches!(err, IndexError::Fetch(FetchError::Url(_))),
            "wrong variant: {err:?}"
        );
    }

    /// The await's forcing mechanism is `fetch_index`'s own ten second
    /// budget: a server that never answers fails this test rather than
    /// hanging it.
    #[tokio::test]
    async fn a_loopback_http_index_is_read_because_that_is_how_a_local_one_is_served() {
        let url = serve_index(one_entry_with_description("clean\u{1b}[2Jhere")).await;
        let index = fetch_index(&url).await.expect("read");
        assert_eq!(index.dogs.len(), 1);
        assert_eq!(index.sanitised, 1);
        assert!(!index.dogs[0].description.contains('\u{1b}'));
    }
}
