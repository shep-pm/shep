//! Fixtures and helpers shared by this module's tests.

use super::config::BarkConfig;
use super::rules::Rules;
use super::sinks::Sink;
use super::source::{ConfigSource, EventSource, FlockSource, Resubscribe};
use super::*;
use crate::http::{HttpRequest, read_request, write_response};
use shep_client::{LinkLost, RequestError};
use shep_core::barks::{self};
use shep_core::protocol::{BusEvent, ProcessInfo};
use shep_core::protocol::{ProcessEventKind, RpcError, RpcErrorCode};
use shep_core::status::ProcStatus;
use shep_core::values::UpDuration;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};

/// [`EventSource`] over the real thing bark's subscription lags on: a
/// `tokio::sync::broadcast::Receiver`. The production path implements
/// the trait for [`ClientEvents`](super::source::ClientEvents).
impl EventSource for broadcast::Receiver<BusEvent> {
    async fn next(&mut self) -> Option<Result<BusEvent, u64>> {
        match self.recv().await {
            Ok(event) => Some(Ok(event)),
            Err(broadcast::error::RecvError::Lagged(count)) => Some(Err(count)),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }

    /// A bare receiver has no shepherd behind it to ask again, so this
    /// stands for the shepherd that never came back.
    async fn resubscribe(&mut self) -> Result<(), Resubscribe> {
        Err(Resubscribe::Lost(LinkLost::Budget {
            waited: Duration::ZERO,
        }))
    }
}

/// An [`EventSource`] that ends the way a handover ends a real
/// subscription, and hands out the next generation when asked.
///
/// The whole shape of a handover from a dog's side: the stream stops,
/// and whether the dog lives depends on there being a successor to
/// subscribe to.
pub(super) struct HandoverSource {
    current: Option<broadcast::Receiver<BusEvent>>,
    /// Generations still to come, oldest first. Empty means no
    /// shepherd answered before the budget ran out.
    later: std::collections::VecDeque<broadcast::Receiver<BusEvent>>,
    /// What [`Self::resubscribe`] reports when `later` is empty.
    when_gone: LinkLost,
    /// Set instead of `when_gone` when the shepherd answers and
    /// refuses, which exits on the error's own code rather than on the
    /// budget's.
    refuses: Option<RpcErrorCode>,
    resubscribes: Arc<std::sync::atomic::AtomicU32>,
}

impl HandoverSource {
    pub(super) fn across(
        generations: Vec<broadcast::Receiver<BusEvent>>,
        when_gone: LinkLost,
    ) -> (Self, Arc<std::sync::atomic::AtomicU32>) {
        Self::across_inner(generations, when_gone, None)
    }

    /// A source whose shepherd answers the re-subscribe and refuses it.
    pub(super) fn refusing(
        generations: Vec<broadcast::Receiver<BusEvent>>,
        code: RpcErrorCode,
    ) -> (Self, Arc<std::sync::atomic::AtomicU32>) {
        Self::across_inner(
            generations,
            LinkLost::Budget {
                waited: Duration::ZERO,
            },
            Some(code),
        )
    }

    fn across_inner(
        generations: Vec<broadcast::Receiver<BusEvent>>,
        when_gone: LinkLost,
        refuses: Option<RpcErrorCode>,
    ) -> (Self, Arc<std::sync::atomic::AtomicU32>) {
        let mut later: std::collections::VecDeque<_> = generations.into();
        let current = later.pop_front();
        let resubscribes = Arc::new(std::sync::atomic::AtomicU32::new(0));
        (
            Self {
                current,
                later,
                when_gone,
                refuses,
                resubscribes: Arc::clone(&resubscribes),
            },
            resubscribes,
        )
    }
}

impl EventSource for HandoverSource {
    async fn next(&mut self) -> Option<Result<BusEvent, u64>> {
        let stream = self.current.as_mut()?;
        match stream.recv().await {
            Ok(event) => Some(Ok(event)),
            Err(broadcast::error::RecvError::Lagged(count)) => Some(Err(count)),
            Err(broadcast::error::RecvError::Closed) => {
                self.current = None;
                None
            }
        }
    }

    async fn resubscribe(&mut self) -> Result<(), Resubscribe> {
        self.resubscribes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(code) = self.refuses {
            return Err(Resubscribe::Request(RequestError::Rpc(RpcError {
                code,
                message: "this shepherd does not serve that topic".into(),
                daemon_version: None,
            })));
        }
        match self.later.pop_front() {
            Some(stream) => {
                self.current = Some(stream);
                Ok(())
            }
            None => Err(Resubscribe::Lost(self.when_gone.clone())),
        }
    }
}

/// A [`FlockSource`] answering one fixed listing, counting how many
/// times it was asked.
#[derive(Clone)]
pub(super) struct ScriptedFlock {
    answer: Arc<Vec<ProcessInfo>>,
    calls: Arc<std::sync::atomic::AtomicU32>,
}

impl ScriptedFlock {
    pub(super) fn answering(answer: Vec<ProcessInfo>) -> Self {
        Self {
            answer: Arc::new(answer),
            calls: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    pub(super) fn calls(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl FlockSource for ScriptedFlock {
    async fn flock(&self) -> Result<Vec<ProcessInfo>, RequestError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok((*self.answer).clone())
    }
}

/// A [`ConfigSource`] answering one fixed `[bark]` section, counting
/// how many times it was asked. The count is what proves the loop
/// re-asks on a `config.dog.bark` frame rather than acting on it.
#[derive(Clone)]
pub(super) struct ScriptedConfig {
    section: Arc<String>,
    calls: Arc<std::sync::atomic::AtomicU32>,
}

impl ScriptedConfig {
    pub(super) fn answering(section: String) -> Self {
        Self {
            section: Arc::new(section),
            calls: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    pub(super) fn calls(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl ConfigSource for ScriptedConfig {
    async fn section(&self) -> Result<String, RequestError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok((*self.section).clone())
    }
}

/// Binds an ephemeral port, accepts exactly one connection, answers
/// `status`/`body`, and hands the captured request back through the
/// returned receiver.
pub(super) async fn one_shot_sink(
    status: u16,
    body: &str,
) -> (SocketAddr, oneshot::Receiver<HttpRequest>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    let body = body.to_string();
    tokio::spawn(async move {
        let (mut stream, _peer) = listener.accept().await.unwrap();
        let req = read_request(&mut stream, Duration::from_secs(5))
            .await
            .unwrap();
        write_response(&mut stream, status, "application/json", body.as_bytes())
            .await
            .unwrap();
        let _ = tx.send(req);
    });
    (addr, rx)
}

/// A sink that accepts one connection and then never answers, plus a
/// signal that fires the moment it has accepted.
///
/// The signal lets a caller assert an order rather than a duration,
/// which would be a claim about how a runner schedules two tasks.
pub(super) async fn slow_sink() -> (SocketAddr, tokio::sync::oneshot::Receiver<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (connected, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (_stream, _peer) = listener.accept().await.unwrap();
        // Ignored: a caller that does not care drops the receiver.
        let _ = connected.send(());
        core::future::pending::<()>().await;
    });
    (addr, rx)
}

pub(super) fn base_info(name: &str, status: ProcStatus, restarts: u32) -> ProcessInfo {
    ProcessInfo::builder(1, name, status)
        .pid(Some(4242))
        .restarts(restarts)
        .uptime_ms(1_000)
        .build()
}

pub(super) fn errored_info(name: &str, restarts: u32) -> ProcessInfo {
    base_info(name, ProcStatus::Errored, restarts)
}

pub(super) fn process_event(name: &str, kind: ProcessEventKind) -> BusEvent {
    BusEvent::Process {
        event: kind,
        info: base_info(name, ProcStatus::Online, 0),
        manually: false,
        at_ms: 0,
    }
}

pub(super) fn errored_event(name: &str) -> BusEvent {
    process_event(name, ProcessEventKind::Errored)
}

/// A JSON sink POSTing to `url` with the default body. Every sink
/// these tests configure is this one.
pub(super) fn json_sink(url: String) -> Sink {
    Sink::Json { url, body: None }
}

/// A cheap bus event no rule below fires on: filler for overflowing
/// the broadcast channel's small capacity.
pub(super) fn log_event(i: u32) -> BusEvent {
    BusEvent::LogOut {
        id: i,
        line: format!("log line {i}"),
    }
}

/// One `gave_up` rule routed to the sink named `"ops"`, the name
/// [`config_with_sink`] defines.
///
/// The debounce is a real five minutes, not zero: the reconciliation
/// test's channel yields `errored_event("web")` again as an ordinary
/// item after the lag notice, and a zero debounce suppresses nothing.
pub(super) fn gave_up_rules() -> Rules {
    let mut sinks = BTreeMap::new();
    sinks.insert(
        "ops".to_owned(),
        json_sink("http://127.0.0.1:1/hook".to_owned()),
    );
    Rules::new(
        vec![rules::Rule {
            when: rules::Trigger::GaveUp {},
            sinks: vec!["ops".to_owned()],
            debounce: UpDuration::from_millis(5 * 60_000),
        }],
        &sinks,
    )
    .unwrap()
}

/// A [`BarkConfig`] with one sink, `"ops"`, POSTing to `addr`. `poll`
/// is 60s, past every timeout these tests bound themselves by, so a
/// poll that fires is attributable to the lag path.
pub(super) fn config_with_sink(addr: SocketAddr, _barks_path: &Path) -> BarkConfig {
    let mut sinks = BTreeMap::new();
    sinks.insert("ops".to_owned(), json_sink(format!("http://{addr}/hook")));
    BarkConfig {
        sinks,
        rules: Vec::new(),
        poll: UpDuration::from_millis(60_000),
        history_bytes: barks::DEFAULT_MAX_BYTES,
        sink_timeout: UpDuration::from_millis(5_000),
    }
}
