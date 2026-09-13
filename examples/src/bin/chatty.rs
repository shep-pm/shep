//! Speaks the shepherd channel: readiness, a metric, and custom actions.
//!
//! The contract is `docs/shepherd-channel.md`. This is the Rust half of
//! four apps that implement the same three actions, one per language, so
//! the same `shep trigger` reads the same way against any of them.
//!
//! Uses the `shep-channel` crate rather than framing JSON by hand. That
//! crate is in this workspace and published, so hand-rolling here would
//! be advice against shep's own library. It also answers an action name
//! nobody registered and echoes each action's `id`, which are two of the
//! three things the contract asks an author to remember. The third is
//! `params`, and no library can do that one: see `level` below.
//!
//! `examples/polyglot/` holds the Go, Node and Python versions, which
//! frame the wire themselves because those apps take no dependency.
//!
//! # Usage
//!
//! ```text
//! chatty
//! ```
//!
//! Needs `channel = true` in the Flockfile, or `wait_ready` /
//! `shutdown_with_message`, which open one too. Run it and reach it:
//!
//! ```text
//! shep trigger chatty ping
//! shep trigger chatty metric queue-depth
//! shep trigger chatty level "debug rate=0.5"
//! shep trigger chatty nonsense
//! ```

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use shep_channel::CHANNEL_VERSION;

fn main() {
    let started = Instant::now();
    let shepherd = shep_channel::serve();

    // A stamp, not a negotiation. Saying so beats failing to parse a line
    // later with nothing to connect that failure to.
    match shepherd.version() {
        Some(stamp) if stamp != CHANNEL_VERSION => {
            eprintln!("chatty: shepherd speaks channel {stamp}, this app speaks {CHANNEL_VERSION}");
        }
        Some(_) => {}
        None => eprintln!("chatty: no shepherd channel; every call below is a no-op"),
    }

    shepherd.on_action("ping", move |_params, _name| {
        format!(
            "pong from rust pid={}, up {:.1}s",
            std::process::id(),
            started.elapsed().as_secs_f64()
        )
    });

    // The crate's outbox never blocks the caller, so a handler can emit a
    // sample. The hand-rolled examples do the same from their read loop,
    // for a different reason: a ticker there would be a second thread.
    let emitter = shepherd.clone();
    let samples = AtomicU64::new(0);
    shepherd.on_action("metric", move |params, _name| {
        let name = params.unwrap_or("triggers").to_owned();
        let value = samples.fetch_add(1, Ordering::Relaxed) + 1;
        emitter.metric(name.clone(), value as f64);
        format!("sent {name}={value}")
    });

    shepherd.on_action("level", |params, _name| match parse_level(params) {
        Some(level) => format!("log level is now {level}"),
        None => "usage: level <trace|debug|info|warn|error> [key=value ...]".to_owned(),
    });

    shepherd.on_shutdown(|| {
        println!("chatty: the shepherd asked us to stop");
        std::process::exit(0);
    });

    shepherd
        .ready()
        .expect("the shepherd went away before readiness");
    shepherd.metric("starts", 1.0);
    println!(
        "chatty pid={} ready on the shepherd channel",
        std::process::id()
    );

    // The crate's reader thread runs the handlers, so main only stays alive.
    loop {
        std::thread::park();
    }
}

/// Reads a level out of one action's `params`, in this app's own grammar.
///
/// `params` is one opaque string and shep never splits it, so every app
/// owns the grammar for its own actions. This one is whitespace separated
/// and reads the first word, which means a level can never contain a
/// space. An app needing one would carry JSON in this string instead.
fn parse_level(params: Option<&str>) -> Option<&str> {
    let level = params?.split_whitespace().next()?;
    ["trace", "debug", "info", "warn", "error"]
        .contains(&level)
        .then_some(level)
}
