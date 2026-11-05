//! A chunked body through [`get`], which is how Cloudflare serves the dog
//! index.

use super::*;

/// What [`get`] answers for a response opening with `head` and ending with
/// `body`, under a 16-byte limit.
async fn fetch(head: &str, body: &[u8]) -> Result<Vec<u8>, FetchError> {
    let mut response = format!("HTTP/1.1 200 OK\r\n{head}\r\n\r\n").into_bytes();
    response.extend_from_slice(body);
    let target = serve_owned(response).await;
    get(&target, 16, Duration::from_secs(5)).await
}

/// [`fetch`] with `Transfer-Encoding: chunked`.
async fn chunked(body: &[u8]) -> Result<Vec<u8>, FetchError> {
    fetch("Transfer-Encoding: chunked", body).await
}

/// Fails unless `result` is the [`FetchError::Transport`] for a bad framing.
#[track_caller]
fn assert_malformed(result: Result<Vec<u8>, FetchError>) {
    assert!(
        matches!(result, Err(FetchError::Transport(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn chunks_join_into_the_body() {
    let body = chunked(b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n").await;
    assert_eq!(body.unwrap(), b"hello world");
}

#[tokio::test]
async fn a_size_reads_as_hex_in_either_case() {
    let body = chunked(b"A\r\n0123456789\r\n0\r\n\r\n").await;
    assert_eq!(body.unwrap(), b"0123456789");
    let body = chunked(b"a\r\n0123456789\r\n0\r\n\r\n").await;
    assert_eq!(body.unwrap(), b"0123456789");
}

#[tokio::test]
async fn an_extension_on_a_size_line_is_skipped() {
    let body = chunked(b"5;name=value\r\nhello\r\n0\r\n\r\n").await;
    assert_eq!(body.unwrap(), b"hello");
}

#[tokio::test]
async fn trailers_after_the_last_chunk_are_ignored() {
    let body = chunked(b"5\r\nhello\r\n0\r\nX-Trailer: yes\r\n\r\n").await;
    assert_eq!(body.unwrap(), b"hello");
}

#[tokio::test]
async fn the_coding_name_is_case_insensitive() {
    let body = fetch("Transfer-Encoding: Chunked", b"2\r\nhi\r\n0\r\n\r\n").await;
    assert_eq!(body.unwrap(), b"hi");
}

#[tokio::test]
async fn chunked_outranks_a_content_length() {
    let head = "Content-Length: 1\r\nTransfer-Encoding: chunked";
    let body = fetch(head, b"5\r\nhello\r\n0\r\n\r\n").await;
    assert_eq!(body.unwrap(), b"hello");
}

#[tokio::test]
async fn one_chunk_over_the_limit_is_too_large() {
    let body = chunked(b"11\r\n").await;
    assert!(matches!(body, Err(FetchError::TooLarge { limit: 16 })));
}

#[tokio::test]
async fn chunks_that_add_up_past_the_limit_are_too_large() {
    let body = chunked(b"9\r\n012345678\r\n8\r\n01234567\r\n0\r\n\r\n").await;
    assert!(matches!(body, Err(FetchError::TooLarge { limit: 16 })));
}

#[tokio::test]
async fn a_body_of_exactly_the_limit_is_read() {
    let body = chunked(b"10\r\n0123456789abcdef\r\n0\r\n\r\n").await;
    assert_eq!(body.unwrap(), b"0123456789abcdef");
}

#[tokio::test]
async fn a_stream_closing_inside_a_chunk_is_refused() {
    assert_malformed(chunked(b"5\r\nhel").await);
}

#[tokio::test]
async fn a_stream_closing_before_the_last_chunk_is_refused() {
    assert_malformed(chunked(b"5\r\nhello\r\n").await);
}

#[tokio::test]
async fn data_past_its_declared_size_is_refused() {
    assert_malformed(chunked(b"2\r\nhello\r\n0\r\n\r\n").await);
}

#[tokio::test]
async fn a_size_that_is_not_hex_is_refused() {
    for line in ["zz", "", "+5", "-5", " 5"] {
        let body = format!("{line}\r\nhello\r\n0\r\n\r\n");
        assert_malformed(chunked(body.as_bytes()).await);
    }
}

#[tokio::test]
async fn a_size_too_large_for_a_u64_is_refused() {
    assert_malformed(chunked(b"10000000000000000\r\n").await);
}

#[tokio::test]
async fn a_size_line_that_never_ends_is_refused() {
    assert_malformed(chunked(&[b'0'; 2048]).await);
}

#[tokio::test]
async fn another_coding_is_refused() {
    let body = fetch("Transfer-Encoding: gzip", b"hello").await;
    assert!(matches!(body, Err(FetchError::TransferEncoding)));
}

#[tokio::test]
async fn a_coding_stacked_on_chunked_is_refused() {
    let body = fetch("Transfer-Encoding: gzip, chunked", b"0\r\n\r\n").await;
    assert!(matches!(body, Err(FetchError::TransferEncoding)));
}

#[tokio::test]
async fn two_transfer_encoding_headers_are_refused() {
    let head = "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked";
    let body = fetch(head, b"0\r\n\r\n").await;
    assert!(matches!(body, Err(FetchError::TransferEncoding)));
}
