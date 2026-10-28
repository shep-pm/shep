//! `shep serve`'s worker: binds a listener, accepts connections, and
//! answers each one against [`ServeConfig`].
//!
//! `path`, `mime`, `listing` and `auth` stay pure; this module calls them.
//! `answer` owns what a request gets and `response` owns writing a reply.
//!
//! The accept loop admits through a [`tokio::sync::Semaphore`] and wraps
//! each connection in a [`tokio::time::timeout`], so a stalled client cannot
//! hold a task, an open file and a socket.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use super::auth::Credentials;
use crate::exit::ExitCode;
use crate::http;

mod answer;
mod response;
#[cfg(all(test, unix))]
mod testing;

use answer::{answer, path_of};
use response::log_access;

/// How long [`http::read_request`] waits for a connected peer to finish
/// sending its request. Small enough that a peer that connects and says
/// nothing does not hold a task, and here a semaphore permit, open
/// indefinitely.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How many connections may be in flight at once.
///
/// A permit is held for the whole life of a connection task, so this
/// bounds tasks, sockets and open files together. A full 512 can want
/// 1024 descriptors: over macOS's 256 `maxfiles`, over Linux's 1024.
const MAX_CONNECTIONS: usize = 512;

/// The whole-connection deadline every real `shep serve` runs with: read,
/// resolve, respond and copy together.
///
/// `READ_TIMEOUT` bounds only the read phase. A test overrides it through
/// [`ServeConfig::connection_deadline`].
pub const CONNECTION_DEADLINE: Duration = Duration::from_secs(60);

/// What a running `serve` worker was told to do.
///
/// No `Debug`: [`Credentials`] carries none, and an
/// `Option<Credentials>` inherits that.
pub struct ServeConfig {
    /// The docroot, already canonical and already known to be a directory.
    pub root: PathBuf,
    /// Where to listen. Loopback unless the operator said otherwise.
    pub bind: SocketAddr,
    /// Serve `<root>/index.html` for a would-be 404 that accepts HTML.
    pub spa: bool,
    /// Render a listing for a directory with no index.
    pub listing: bool,
    /// Serve paths with a leading-dot segment, and list them. Off by
    /// default: `shep serve .` in a repo checkout would otherwise publish
    /// `.env` and the whole `.git` object store.
    pub hidden: bool,
    /// Credentials every request must satisfy, if any.
    pub auth: Option<Credentials>,
    /// Permit a symlink anywhere in the resolved path, falling back to a
    /// canonicalize-then-`starts_with` containment check. Off by default;
    /// passed straight through to `fs::contain`, whose own doc names the
    /// residual race each mode still leaves open.
    pub follow_symlinks: bool,
    /// How long one connection may live, start to finish. A test picks
    /// its own; [`CONNECTION_DEADLINE`] everywhere else.
    pub connection_deadline: Duration,
}

/// Runs `shep serve`'s worker until it is signalled: binds `cfg.bind` and
/// serves until `SIGINT` or `SIGTERM`, the latter being what the
/// shepherd's own kill ladder sends first.
///
/// A refused bind is fatal: a worker bound to nothing looks fine from
/// outside. Tests drive [`accept_forever`] instead, since exercising
/// `SIGTERM` needs a real stop ladder.
pub async fn run(cfg: ServeConfig) -> ExitCode {
    let listener = match TcpListener::bind(cfg.bind).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("shep serve: could not bind {}: {err}", cfg.bind);
            return ExitCode::Failure;
        }
    };
    let mut sigterm = match crate::shutdown::Terminate::install() {
        Ok(sigterm) => sigterm,
        Err(err) => {
            eprintln!("shep serve: could not install a SIGTERM handler: {err}");
            return ExitCode::Failure;
        }
    };
    let cfg = Arc::new(cfg);
    let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
        () = accept_forever(listener, cfg, semaphore) => {}
    }
    ExitCode::Success
}

/// Accepts connections off `listener` forever, one task per connection.
/// Never returns: [`run`] races it against the shutdown signals, and a
/// test aborts the [`tokio::task::JoinHandle`] it returns instead.
///
/// A permit is acquired before the task is spawned, non-blockingly: a
/// connection past the cap is closed rather than queued, since queuing
/// would hold the socket with no task.
async fn accept_forever(listener: TcpListener, cfg: Arc<ServeConfig>, semaphore: Arc<Semaphore>) {
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let cfg = Arc::clone(&cfg);
                tokio::spawn(async move {
                    let _permit = permit;
                    let deadline = cfg.connection_deadline;
                    let _ = tokio::time::timeout(deadline, handle_connection(stream, cfg)).await;
                });
            }
            Err(err) => {
                eprintln!("shep serve: accept failed: {err}");
            }
        }
    }
}

/// Answers exactly one request on `stream`: read, auth, method, body,
/// resolve, hidden, contain, then a directory or a file, in that order.
///
/// The reply is [`answer::answer`]'s; the single [`log_access`] call is here, so
/// no exit path can answer a request and leave it out of the log.
async fn handle_connection(mut stream: TcpStream, cfg: Arc<ServeConfig>) {
    let peer = stream.peer_addr().ok();

    // A failed read leaves no well-formed request to answer or log.
    let request = match http::read_request(&mut stream, READ_TIMEOUT).await {
        Ok(request) => request,
        Err(_err) => return,
    };

    let mut logged_path = path_of(&request.target).to_string();
    let (status, bytes) = answer(&mut stream, &cfg, &request, &mut logged_path).await;
    log_access(peer, &request.method, &logged_path, status, bytes);
}

// unix only: the cases assert file modes on served files.
// Windows coverage is in `tests/cli_e2e.rs`.
#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::MAX_CONNECTIONS;
    use super::testing::{config, serve_on_free_port, write_tree};

    /// fails if there is no ceiling on concurrent connections. Opens
    /// `MAX_CONNECTIONS` sockets and holds them open, then asserts a
    /// connection past the cap is closed within a fraction of a second
    /// rather than held.
    #[tokio::test]
    async fn connections_beyond_the_cap_are_closed_rather_than_queued_forever() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        write_tree(&root, &[("index.html", "<h1>home</h1>")]);
        let server = serve_on_free_port(config(&root)).await;

        let mut held = Vec::with_capacity(MAX_CONNECTIONS);
        for _ in 0..MAX_CONNECTIONS {
            let stream =
                tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(server.addr()))
                    .await
                    .expect("connect must not hang")
                    .unwrap();
            held.push(stream);
        }
        // Give every handler task a moment to actually reach its own
        // `read_request` call (and so acquire its permit) before the
        // one-over-the-cap connection below is attempted.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut over_cap =
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(server.addr()))
                .await
                .expect("connect must not hang")
                .unwrap();
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_millis(500), over_cap.read_to_end(&mut buf))
            .await
            .expect("a connection beyond the cap must close promptly rather than hang")
            .unwrap();
        assert!(
            buf.is_empty(),
            "nothing should be written to a refused connection: {buf:?}"
        );

        drop(held);
    }

    /// fails if a connection that stops reading mid-body is held for the
    /// life of the process.
    ///
    /// Runs on a real clock with a tiny deadline. A paused clock races
    /// tokio's auto-advance against real socket IO, and the wrong side
    /// can win.
    #[tokio::test]
    async fn a_connection_that_stops_reading_is_dropped_at_the_deadline() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("www");
        // Larger than an ordinary kernel send buffer, so the server's write
        // genuinely blocks on backpressure once the client stops reading,
        // rather than completing into the buffer regardless.
        let big = "a".repeat(8 * 1024 * 1024);
        write_tree(&root, &[("big.txt", &big)]);
        let mut cfg = config(&root);
        // Short enough to wait out in a test, long enough that a loaded
        // machine still gets the connection established and the write
        // started before it fires.
        cfg.connection_deadline = Duration::from_millis(300);
        let server = serve_on_free_port(cfg).await;

        let mut stream = TcpStream::connect(server.addr()).await.unwrap();
        stream
            .write_all(b"GET /big.txt HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        // Never read anything: connect and send only. The deadline must
        // close the connection, which the client sees as EOF. The guard is
        // generous because it exists to turn a hang into a failure, not to
        // measure the deadline.
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut rest))
            .await
            .expect("the deadline must close the connection rather than hang forever")
            .unwrap();
    }
}
