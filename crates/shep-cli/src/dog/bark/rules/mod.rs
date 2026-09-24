//! What makes a bark fire: [`Rule`], [`Trigger`] and the [`Rules`] engine
//! that turns a bus event or a reconciliation poll into zero or more
//! [`Firing`]s.
//!
//! [`Rules::on_poll`] catches what the bus drops: it evaluates the same
//! rules against the flock's current state instead of one event. Both
//! routes share one per-subject debounce in [`Rules`]'s `subjects` map, so
//! a rule fired by one route is not fired again by the other.
//!
//! Debounce is per rule per subject, never global: a global debounce would
//! silence the second sheep to go down during an incident.

use core::fmt;
use std::collections::BTreeMap;

use serde::Deserialize;
use shep_core::barks::Bark;
use shep_core::protocol::{BusEvent, ProcessEventKind, ProcessInfo};
use shep_core::status::ProcStatus;
use shep_core::values::{MemSize, UpDuration};

use super::sinks::{self, Sink, SinkConfigError};

/// How long, by default, a rule stays quiet for a subject after firing.
///
/// Five minutes: long enough that a flapping sheep does not page an
/// operator once a minute, short enough that a still-down sheep gets a
/// reminder inside the same incident rather than only its first alert.
fn default_debounce() -> UpDuration {
    UpDuration::from_millis(5 * 60 * 1_000)
}

/// One entry under `[[bark.rules]]` in `dogs.toml`.
///
/// A misspelled key anywhere in a rule is a startup error naming the bad
/// key, never a silently ignored setting. See
/// [`BarkConfig`](super::BarkConfig)'s own doc for why that posture
/// matters.
// deny_unknown_fields cannot sit here: serde refuses it alongside
// #[serde(flatten)]. Keys `sinks` and `debounce` don't claim flow into
// Trigger's own deserialize instead, which is where a typo is caught.
#[derive(Debug, Clone, PartialEq, Deserialize, schemars::JsonSchema)]
pub struct Rule {
    /// What fires it.
    #[serde(flatten)]
    pub when: Trigger,
    /// Sinks by name, from `[bark.sinks]` in `dogs.toml`. At least one; a
    /// rule routing nowhere is a rule that fires into a file and is
    /// refused at startup rather than discovered during an incident.
    pub sinks: Vec<String>,
    /// How long after one firing this rule stays quiet FOR THE SAME
    /// SUBJECT. Per-subject, never global: a flock where one sheep flaps
    /// must not mute the alert for a different sheep going down.
    #[serde(default = "default_debounce")]
    pub debounce: UpDuration,
}

/// What makes a rule fire.
///
/// `deny_unknown_fields` lives here rather than on [`Rule`] — see that
/// type's own doc for why the combination with `#[serde(flatten)]` forced
/// the move.
#[derive(Debug, Clone, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(tag = "on", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    /// Any of these bus event kinds, by their wire spelling
    /// (`exit`, `errored`, `online`, ...).
    Event {
        /// The kinds this rule fires on.
        kinds: Vec<String>,
    },
    /// The shepherd gave up: a sheep reached `Errored`. On by DEFAULT with
    /// no configuration at all, because it is the alert that must not be
    /// missed — the app is down and staying down — and because it cannot
    /// disagree with the shepherd: it is keyed to the shepherd's own
    /// decision rather than to a threshold bark chose.
    // A struct variant, not a unit variant: an internally tagged unit
    // variant skips field checking entirely, so deny_unknown_fields would
    // not catch a typo beside `on = "gave_up"`.
    GaveUp {},
    /// The early warning: `restarts` restarts within `within`. Opt-in,
    /// because it is the one that pages at 3am for a blip, and the
    /// threshold should be one the operator chose.
    RestartRate {
        /// How many restarts.
        restarts: u32,
        /// Within how long.
        within: UpDuration,
    },
    /// A sheep's memory crossed a ceiling, read from the reconciliation
    /// poll rather than from the bus — memory is a level, and the bus
    /// carries events.
    MemoryAbove {
        /// The ceiling.
        bytes: MemSize,
    },
}

/// What a [`Trigger::GaveUp`] firing says about `name`.
///
/// One function rather than the literal at both trigger sites: the bus
/// route reads an `Errored` event and the poll route reads an `Errored`
/// status, and an operator seeing two spellings of the same alert would
/// read it as two different alerts.
fn gave_up_message(name: &str) -> String {
    format!("{name} gave up: restart budget exhausted")
}

/// The rule-kind name [`Bark::rule`] records for a firing: the same
/// snake_case spelling a `[dog.bark.rules]` entry's own `on = "..."` key
/// uses, so an operator reading `barks.jsonl` sees no vocabulary mismatch
/// against what they configured.
fn trigger_name(when: &Trigger) -> &'static str {
    match when {
        Trigger::Event { .. } => "event",
        Trigger::GaveUp {} => "gave_up",
        Trigger::RestartRate { .. } => "restart_rate",
        Trigger::MemoryAbove { .. } => "memory_above",
    }
}

/// `kind`'s wire spelling, the string `kinds` in a rule names it by.
///
/// Hand-listed rather than read from `ProcessEventKind`'s own `Serialize`,
/// because this runs once per bus event and the serde round-trip allocated a
/// `Value` to answer a question fixed at compile time.
///
/// `ProcessEventKind` is `#[non_exhaustive]`, so the last arm is the one that
/// catches a variant this build has not been taught. It is also the spelling
/// `#[serde(other)]` gives `Unrecognized`, which is what the round-trip this
/// replaced returned for it. A variant added upstream therefore needs an arm
/// here as soon as `is_known_kind` starts accepting its name, or a rule naming
/// it would compare against `"unrecognized"` and never fire.
fn wire_spelling(kind: ProcessEventKind) -> &'static str {
    match kind {
        ProcessEventKind::Start => "start",
        ProcessEventKind::Online => "online",
        ProcessEventKind::Exit => "exit",
        ProcessEventKind::Restart => "restart",
        ProcessEventKind::Reload => "reload",
        ProcessEventKind::Reloaded => "reloaded",
        ProcessEventKind::ReloadAbandoned => "reload_abandoned",
        ProcessEventKind::Stop => "stop",
        ProcessEventKind::Delete => "delete",
        ProcessEventKind::Errored => "errored",
        _ => "unrecognized",
    }
}

/// Whether `kind` is a spelling [`ProcessEventKind`] actually has, read from
/// its own `Deserialize` rather than hand-listed the way [`wire_spelling`] is.
fn is_known_kind(kind: &str) -> bool {
    // `ProcessEventKind`'s `Deserialize` now accepts any string, decoding an
    // unrecognized one as `Unrecognized` rather than erroring, so validity
    // has to be judged from the decoded variant instead of from success.
    !matches!(
        serde_json::from_value::<ProcessEventKind>(serde_json::Value::String(kind.to_owned())),
        Err(_) | Ok(ProcessEventKind::Unrecognized)
    )
}

/// Why [`Rules::new`] refused a configuration.
#[derive(Debug)]
pub enum RulesError {
    /// Rule at position `index` (0-based, in configuration order) routes
    /// to a sink name `[bark.sinks]` does not define.
    UnknownSink {
        /// Position in the configured rule list.
        index: usize,
        /// The sink name that does not exist.
        sink: String,
    },
    /// Rule at position `index` routes to no sink at all.
    NoSinks {
        /// Position in the configured rule list.
        index: usize,
    },
    /// Rule at position `index`'s `Event` trigger names an event kind that
    /// is not on the wire.
    UnknownKind {
        /// Position in the configured rule list.
        index: usize,
        /// The kind string that matches no [`ProcessEventKind`].
        kind: String,
    },
    /// A `[bark.sinks]` entry's url cannot work: a Discord or Slack
    /// webhook over `http://`, or a url carrying credentials before the
    /// host. See [`sinks::require_usable_url`].
    UnusableSink(SinkConfigError),
}

impl fmt::Display for RulesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSink { index, sink } => write!(
                f,
                "rule {index} routes to sink \"{sink}\", which [bark.sinks] in dogs.toml does \
                 not define"
            ),
            Self::NoSinks { index } => write!(f, "rule {index} routes to no sink at all"),
            Self::UnknownKind { index, kind } => write!(
                f,
                "rule {index}'s event trigger names \"{kind}\", which is not an event kind on the wire"
            ),
            Self::UnusableSink(source) => write!(f, "{source}"),
        }
    }
}

impl core::error::Error for RulesError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::UnusableSink(source) => Some(source),
            Self::UnknownSink { .. } | Self::NoSinks { .. } | Self::UnknownKind { .. } => None,
        }
    }
}

impl From<SinkConfigError> for RulesError {
    fn from(source: SinkConfigError) -> Self {
        Self::UnusableSink(source)
    }
}

/// Per-subject bookkeeping [`Rules`] keeps to make the bus route and the
/// poll route agree on one firing rather than two, and to let
/// [`Trigger::RestartRate`] measure a window without bark keeping its own
/// restart tally.
#[derive(Debug, Default)]
struct SubjectState {
    /// Rule index -> unix millis it last fired for this subject. Read and
    /// written by both [`Rules::on_event`] and [`Rules::on_poll`], which is
    /// the whole mechanism behind "an `Errored` seen by both routes fires
    /// once."
    last_fired: BTreeMap<usize, u64>,
    /// Rule index -> (the shepherd's restart count when this rule's
    /// window last reset for this subject, when that reset happened).
    /// Only `RestartRate` rules ever populate this.
    restart_windows: BTreeMap<usize, (u32, u64)>,
}

/// The subject's entry in `subjects`, created on first sight.
///
/// Looks before inserting: on a steady flock every event's subject is already
/// in the map, and `entry` takes its key by value, so it would allocate a
/// `String` on every call only to drop it when the key is already there. The
/// owned key is built only on a miss, which is once per subject per bark run.
///
/// `contains_key` first rather than a `get_mut` match, because a reference
/// returned from the lookup has to outlive the insert that follows it.
fn subject_state<'a>(
    subjects: &'a mut BTreeMap<String, SubjectState>,
    subject: &str,
) -> &'a mut SubjectState {
    if !subjects.contains_key(subject) {
        subjects.insert(subject.to_owned(), SubjectState::default());
    }
    subjects
        .get_mut(subject)
        .expect("subject key is present: either it was, or the insert above added it")
}

/// Bark's whole state: the rules, and what each subject last looked like to
/// each rule.
#[derive(Debug)]
pub struct Rules {
    rules: Vec<Rule>,
    subjects: BTreeMap<String, SubjectState>,
}

impl Rules {
    /// Builds the engine, refusing a configuration that cannot work.
    ///
    /// # Errors
    /// - [`RulesError::UnknownSink`]: a rule routes to a sink
    ///   `[dog.bark.sinks]` does not define.
    /// - [`RulesError::NoSinks`]: a rule routes to no sink.
    /// - [`RulesError::UnknownKind`]: an `Event` rule names a kind that is
    ///   not on the wire.
    /// - [`RulesError::UnusableSink`]: a `[dog.bark.sinks]` entry is a
    ///   Discord or Slack webhook using `http://`, or carries credentials
    ///   before its host. Both are checked whether or not any rule routes
    ///   to that sink.
    pub fn new(rules: Vec<Rule>, sinks: &BTreeMap<String, Sink>) -> Result<Self, RulesError> {
        for (name, sink) in sinks {
            sinks::require_usable_url(name, sink)?;
        }
        for (index, rule) in rules.iter().enumerate() {
            if rule.sinks.is_empty() {
                return Err(RulesError::NoSinks { index });
            }
            for sink in &rule.sinks {
                if !sinks.contains_key(sink) {
                    return Err(RulesError::UnknownSink {
                        index,
                        sink: sink.clone(),
                    });
                }
            }
            if let Trigger::Event { kinds } = &rule.when {
                for kind in kinds {
                    if !is_known_kind(kind) {
                        return Err(RulesError::UnknownKind {
                            index,
                            kind: kind.clone(),
                        });
                    }
                }
            }
        }
        Ok(Self {
            rules,
            subjects: BTreeMap::new(),
        })
    }

    /// The default rule set, for a `[dog.bark]` that configured none: one
    /// `GaveUp` rule routed to every configured sink.
    #[must_use]
    pub fn default_rules(sinks: &BTreeMap<String, Sink>) -> Vec<Rule> {
        vec![Rule {
            when: Trigger::GaveUp {},
            sinks: sinks.keys().cloned().collect(),
            debounce: default_debounce(),
        }]
    }

    /// The firing rule `idx` produces for `subject` now, or `None` when
    /// its debounce has not elapsed. Shared by the bus route and the poll
    /// route, so an event both see fires once.
    ///
    /// The caller decides whether the rule triggered at all and supplies
    /// the `message`; everything from the debounce onward is the same on
    /// both routes.
    fn fire(&mut self, idx: usize, subject: &str, now_ms: u64, message: String) -> Option<Firing> {
        let debounce = self.rules[idx].debounce;
        let state = subject_state(&mut self.subjects, subject);
        let ready = state
            .last_fired
            .get(&idx)
            .is_none_or(|&last| now_ms.saturating_sub(last) >= debounce.as_millis());
        if !ready {
            return None;
        }
        state.last_fired.insert(idx, now_ms);
        Some(Firing {
            bark: Bark {
                at_ms: now_ms,
                rule: trigger_name(&self.rules[idx].when).to_owned(),
                subject: subject.to_owned(),
                message,
                sinks: Vec::new(),
            },
            sinks: self.rules[idx].sinks.clone(),
        })
    }

    /// Whether a `RestartRate` rule has accumulated `threshold` or more
    /// restarts for `subject` since the window opened, sliding the window
    /// forward once `within` has elapsed.
    ///
    /// The baseline starts at zero on first observation, so restarts from
    /// before bark's own first poll count toward it. Once `within` elapses
    /// with no new firing, the baseline resets to the current count, so a
    /// sheep that stopped flapping stops re-triggering the rule.
    fn restart_window_crossed(
        &mut self,
        idx: usize,
        subject: &str,
        current_restarts: u32,
        threshold: u32,
        within: UpDuration,
        now_ms: u64,
    ) -> bool {
        let state = subject_state(&mut self.subjects, subject);
        let window = state.restart_windows.entry(idx).or_insert((0, now_ms));
        if now_ms.saturating_sub(window.1) > within.as_millis() {
            *window = (current_restarts, now_ms);
        }
        current_restarts.saturating_sub(window.0) >= threshold
    }

    /// What one bus event fires, after debounce.
    #[must_use]
    pub fn on_event(&mut self, event: &BusEvent, now_ms: u64) -> Vec<Firing> {
        let BusEvent::Process {
            event: kind, info, ..
        } = event
        else {
            return Vec::new();
        };
        let kind = *kind;
        let kind_wire = wire_spelling(kind);
        let mut firings = Vec::new();
        for idx in 0..self.rules.len() {
            let trigger = self.rules[idx].when.clone();
            let message = match &trigger {
                Trigger::Event { kinds } if kinds.iter().any(|k| k == kind_wire) => {
                    Some(format!("{} {kind_wire}", info.name))
                }
                Trigger::GaveUp {} if kind == ProcessEventKind::Errored => {
                    Some(gave_up_message(&info.name))
                }
                _ => None,
            };
            let Some(message) = message else { continue };
            firings.extend(self.fire(idx, &info.name, now_ms, message));
        }
        firings
    }

    /// What the reconciliation poll fires: everything the bus should have
    /// carried and did not, plus the level-triggered rules that have no bus
    /// event at all.
    ///
    /// Reads `ProcessInfo::restarts`, the shepherd's own count, rather
    /// than tallying restarts itself: a private tally could drift from
    /// what the shepherd acts on.
    #[must_use]
    pub fn on_poll(&mut self, flock: &[ProcessInfo], now_ms: u64) -> Vec<Firing> {
        let mut firings = Vec::new();
        for info in flock {
            for idx in 0..self.rules.len() {
                let trigger = self.rules[idx].when.clone();
                let message = match &trigger {
                    Trigger::Event { .. } => None,
                    Trigger::GaveUp {} => (info.status == ProcStatus::Errored)
                        .then(|| gave_up_message(&info.name)),
                    Trigger::RestartRate { restarts, within } => self
                        .restart_window_crossed(
                            idx,
                            &info.name,
                            info.restarts,
                            *restarts,
                            *within,
                            now_ms,
                        )
                        .then(|| {
                            format!(
                                "{} restarted {} times, at or past the {restarts}-within-{within} early warning",
                                info.name, info.restarts
                            )
                        }),
                    Trigger::MemoryAbove { bytes } => info.memory_bytes.and_then(|used| {
                        (used >= bytes.bytes()).then(|| {
                            format!(
                                "{} memory at {}, at or above the {bytes} limit",
                                info.name,
                                MemSize::from_bytes(used)
                            )
                        })
                    }),
                };
                let Some(message) = message else { continue };
                firings.extend(self.fire(idx, &info.name, now_ms, message));
            }
        }
        firings
    }
}

/// One rule firing for one subject: the bark to write and where to send it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firing {
    /// The record, with [`Bark::sinks`] still empty until delivery fills
    /// it in.
    pub bark: Bark,
    /// The sink names it routes to.
    pub sinks: Vec<String>,
}

#[cfg(test)]
mod tests;
