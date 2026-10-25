//! What a request gets: the target's path, the order the checks run in,
//! and the reply each outcome answers with.
//!
//! The order is the security order: auth, method, request body,
//! resolution, hidden, containment, then a directory or a file. Auth runs
//! before resolution so a client cannot map the filesystem through
//! 400-vs-404, and every 404 funnels through [`send_not_found`], which is
//! also where the SPA fallback lives.
//!
//! Writing a reply and logging it is `response`'s: this module only picks
//! which reply to write. For a file, which of the four replies a request
//! gets (200, 206, 304, 416) is `serve::conditional`'s call, made from the
//! metadata the open handle already carries.

use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::net::TcpStream;

use super::ServeConfig;
use super::response::{reply, respond};
use crate::http::{Header, HttpRequest};
use crate::serve::{conditional, fs, listing, mime, path};

/// A request target's path: everything before the first `?` or `#`, or the
/// whole target when it carries neither.
pub(super) fn path_of(target: &str) -> &str {
    target
        .split_once(['?', '#'])
        .map_or(target, |(path, _rest)| path)
}

/// The `WWW-Authenticate` header every 401 this worker answers carries.
const WWW_AUTHENTICATE: Header<'static> = Header {
    name: "WWW-Authenticate",
    value: "Basic realm=\"shep\"",
};

/// The `Allow` header every 405 this worker answers carries.
const ALLOW: Header<'static> = Header {
    name: "Allow",
    value: "GET, HEAD",
};

/// Writes one reply to `stream`, returning the status answered and the
/// body byte count for [`super::handle_connection`]'s access-log line.
///
/// `logged_path` arrives holding the raw target and is overwritten with
/// the resolved path as soon as resolution succeeds, so a refusal reached
/// before then is logged under the target the client sent.
pub(super) async fn answer(
    stream: &mut TcpStream,
    cfg: &ServeConfig,
    request: &HttpRequest,
    logged_path: &mut String,
) -> (u16, u64) {
    let raw_path = path_of(&request.target);

    // Auth before resolution, so an unauthenticated client cannot map the
    // filesystem through 400-vs-404.
    if let Some(creds) = &cfg.auth {
        let header = request.headers.get("authorization").map(String::as_str);
        if !creds.satisfies(header) {
            let body = b"unauthorized\n";
            return reply(stream, 401, "text/plain", body, vec![WWW_AUTHENTICATE]).await;
        }
    }

    if request.method != "GET" && request.method != "HEAD" {
        let body = b"method not allowed; this server answers GET and HEAD\n";
        return reply(stream, 405, "text/plain", body, vec![ALLOW]).await;
    }

    // This server never reads a request body.
    let has_declared_body = request
        .headers
        .get("content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|len| len > 0)
        || request.headers.contains_key("transfer-encoding");
    if has_declared_body {
        let body = b"this server does not accept a request body\n";
        return reply(stream, 400, "text/plain", body, vec![]).await;
    }

    let segments = match path::resolve(&request.target) {
        Ok(segments) => segments,
        Err(refusal) => {
            let body = refusal.to_string();
            return reply(stream, 400, "text/plain", body.as_bytes(), vec![]).await;
        }
    };
    *logged_path = format!("/{}", segments.join("/"));

    // Hidden: a 404, not a 403, checked before any filesystem access.
    if path::is_hidden(&segments) && !cfg.hidden {
        return send_not_found(stream, cfg, request).await;
    }

    let Some(resolved) = fs::contain(&cfg.root, &segments, cfg.follow_symlinks).await else {
        return send_not_found(stream, cfg, request).await;
    };

    // A directory goes to the directory branch below. A fifo, a socket or
    // a device node reaches `fs::open_regular`, which refuses it.
    let metadata = match tokio::fs::metadata(&resolved).await {
        Ok(metadata) => metadata,
        Err(_err) => return send_not_found(stream, cfg, request).await,
    };

    if metadata.is_dir() {
        if !raw_path.ends_with('/') {
            let mut location = String::from("/");
            location.push_str(
                &segments
                    .iter()
                    .map(|segment| listing::encode_segment(segment))
                    .collect::<Vec<_>>()
                    .join("/"),
            );
            location.push('/');
            let header = Header {
                name: "Location",
                value: location.as_str(),
            };
            return match respond(stream, 301, "text/plain", 0, vec![header]).await {
                Ok(()) => (301, 0),
                Err(_err) => {
                    // A peer disconnect, not a bad header: `encode_segment`
                    // only ever produces printable ASCII.
                    let body = b"could not build the redirect\n";
                    reply(stream, 500, "text/plain", body, vec![]).await
                }
            };
        }

        if let Some((file, metadata)) = fs::open_regular(&resolved.join("index.html")).await {
            return serve_file(
                stream,
                request,
                mime::content_type("index.html"),
                file,
                &metadata,
            )
            .await;
        }

        if cfg.listing {
            let entries = read_listing(&resolved, cfg.hidden)
                .await
                .unwrap_or_default();
            let mut prefix = String::from("/");
            prefix.push_str(&segments.join("/"));
            if !prefix.ends_with('/') {
                prefix.push('/');
            }
            let html = listing::render(&prefix, &entries);
            let body = html.as_bytes();
            return reply(stream, 200, "text/html; charset=utf-8", body, vec![]).await;
        }

        return send_not_found(stream, cfg, request).await;
    }

    let Some((file, metadata)) = fs::open_regular(&resolved).await else {
        return send_not_found(stream, cfg, request).await;
    };
    let content_type = mime::content_type(logged_path.as_str());
    serve_file(stream, request, content_type, file, &metadata).await
}

/// Answers a 404, or, if `cfg.spa` is set, the method is `GET`/`HEAD`, and
/// the request's `Accept` header names `text/html`, serves the docroot's
/// `index.html` with a 200 instead. Every 404 this worker would otherwise
/// answer funnels through here, so the gate lives in one place.
///
/// The fallback goes through the same file path as an ordinary hit, so the
/// `index.html` an SPA gets carries the same validators, and a conditional
/// request against a fallback answers 304 rather than re-sending the page.
async fn send_not_found(
    stream: &mut TcpStream,
    cfg: &ServeConfig,
    request: &HttpRequest,
) -> (u16, u64) {
    if cfg.spa
        && matches!(request.method.as_str(), "GET" | "HEAD")
        && request
            .headers
            .get("accept")
            .is_some_and(|accept| accept.contains("text/html"))
        && let Some((file, metadata)) = fs::open_regular(&cfg.root.join("index.html")).await
    {
        return serve_file(
            stream,
            request,
            mime::content_type("index.html"),
            file,
            &metadata,
        )
        .await;
    }
    reply(stream, 404, "text/plain", b"not found\n", vec![]).await
}

/// Streams `file` (already open, its `metadata` read from that same handle)
/// as the body of whichever reply the request's own `Range` and validator
/// headers pick: a 200, a 206, a 304 or a 416.
///
/// Returns the status answered and the body byte count for
/// [`super::handle_connection`]'s access-log line, which is the length the
/// head declared, the same number every in-memory reply reports.
///
/// A slice is reached by seeking and then `take`ing, never by reading the
/// file whole. The byte count came from the open handle's own metadata, so a
/// file that grew between the open and this copy still matches the
/// `Content-Length` already declared. A file that shrank still sends fewer
/// bytes than declared; `Connection: close` turns that into a visible
/// truncation rather than a hang.
async fn serve_file(
    stream: &mut TcpStream,
    request: &HttpRequest,
    content_type: &str,
    mut file: tokio::fs::File,
    metadata: &std::fs::Metadata,
) -> (u16, u64) {
    let len = metadata.len();
    let validators = conditional::Validators::new(len, metadata.modified().ok());
    let outcome = conditional::decide(&request.headers, len, &validators);

    // `(start, stop)`, `stop` exclusive, or `None` for a reply with no body.
    let body = match outcome {
        conditional::Outcome::Whole => Some((0_u64, len)),
        conditional::Outcome::Partial { start, end } => Some((start, end + 1)),
        conditional::Outcome::Unsatisfiable | conditional::Outcome::NotModified => None,
    };
    let status = match outcome {
        conditional::Outcome::Whole => 200,
        conditional::Outcome::Partial { .. } => 206,
        conditional::Outcome::Unsatisfiable => 416,
        conditional::Outcome::NotModified => 304,
    };
    let body_len = body.map_or(0, |(start, stop)| stop - start);

    // The seek happens before the head, so a slice this server cannot reach
    // is refused rather than declared and then left short.
    let mut slice = match body {
        Some((start, stop)) if request.method == "GET" => match file.seek(SeekFrom::Start(start)).await {
            Ok(_here) => Some(file.take(stop - start)),
            Err(_err) => return (500, 0),
        },
        _ => None,
    };

    let last_modified = validators.last_modified();
    let content_range = match outcome {
        conditional::Outcome::Partial { start, end } => Some(format!("bytes {start}-{end}/{len}")),
        conditional::Outcome::Unsatisfiable => Some(format!("bytes */{len}")),
        _ => None,
    };
    let mut headers = vec![
        Header {
            name: "Accept-Ranges",
            value: "bytes",
        },
        Header {
            name: "ETag",
            value: validators.etag(),
        },
    ];
    if let Some(value) = &last_modified {
        headers.push(Header {
            name: "Last-Modified",
            value: value.as_str(),
        });
    }
    if let Some(value) = &content_range {
        headers.push(Header {
            name: "Content-Range",
            value: value.as_str(),
        });
    }

    if respond(stream, status, content_type, body_len, headers).await.is_err() {
        return (500, 0);
    }
    if let Some(slice) = &mut slice {
        let _ = tokio::io::copy(slice, stream).await;
    }
    (status, body_len)
}

/// Reads `dir`'s entries, dropping a leading-dot name unless `hidden` is
/// set, directories before files.
async fn read_listing(dir: &std::path::Path, hidden: bool) -> std::io::Result<Vec<listing::Entry>> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut read_dir = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !hidden && path::is_hidden(std::slice::from_ref(&name)) {
            continue;
        }
        if entry.file_type().await?.is_dir() {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }
    dirs.sort();
    files.sort();
    let mut entries = Vec::with_capacity(dirs.len() + files.len());
    entries.extend(dirs.into_iter().map(listing::Entry::dir));
    entries.extend(files.into_iter().map(listing::Entry::file));
    Ok(entries)
}

// unix only: the cases assert file modes on served files.
// Windows coverage is in `tests/cli_e2e.rs`.
#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use super::super::testing::*;

    /// The traversal table again, end to end over a real socket, so it
    /// covers the handler's ordering and not only the resolver. Each case
    /// asserts the exact status, since `assert_ne!(status, 200)` passes
    /// against a server that 500s on everything.
    #[tokio::test]
    async fn every_traversal_shape_is_refused_over_a_real_socket() {
        // The docroot is a child of the tempdir, and the negative control
        // is its sibling: `tempdir().path()` alone is shared and never
        // cleaned up.
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(
            &root,
            &[
                ("index.html", "<h1>home</h1>"),
                ("assets/app.css", "body{}"),
            ],
        );
        std::fs::write(outer.path().join("secret.txt"), "nope").unwrap();
        let server = serve_on_free_port(config(&root)).await;

        // positive control first: the server serves.
        assert_eq!(get(server.addr(), "/index.html").await.status, 200);
        assert_eq!(get(server.addr(), "/assets/app.css").await.status, 200);

        for (target, want) in [
            ("/../secret.txt", 400),
            ("/../../etc/passwd", 400),
            ("/%2e%2e/secret.txt", 400),
            ("/..%2fsecret.txt", 400),
            ("/x%00.png", 400),
            ("/a%0d%0aSet-Cookie:%20x", 400),
            ("/C:/Windows/System32/config/SAM", 400),
            ("/etc/passwd", 404),
            ("/nope.txt", 404),
            ("/.env", 404),
        ] {
            let response = get(server.addr(), target).await;
            assert_eq!(response.status, want, "{target} answered {response:?}");
            assert!(!response.body.contains("nope"), "{target} leaked the file");
            assert_eq!(
                response.headers["x-content-type-options"], "nosniff",
                "{target}: every response carries it, refusals included"
            );
        }
    }
    /// fails if a symlink out of the docroot is served over the socket.
    #[tokio::test]
    async fn a_symlink_out_of_the_docroot_is_a_404_and_not_a_body() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("ok.txt", "served")]);
        std::fs::write(outer.path().join("secret.txt"), "nope").unwrap();
        std::os::unix::fs::symlink(outer.path().join("secret.txt"), root.join("escape.txt"))
            .unwrap();
        let server = serve_on_free_port(config(&root)).await;

        assert_eq!(
            get(server.addr(), "/ok.txt").await.status,
            200,
            "positive control"
        );

        let response = get(server.addr(), "/escape.txt").await;
        assert_eq!(response.status, 404);
        assert!(!response.body.contains("nope"), "{response:?}");
    }

    /// fails if the follow-symlinks flag on `ServeConfig` does not reach
    /// `fs::contain` through the worker.
    #[tokio::test]
    async fn a_symlinked_deploy_layout_is_served_only_with_follow_symlinks() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(
            &root,
            &[("releases/2026-08-15/index.html", "<h1>home</h1>")],
        );
        std::os::unix::fs::symlink(root.join("releases/2026-08-15"), root.join("current")).unwrap();

        let refusing = serve_on_free_port(config(&root)).await;
        assert_eq!(
            get(refusing.addr(), "/current/index.html").await.status,
            404
        );

        let following = serve_on_free_port(config_with_follow_symlinks(&root)).await;
        let response = get(following.addr(), "/current/index.html").await;
        assert_eq!(response.status, 200);
        assert!(response.body.contains("<h1>home</h1>"));
    }

    /// fails if `--follow-symlinks` is mistaken for "trust every path"
    /// instead of "permit a symlink component, still enforce
    /// containment".
    #[tokio::test]
    async fn follow_symlinks_does_not_let_a_symlink_escape_the_root() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("index.html", "<h1>home</h1>")]);
        std::fs::write(outer.path().join("secret.txt"), "nope").unwrap();
        std::os::unix::fs::symlink(outer.path().join("secret.txt"), root.join("escape.txt"))
            .unwrap();

        let server = serve_on_free_port(config_with_follow_symlinks(&root)).await;
        let response = get(server.addr(), "/escape.txt").await;
        assert_eq!(response.status, 404);
        assert!(
            !response.body.contains("nope"),
            "the escape must not be served"
        );
    }

    /// fails if auth is checked after path resolution. An unauthenticated
    /// client must not be able to tell a refused traversal (400) from a
    /// missing file (404) apart.
    #[tokio::test]
    async fn an_unauthenticated_request_is_401_whatever_the_path_says() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("index.html", "<h1>home</h1>")]);
        std::fs::write(outer.path().join("secret.txt"), "nope").unwrap();
        let server = serve_on_free_port(config_with_auth(&root)).await;

        for target in ["/index.html", "/../secret.txt", "/nope.txt"] {
            let response = get(server.addr(), target).await;
            assert_eq!(response.status, 401, "{target}");
            assert!(
                response.headers.contains_key("www-authenticate"),
                "{target}"
            );
        }
        // positive control: with the credential, the same three answer
        // 200/400/404, not 401.
        assert_eq!(get_auth(server.addr(), "/index.html").await.status, 200);
        assert_eq!(get_auth(server.addr(), "/../secret.txt").await.status, 400);
        assert_eq!(get_auth(server.addr(), "/nope.txt").await.status, 404);
    }

    /// fails if a POST is served, or if the 405 forgets to say what is
    /// allowed.
    #[tokio::test]
    async fn a_method_other_than_get_or_head_is_405_with_an_allow_header() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("index.html", "<h1>home</h1>")]);
        let server = serve_on_free_port(config(&root)).await;

        let response = request_with_method(server.addr(), "POST", "/index.html").await;
        assert_eq!(response.status, 405);
        assert_eq!(response.headers["allow"], "GET, HEAD");
        assert!(!response.body.contains("<h1>home</h1>"), "{response:?}");
    }

    /// fails if a refusal reply loses its status, its own header, or the
    /// byte count it declared.
    #[tokio::test]
    async fn a_refusal_reply_carries_its_status_headers_and_body_length() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("index.html", "<h1>home</h1>")]);
        let guarded = serve_on_free_port(config_with_auth(&root)).await;
        let open = serve_on_free_port(config(&root)).await;

        let unauthorized = get(guarded.addr(), "/index.html").await;
        assert_eq!(unauthorized.status, 401);
        assert_eq!(unauthorized.body, "unauthorized\n");
        assert_eq!(
            unauthorized.headers["www-authenticate"],
            "Basic realm=\"shep\""
        );
        assert_eq!(unauthorized.headers["content-length"], "13");

        let refused = "method not allowed; this server answers GET and HEAD\n";
        let not_allowed = request_with_method(open.addr(), "POST", "/index.html").await;
        assert_eq!(not_allowed.status, 405);
        assert_eq!(not_allowed.body, refused);
        assert_eq!(not_allowed.headers["allow"], "GET, HEAD");
        assert_eq!(not_allowed.headers["content-length"], "53");
    }

    /// fails if a HEAD grows a body, or loses its Content-Length.
    #[tokio::test]
    async fn a_head_carries_the_length_and_no_body() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("index.html", "<h1>home</h1>")]);
        let server = serve_on_free_port(config(&root)).await;

        let response = request_with_method(server.addr(), "HEAD", "/index.html").await;
        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers["content-length"],
            "<h1>home</h1>".len().to_string()
        );
        assert!(response.body.is_empty(), "{response:?}");
    }

    /// fails if a directory without a trailing slash is served in place,
    /// which breaks every relative link in the page it serves.
    #[tokio::test]
    async fn a_directory_without_a_trailing_slash_redirects_to_one() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("docs/index.html", "<h1>docs</h1>")]);
        let server = serve_on_free_port(config(&root)).await;

        let response = get(server.addr(), "/docs").await;
        assert_eq!(response.status, 301);
        assert_eq!(response.headers["location"], "/docs/");
    }

    /// fails if listing is on by default.
    #[tokio::test]
    async fn a_directory_with_no_index_is_404_unless_listing_is_on() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("empty/.keep", "")]);

        let plain = serve_on_free_port(config(&root)).await;
        assert_eq!(get(plain.addr(), "/empty/").await.status, 404);

        let listing_on = serve_on_free_port(config_with_listing(&root)).await;
        let response = get(listing_on.addr(), "/empty/").await;
        assert_eq!(response.status, 200);
        assert!(response.body.contains("Index of"), "{response:?}");
    }

    /// fails if the SPA fallback fires for an asset request: a missing
    /// `/assets/app.js` must 404, not answer HTML with a 200.
    #[tokio::test]
    async fn the_spa_fallback_serves_index_for_navigations_and_404s_for_assets() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("index.html", "<h1>home</h1>")]);
        let server = serve_on_free_port(config_with_spa(&root)).await;

        let nav = get_with_accept(server.addr(), "/deep/link", "text/html").await;
        assert_eq!(nav.status, 200);
        assert!(nav.body.contains("<h1>home</h1>"));

        let asset = get_with_accept(server.addr(), "/assets/missing.js", "*/*").await;
        assert_eq!(asset.status, 404);
    }

    /// fails if `nosniff` is dropped, or if the MIME table is not reaching
    /// the response.
    #[tokio::test]
    async fn a_css_file_is_served_as_css_with_nosniff() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("app.css", "body{}")]);
        let server = serve_on_free_port(config(&root)).await;

        let response = get(server.addr(), "/app.css").await;
        assert_eq!(response.status, 200);
        assert_eq!(response.headers["content-type"], "text/css; charset=utf-8");
        assert_eq!(response.headers["x-content-type-options"], "nosniff");
        assert_eq!(response.body, "body{}");
    }

    /// fails if a body larger than any buffer is truncated or read whole
    /// into memory. 2 MiB is enough to cross every buffer in this path and
    /// small enough to write in a test.
    #[tokio::test]
    async fn a_file_larger_than_the_buffers_is_served_whole() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        std::fs::create_dir_all(&root).unwrap();
        let big = "a".repeat(2 * 1024 * 1024);
        std::fs::write(root.join("big.txt"), &big).unwrap();
        let server = serve_on_free_port(config(&root)).await;

        let response = get(server.addr(), "/big.txt").await;
        assert_eq!(response.status, 200);
        assert_eq!(response.headers["content-length"], big.len().to_string());
        assert_eq!(response.body.len(), big.len());
    }

    /// fails if a dotfile is served, or if `--hidden` stops serving one.
    #[tokio::test]
    async fn a_dotfile_is_404_unless_hidden_is_set() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(
            &root,
            &[
                (".env", "SECRET=1"),
                (".git/config", "[core]"),
                ("index.html", "<h1>home</h1>"),
                (".well-known/acme-challenge/x", "token"),
            ],
        );

        let plain = serve_on_free_port(config(&root)).await;
        assert_eq!(get(plain.addr(), "/.env").await.status, 404);
        assert_eq!(get(plain.addr(), "/.git/config").await.status, 404);
        assert_eq!(
            get(plain.addr(), "/index.html").await.status,
            200,
            "positive control"
        );

        let hidden = serve_on_free_port(config_with_hidden(&root)).await;
        assert_eq!(
            get(hidden.addr(), "/.well-known/acme-challenge/x")
                .await
                .status,
            200
        );
    }

    /// fails if a listing names a file the server will not serve, or
    /// drops `nosniff` on its own response.
    #[tokio::test]
    async fn a_listing_omits_hidden_entries_and_carries_nosniff() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[(".env", "SECRET=1"), ("app.js", "console.log(1)")]);
        let server = serve_on_free_port(config_with_listing(&root)).await;

        let response = get(server.addr(), "/").await;
        assert_eq!(response.status, 200);
        assert!(!response.body.contains(".env"), "{response:?}");
        assert!(response.body.contains("app.js"), "{response:?}");
        assert_eq!(response.headers["x-content-type-options"], "nosniff");
    }

    /// fails if a listing goes out as plain text, or declares a length
    /// that disagrees with the HTML it sent.
    #[tokio::test]
    async fn a_directory_listing_reply_is_html_and_counts_its_bytes() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("a.txt", "a"), ("b.txt", "bb")]);
        let server = serve_on_free_port(config_with_listing(&root)).await;

        let response = get(server.addr(), "/").await;
        assert_eq!(response.status, 200);
        assert_eq!(response.headers["content-type"], "text/html; charset=utf-8");
        assert!(
            response.body.contains("<h1>Index of /</h1>"),
            "{response:?}"
        );
        assert!(response.body.contains("a.txt"), "{response:?}");
        let declared = response.body.len().to_string();
        assert_eq!(response.headers["content-length"], declared);
    }

    /// fails if a directory whose name is not ASCII answers 500 instead
    /// of a percent-encoded redirect.
    #[tokio::test]
    async fn a_directory_with_a_non_ascii_name_redirects_rather_than_500ing() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("документы/index.html", "<h1>docs</h1>")]);
        let server = serve_on_free_port(config(&root)).await;

        let response = get(server.addr(), "/документы").await;
        assert_eq!(response.status, 301);
        assert_eq!(
            response.headers["location"],
            "/%D0%B4%D0%BE%D0%BA%D1%83%D0%BC%D0%B5%D0%BD%D1%82%D1%8B/"
        );
    }

    /// A modification time no freshly written tempdir file can carry:
    /// 2027-01-15T00:00:00Z.
    fn later_than_the_write() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    /// Moves `path`'s mtime to `when` with its contents untouched, so a
    /// "changed after the first GET" case runs on no clock.
    fn touch_at(path: &Path, when: SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    /// A docroot holding one ten-byte file, the fixture every range case
    /// reads its expected bytes out of by hand.
    async fn ten_byte_server() -> (tempfile::TempDir, RunningServer) {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("clip.txt", "0123456789")]);
        let server = serve_on_free_port(config(&root)).await;
        (outer, server)
    }

    /// fails if a single byte range is answered with the whole file.
    #[tokio::test]
    async fn a_single_byte_range_answers_206_with_that_slice_and_a_content_range() {
        let (_outer, server) = ten_byte_server().await;

        let response = get_with_headers(server.addr(), "/clip.txt", &[("Range", "bytes=2-5")]).await;
        assert_eq!(response.status, 206);
        assert_eq!(response.body, "2345");
        assert_eq!(response.headers["content-range"], "bytes 2-5/10");
        assert_eq!(response.headers["content-length"], "4");
    }

    /// fails if an open-ended range or a suffix range is answered from the
    /// front of the file.
    #[tokio::test]
    async fn an_open_ended_range_and_a_suffix_range_answer_the_two_ends() {
        let (_outer, server) = ten_byte_server().await;

        let tail = get_with_headers(server.addr(), "/clip.txt", &[("Range", "bytes=7-")]).await;
        assert_eq!(tail.status, 206);
        assert_eq!(tail.body, "789");
        assert_eq!(tail.headers["content-range"], "bytes 7-9/10");
        assert_eq!(tail.headers["content-length"], "3");

        let suffix = get_with_headers(server.addr(), "/clip.txt", &[("Range", "bytes=-3")]).await;
        assert_eq!(suffix.status, 206);
        assert_eq!(suffix.body, "789");
        assert_eq!(suffix.headers["content-range"], "bytes 7-9/10");
    }

    /// fails if a range past the end is answered with a 200 of the whole
    /// file, or a 206 of nothing.
    #[tokio::test]
    async fn a_range_past_the_end_is_416_naming_a_length_it_does_not_have() {
        let (_outer, server) = ten_byte_server().await;

        let response = get_with_headers(server.addr(), "/clip.txt", &[("Range", "bytes=999-")]).await;
        assert_eq!(response.status, 416);
        assert_eq!(response.headers["content-range"], "bytes */10");
        assert!(response.body.is_empty(), "{response:?}");
        assert_eq!(response.headers["content-length"], "0");
    }

    /// fails if a header naming two ranges is refused, or honoured as if
    /// this server could write a multipart body.
    #[tokio::test]
    async fn a_header_naming_two_ranges_is_ignored_and_answered_with_the_whole_file() {
        let (_outer, server) = ten_byte_server().await;

        let response =
            get_with_headers(server.addr(), "/clip.txt", &[("Range", "bytes=0-1,4-5")]).await;
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "0123456789");
        assert_eq!(response.headers["content-length"], "10");
        assert!(!response.headers.contains_key("content-range"), "{response:?}");
    }

    /// fails if `Accept-Ranges` is dropped from a file reply, which is what
    /// tells a client it may ask for a slice next time, or if a refusal
    /// claims one.
    #[tokio::test]
    async fn every_file_reply_says_the_server_takes_ranges() {
        let (_outer, server) = ten_byte_server().await;

        let cases: [(&str, &[(&str, &str)], u16); 4] = [
            ("/clip.txt", &[], 200),
            ("/clip.txt", &[("Range", "bytes=2-5")], 206),
            ("/clip.txt", &[("Range", "bytes=999-")], 416),
            ("/clip.txt", &[("Range", "bytes=0-1,4-5")], 200),
        ];
        for (target, extra, want) in cases {
            let response = get_with_headers(server.addr(), target, extra).await;
            assert_eq!(response.status, want, "{target} {extra:?}");
            assert_eq!(
                response.headers["accept-ranges"], "bytes",
                "{target} {extra:?}: {response:?}"
            );
        }

        let missing = get(server.addr(), "/nope.txt").await;
        assert_eq!(missing.status, 404);
        assert!(
            !missing.headers.contains_key("accept-ranges"),
            "a refusal offers no ranges: {missing:?}"
        );
    }

    /// fails if a repeat GET re-reads a file the client already holds: the
    /// weak ETag the first reply gave must earn a 304 with no body.
    #[tokio::test]
    async fn a_repeat_get_with_the_etag_it_was_given_answers_304() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("app.js", "console.log(1)")]);
        let server = serve_on_free_port(config(&root)).await;

        let first = get(server.addr(), "/app.js").await;
        assert_eq!(first.status, 200);
        let etag = first.headers["etag"].clone();
        assert!(etag.starts_with("W/\""), "{etag}: built from metadata");
        assert!(first.headers.contains_key("last-modified"), "{first:?}");

        let again = get_with_headers(server.addr(), "/app.js", &[("If-None-Match", &etag)]).await;
        assert_eq!(again.status, 304);
        assert!(again.body.is_empty(), "{again:?}");
        assert!(!again.headers.contains_key("content-length"), "{again:?}");
        assert_eq!(again.headers["etag"], etag, "a 304 repeats its validator");
        assert_eq!(
            again.headers["x-content-type-options"], "nosniff",
            "a 304 is a reply like any other"
        );
    }

    /// fails if `Last-Modified` is written in a form this same server cannot
    /// read back.
    #[tokio::test]
    async fn a_repeat_get_with_the_last_modified_it_was_given_answers_304() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("app.js", "console.log(1)")]);
        let server = serve_on_free_port(config(&root)).await;

        let first = get(server.addr(), "/app.js").await;
        let last_modified = first.headers["last-modified"].clone();

        let again = get_with_headers(
            server.addr(),
            "/app.js",
            &[("If-Modified-Since", &last_modified)],
        )
        .await;
        assert_eq!(again.status, 304);
        assert!(again.body.is_empty(), "{again:?}");
        assert_eq!(again.headers["last-modified"], last_modified);
    }

    /// fails if a file touched after the first GET keeps answering 304 off
    /// the validator the client holds.
    #[tokio::test]
    async fn a_file_touched_after_the_first_get_answers_200_again() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("app.css", "body{}")]);
        let server = serve_on_free_port(config(&root)).await;

        let first = get(server.addr(), "/app.css").await;
        let etag = first.headers["etag"].clone();
        let last_modified = first.headers["last-modified"].clone();

        touch_at(&root.join("app.css"), later_than_the_write());

        let by_tag = get_with_headers(server.addr(), "/app.css", &[("If-None-Match", &etag)]).await;
        assert_eq!(by_tag.status, 200, "the tag no longer names this file");
        assert_eq!(by_tag.body, "body{}");
        assert_ne!(by_tag.headers["etag"], etag);

        let by_date = get_with_headers(
            server.addr(),
            "/app.css",
            &[("If-Modified-Since", &last_modified)],
        )
        .await;
        assert_eq!(by_date.status, 200, "the file is newer than the copy held");
        assert_eq!(by_date.body, "body{}");
    }

    /// fails if `If-Modified-Since` is consulted after an `If-None-Match`
    /// that did not match (RFC 9110 §13.2.2).
    #[tokio::test]
    async fn if_none_match_outranks_if_modified_since() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("app.js", "console.log(1)")]);
        let server = serve_on_free_port(config(&root)).await;

        let first = get(server.addr(), "/app.js").await;
        let last_modified = first.headers["last-modified"].clone();

        let response = get_with_headers(
            server.addr(),
            "/app.js",
            &[("If-None-Match", "W/\"not-this-file\""), ("If-Modified-Since", &last_modified)],
        )
        .await;
        assert_eq!(response.status, 200, "the newer validator is the one that answers");
        assert_eq!(response.body, "console.log(1)");
    }

    /// fails if a stale `If-Range` still gets its slice, which would hand
    /// the client bytes stitched onto a copy it no longer holds.
    #[tokio::test]
    async fn a_stale_if_range_ignores_the_range_and_sends_the_whole_file() {
        let (_outer, server) = ten_byte_server().await;

        let stale = get_with_headers(
            server.addr(),
            "/clip.txt",
            &[("If-Range", "Sun, 06 Nov 1994 08:49:37 GMT"), ("Range", "bytes=2-5")],
        )
        .await;
        assert_eq!(stale.status, 200);
        assert_eq!(stale.body, "0123456789");
        assert!(!stale.headers.contains_key("content-range"), "{stale:?}");

        // positive control: the file's own `Last-Modified` does apply it.
        let first = get(server.addr(), "/clip.txt").await;
        let last_modified = first.headers["last-modified"].clone();
        let slice = get_with_headers(
            server.addr(),
            "/clip.txt",
            &[("If-Range", &last_modified), ("Range", "bytes=2-5")],
        )
        .await;
        assert_eq!(slice.status, 206);
        assert_eq!(slice.body, "2345");
    }

    /// fails if a HEAD grows a body on a 206 or a 304, or answers a range
    /// with a different head than the GET would.
    #[tokio::test]
    async fn a_head_answers_the_same_status_and_headers_as_the_get_with_no_body() {
        let (_outer, server) = ten_byte_server().await;

        let got = get_with_headers(server.addr(), "/clip.txt", &[("Range", "bytes=2-5")]).await;
        let headed =
            request_with_headers(server.addr(), "HEAD", "/clip.txt", &[("Range", "bytes=2-5")]).await;
        assert_eq!(headed.status, 206);
        assert!(headed.body.is_empty(), "{headed:?}");
        assert_eq!(headed.headers["content-range"], "bytes 2-5/10");
        assert_eq!(headed.headers["content-length"], "4");
        assert_eq!(headed.headers["etag"], got.headers["etag"]);

        let etag = got.headers["etag"].clone();
        let not_modified = request_with_headers(
            server.addr(),
            "HEAD",
            "/clip.txt",
            &[("If-None-Match", &etag)],
        )
        .await;
        assert_eq!(not_modified.status, 304);
        assert!(not_modified.body.is_empty(), "{not_modified:?}");
        assert!(
            !not_modified.headers.contains_key("content-length"),
            "{not_modified:?}"
        );
    }

    /// fails if the `--spa` fallback answers with a bare 200 a browser has
    /// nothing to cache against.
    #[tokio::test]
    async fn the_spa_fallback_carries_the_same_validators_as_a_real_hit() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("index.html", "<h1>home</h1>")]);
        let server = serve_on_free_port(config_with_spa(&root)).await;

        let first = get_with_accept(server.addr(), "/deep/link", "text/html").await;
        assert_eq!(first.status, 200);
        assert!(first.body.contains("<h1>home</h1>"));
        let etag = first.headers["etag"].clone();
        assert_eq!(first.headers["accept-ranges"], "bytes");

        let again = get_with_headers(
            server.addr(),
            "/deep/link",
            &[("If-None-Match", &etag), ("Accept", "text/html")],
        )
        .await;
        assert_eq!(again.status, 304);
        assert!(again.body.is_empty(), "{again:?}");

        let slice = get_with_headers(
            server.addr(),
            "/deep/link",
            &[("Accept", "text/html"), ("Range", "bytes=0-3")],
        )
        .await;
        assert_eq!(slice.status, 206);
        assert_eq!(slice.body, "<h1>");
    }
}
