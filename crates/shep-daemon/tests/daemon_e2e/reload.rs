//! A reload under load, counted connection by connection: whether the app
//! that drains loses any, and whether the one that will not finish its work
//! loses that work.

use super::*;

/// Serializes this file's reload measurements: each hands a fixed port to a
/// child that binds it twice over, so the two cannot interleave.
///
/// `tokio::sync::Mutex` because the guard is held across `.await`, where
/// clippy's `await_holding_lock` denies a blocking guard. It does not
/// serialize against other test binaries, which cargo also runs concurrently.
pub(crate) static RELOAD_PORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How long [`reuse_port_sheep`] holds a connection before answering it.
///
/// At [`CONNECT_INTERVAL`] this keeps around fifteen connections open at every
/// instant, which an instance killed mid-flight destroys.
pub(crate) const HOLD_MS: u64 = 60;

/// One new connection every 4ms for as long as a reload lasts.
///
/// Fast enough that the window between the drainee emptying its accept queue
/// and closing its listener is a real chance to lose something, slow enough
/// that a loss is never the fixture's queue overflowing.
pub(crate) const CONNECT_INTERVAL: Duration = Duration::from_millis(4);

/// How long one connection gets before it counts as lost. Two orders of
/// magnitude over [`HOLD_MS`]: slack for a loaded runner, not an expected
/// duration.
pub(crate) const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

/// The `AwaitReady` window a replacement gets, as `listen_timeout`.
///
/// The fixture signals no readiness, so this is the `Heuristic` case: the
/// deadline elapsing is the verdict, and it holds the drainee's kill ladder
/// back until the replacement has bound. Half a second against a process that
/// binds in single-digit milliseconds.
pub(crate) const READY_WINDOW: UpDuration = UpDuration::from_millis(500);

/// The drain window a replaced instance gets, as `graceful_timeout`, and for
/// an instance that will not take its stop signal, how long it is before
/// `SIGKILL`. Short because nothing here needs longer; the spec default is 8s.
pub(crate) const DRAIN_WINDOW: UpDuration = UpDuration::from_millis(1_000);

/// Connections opened at once before a reload and again after it, to establish
/// which process owns the port at each end of the swap.
pub(crate) const BURST: usize = 10;

/// What one connection to the fixture got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Attempt {
    /// Answered, by the process with this pid, which keeps "the port
    /// answered" and "that process answered" separate claims.
    Served(u32),
    /// Refused, reset, timed out, or closed with nothing on it, carrying the
    /// reason. A connection accepted into a backlog whose listener then closed
    /// arrives as an empty answer, not a connect error.
    Failed(String),
}

impl Attempt {
    pub(crate) fn failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// One connection: open it, read what the server says, classify the outcome.
pub(crate) async fn attempt(port: u16) -> Attempt {
    let exchange = tokio::time::timeout(ATTEMPT_TIMEOUT, async {
        let mut conn = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        let mut answer = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut conn, &mut answer).await?;
        std::io::Result::Ok(answer)
    });
    match exchange.await {
        Err(_) => Attempt::Failed(format!("no answer inside {ATTEMPT_TIMEOUT:?}")),
        Ok(Err(error)) => Attempt::Failed(error.to_string()),
        Ok(Ok(answer)) => match answer.trim().parse() {
            Ok(pid) => Attempt::Served(pid),
            Err(_) => Attempt::Failed(format!("answered {answer:?}")),
        },
    }
}

/// Opens [`BURST`] connections at once and hands back what each got.
pub(crate) async fn burst(port: u16) -> Vec<Attempt> {
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..BURST {
        set.spawn(attempt(port));
    }
    let mut attempts = Vec::new();
    while let Some(outcome) = set.join_next().await {
        attempts.push(outcome.expect("an attempt cannot panic"));
    }
    attempts
}

/// A caller that keeps connecting for as long as a reload takes.
pub(crate) struct Hammer {
    pub(crate) stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) task: tokio::task::JoinHandle<Vec<Attempt>>,
}

impl Hammer {
    /// Starts opening one connection every [`CONNECT_INTERVAL`].
    pub(crate) fn start(port: u16) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = tokio::spawn({
            let stop = std::sync::Arc::clone(&stop);
            async move {
                let mut set = tokio::task::JoinSet::new();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    set.spawn(attempt(port));
                    tokio::time::sleep(CONNECT_INTERVAL).await;
                }
                // Connections already open are waited out: those in flight at
                // the swap are the ones an instance killed mid-answer loses.
                let mut attempts = Vec::new();
                while let Some(outcome) = set.join_next().await {
                    attempts.push(outcome.expect("an attempt cannot panic"));
                }
                attempts
            }
        });
        Self { stop, task }
    }

    /// Stops connecting and reports every attempt, in-flight ones included.
    pub(crate) async fn finish(self) -> Vec<Attempt> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.task.await.expect("the hammer cannot panic")
    }
}

/// A one-line tally of a run of attempts, for a failure message.
pub(crate) fn tally(attempts: &[Attempt]) -> String {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for attempt in attempts {
        let key = match attempt {
            Attempt::Served(pid) => format!("served by {pid}"),
            Attempt::Failed(reason) => reason.clone(),
        };
        *counts.entry(key).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(reason, count)| format!("{count}x {reason}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Waits for `pid` to be the process answering `port`, failing at
/// [`RECV_TIMEOUT`].
///
/// The bus cannot say when this sheep has bound: it configures neither a
/// channel nor a probe, so it is `Online` from the moment it is spawned.
pub(crate) async fn await_serving(port: u16, pid: u32) {
    let serving = tokio::time::timeout(RECV_TIMEOUT, async {
        while attempt(port).await != Attempt::Served(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        serving.is_ok(),
        "pid {pid} must be the process answering 127.0.0.1:{port}"
    );
}

/// Polls `ListFlock` until `id` is `Online`.
///
/// A probed app reaches `Online` a probe interval after it starts answering,
/// so [`await_serving`] returning is not the same fact, and a reload arriving
/// in that gap finds nothing replaceable.
#[cfg(unix)]
pub(crate) async fn await_online(client: &mut Client, id: u32) {
    let online = tokio::time::timeout(RECV_TIMEOUT, async {
        loop {
            let listed = client.request(Request::ListFlock).await;
            let Response::Flock(flock) = listed.result.unwrap() else {
                panic!("expected flock")
            };
            if flock
                .iter()
                .any(|info| info.id == id && info.status == ProcStatus::Online)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(online.is_ok(), "id {id} must reach Online");
}

/// A port with nothing on it: bind `:0`, read what the OS chose, release it.
///
/// Check-then-use: a stranger can take the port before the fixture binds it.
/// That loss is loud, since the fixture panics with the bind error.
pub(crate) fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the OS must have a free loopback port")
        .local_addr()
        .expect("a bound listener has an address")
        .port()
}

/// The fixture server's binary, built as `examples/reuse_port_sheep.rs`.
///
/// Located rather than named: `env!("CARGO_BIN_EXE_<name>")` covers `[[bin]]`
/// targets only. Cargo puts an example at `<profile>/examples/<name>` and this
/// test binary at the sibling `deps/<name>-<hash>`.
pub(crate) fn reuse_port_sheep() -> std::path::PathBuf {
    let test_binary = std::env::current_exe().expect("a running test knows its own path");
    let path = test_binary
        .parent()
        .and_then(std::path::Path::parent)
        .expect("a test binary lives at <profile>/deps/<name>")
        .join("examples")
        .join("reuse_port_sheep");
    assert!(
        path.is_file(),
        "{} must exist: a plain `cargo test` builds the package's examples, so a \
         missing one means this test was run some way that does not",
        path.display()
    );
    path
}

#[cfg(unix)]
/// Reloads one `reuse_port_sheep` while a caller connects continuously, and
/// hands back every attempt made between the request and the swap finishing.
///
/// Asserts what holds whatever the app does with its stop signal: the swap
/// completes, the replacement answers the port, and the instance it replaced
/// is gone. The counting is the caller's.
pub(crate) async fn reload_under_load(name: &str, defiant: bool) -> Vec<Attempt> {
    let _port_guard = RELOAD_PORT_LOCK.lock().await;
    let port = free_port();

    let fixture = Fixture::boot(tempfile::tempdir().unwrap(), false).await;
    let mut client = fixture.connect().await;
    let subscribed = client
        .request(Request::Subscribe {
            topics: vec!["process.*".to_string()],
        })
        .await;
    assert_eq!(subscribed.result.unwrap(), Response::Subscribed);

    let mut app = AppConfig::minimal(name, &reuse_port_sheep().display().to_string());
    app.interpreter = Some("none".to_string());
    app.env
        .insert("SHEEP_PORT_BASE".to_string(), port.to_string());
    app.env
        .insert("SHEEP_HOLD_MS".to_string(), HOLD_MS.to_string());
    if defiant {
        app.env.insert("SHEEP_DEFIANT".to_string(), "1".to_string());
    }
    app.listen_timeout = READY_WINDOW;
    app.graceful_timeout = DRAIN_WINDOW;
    // Teardown's ladder, not the reload's: a defiant replacement is SIGKILLed
    // at the end of the test too, and the spec's 1.6s default would be spent
    // waiting for a process that never answers.
    app.kill_timeout = DRAIN_WINDOW;
    // Nothing may respawn behind the measurement: a restart would put a third
    // process on this port.
    app.autorestart = false;

    let started = client.request(Request::Start { apps: vec![app] }).await;
    let Response::Started(infos) = started.result.unwrap() else {
        panic!("expected started")
    };
    let drainee_id = infos[0].id;
    let drainee_pid = infos[0].pid.expect("a real spawn reports a real pid");
    client
        .await_process_event(drainee_id, ProcessEventKind::Online)
        .await;
    await_serving(port, drainee_pid).await;

    // Answering as the process this test thinks is on the port rules out a
    // stranger or a leftover being the reason a later attempt fails.
    let before = burst(port).await;
    assert_eq!(
        tally(&before),
        format!("{BURST}x served by {drainee_pid}"),
        "the sheep must own the port outright before its reload begins"
    );

    let hammer = Hammer::start(port);
    let accepted = client
        .request(Request::Reload {
            selector: SelectorSpec::Name(name.to_string()),
        })
        .await;
    let Response::Reloading { accepted, .. } = accepted.result.unwrap() else {
        panic!("expected an accepted reload")
    };
    assert_eq!(accepted.len(), 1);

    // `Reloaded` rather than a duration bounds the window to the reload.
    let replacement = client
        .await_any_process_event(ProcessEventKind::Reloaded)
        .await;
    let during = hammer.finish().await;
    let replacement_pid = replacement.pid.expect("a replacement has a pid");
    assert_ne!(replacement_pid, drainee_pid);
    assert_reaped(i32::try_from(drainee_pid).unwrap()).await;

    // One row, the replacement's: the drainee's registration went with the
    // process.
    let listed = client.request(Request::ListFlock).await;
    let Response::Flock(flock) = listed.result.unwrap() else {
        panic!("expected flock")
    };
    assert_eq!(flock.len(), 1);
    assert_eq!(flock[0].id, replacement.id);
    // The status catches a swap committed before its replacement could prove
    // anything, which costs no connections.
    assert_eq!(flock[0].status, ProcStatus::Online);

    // The fixture derives its port from `SHEP_INSTANCE`: a replacement in
    // another slot would answer somewhere else.
    let after = burst(port).await;
    assert_eq!(
        tally(&after),
        format!("{BURST}x served by {replacement_pid}"),
        "the replacement must own the port outright once the swap is done"
    );

    let dir = fixture.shutdown().await;
    assert_reaped(i32::try_from(replacement_pid).unwrap()).await;
    drop(dir);

    during
}

#[cfg(unix)]
/// shep promises the overlap, not zero downtime: a listener's accept backlog
/// is reset when it closes, so what is queued and unaccepted is lost unless
/// the app drains inside `graceful_timeout`.
///
/// The count is asserted on Linux only. Linux load-balances new connections
/// over every listener in the `SO_REUSEPORT` group, so the drainee keeps a
/// share until it closes; macOS gives every new connection to the last
/// socket to bind, so only the duration is asserted there.
#[tokio::test]
async fn a_reload_costs_a_draining_app_no_connections() {
    let during = reload_under_load("drainer", false).await;
    let failures = during.iter().filter(|attempt| attempt.failed()).count();
    // Printed on every platform, asserted on one.
    println!(
        "draining app, {} attempts across the reload, {failures} lost: {}",
        during.len(),
        tally(&during)
    );
    assert!(
        during.len() > 20,
        "the reload must last long enough to be measured: {}",
        tally(&during)
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        failures,
        0,
        "an app that drains inside its graceful timeout must lose nothing: {}",
        tally(&during)
    );
}

#[cfg(unix)]
/// An instance that will not stop accepting, finish what it has, and exit
/// inside `graceful_timeout` reaches the end of that window still holding
/// work, and `SIGKILL` takes the work with it. No supervisor can give that
/// app zero downtime.
///
/// The count is asserted on Linux only, for the sibling case's reason: there
/// the defiant instance is still being handed a share of every new connection
/// when `SIGKILL` lands, while on macOS it is killed empty.
#[tokio::test]
async fn a_reload_costs_a_defiant_app_the_work_it_will_not_finish() {
    let during = reload_under_load("defier", true).await;
    let failures = during.iter().filter(|attempt| attempt.failed()).count();
    println!(
        "defiant app, {} attempts across the reload, {failures} lost: {}",
        during.len(),
        tally(&during)
    );
    assert!(
        during.len() > 20,
        "the reload must last long enough to be measured: {}",
        tally(&during)
    );
    #[cfg(target_os = "linux")]
    assert!(
        failures > 0,
        "an app that will not drain must be seen to lose connections: {}",
        tally(&during)
    );
}
