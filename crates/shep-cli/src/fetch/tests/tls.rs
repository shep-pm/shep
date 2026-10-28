//! shep's own TLS wiring, driven over a real handshake on loopback.
//!
//! The handshake and the record layer are rustls's tested surface. What is
//! under test here is shep's half: the root store it hands rustls, and what
//! it says to the operator who has to fix a refused certificate.
//!
//! The fixtures in `testdata/` are a throwaway CA, a leaf it signed, and a
//! self-signed leaf, all named `shep.test`, which no public CA will issue.
//! Every await here runs on the real clock: a paused one would never let a
//! handshake budget expire.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::rustls::client::WebPkiServerVerifier;
use tokio_rustls::rustls::client::danger::ServerCertVerifier;
use tokio_rustls::rustls::pki_types::{
    CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject,
};
use tokio_rustls::rustls::{CertificateError, Error as RustlsError, RootCertStore, ServerConfig};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use super::super::transport::connect_with;
use super::super::{Target, root_store};

/// The fixture CA, and the two leaves the tests put in front of a client.
const FIXTURE_CA: &[u8] = include_bytes!("../testdata/ca.pem");
const PRIVATE_LEAF: &[u8] = include_bytes!("../testdata/private-leaf.pem");
const PRIVATE_LEAF_KEY: &[u8] = include_bytes!("../testdata/private-leaf.key.pem");
const SELF_SIGNED_LEAF: &[u8] = include_bytes!("../testdata/selfsigned-leaf.pem");
const SELF_SIGNED_LEAF_KEY: &[u8] = include_bytes!("../testdata/selfsigned-leaf.key.pem");

/// How long a handshake here may take before the test calls it stuck.
///
/// The bound is the forcing mechanism: a client that never comes back, or
/// comes back on a state the test was not waiting for, fails here rather
/// than at the harness's own process timeout.
const HANDSHAKE_BUDGET: Duration = Duration::from_secs(10);

/// The request bytes a successful handshake would be asked to write. None of
/// these tests gets that far.
const REQUEST: &[u8] = b"POST /hook HTTP/1.1\r\nContent-Length: 0\r\n\r\n";

/// A [`ServerConfig`] presenting `leaf`, then `chain`, with `key`.
fn server_config(leaf: &[u8], chain: &[&[u8]], key: &[u8]) -> ServerConfig {
    let certs = std::iter::once(leaf)
        .chain(chain.iter().copied())
        .map(|pem| CertificateDer::from_pem_slice(pem).expect("a committed PEM fixture"))
        .collect();
    let key = PrivateKeyDer::from_pem_slice(key).expect("a committed PEM fixture");
    ServerConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("the ring provider covers rustls's default protocol versions")
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .expect("a committed leaf and key")
}

/// A [`TlsConnector`] trusting only `ca_pem`.
///
/// Not the connector shep uses. It exists so rustls gets past its chain
/// check and into its name check, which is the only way that check is
/// reachable at all: shep's own store cannot contain a fixture CA.
fn connector_trusting(ca_pem: &[u8]) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates([CertificateDer::from_pem_slice(ca_pem).unwrap()]);
    let client = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("the ring provider covers rustls's default protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    TlsConnector::from(Arc::new(client))
}

/// A [`WebPkiServerVerifier`] over `roots`: the verifier a
/// [`super::tls_connector`] config ends up holding.
fn verifier(roots: RootCertStore) -> Arc<WebPkiServerVerifier> {
    WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(tokio_rustls::rustls::crypto::ring::default_provider()),
    )
    .build()
    .unwrap()
}

/// The fixtures and the two stores, checked with no socket in the way.
///
/// rustls checks the chain before the name, so a name refusal out of the
/// fixture store is also the proof that the fixture chain verifies under it.
#[test]
fn the_fixture_leaf_chains_to_the_fixture_ca_and_nowhere_else() {
    let leaf = CertificateDer::from_pem_slice(PRIVATE_LEAF).unwrap();
    let ca = CertificateDer::from_pem_slice(FIXTURE_CA).unwrap();
    let intermediates = [ca];
    let name = ServerName::try_from("127.0.0.1").unwrap();

    let mut fixture = RootCertStore::empty();
    fixture.add_parsable_certificates([intermediates[0].clone()]);
    let err = verifier(fixture)
        .verify_server_cert(&leaf, &intermediates, &name, &[], UnixTime::now())
        .expect_err("the leaf is named shep.test, not 127.0.0.1");
    assert!(
        matches!(
            err,
            RustlsError::InvalidCertificate(CertificateError::NotValidForNameContext { .. })
        ),
        "{err:?}"
    );

    let err = verifier(root_store().clone())
        .verify_server_cert(&leaf, &intermediates, &name, &[], UnixTime::now())
        .expect_err("the fixture CA is not a public one");
    assert!(
        matches!(
            err,
            RustlsError::InvalidCertificate(CertificateError::UnknownIssuer)
        ),
        "{err:?}"
    );
}

/// Serves TLS on an ephemeral loopback port and hands back the [`Target`]
/// that reaches it.
///
/// Every server-side failure is ignored on purpose: these tests want the
/// client to be the one that refuses, and a refusing client closes the
/// socket mid-handshake.
async fn serve_tls(config: ServerConfig) -> Target {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                        .await;
                }
            });
        }
    });
    Target {
        https: true,
        host: "127.0.0.1".to_string(),
        port: addr.port(),
        path: "/".to_string(),
    }
}

/// Connects to `target` over `connector`, or reports that it never answered.
async fn try_connect(connector: &TlsConnector, target: &Target) -> Result<(), std::io::Error> {
    match tokio::time::timeout(HANDSHAKE_BUDGET, connect_with(connector, target, REQUEST)).await {
        Ok(result) => result.map(|_stream| ()),
        Err(_elapsed) => panic!("the handshake never answered within {HANDSHAKE_BUDGET:?}"),
    }
}

/// A leaf under a CA `webpki-roots` does not carry is refused, and the
/// refusal names the two things the operator can actually do.
#[tokio::test]
async fn an_unknown_issuer_is_refused_with_a_hint() {
    let target = serve_tls(server_config(PRIVATE_LEAF, &[FIXTURE_CA], PRIVATE_LEAF_KEY)).await;
    let err = try_connect(super::super::tls_connector(), &target)
        .await
        .expect_err("a fixture CA is not a public one");
    assert_eq!(
        err.to_string(),
        "the certificate this endpoint presented chains to a certificate authority shep does \
         not trust; shep trusts only public certificate authorities, and offers no way to add a \
         private one or to skip verification; use http:// for a private endpoint, or a publicly \
         trusted certificate (Let's Encrypt, or a Cloudflare-proxied hostname)"
    );
    assert!(
        !err.to_string().contains("shep.test"),
        "the peer's own names reached the message: {err}"
    );
}

/// A self-signed leaf is refused the same way, because it chains nowhere
/// shep trusts either.
#[tokio::test]
async fn a_self_signed_server_is_refused() {
    let target = serve_tls(server_config(SELF_SIGNED_LEAF, &[], SELF_SIGNED_LEAF_KEY)).await;
    let err = try_connect(super::super::tls_connector(), &target)
        .await
        .expect_err("a self-signed leaf is not issued by a public CA");
    assert_eq!(
        err.to_string(),
        "the certificate this endpoint presented chains to a certificate authority shep does \
         not trust; shep trusts only public certificate authorities, and offers no way to add a \
         private one or to skip verification; use http:// for a private endpoint, or a publicly \
         trusted certificate (Let's Encrypt, or a Cloudflare-proxied hostname)"
    );
}

/// The name check runs after the chain check, so this is the one case that
/// hands in its own store: over shep's, the refusal would be the issuer's
/// and the name would never be looked at.
///
/// The leaf carries `shep.test`; the client asked for `127.0.0.1`. The
/// message names the host shep was given, never the names the certificate
/// does carry.
#[tokio::test]
async fn a_wrong_name_is_refused_without_echoing_the_peer_names() {
    let target = serve_tls(server_config(PRIVATE_LEAF, &[FIXTURE_CA], PRIVATE_LEAF_KEY)).await;
    let err = try_connect(&connector_trusting(FIXTURE_CA), &target)
        .await
        .expect_err("the fixture leaf is named for shep.test, not 127.0.0.1");
    let shown = err.to_string();
    assert_eq!(
        shown,
        "the certificate for 127.0.0.1 is not valid for that name"
    );
    assert!(!shown.contains("shep.test"), "{shown}");
    assert!(
        !shown.chars().any(char::is_control),
        "a control character reached the message: {shown:?}"
    );
}

/// An empty store would refuse every `https://` endpoint, which is the
/// failure mode a refactor of [`root_store`] would produce.
#[test]
fn the_root_store_is_not_empty() {
    let roots = root_store();
    assert!(!roots.is_empty(), "shep's root store loaded empty");
    assert_eq!(
        roots.len(),
        webpki_roots::TLS_SERVER_ROOTS.len(),
        "the connector is not carrying the whole public set"
    );
}
