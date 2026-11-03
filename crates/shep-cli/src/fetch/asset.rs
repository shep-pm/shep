//! [`get_asset`]: [`get`](super::get) for a release asset, which GitHub
//! answers with one redirect to its asset host.
//!
//! Exactly one redirect is followed, and only from the policy's host to one
//! of its asset hosts over https. A second hop, any other host and plain
//! http all refuse. The index fetch keeps using [`get`](super::get), which
//! refuses every redirect.

use core::fmt;
use std::time::Duration;

use super::{FetchError, Target, get_inner, parse_url, url_for_message};

/// Where an asset fetch may be sent on.
#[derive(Debug, Clone, Copy)]
pub struct RedirectPolicy {
    /// The only host a redirect may come from.
    from_host: &'static str,
    /// The hosts it may go to.
    to_hosts: &'static [&'static str],
    /// Whether the first URL and the redirect must both be https.
    https_only: bool,
}

impl RedirectPolicy {
    /// A GitHub release asset: from `github.com` to the host GitHub serves
    /// release assets from, over https.
    pub const GITHUB_RELEASES: Self = Self {
        from_host: "github.com",
        to_hosts: &[
            "release-assets.githubusercontent.com",
            "objects.githubusercontent.com",
            "github-releases.githubusercontent.com",
        ],
        https_only: true,
    };

    /// A policy over plain http, so a test can stand a loopback server in for
    /// both hosts.
    #[cfg(test)]
    pub(crate) const fn loopback(
        from_host: &'static str,
        to_hosts: &'static [&'static str],
    ) -> Self {
        Self {
            from_host,
            to_hosts,
            https_only: false,
        }
    }

    /// The first request's target, or why it may not be made.
    fn admit_first(&self, url: &str) -> Result<Target, AssetError> {
        let target = parse_url(url)?;
        if self.https_only && !target.https {
            return Err(AssetError::NotHttps {
                url: url_for_message(url).to_owned(),
            });
        }
        Ok(target)
    }

    /// Where the redirect `location`, answered by `from`, may lead.
    ///
    /// # Errors
    /// [`AssetError::NotHttps`] for a plain-http location,
    /// [`AssetError::OffHost`] for one off the policy's hosts or from a host
    /// that may not redirect, and [`AssetError::Fetch`] for one that is not
    /// an absolute URL or carries credentials.
    pub(crate) fn admit_redirect(
        &self,
        from: &Target,
        location: &str,
    ) -> Result<Target, AssetError> {
        let next = parse_url(location)?;
        if self.https_only && !next.https {
            return Err(AssetError::NotHttps {
                url: location.to_owned(),
            });
        }
        let allowed = from.host.eq_ignore_ascii_case(self.from_host)
            && self
                .to_hosts
                .iter()
                .any(|host| next.host.eq_ignore_ascii_case(host));
        if !allowed {
            return Err(AssetError::OffHost {
                location: location.to_owned(),
            });
        }
        Ok(next)
    }
}

/// Why [`get_asset`] failed.
///
/// `Debug` needs no redaction: every field is a URL this module refused to
/// follow, already cleaned of what could drive a terminal, or a
/// [`FetchError`].
#[derive(Debug)]
pub enum AssetError {
    /// A URL, the first or a redirect's, that is not https where the policy
    /// requires it.
    NotHttps {
        /// The URL, as [`url_for_message`] allows it to be named.
        url: String,
    },
    /// A redirect to a host the policy does not name, or from a host it does
    /// not let redirect.
    OffHost {
        /// The `Location` the response named.
        location: String,
    },
    /// The asset host answered with a redirect of its own.
    SecondRedirect {
        /// The `Location` the second response named.
        location: String,
    },
    /// The fetch itself failed, or a URL did not parse.
    Fetch(FetchError),
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotHttps { url } => write!(
                f,
                "{url} is not https; shep downloads a binary only over https"
            ),
            Self::OffHost { location } => write!(
                f,
                "the download was redirected to {location}, which is not GitHub's release asset \
                 host; shep follows one redirect, from github.com to that host"
            ),
            Self::SecondRedirect { location } => write!(
                f,
                "the download was redirected a second time, to {location}; shep follows one"
            ),
            Self::Fetch(source) => source.fmt(f),
        }
    }
}

impl core::error::Error for AssetError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Fetch(source) => Some(source),
            Self::NotHttps { .. } | Self::OffHost { .. } | Self::SecondRedirect { .. } => None,
        }
    }
}

impl From<FetchError> for AssetError {
    fn from(source: FetchError) -> Self {
        Self::Fetch(source)
    }
}

/// Fetches `url` with [`get`](super::get)'s refusals, except that one
/// redirect `policy` admits is followed. `timeout` bounds both requests
/// together.
///
/// # Errors
/// The [`AssetError`] variant for each refusal named in the module doc, and
/// [`AssetError::Fetch`] for everything [`get`](super::get) refuses.
pub async fn get_asset(
    url: &str,
    policy: &RedirectPolicy,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, AssetError> {
    match tokio::time::timeout(timeout, follow(url, policy, limit)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(AssetError::Fetch(FetchError::Timeout)),
    }
}

/// [`get_asset`] without the clock.
async fn follow(url: &str, policy: &RedirectPolicy, limit: usize) -> Result<Vec<u8>, AssetError> {
    let first = policy.admit_first(url)?;
    let location = match get_inner(&first, limit).await {
        Err(FetchError::Redirect { location }) => location,
        other => return Ok(other?),
    };
    let second = policy.admit_redirect(&first, &location)?;
    match get_inner(&second, limit).await {
        Err(FetchError::Redirect { location }) => Err(AssetError::SecondRedirect { location }),
        other => Ok(other?),
    }
}
