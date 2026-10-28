//! The connect-and-send half of [`crate::fetch`], shared by its GET, its
//! POST and `dog::bark::sinks`, so the guards on it exist once.

use core::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{CertificateError, Error as RustlsError};

use super::{Target, tls_connector};
use crate::terminal_safe;

/// The reading half of a connection [`connect_and_send`] opened, plain or
/// TLS: all a reader needs once the request is out.
pub(crate) type ReplyStream = Pin<Box<dyn AsyncRead + Send>>;

/// What [`peer_transport_error`] says when rustls reports `UnknownIssuer`.
///
/// Named here rather than inline because an operator has to be able to act
/// on it, and the action is a change to the endpoint, not to shep. rustls
/// reports that one variant both for a chain that ends at an issuer shep
/// holds no root for and for a publicly trusted chain that left an
/// intermediate out, so the text covers both.
pub(super) const UNKNOWN_ISSUER: &str = concat!(
    "the certificate chain this endpoint presented does not reach a certificate authority shep trusts; ",
    "shep trusts only public certificate authorities, and offers no way to add a private one or to skip verification; ",
    "a leaf served without its intermediate is refused the same way, so check the chain the endpoint serves, ",
    "use http:// for a private endpoint, or use a publicly trusted certificate (Let's Encrypt, or a Cloudflare-proxied hostname)"
);

/// Connects to `target` with [`tls_connector`], writes `request`, flushes,
/// and hands back the stream to read the reply from.
///
/// The one connect-and-send every client in this crate shares, so the
/// guards on it exist once: a TLS handshake failure comes back through
/// [`peer_transport_error`]. The flush matters on the TLS branch:
/// `tokio-rustls` buffers writes in `rustls`'s record layer, and skipping
/// it leaves a request the peer never sees.
///
/// # Errors
/// Any connect, handshake, write or flush failure. The error is an
/// [`std::io::Error`] so each caller wraps it in its own error type.
pub(crate) async fn connect_and_send(
    target: &Target,
    request: &[u8],
) -> Result<ReplyStream, std::io::Error> {
    connect_with(tls_connector(), target, request).await
}

/// [`connect_and_send`] over a connector the caller chose.
///
/// rustls checks a certificate chain before it checks a name, so reaching
/// the name check at all needs a store the presented chain lands in.
pub(super) async fn connect_with(
    connector: &tokio_rustls::TlsConnector,
    target: &Target,
    request: &[u8],
) -> Result<ReplyStream, std::io::Error> {
    let tcp = TcpStream::connect((target.host.as_str(), target.port)).await?;
    if target.https {
        let domain = ServerName::try_from(target.host.clone()).map_err(std::io::Error::other)?;
        let mut tls = connector
            .connect(domain, tcp)
            .await
            .map_err(|source| peer_transport_error(&target.host, source))?;
        send(&mut tls, request).await?;
        Ok(Box::pin(tls))
    } else {
        let mut tcp = tcp;
        send(&mut tcp, request).await?;
        Ok(Box::pin(tcp))
    }
}

/// Writes `request` and flushes.
async fn send<S: AsyncWrite + Unpin>(stream: &mut S, request: &[u8]) -> Result<(), std::io::Error> {
    stream.write_all(request).await?;
    stream.flush().await
}

/// A transport failure the peer had a hand in wording.
///
/// The OS writes most of these and is not the threat, and they keep their
/// text, sanitised. A rustls certificate refusal is the exception: its
/// message names the subject names the peer's certificate chose to carry,
/// so those two get shep's own wording. The [`std::io::ErrorKind`] is kept.
fn peer_transport_error(host: &str, source: std::io::Error) -> std::io::Error {
    let message = match source
        .get_ref()
        .and_then(|src| src.downcast_ref::<RustlsError>())
    {
        Some(RustlsError::InvalidCertificate(CertificateError::UnknownIssuer)) => {
            UNKNOWN_ISSUER.to_owned()
        }
        Some(RustlsError::InvalidCertificate(
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
        )) => format!("the certificate for {host} is not valid for that name"),
        _ => source.to_string(),
    };
    std::io::Error::new(source.kind(), terminal_safe::sanitise(&message).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fallback arm: a peer-worded failure that is not a rustls
    /// certificate refusal keeps its text, minus what would drive a terminal.
    #[test]
    fn a_handshake_error_the_peer_worded_cannot_drive_a_terminal() {
        let source = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "tls handshake eof \u{1b}[2J\u{7}",
        );
        let kept = peer_transport_error("127.0.0.1", source);
        assert_eq!(kept.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(kept.to_string(), "tls handshake eof [2J");
    }

    /// The wrapping `tokio-rustls` puts on a rustls failure is what
    /// `get_ref` has to see through, so the mapping is pinned without a
    /// socket in the way. This is also where the operator-facing text is
    /// pinned, as a literal rather than against the constant.
    #[test]
    fn a_rustls_unknown_issuer_becomes_the_hint() {
        let source = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            RustlsError::InvalidCertificate(CertificateError::UnknownIssuer),
        );
        assert_eq!(
            peer_transport_error("hooks.example.com", source).to_string(),
            "the certificate chain this endpoint presented does not reach a certificate \
             authority shep trusts; shep trusts only public certificate authorities, and offers \
             no way to add a private one or to skip verification; a leaf served without its \
             intermediate is refused the same way, so check the chain the endpoint serves, use \
             http:// for a private endpoint, or use a publicly trusted certificate (Let's \
             Encrypt, or a Cloudflare-proxied hostname)"
        );
    }

    /// `host` comes from a URL, and a URL is not a trusted string.
    #[test]
    fn a_reworded_name_refusal_cannot_carry_a_control_character() {
        let source = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            RustlsError::InvalidCertificate(CertificateError::NotValidForNameContext {
                expected: ServerName::try_from("example.com").unwrap().to_owned(),
                presented: vec!["evil\u{1b}[2J.shep.test\u{7}".to_owned()],
            }),
        );
        let kept = peer_transport_error("127.0.0.\u{1b}[2J1", source);
        assert_eq!(
            kept.to_string(),
            "the certificate for 127.0.0.[2J1 is not valid for that name"
        );
    }
}
