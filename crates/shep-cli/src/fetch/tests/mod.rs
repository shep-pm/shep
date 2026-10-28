//! Tests for `crate::fetch`: what a GET refuses, and the URL grammar
//! behind it.
//!
//! These live under the module they exercise rather than in `tests/`, so
//! they can reach its private items directly. `tls` holds the HTTPS half,
//! driven over a real handshake.

use tokio::io::AsyncWriteExt;

use super::*;

mod tls;

/// Serves one canned response on an ephemeral port, then stops.
async fn serve(response: &'static [u8]) -> Target {
    serve_owned(response.to_vec()).await
}

/// [`serve`] for a response a test builds at run time: the header-cap
/// cases, whose responses are tens of kilobytes of padding.
async fn serve_owned(response: Vec<u8>) -> Target {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _peer) = listener.accept().await.unwrap();
        // Drains the request so the client's write never stalls on a
        // full socket buffer.
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await;
        let _ = stream.write_all(&response).await;
        let _ = stream.shutdown().await;
    });
    Target {
        https: false,
        host: "127.0.0.1".to_string(),
        port: addr.port(),
        path: "/".to_string(),
    }
}

/// Serves a response that opens a header and never ends it, for as
/// long as anything is still reading.
async fn serve_endless_header() -> Target {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _peer) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await;
        if stream
            .write_all(b"HTTP/1.1 200 OK\r\nX-Filler: ")
            .await
            .is_err()
        {
            return;
        }
        // Ends itself: a client that refuses closes the socket, and
        // the next write fails.
        let filler = vec![b'A'; 8 * 1024];
        while stream.write_all(&filler).await.is_ok() {}
    });
    Target {
        https: false,
        host: "127.0.0.1".to_string(),
        port: addr.port(),
        path: "/".to_string(),
    }
}

/// Asserts the exact cleaned string, not merely the absence of an
/// escape: the redirect must still name where it pointed.
#[tokio::test]
async fn a_hostile_location_header_cannot_drive_the_terminal_it_prints_to() {
    let target = serve(
        b"HTTP/1.1 302 Found\r\nLocation: \x1b[2J\x1b]0;pwned\x07/gone\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let err = get(&target, 1 << 20, Duration::from_secs(5))
        .await
        .expect_err("refused");
    let FetchError::Redirect { location } = &err else {
        panic!("wrong variant: {err:?}")
    };
    assert_eq!(location, "[2J]0;pwned/gone", "location was not sanitised");
    assert!(
        !err.to_string().chars().any(char::is_control),
        "a control character reached the message: {:?}",
        err.to_string()
    );
}

/// Every response here is hostile in a different place and drives a
/// different refusal, so a future variant that starts carrying
/// response-derived text has to pass this to ship.
#[tokio::test]
async fn no_refusal_hands_a_terminal_a_character_the_host_chose() {
    let hostile: [(&'static [u8], &str); 6] = [
        (
            b"HTTP/1.1 302 Found\r\nLocation: \x1b[2J\x07\r\nContent-Length: 0\r\n\r\n",
            "a redirect naming a hostile location",
        ),
        (
            b"HTTP/1.1 404 \x1b[2JNot Found\r\nContent-Length: 0\r\n\r\n",
            "a non-2xx whose reason phrase is hostile",
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: \x1b[2Jchunked\r\n\r\n0\r\n\r\n",
            "a chunked refusal whose header value is hostile",
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: \x1b[2Jnope\r\n\r\n",
            "an unparseable content-length that is hostile",
        ),
        (
            b"\x1b[2J\x07 NOT HTTP AT ALL\r\n\r\n",
            "a status line that is not HTTP and is hostile",
        ),
        (
            b"HTTP/1.1 200 OK\r\n\x1b[2Jno-colon-here\r\n\r\n",
            "a header line with no colon, hostile",
        ),
    ];
    for (response, why) in hostile {
        let target = serve(response).await;
        let err = get(&target, 1 << 20, Duration::from_secs(5))
            .await
            .expect_err(why);
        let shown = err.to_string();
        assert!(
            !shown.chars().any(char::is_control),
            "{why}: a control character reached the message: {shown:?}"
        );
    }
}

/// The two-second budget is the forcing mechanism: a bounded refusal
/// comes back in milliseconds.
#[tokio::test]
async fn a_header_that_never_ends_is_refused_rather_than_read_forever() {
    let target = serve_endless_header().await;
    let err = get(&target, 1 << 20, Duration::from_secs(2))
        .await
        .expect_err("refused");
    assert!(
        matches!(
            err,
            FetchError::HeadersTooLarge {
                limit: MAX_HEADER_BYTES
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_header_block_just_under_the_cap_is_still_read() {
    let tail = b"\r\nContent-Length: 5\r\n\r\nhello";
    let head = b"HTTP/1.1 200 OK\r\nX-Pad: ";
    let pad = MAX_HEADER_BYTES - head.len() - tail.len();
    let mut response = head.to_vec();
    response.extend(std::iter::repeat_n(b'A', pad));
    response.extend_from_slice(tail);
    let target = serve_owned(response).await;
    let body = get(&target, 1 << 20, Duration::from_secs(5))
        .await
        .expect("read");
    assert_eq!(body, b"hello");
}

#[tokio::test]
async fn a_content_length_body_is_read_exactly() {
    let target = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello").await;
    let body = get(&target, 1 << 20, Duration::from_secs(5))
        .await
        .expect("read");
    assert_eq!(body, b"hello");
}

#[tokio::test]
async fn a_chunked_response_is_refused_rather_than_misparsed() {
    let target =
        serve(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n")
            .await;
    assert!(matches!(
        get(&target, 1 << 20, Duration::from_secs(5)).await,
        Err(FetchError::Chunked)
    ));
}

#[tokio::test]
async fn a_redirect_is_refused_and_names_where_it_pointed() {
    let target =
        serve(b"HTTP/1.1 301 Moved\r\nLocation: https://elsewhere/\r\nContent-Length: 0\r\n\r\n")
            .await;
    let err = get(&target, 1 << 20, Duration::from_secs(5))
        .await
        .expect_err("refused");
    let FetchError::Redirect { location } = err else {
        panic!("wrong variant: {err:?}")
    };
    assert_eq!(location, "https://elsewhere/");
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused_before_it_is_read() {
    let target = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n").await;
    assert!(matches!(
        get(&target, 10, Duration::from_secs(5)).await,
        Err(FetchError::TooLarge { limit: 10 })
    ));
}

#[tokio::test]
async fn a_non_2xx_carries_its_status() {
    let target = serve(b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n").await;
    assert!(matches!(
        get(&target, 1 << 20, Duration::from_secs(5)).await,
        Err(FetchError::Status(500))
    ));
}

#[tokio::test]
async fn a_peer_that_closes_mid_body_is_an_error_not_a_short_read() {
    let target = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort").await;
    let err = get(&target, 1 << 20, Duration::from_secs(5))
        .await
        .expect_err("refused");
    assert!(
        matches!(
            err,
            FetchError::Truncated {
                expected: 10,
                got: 5
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_missing_content_length_is_refused() {
    let target = serve(b"HTTP/1.1 200 OK\r\n\r\nbody").await;
    assert!(get(&target, 1 << 20, Duration::from_secs(5)).await.is_err());
}

#[test]
fn a_url_that_is_not_http_or_https_is_refused() {
    assert!(matches!(
        parse_url("file:///etc/passwd"),
        Err(FetchError::Url(_))
    ));
    assert!(matches!(parse_url("not a url"), Err(FetchError::Url(_))));
}

/// Both halves matter. The refusal is the fix; the exact string is
/// what proves the refusal did not itself print the password, which is
/// what quoting `url` in the message would have done.
#[test]
fn a_url_carrying_credentials_is_refused_without_echoing_them() {
    for url in [
        "https://user:hunter2@example.com:8443/webhook",
        "https://user:hunter2@example.com/webhook",
        "http://hunter2@example.com/webhook",
        // Schemes this module cannot fetch at all. Each used to be
        // refused on the scheme, in a message quoting the whole url.
        "ftp://user:hunter2@example.com/webhook",
        "HTTPS://user:hunter2@example.com/webhook",
        "user:hunter2@example.com/webhook",
        // Scheme-relative: an authority with nothing in front of it,
        // so there is no `://` to find and the leading `//` has to be
        // stepped over or the authority reads as empty.
        "//user:hunter2@example.com/webhook",
    ] {
        let err = parse_url(url).unwrap_err();
        assert!(matches!(err, FetchError::Url(_)), "{url}: {err:?}");
        assert_eq!(
            err.to_string(),
            "not a fetchable url: credentials before the host (`user@` or `user:pass@`) are \
             not supported; the url is not echoed, since it carries one",
            "{url}"
        );
        assert!(!format!("{err} {err:?}").contains("hunter2"), "{url}");
    }
}

/// The backstop under [`url_carries_credentials`]. That predicate
/// reads an authority, and a url far enough off the grammar is one it
/// can misread: in each of these the `@` is in a path, so the
/// predicate says `false` and some other refusal is what sees the url.
/// Every refusal names the url through [`url_for_message`] rather than
/// trusting the predicate to have been right about where the authority
/// ended.
#[test]
fn no_refusal_quotes_a_url_holding_an_at_sign() {
    for url in [
        // Refused on the scheme.
        "file:///etc/pass@wd",
        // Refused on the port, with the `@` off in the path.
        "https://example.com:notaport/etc/pass@wd",
    ] {
        assert!(!url_carries_credentials(url), "{url}");
        let err = parse_url(url).unwrap_err();
        let rendered = format!("{err} {err:?}");
        assert!(!rendered.contains("pass@wd"), "{url}: {rendered}");
        assert!(
            rendered.contains("a url that may carry credentials"),
            "{url}: {rendered}"
        );
    }
}

/// Neither a query nor a fragment is part of the host. Reading them as
/// authority made an `@` in a query look like a credential, so
/// `?contact=alice@example.com` was refused as one.
///
/// They part company after that: a query is the request target's, a
/// fragment is the client's and is dropped.
#[test]
fn a_query_joins_the_path_and_a_fragment_is_dropped() {
    for (url, path) in [
        (
            "https://example.com?contact=alice@example.com",
            "/?contact=alice@example.com",
        ),
        ("https://example.com#a@b", "/"),
        ("https://example.com/hook#frag", "/hook"),
        ("https://example.com/hook?a=b#frag", "/hook?a=b"),
        ("https://example.com/hook?a=b", "/hook?a=b"),
        ("https://example.com", "/"),
    ] {
        let target = parse_url(url).unwrap_or_else(|err| panic!("{url}: {err}"));
        assert_eq!(target.host, "example.com", "{url}");
        assert_eq!(target.port, 443, "{url}");
        assert_eq!(target.path, path, "{url}");
    }
    assert!(!url_carries_credentials(
        "https://example.com?contact=alice@example.com"
    ));
}

/// The door the fragment would have gone out of. `parse_url` dropping
/// it is the fix; this is the assertion that nothing downstream puts
/// it back.
#[test]
fn no_fragment_reaches_the_request_line() {
    let target = parse_url("https://example.com/hook?a=b#sentinelfragment")
        .expect("a url with a query and a fragment parses");
    let request = build_get_request(&target);
    assert!(
        request.starts_with("GET /hook?a=b HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(!request.contains("sentinelfragment"), "{request}");
}

/// A Discord webhook URL is a bearer credential and carries its token
/// as a path segment, so `Target`'s `Debug` withholds the path the way
/// `Sink`'s withholds the whole URL.
#[test]
fn a_targets_debug_redacts_the_path() {
    let target = parse_url("https://discord.com/api/webhooks/123/s3cr3t-token")
        .expect("a plain https url parses");
    let rendered = format!("{target:?}");
    assert_eq!(
        rendered,
        r#"Target { https: true, host: "discord.com", port: 443, path: <redacted> }"#
    );
    assert!(!rendered.contains("s3cr3t-token"));
}

/// A smuggling-style ambiguity: picking one would let a proxy make
/// this client and whatever is downstream of it disagree about where
/// the body ends.
#[tokio::test]
async fn two_disagreeing_content_lengths_are_refused() {
    let target =
        serve(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nhello!").await;
    assert!(get(&target, 1 << 20, Duration::from_secs(5)).await.is_err());
}

#[tokio::test]
async fn a_status_line_that_is_not_http_is_refused() {
    let target = serve(b"NOT HTTP AT ALL\r\n\r\n").await;
    assert!(get(&target, 1 << 20, Duration::from_secs(5)).await.is_err());
}

/// An EOF here must not read as a zero-length body.
#[tokio::test]
async fn headers_with_no_terminating_blank_line_are_refused() {
    let target = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 5").await;
    assert!(get(&target, 1 << 20, Duration::from_secs(5)).await.is_err());
}

/// Refused as an ordinary [`FetchError::Status`], having nowhere to
/// point a caller.
#[tokio::test]
async fn a_redirect_with_no_location_is_still_refused() {
    let target = serve(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n").await;
    assert!(matches!(
        get(&target, 1 << 20, Duration::from_secs(5)).await,
        Err(FetchError::Status(302))
    ));
}
