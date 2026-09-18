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

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use shep_channel::CHANNEL_VERSION;

fn main() {
    let started = Instant::now();
    let shepherd = shep_channel::serve();

    // The crate hands back a working handle either way, so an ordinary app
    // needs no branch here. This one is the channel and nothing else, so it
    // refuses rather than running as a no-op, which is what the other three
    // do when the environment names a channel they cannot open.
    if !shepherd.is_active() {
        eprintln!(
            "chatty: no shepherd channel. Set channel = true on this app in \
             the Flockfile, or wait_ready, or shutdown_with_message."
        );
        std::process::exit(1);
    }

    // A stamp, not a negotiation. Saying so beats failing to parse a line
    // later with nothing to connect that failure to.
    if let Some(stamp) = shepherd.version()
        && stamp != CHANNEL_VERSION
    {
        eprintln!("chatty: shepherd speaks channel {stamp}, this app speaks {CHANNEL_VERSION}");
    }

    shepherd.on_action("ping", move |_params, _name| {
        format!(
            "pong from rust pid={}, up {:.1}s, level {}",
            std::process::id(),
            started.elapsed().as_secs_f64(),
            LEVELS[LEVEL.load(Ordering::Relaxed)]
        )
    });

    // The crate's outbox never blocks the caller, so a handler can emit a
    // sample. The hand-rolled examples do the same from their read loop,
    // for a different reason: a ticker there would be a second thread.
    let emitter = shepherd.clone();
    let samples = AtomicU64::new(0);
    shepherd.on_action("metric", move |params, _name| {
        let name = metric_name(params);
        let value = samples.fetch_add(1, Ordering::Relaxed) + 1;
        emitter.metric(name, value as f64);
        format!("sent {name}={value}")
    });

    shepherd.on_action("level", |params, _name| match parse_level(params) {
        Some((level, rest)) => {
            LEVEL.store(level_index(level), Ordering::Relaxed);
            if rest.is_empty() {
                format!("log level is now {level}")
            } else {
                format!("log level is now {level}, ignored {rest:?}")
            }
        }
        None => usage(),
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

/// The levels `level` accepts, and the only place they are listed.
const LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// The level `level` last set, as an index into [`LEVELS`]. `ping` reads it
/// back, so the reply that says the level changed is answerable.
static LEVEL: AtomicUsize = AtomicUsize::new(2);

/// Where `level` sits in [`LEVELS`]. Only ever called with a name
/// [`parse_level`] already matched, so a miss means those two disagree and
/// falling back to `info` is the one answer that cannot name a level the
/// app does not have.
fn level_index(level: &str) -> usize {
    LEVELS.iter().position(|&l| l == level).unwrap_or(2)
}

/// What an unparsable `level` gets back. Built from [`LEVELS`] rather than
/// spelled out, so adding one cannot leave the message listing the old set.
/// Says the rest is dropped rather than inviting arguments it does not read.
fn usage() -> String {
    format!("usage: level <{}> [rest is ignored]", LEVELS.join("|"))
}

/// Names the metric one `metric` action should send.
///
/// `params` reaches an app exactly as the operator typed it, so an empty
/// or blank one is ordinary rather than a mistake. Both fall back, since
/// a metric named `""` is worse on the bus than no custom name at all.
fn metric_name(params: Option<&str>) -> &str {
    match params.map(str::trim) {
        Some(name) if !name.is_empty() => name,
        _ => "triggers",
    }
}

/// Reads a level out of one action's `params`, with whatever followed it.
///
/// `params` is one opaque string and shep never splits it, so every app
/// owns the grammar for its own actions. This one is whitespace separated
/// and reads the first word, which means a level can never contain a
/// space. An app needing one would carry JSON in this string instead.
///
/// The remainder comes back so the reply can say it was dropped, because
/// an app that silently ignores half of what it was handed is the thing
/// this action exists to warn about.
fn parse_level(params: Option<&str>) -> Option<(&str, &str)> {
    let text = params?.trim();
    let (level, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    LEVELS.contains(&level).then(|| (level, rest.trim()))
}

#[cfg(test)]
mod tests {
    use super::{LEVELS, level_index, metric_name, parse_level, usage};

    /// Every level parse_level accepts has a place to be stored. The
    /// fallback in level_index exists for a disagreement between the two,
    /// so nothing here should ever reach it.
    #[test]
    fn every_level_maps_to_its_own_slot() {
        for (want, level) in LEVELS.iter().enumerate() {
            assert_eq!(level_index(level), want, "{level} lands in the wrong slot");
        }
    }

    #[test]
    fn a_known_level_is_read_from_the_first_word() {
        assert_eq!(parse_level(Some("debug")), Some(("debug", "")));
        assert_eq!(parse_level(Some("  warn  ")), Some(("warn", "")));
    }

    /// The reply names the remainder, so an operator can see the app read
    /// one word of what they typed and dropped the rest.
    #[test]
    fn what_followed_the_level_comes_back_with_it() {
        assert_eq!(
            parse_level(Some("debug rate=0.5")),
            Some(("debug", "rate=0.5"))
        );
        assert_eq!(parse_level(Some("info  a  b ")), Some(("info", "a  b")));
    }

    /// The usage line and the levels it lists cannot drift apart, because
    /// one is built from the other.
    #[test]
    fn the_usage_line_names_every_level_it_accepts() {
        let text = usage();
        for level in LEVELS {
            assert!(text.contains(level), "{text} omits {level}");
        }
    }

    #[test]
    fn an_unknown_or_missing_level_is_refused() {
        assert_eq!(parse_level(None), None);
        assert_eq!(parse_level(Some("")), None);
        assert_eq!(parse_level(Some("   ")), None);
        assert_eq!(parse_level(Some("shout")), None);
        assert_eq!(parse_level(Some("rate=0.5 debug")), None);
    }

    /// `shep trigger chatty metric ""` reaches an app as `Some("")`, which
    /// named the metric `""` on the wire until this fell back.
    #[test]
    fn a_blank_metric_name_falls_back_rather_than_naming_nothing() {
        assert_eq!(metric_name(None), "triggers");
        assert_eq!(metric_name(Some("")), "triggers");
        assert_eq!(metric_name(Some("   ")), "triggers");
    }

    #[test]
    fn a_real_metric_name_survives_with_its_padding_trimmed() {
        assert_eq!(metric_name(Some("queue-depth")), "queue-depth");
        assert_eq!(metric_name(Some("  queue-depth  ")), "queue-depth");
    }
}
