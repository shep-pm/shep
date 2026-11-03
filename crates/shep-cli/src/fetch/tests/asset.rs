//! `get_asset`: the one redirect it follows and every other it refuses.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::super::asset::{AssetError, RedirectPolicy, get_asset};
use super::super::{Duration, FetchError};

const BUDGET: Duration = Duration::from_secs(5);

/// Answers the connections it gets in order, one canned response each, with
/// `{port}` in a response standing for its own port. Returns the port and a
/// count of the connections served.
async fn serve_in_order(responses: Vec<&'static str>) -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&served);
    tokio::spawn(async move {
        for response in responses {
            let (mut stream, _peer) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            count.fetch_add(1, Ordering::SeqCst);
            let body = response.replace("{port}", &port.to_string());
            let _ = stream.write_all(body.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    (port, served)
}

fn redirect_to(location: &str) -> String {
    format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// Loopback stands in for github.com and its asset host, over plain HTTP.
fn loopback() -> RedirectPolicy {
    RedirectPolicy::loopback("127.0.0.1", &["localhost"])
}

#[tokio::test]
async fn one_redirect_to_the_asset_host_is_followed() {
    let (port, served) = serve_in_order(vec![
        leak(redirect_to("http://localhost:{port}/asset")),
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
    ])
    .await;

    let body = get_asset(
        &format!("http://127.0.0.1:{port}/dog"),
        &loopback(),
        1 << 20,
        BUDGET,
    )
    .await
    .expect("followed");

    assert_eq!(body, b"hello");
    assert_eq!(served.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_second_redirect_is_refused_naming_where_it_pointed() {
    let (port, served) = serve_in_order(vec![
        leak(redirect_to("http://localhost:{port}/one")),
        leak(redirect_to("http://localhost:{port}/two")),
    ])
    .await;

    let err = get_asset(
        &format!("http://127.0.0.1:{port}/dog"),
        &loopback(),
        1 << 20,
        BUDGET,
    )
    .await
    .expect_err("refused");

    assert!(
        matches!(&err, AssetError::SecondRedirect { location } if location.ends_with("/two")),
        "{err}"
    );
    assert_eq!(served.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_redirect_off_the_asset_host_is_refused_without_connecting() {
    let (port, served) =
        serve_in_order(vec![leak(redirect_to("http://elsewhere.test/asset"))]).await;

    let err = get_asset(
        &format!("http://127.0.0.1:{port}/dog"),
        &loopback(),
        1 << 20,
        BUDGET,
    )
    .await
    .expect_err("refused");

    assert!(
        matches!(&err, AssetError::OffHost { location } if location == "http://elsewhere.test/asset"),
        "{err}"
    );
    assert_eq!(served.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_redirect_from_a_host_other_than_the_policys_is_refused() {
    let (port, _served) =
        serve_in_order(vec![leak(redirect_to("http://localhost:{port}/asset"))]).await;
    let policy = RedirectPolicy::loopback("github.example", &["localhost"]);

    let err = get_asset(
        &format!("http://127.0.0.1:{port}/dog"),
        &policy,
        1 << 20,
        BUDGET,
    )
    .await
    .expect_err("refused");

    assert!(matches!(err, AssetError::OffHost { .. }), "{err}");
}

#[tokio::test]
async fn a_redirect_to_plain_http_is_refused_under_the_github_policy() {
    let policy = RedirectPolicy::GITHUB_RELEASES;
    let from = super::super::parse_url("https://github.com/o/r/releases/download/v1/r-1").unwrap();

    let err = policy
        .admit_redirect(&from, "http://release-assets.githubusercontent.com/x")
        .expect_err("refused");

    assert!(matches!(err, AssetError::NotHttps { .. }), "{err}");
}

#[test]
fn github_may_redirect_to_its_release_asset_host_over_https_and_nowhere_else() {
    let policy = RedirectPolicy::GITHUB_RELEASES;
    let from = super::super::parse_url("https://github.com/o/r/releases/download/v1/r-1").unwrap();

    let next = policy
        .admit_redirect(
            &from,
            "https://release-assets.githubusercontent.com/x?sig=1",
        )
        .expect("admitted");
    assert_eq!(next.host, "release-assets.githubusercontent.com");
    assert!(next.https);

    for refused in [
        "https://evil.test/x",
        "https://github.com.evil.test/x",
        "https://githubusercontent.com/x",
        "/relative/path",
        "https://user:pw@release-assets.githubusercontent.com/x",
    ] {
        assert!(policy.admit_redirect(&from, refused).is_err(), "{refused}");
    }

    let elsewhere = super::super::parse_url("https://gitlab.com/o/r/x").unwrap();
    assert!(
        policy
            .admit_redirect(&elsewhere, "https://release-assets.githubusercontent.com/x")
            .is_err()
    );
}

#[tokio::test]
async fn plain_http_is_refused_under_the_github_policy_before_any_connection() {
    let err = get_asset(
        "http://github.com/o/r/releases/download/v1/r-1",
        &RedirectPolicy::GITHUB_RELEASES,
        1 << 20,
        BUDGET,
    )
    .await
    .expect_err("refused");

    assert!(matches!(err, AssetError::NotHttps { .. }), "{err}");
}

#[tokio::test]
async fn a_plain_answer_needs_no_redirect_and_a_bad_status_is_the_fetchs() {
    let (port, _served) =
        serve_in_order(vec!["HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"]).await;

    let err = get_asset(
        &format!("http://127.0.0.1:{port}/dog"),
        &loopback(),
        1 << 20,
        BUDGET,
    )
    .await
    .expect_err("refused");

    assert!(
        matches!(err, AssetError::Fetch(FetchError::Status(404))),
        "{err}"
    );
}
