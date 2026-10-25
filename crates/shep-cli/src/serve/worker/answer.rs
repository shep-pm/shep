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
mod tests;
