//! The connect-and-send half of [`crate::fetch`], shared by its GET, its
//! POST and `dog::bark::sinks`, so the guards on it exist once.

use core::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;

use super::{Target, tls_connector};
use crate::terminal_safe;

/// The reading half of a connection [`connect_and_send`] opened, plain or
/// TLS: all a reader needs once the request is out.
pub(crate) type ReplyStream = Pin<Box<dyn AsyncRead + Send>>;

/// Connects to `target`, over TLS when `target.https`, writes `request`,
/// flushes, and hands back the stream to read the reply from.
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
    let tcp = TcpStream::connect((target.host.as_str(), target.port)).await?;
    if target.https {
        let domain = ServerName::try_from(target.host.clone()).map_err(std::io::Error::other)?;
        let mut tls = tls_connector()
            .connect(domain, tcp)
            .await
            .map_err(peer_transport_error)?;
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

/// A transport failure the peer had a hand in wording, sanitised.
///
/// The OS writes most of these and is not the threat. A TLS handshake is the
/// exception: rustls's `CertificateError::NotValidForNameContext` prints the
/// names the peer's certificate presented.
///
/// Keeps the [`std::io::ErrorKind`] and drops the nested source.
fn peer_transport_error(source: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        source.kind(),
        terminal_safe::sanitise(&source.to_string()).0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rustls handshake error prints the names the peer's certificate
    /// carried, so the text it keeps is the sanitised one.
    #[test]
    fn a_handshake_error_the_peer_worded_cannot_drive_a_terminal() {
        let source = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "certificate not valid for name \"evil\u{1b}[2J.example\u{7}\"",
        );
        let kept = peer_transport_error(source);
        assert_eq!(kept.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            kept.to_string(),
            "certificate not valid for name \"evil[2J.example\""
        );
    }
}
