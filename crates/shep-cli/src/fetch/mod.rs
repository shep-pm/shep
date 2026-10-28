//! A bounded, redirect-refusing GET over `tokio-rustls`: [`get`], plus the
//! URL parsing ([`parse_url`]) and TLS trust ([`root_store`],
//! [`tls_connector`]) it shares with `dog::bark::sinks`. A URL carrying
//! credentials is refused.
//!
//! [`get`] refuses, in this order: a 3xx naming a `Location`; any other
//! non-2xx; any `Transfer-Encoding`; a bad `Content-Length`; and a body above
//! the caller's `limit`. The head has its own cap, [`MAX_HEADER_BYTES`].
//!
//! The `otel` feature adds `post_json` in the `post` submodule.
use core::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

use crate::terminal_safe;

/// The POST half, compiled only for a build with the `otel` feature.
#[cfg(feature = "otel")]
mod post;

/// The one item outside this module uses from it, kept at the path the rest
/// of the crate reaches it as. Its `PostReply` is named where it is made.
#[cfg(feature = "otel")]
pub use post::post_json;

/// Connect, TLS handshake, write and flush: the half every request here shares.
mod transport;

pub(crate) use transport::connect_and_send;

/// The most this module will read before the blank line that ends a
/// response's header block, status line included.
///
/// 64 KiB: far more than any real response needs, and the only bound on a
/// peer that never sends a newline. Not the caller's `limit`, which is a
/// body budget.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

/// A URL, parsed into what [`get`] needs to reach it.
///
/// `Debug` is REDACTED (IR-41), and `path` is the field that needs it: a
/// Discord or Slack webhook URL carries its token as a path segment, which
/// is why [`Sink`](crate::dog::bark::sinks::Sink) redacts its own `Debug`.
///
/// `host` needs no redaction: [`parse_url`] refuses a credentialed authority.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    /// `true` for `https://`, `false` for `http://`.
    pub https: bool,
    /// The host, without a port.
    pub host: String,
    /// The port: the URL's own, or the scheme's default (443/80).
    pub port: u16,
    /// The request path, always starting with `/`.
    pub path: String,
}

impl Target {
    /// The port a URL of this scheme reaches when it names none: 443 for
    /// `https://`, 80 for `http://`.
    ///
    /// One spelling for both readers of the rule: [`parse_url`], which
    /// assigns it, and [`build_get_request`], which decides from it whether
    /// the `Host` header names a port at all.
    const fn default_port(https: bool) -> u16 {
        if https { 443 } else { 80 }
    }
}

/// Manual: a derived `Debug` would print `path` in full, and `path` is
/// where a webhook's own credential lives. Every `Target` collapses to its
/// scheme, host and port, with `path` withheld.
impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Target {{ https: {}, host: {:?}, port: {}, path: <redacted> }}",
            self.https, self.host, self.port
        )
    }
}

/// Why [`parse_url`] or [`get`] failed.
///
/// `Debug` needs no redaction: every field is a URL this module was given
/// or redirected to, an HTTP status, a byte count, or an OS error.
#[derive(Debug)]
pub enum FetchError {
    /// `url` did not parse as an absolute `http://`/`https://` URL. Carries
    /// a human-readable reason, never the raw bytes of a malformed input.
    ///
    /// The reason quotes `url` only through [`url_for_message`]. An authority
    /// that carries `user@` or `user:pass@` gets its own reason,
    /// [`CREDENTIALS_REFUSAL`].
    Url(String),
    /// The connection failed, the TLS handshake failed, or the response
    /// was not well-formed HTTP: no parseable status line, a header block
    /// that never reached its terminating blank line, or a `Content-Length`
    /// that was missing, not a number, or contradicted by a second
    /// `Content-Length` header on the same response.
    Transport(std::io::Error),
    /// The response was outside 2xx (including a 3xx with no `Location` to
    /// report as a [`Self::Redirect`]).
    Status(u16),
    /// The response was a 3xx naming a `Location`, refused rather than
    /// followed.
    Redirect {
        /// The `Location` header's value, [`crate::terminal_safe::sanitise`]d
        /// at capture: a string the host chose that this `Display` prints
        /// to a terminal.
        location: String,
    },
    /// The response carried a `Transfer-Encoding` header. This client reads
    /// exactly `Content-Length` bytes, so a chunked body would be
    /// misparsed rather than decoded.
    Chunked,
    /// The declared or actual body size exceeded the caller's limit.
    TooLarge {
        /// The limit the caller passed to [`get`].
        limit: usize,
    },
    /// The status line and header block together ran past
    /// [`MAX_HEADER_BYTES`] without reaching the blank line that ends them.
    /// Separate from [`Self::TooLarge`], which is the caller's body budget.
    HeadersTooLarge {
        /// [`MAX_HEADER_BYTES`], named here so the message can state it.
        limit: usize,
    },
    /// `timeout` elapsed before the exchange finished.
    Timeout,
    /// The peer closed the connection before `Content-Length` bytes
    /// arrived.
    Truncated {
        /// The `Content-Length` the response declared.
        expected: usize,
        /// How many bytes actually arrived before the peer closed.
        got: usize,
    },
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(reason) => write!(f, "not a fetchable url: {reason}"),
            Self::Transport(source) => write!(f, "fetch failed: {source}"),
            Self::Status(code) => write!(f, "fetch answered {code}"),
            Self::Redirect { location } => write!(f, "fetch was redirected to {location}"),
            Self::Chunked => {
                write!(
                    f,
                    "fetch response used transfer-encoding, which this client refuses to decode"
                )
            }
            Self::TooLarge { limit } => write!(f, "fetch response exceeded the {limit}-byte limit"),
            Self::HeadersTooLarge { limit } => {
                write!(f, "fetch response headers exceeded the {limit}-byte limit")
            }
            Self::Timeout => write!(f, "fetch timed out"),
            Self::Truncated { expected, got } => {
                write!(
                    f,
                    "fetch response was truncated: expected {expected} bytes, got {got}"
                )
            }
        }
    }
}

impl core::error::Error for FetchError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Transport(source) => Some(source),
            Self::Url(_)
            | Self::Status(_)
            | Self::Redirect { .. }
            | Self::Chunked
            | Self::TooLarge { .. }
            | Self::HeadersTooLarge { .. }
            | Self::Timeout
            | Self::Truncated { .. } => None,
        }
    }
}

impl From<std::io::Error> for FetchError {
    fn from(source: std::io::Error) -> Self {
        Self::Transport(source)
    }
}

/// The reason [`parse_url`] and [`super::dog::bark::sinks`] both give for
/// a URL carrying credentials. It names no URL, which is the point: the
/// text being refused is the text holding the password.
pub const CREDENTIALS_REFUSAL: &str = concat!(
    "credentials before the host (`user@` or `user:pass@`) are not supported; ",
    "the url is not echoed, since it carries one"
);

/// How `url` may be named in a message: itself, or a fixed placeholder.
///
/// Blunter than [`url_carries_credentials`] and the only rule any message
/// consults: an `@` anywhere is enough to decline. Two rules for one question
/// drift, and printing a url a second rule let through leaks its password.
pub fn url_for_message(url: &str) -> &str {
    if url.contains('@') {
        "a url that may carry credentials"
    } else {
        url
    }
}

/// Where `rest`, an authority followed by whatever came after it, stops
/// being the authority.
///
/// `/`, `?` and `#` all end it. Splitting on `/` alone reads
/// `example.com?contact=alice@example.com` as one authority, which makes
/// [`url_carries_credentials`] call an `@` in a query a credential.
fn authority_of(rest: &str) -> &str {
    match rest.find(['/', '?', '#']) {
        Some(i) => &rest[..i],
        None => rest,
    }
}

/// Whether `url`'s authority carries a `user@` or `user:pass@` prefix.
///
/// The rule [`parse_url`] refuses on, separated out so a caller can refuse
/// the same shape at config-load time.
///
/// Blind to the scheme, so a refusal never echoes the url. `mailto:` reads as
/// credentials too; a wrong reason is cheaper than a printed password.
pub fn url_carries_credentials(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
    // A scheme-relative url has an authority with no scheme in front of
    // it, so there is no `://` to split on and the authority would
    // otherwise read as the empty string before the first `/`.
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    authority_of(rest).contains('@')
}

/// Parses `url` into a [`Target`] naming where [`get`] should connect.
///
/// Credentials before the host are refused rather than stripped.
/// `ProbeTarget::parse` refuses the same shape. The authority ends at the
/// first `/`, `?` or `#`; a fragment is dropped.
///
/// # Errors
/// - [`FetchError::Url`] if `url` does not start with `http://` or
///   `https://`, carries a `user@` or `user:pass@` authority, names a
///   non-numeric port, or names no host. No such message quotes a `url`
///   holding an `@`; see [`url_for_message`].
pub fn parse_url(url: &str) -> Result<Target, FetchError> {
    // Ahead of the host/port split, which trusts the last colon: otherwise
    // `user:pass@host:8443` parses to the host `user:pass@host`, and a password
    // reaches a `Target` field or an error message quoting the whole URL.
    if url_carries_credentials(url) {
        return Err(FetchError::Url(CREDENTIALS_REFUSAL.to_owned()));
    }
    let (https, rest) = match url.strip_prefix("https://") {
        Some(rest) => (true, rest),
        None => match url.strip_prefix("http://") {
            Some(rest) => (false, rest),
            None => {
                return Err(FetchError::Url(format!(
                    "{} does not start with http:// or https://",
                    url_for_message(url)
                )));
            }
        },
    };
    let authority = authority_of(rest);
    let remainder = &rest[authority.len()..];
    // A fragment is the client's own: RFC 3986 gives it no meaning to a
    // server and RFC 7230's origin-form target has no field for it, so
    // carrying it into `path` would put it on the wire in the request
    // line. A query is the opposite and belongs there.
    let remainder = remainder.split('#').next().unwrap_or(remainder);
    // A `?` remainder has no leading `/` of its own and an origin-form
    // target needs one, as does an absent path.
    let path = match remainder.chars().next() {
        Some('/') => remainder.to_owned(),
        Some(_) => format!("/{remainder}"),
        None => "/".to_owned(),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse().map_err(|_err| {
                FetchError::Url(format!("{} has a non-numeric port", url_for_message(url)))
            })?,
        ),
        None => (authority, Target::default_port(https)),
    };
    if host.is_empty() {
        return Err(FetchError::Url(format!(
            "{} has no host",
            url_for_message(url)
        )));
    }
    Ok(Target {
        https,
        host: host.to_string(),
        port,
        path,
    })
}

/// The roots every `https://` connection this crate makes trusts: the
/// public set compiled in by `webpki-roots`, built once on first use.
///
/// This is shep's whole trust policy: no `ca_file`, no OS trust store, no
/// way to skip verification. A private-CA or self-signed endpoint is
/// refused, and `http://` is the private route. Let's Encrypt, a
/// Cloudflare-proxied hostname and Tailscale all chain into it.
pub fn root_store() -> &'static tokio_rustls::rustls::RootCertStore {
    static ROOTS: std::sync::LazyLock<tokio_rustls::rustls::RootCertStore> =
        std::sync::LazyLock::new(|| {
            tokio_rustls::rustls::RootCertStore::from_iter(
                webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
            )
        });
    &ROOTS
}

/// The TLS connector every `https://` connection this crate makes shares,
/// built once on first use: a fresh one per connection would re-derive the
/// cipher suite set every time.
pub fn tls_connector() -> &'static tokio_rustls::TlsConnector {
    static CONNECTOR: std::sync::LazyLock<tokio_rustls::TlsConnector> =
        std::sync::LazyLock::new(|| {
            let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
            let config = tokio_rustls::rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring's default cipher suites cover rustls's own default protocol versions")
                .with_root_certificates(root_store().clone())
                .with_no_client_auth();
            tokio_rustls::TlsConnector::from(Arc::new(config))
        });
    &CONNECTOR
}

/// Fetches `target` with a single GET, refusing anything but a plain 2xx
/// body no larger than `limit` bytes, bounded end to end by `timeout`.
///
/// # Errors
/// Every [`FetchError`] variant can come out of this call except
/// [`FetchError::Url`], which only [`parse_url`] produces. The module doc
/// lists the refusal order.
pub async fn get(target: &Target, limit: usize, timeout: Duration) -> Result<Vec<u8>, FetchError> {
    match tokio::time::timeout(timeout, get_inner(target, limit)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(FetchError::Timeout),
    }
}

/// Connects to `target`, sends `request` and reads the reply with
/// [`read_response`].
async fn get_inner(target: &Target, limit: usize) -> Result<Vec<u8>, FetchError> {
    let stream = connect_and_send(target, build_get_request(target).as_bytes()).await?;
    read_response(stream, limit).await
}

/// The request line and headers [`get_inner`] sends. `Host` names the port
/// only when it is off the scheme's own default (443/80), and there is no
/// `Accept-Encoding`.
fn build_get_request(target: &Target) -> String {
    let default_port = Target::default_port(target.https);
    let host = if target.port == default_port {
        target.host.clone()
    } else {
        format!("{}:{}", target.host, target.port)
    };
    format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n",
        path = target.path,
    )
}

/// Reads the status line, then headers to the blank line that ends them,
/// then exactly `Content-Length` bytes of body, refusing in the order the
/// module doc lists.
async fn read_response<S: AsyncRead + Unpin>(
    stream: S,
    limit: usize,
) -> Result<Vec<u8>, FetchError> {
    // One budget for the status line and every header line together, spent
    // through a `Take`: `read_line` itself is what is unbounded. Out of
    // budget it reports a clean zero, exactly as EOF does, and
    // `headers.limit()` is what tells the two apart below.
    let mut headers = BufReader::new(stream).take(MAX_HEADER_BYTES as u64);

    let mut status_line = String::new();
    headers.read_line(&mut status_line).await?;
    let code = parse_status_line(&status_line)?;

    let mut location: Option<String> = None;
    let mut transfer_encoding = false;
    let mut content_length: Option<u64> = None;
    // Set on an unparseable value or a second, disagreeing
    // `Content-Length`; the refusal is deferred to after the loop so it
    // keeps its place in the documented order.
    let mut content_length_ok = true;

    loop {
        let mut line = String::new();
        let read = headers.read_line(&mut line).await?;
        if read == 0 {
            if headers.limit() == 0 {
                return Err(FetchError::HeadersTooLarge {
                    limit: MAX_HEADER_BYTES,
                });
            }
            return Err(FetchError::Transport(std::io::Error::other(
                "response headers ended without a blank line",
            )));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(FetchError::Transport(std::io::Error::other(
                "malformed header line: no colon",
            )));
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            // Sanitised at this one seam, where a header value becomes an
            // owned string this module keeps. Not hoisted over the whole
            // `match`: `content-length` is parsed below, and cleaning it
            // first would repair `1\u{200b}2` into a `12` this would honour.
            "location" => location = Some(terminal_safe::sanitise(value).0),
            "transfer-encoding" => transfer_encoding = true,
            "content-length" => match value.parse::<u64>() {
                Ok(parsed) => match content_length {
                    Some(existing) if existing != parsed => content_length_ok = false,
                    Some(_) => {}
                    None => content_length = Some(parsed),
                },
                Err(_err) => content_length_ok = false,
            },
            _ => {}
        }
    }

    if (300..400).contains(&code)
        && let Some(location) = location
    {
        return Err(FetchError::Redirect { location });
    }
    // A 3xx with no Location has nothing left to name.
    if !(200..300).contains(&code) {
        return Err(FetchError::Status(code));
    }
    if transfer_encoding {
        return Err(FetchError::Chunked);
    }
    if !content_length_ok {
        return Err(FetchError::Transport(std::io::Error::other(
            "content-length header was not a number, or two content-length headers disagreed",
        )));
    }
    let Some(content_length) = content_length else {
        return Err(FetchError::Transport(std::io::Error::other(
            "response carried no content-length",
        )));
    };
    if content_length > limit as u64 {
        return Err(FetchError::TooLarge { limit });
    }
    // Safe: just checked `content_length <= limit`, and `limit` is itself a
    // `usize`, so `content_length` fits in one.
    let expected = content_length as usize;

    // Anything the `BufReader` read past the blank line is still in its
    // buffer, so unwrapping the `Take` resumes where the header loop
    // stopped.
    let mut reader = headers.into_inner();

    let mut body = vec![0u8; expected];
    let mut filled = 0;
    while filled < expected {
        let read = reader.read(&mut body[filled..]).await?;
        if read == 0 {
            return Err(FetchError::Truncated {
                expected,
                got: filled,
            });
        }
        filled += read;
        // Unreachable in practice: `body` is sized to `expected`, which
        // `limit` already bounds, so `filled` cannot pass it here.
        if filled > limit {
            return Err(FetchError::TooLarge { limit });
        }
    }
    Ok(body)
}

/// The status code out of `status_line` (`"HTTP/1.1 200 OK\r\n"`), refusing
/// anything not shaped like an HTTP status line at all.
pub(crate) fn parse_status_line(status_line: &str) -> Result<u16, std::io::Error> {
    let mut parts = status_line.split_whitespace();
    match parts.next() {
        Some(version) if version.starts_with("HTTP/") => {}
        _ => {
            return Err(std::io::Error::other(
                "response did not start with an HTTP status line",
            ));
        }
    }
    parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| std::io::Error::other("malformed http status code"))
}

#[cfg(test)]
mod tests;
