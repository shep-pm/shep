// The loopback HTTP fake split out of `testing`: the gate a probe test dials.
// It answers one accepted connection from the script it was bound with, then
// from whatever the test queues later through `LoopbackHttp::queue_reply`, so
// a test holds the verdict instead of pacing it on the prober's own interval.
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// One scripted reply [`loopback_http`] serves for one accepted connection.
pub(crate) enum HttpReply {
    /// Writes a minimal `HTTP/1.1 {code} OK\r\n\r\n` status line, then closes.
    Status(u16),
    /// Writes `raw` verbatim, then closes, for a response that is not a
    /// well-formed HTTP status line.
    Raw(String),
    /// Accepts the connection and never writes a byte, the way to reach
    /// `OsProber`'s read-side timeout: a reply queued for the connection
    /// always resolves the read.
    Hang,
}

/// Longest request head [`loopback_http`] will read off a connection before
/// replying anyway, so a client that never sends the blank line ending its
/// headers cannot grow this fixture's buffer without bound.
const REQUEST_HEAD_CAP: usize = 8 * 1024;

/// How long [`LoopbackHttp::next_request`] waits for a request to arrive.
/// Failing there beats hanging a test binary that has no per-test deadline.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);

/// A bound loopback HTTP fake, plus the requests it has received.
///
/// Aborts its own accept loop on drop, so a test owns the fake for exactly
/// its own scope and never has to remember the teardown.
pub(crate) struct LoopbackHttp {
    /// Where it is listening. Already bound by the time this struct exists.
    pub(crate) addr: SocketAddr,
    requests: mpsc::UnboundedReceiver<String>,
    replies_tx: mpsc::UnboundedSender<HttpReply>,
    accept_loop: tokio::task::JoinHandle<()>,
}

impl LoopbackHttp {
    /// The request head of the next connection this fake accepted, verbatim.
    ///
    /// Requests are queued as they arrive, so this may return immediately;
    /// what it never does is wait forever.
    pub(crate) async fn next_request(&mut self) -> String {
        tokio::time::timeout(REQUEST_DEADLINE, self.requests.recv())
            .await
            .expect("the fake received no request within the deadline")
            .expect("the fake's accept loop ended without sending a request")
    }

    /// Queues the reply this fake writes on its next connection.
    ///
    /// The fake answers only what it has been given, script first and then
    /// whatever is queued here: a connection waits for its reply rather than
    /// inventing a verdict. That is what lets a test hold a probe failing
    /// until the moment it chooses to let one pass, instead of pacing the
    /// flip on the prober's own interval.
    pub(crate) fn queue_reply(&self, reply: HttpReply) {
        self.replies_tx
            .send(reply)
            .expect("the fake's accept loop had already ended");
    }
}

impl Drop for LoopbackHttp {
    fn drop(&mut self) {
        self.accept_loop.abort();
    }
}

/// Binds a loopback HTTP fake on `127.0.0.1:0`; see [`loopback_http_on`].
pub(crate) async fn loopback_http(script: Vec<HttpReply>) -> LoopbackHttp {
    loopback_http_on("127.0.0.1:0", script).await
}

/// Binds a loopback HTTP fake on `bind` and serves one reply per accepted connection,
/// in queue order: the script first, then anything the test queues through
/// [`LoopbackHttp::queue_reply`]. A connection with no reply queued yet waits for one.
/// Binds before spawning the accept loop, so a probe dialing the address cannot race it.
///
/// Every connection is read before it is replied to, [`HttpReply::Hang`] included:
/// recording the request is how a prober that dropped `Host:` fails.
pub(crate) async fn loopback_http_on(bind: &str, script: Vec<HttpReply>) -> LoopbackHttp {
    let listener = TcpListener::bind(bind)
        .await
        .unwrap_or_else(|err| panic!("bind loopback HTTP fake on {bind}: {err}"));
    let addr = listener.local_addr().expect("read bound loopback address");
    let (requests_tx, requests) = mpsc::unbounded_channel();
    let (replies_tx, mut replies_rx) = mpsc::unbounded_channel();
    for reply in script {
        replies_tx.send(reply).expect("fresh reply receiver");
    }
    let accept_loop = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _peer)) = listener.accept().await else {
                return; // listener gone: nothing left to serve
            };
            if requests_tx
                .send(read_request_head(&mut stream).await)
                .is_err()
            {
                return; // the owning test dropped its LoopbackHttp
            }
            let Some(reply) = replies_rx.recv().await else {
                return; // the owning test dropped its LoopbackHttp
            };
            match reply {
                HttpReply::Status(code) => {
                    let response = format!("HTTP/1.1 {code} OK\r\n\r\n");
                    let _ = stream.write_all(response.as_bytes()).await;
                }
                HttpReply::Raw(raw) => {
                    let _ = stream.write_all(raw.as_bytes()).await;
                }
                HttpReply::Hang => {
                    // Never write, never drop `stream` early: the connection
                    // stays open until the caller's own timeout fires or this
                    // task is aborted.
                    std::future::pending::<()>().await;
                }
            }
        }
    });
    LoopbackHttp {
        addr,
        requests,
        replies_tx,
        accept_loop,
    }
}

/// Reads one request head, everything through the blank line that ends the
/// headers, giving up at EOF, a read error, or [`REQUEST_HEAD_CAP`].
async fn read_request_head(stream: &mut tokio::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut chunk = [0_u8; 256];
    while !head.ends_with(b"\r\n\r\n") && head.len() < REQUEST_HEAD_CAP {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&chunk[..n]),
        }
    }
    // Lossy, not `expect`: what a prober writes is exactly what a test needs
    // to see, including bytes that are not UTF-8 at all.
    String::from_utf8_lossy(&head).into_owned()
}
