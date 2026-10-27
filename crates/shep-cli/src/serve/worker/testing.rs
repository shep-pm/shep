//! Fixtures and helpers shared by this module's tests: a docroot written
//! to disk, a [`ServeConfig`] over it, a worker serving it on a free
//! port, and one raw HTTP/1.1 exchange against that worker.
//!
//! `unix` only, for the same reason the test modules are: `config_with_auth`
//! writes a credentials file and sets its permission mode.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use super::{CONNECTION_DEADLINE, MAX_CONNECTIONS, ServeConfig, accept_forever};
use crate::serve::auth;

/// A running worker bound to an OS-assigned loopback port. Aborts its
/// accept loop on drop, so it cannot hold a port past its own test.
pub(super) struct RunningServer {
    addr: SocketAddr,
    handle: JoinHandle<()>,
}

impl RunningServer {
    pub(super) fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// A docroot at `root` holding `files`, each `(relative path, contents)`.
pub(super) fn write_tree(root: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(root).unwrap();
    for (path, contents) in files {
        let full = root.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, contents).unwrap();
    }
}

/// A default [`ServeConfig`] over `root`, bound to a free loopback port.
pub(super) fn config(root: &Path) -> ServeConfig {
    ServeConfig {
        root: std::fs::canonicalize(root).unwrap(),
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        spa: false,
        listing: false,
        hidden: false,
        auth: None,
        follow_symlinks: false,
        connection_deadline: CONNECTION_DEADLINE,
    }
}

/// The credential pair the auth-enabled tests authenticate with.
const TEST_USER: &str = "alice:s3cret";

pub(super) fn config_with_auth(root: &Path) -> ServeConfig {
    let creds_path = root
        .parent()
        .expect("root must sit inside the caller's own tempdir")
        .join("creds");
    std::fs::write(&creds_path, format!("{TEST_USER}\n")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&creds_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut built = config(root);
    built.auth = Some(auth::load(&creds_path).unwrap());
    built
}

pub(super) fn config_with_spa(root: &Path) -> ServeConfig {
    let mut built = config(root);
    built.spa = true;
    built
}

pub(super) fn config_with_listing(root: &Path) -> ServeConfig {
    let mut built = config(root);
    built.listing = true;
    built
}

pub(super) fn config_with_hidden(root: &Path) -> ServeConfig {
    let mut built = config(root);
    built.hidden = true;
    built
}

pub(super) fn config_with_follow_symlinks(root: &Path) -> ServeConfig {
    let mut built = config(root);
    built.follow_symlinks = true;
    built
}

/// Binds `config.bind`, reads back the OS-assigned address, and serves it in the background.
pub(super) async fn serve_on_free_port(config: ServeConfig) -> RunningServer {
    let listener = TcpListener::bind(config.bind).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let handle = tokio::spawn(accept_forever(listener, Arc::new(config), semaphore));
    RunningServer { addr, handle }
}

/// One HTTP response, parsed just enough for these tests: a status
/// code, lower-cased header names, and the body as text.
#[derive(Debug)]
pub(super) struct Response {
    pub(super) status: u16,
    pub(super) headers: HashMap<String, String>,
    pub(super) body: String,
}

fn parse_response(raw: &[u8]) -> Response {
    let text = String::from_utf8_lossy(raw);
    let mut halves = text.splitn(2, "\r\n\r\n");
    let head = halves.next().unwrap_or_default();
    let body = halves.next().unwrap_or_default().to_string();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    Response {
        status,
        headers,
        body,
    }
}

/// Sends one raw request (a full, correctly terminated HTTP/1.1 head)
/// and returns the parsed response. Every step carries a generous
/// timeout, so a hang here is a bug in the worker, not the suite.
async fn send_request(addr: SocketAddr, request: &str) -> Response {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connect must not hang")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), stream.write_all(request.as_bytes()))
        .await
        .expect("write must not hang")
        .unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .expect("read must not hang")
        .unwrap();
    parse_response(&buf)
}

pub(super) async fn get(addr: SocketAddr, target: &str) -> Response {
    request_with_headers(addr, "GET", target, &[]).await
}

pub(super) async fn get_with_accept(addr: SocketAddr, target: &str, accept: &str) -> Response {
    get_with_headers(addr, target, &[("Accept", accept)]).await
}

pub(super) async fn get_auth(addr: SocketAddr, target: &str) -> Response {
    get_with_headers(
        addr,
        target,
        &[("Authorization", &format!("Basic {}", base64(TEST_USER)))],
    )
    .await
}

pub(super) async fn request_with_method(addr: SocketAddr, method: &str, target: &str) -> Response {
    request_with_headers(addr, method, target, &[]).await
}

/// One `GET` carrying `extra`, each `(name, value)`, on top of `Host`.
/// The shape every range and conditional case needs.
pub(super) async fn get_with_headers(
    addr: SocketAddr,
    target: &str,
    extra: &[(&str, &str)],
) -> Response {
    request_with_headers(addr, "GET", target, extra).await
}

/// One request of `method` carrying `extra` headers on top of `Host`,
/// each `(name, value)`, written in the order given.
pub(super) async fn request_with_headers(
    addr: SocketAddr,
    method: &str,
    target: &str,
    extra: &[(&str, &str)],
) -> Response {
    let mut head = format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\n");
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    send_request(addr, &head).await
}

/// [`auth::base64_encode`] over a `&str`, for the header built by hand.
fn base64(input: &str) -> String {
    auth::base64_encode(input.as_bytes())
}
