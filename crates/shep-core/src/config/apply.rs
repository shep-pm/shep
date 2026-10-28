//! How each `AppConfig` field reaches a running sheep.
//!
//! Four answers, and the difference between them is where the daemon reads
//! the field, not what the field means. A value read fresh at each decision
//! can be swapped under a running process with no disruption; one baked
//! into the child at exec cannot change until that process is replaced.

use serde::{Deserialize, Serialize};

/// Where a field's new value takes effect.
///
/// `#[non_exhaustive]`: a field could someday be applied by nudging the
/// running child through `shep reopen`'s SIGUSR2 path, a fifth group
/// distinct from the four below. shep-core is published, so an
/// out-of-tree match on this enum needs a wildcard arm to survive that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApplyGroup {
    /// Read fresh at each decision, so a write to the stored spec is enough.
    Live,
    /// Read when a process spawns, so a write reaches the next one.
    NextSpawn,
    /// Held by the running child, so that instance must be replaced.
    NeedsRespawn,
    /// Identity or flock shape, not a runtime knob.
    Structural,
}

/// One row of the `FIELDS` table: the field's name, the group it belongs
/// to, and whether a write to it reaches a child that is already running.
///
/// The third entry is carried by the row rather than derived from the
/// group. [`ApplyGroup::NextSpawn`] splits, and naming the answer per row
/// is what stops a new `NextSpawn` field from skipping the question.
struct FieldRow {
    field: &'static str,
    group: ApplyGroup,
    reaches_running: bool,
}

/// Every `AppConfig` field name this table has been taught about, paired with
/// its group and with whether a write to it reaches a running child.
///
/// `apply_group`, `reaches_running` and `is_classified` all read this one
/// list, so a field cannot drift between "classified" and "has a group".
///
/// `#[rustfmt::skip]` keeps one row per line: this table is read across.
#[rustfmt::skip]
const FIELDS: &[FieldRow] = &[
    // Read by `brain::decide` when a sheep exits.
    FieldRow { field: "autorestart",                group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "max_restarts",               group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "min_uptime",                 group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "restart_delay",              group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "exp_backoff_restart_delay",  group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "stop_exit_codes",            group: ApplyGroup::Live,          reaches_running: true  },
    // Read by `claim_manual` when a kill ladder runs.
    FieldRow { field: "kill_timeout",               group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "graceful_timeout",           group: ApplyGroup::Live,          reaches_running: true  },
    // Read fresh when extras arms a worker. These need a re-arm to take
    // effect; see `ExtrasRegistry::rearm_name`.
    FieldRow { field: "max_memory",                 group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "watch",                      group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "ignore_watch",               group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "watch_delay",                group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "watch_options",              group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "cron_restart",               group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "cron_timezone",              group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "liveness_probe",             group: ApplyGroup::Live,          reaches_running: true  },
    // Read fresh per command.
    FieldRow { field: "fold",                       group: ApplyGroup::Live,          reaches_running: true  },
    FieldRow { field: "reuse_port",                 group: ApplyGroup::Live,          reaches_running: true  },
    // Never read by a spawn at all: it rides `ProcessInfo` to whichever
    // client is classifying this sheep's log lines, so the next listing
    // carries it.
    FieldRow { field: "level_rules",                group: ApplyGroup::Live,          reaches_running: true  },
    // Never read by the daemon at all: it rides to the dog it names,
    // through `Request::DogSheepSettings`, and nothing else asks for it.
    FieldRow { field: "dogs",                       group: ApplyGroup::Live,          reaches_running: true  },
    // Read fresh from the stored spec each time an action is dispatched, at
    // `supervisor.rs`'s `begin_action` (`config.action_timeout.as_duration()`),
    // not baked into the long-lived per-sheep task.
    FieldRow { field: "action_timeout",             group: ApplyGroup::Live,          reaches_running: true  },
    // Unlike its two ladder-mates above, `kill_signal` is read from the
    // per-sheep task's `ResolvedApp`, moved in once at `spawn_sheep_task`
    // and never refreshed.
    FieldRow { field: "kill_signal",                group: ApplyGroup::NextSpawn,     reaches_running: false },
    FieldRow { field: "listen_timeout",             group: ApplyGroup::NextSpawn,     reaches_running: false },
    FieldRow { field: "readiness_probe",            group: ApplyGroup::NextSpawn,     reaches_running: false },
    // Read once at muster or boot, by `restorable()`.
    FieldRow { field: "autostart",                  group: ApplyGroup::NextSpawn,     reaches_running: true  },
    // Read once when a batch is ordered, at a boot, a muster, or a staged
    // start; see the field's own doc comment on `AppConfig`.
    FieldRow { field: "depends_on",                 group: ApplyGroup::NextSpawn,     reaches_running: true  },
    // Baked into the child at exec: argv, cwd, environment, credentials, the
    // fd table, the log paths it is already writing to.
    FieldRow { field: "script",                     group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "args",                       group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "cwd",                        group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "interpreter",                group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "env",                        group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    // Decides what every `{{secret:...}}` in this child's env resolved to,
    // and those are baked in at exec like the rest of the environment.
    FieldRow { field: "environment",                group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "user",                       group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "group",                      group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "out_file",                   group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "err_file",                   group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "merge_logs",                 group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    // Read once, when the log pump starts beside the child.
    FieldRow { field: "log_timestamps",             group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "channel",                    group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "stdin",                      group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "wait_ready",                 group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    // `shutdown_with_message` belongs here rather than with the kill ladder:
    // `assemble()` ORs it into whether fd 3 is opened, and that is the
    // child's own fd table.
    FieldRow { field: "shutdown_with_message",      group: ApplyGroup::NeedsRespawn,  reaches_running: false },
    FieldRow { field: "name",                       group: ApplyGroup::Structural,    reaches_running: false },
    FieldRow { field: "instances",                  group: ApplyGroup::Structural,    reaches_running: false },
];

/// The group `field` belongs to.
///
/// An unknown name answers [`ApplyGroup::NeedsRespawn`], the most
/// conservative of the four: a field this table has not been taught about
/// gets a restart rather than a silent claim that it applied.
/// `every_appconfig_field_has_a_group` keeps that arm unreachable for real
/// fields.
#[must_use]
pub fn apply_group(field: &str) -> ApplyGroup {
    FIELDS
        .iter()
        .find(|row| row.field == field)
        .map_or(ApplyGroup::NeedsRespawn, |row| row.group)
}

/// Whether a write to `field` reaches a child that is already running.
///
/// `false` for every field a respawn is what applies. The shepherd parks
/// exactly these, and `shep lookout`'s close dialog asks about exactly
/// these, so the two read one answer. The answer is the row's own
/// `reaches_running` entry. An unknown name has no row and answers `false`.
#[must_use]
pub fn reaches_running(field: &str) -> bool {
    FIELDS
        .iter()
        .find(|row| row.field == field)
        .is_some_and(|row| row.reaches_running)
}

/// Whether `field` is named explicitly in the table above, as opposed to
/// reaching the conservative fallback. Test-facing.
#[must_use]
pub fn is_classified(field: &str) -> bool {
    FIELDS.iter().any(|row| row.field == field)
}

/// How much of a Flockfile load overwrites what the operator has set since.
///
/// Two independent axes: whether `env` is reset, and whether a key the
/// template declares (or does not) is reset. Five of the six combinations
/// are named here; each variant's own doc states its column values.
/// `#[non_exhaustive]` buys source compatibility only: an older build
/// still fails to deserialize a variant it predates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResetDepth {
    /// Append keys nobody established. Overwrite nothing. The default,
    /// because a Flockfile arrives from the app's own repository.
    ///
    /// `env`: kept. A key the template declares: kept, unless nobody has
    /// established it yet, which is the append. A key it does not declare:
    /// kept, since there is nothing to append it against.
    #[default]
    None,
    /// Put back what the template declares, and nothing else.
    ///
    /// `env`: kept. A key the template declares: reset. A key it does not
    /// declare: kept, so an app stocked to four instances against a file
    /// with no `instances` line keeps its count.
    File,
    /// Put non-`env` settings back to the template, `env` kept. Every setting
    /// goes back to what a fresh start off the template would give it. `env` is
    /// operator data, the rest is tuned policy: resetting policy is recoverable,
    /// resetting data is not.
    ///
    /// `env`: kept. A key the template declares: reset. A key it does not
    /// declare: reset too, to the template's own default.
    Policy,
    /// Reset `env` back to the template and leave everything else alone.
    ///
    /// Touches data, not policy: on the settings axis this behaves like
    /// `None`, append included, since it widens a load rather than narrowing one.
    ///
    /// `env`: reset. A key the template declares: kept, save for the same
    /// append `None` does. A key it does not declare: kept.
    Env,
    /// Put everything back to the template, `env` included, and drop the
    /// override record.
    ///
    /// `env`: reset. A key the template declares: reset. A key it does not
    /// declare: reset too, to the template's own default.
    All,
}

#[cfg(test)]
mod tests {
    use super::{ApplyGroup, FIELDS, apply_group, is_classified, reaches_running};
    use crate::config::AppConfig;

    /// fails if any AppConfig field is missing from the table. A field added
    /// to the struct without a group would route as its default and either
    /// apply live when it cannot, or need a restart when it does not.
    /// The field names `AppConfig` serializes, which is what both tests
    /// below mean by "every field".
    fn appconfig_fields() -> serde_json::Map<String, serde_json::Value> {
        let serde_json::Value::Object(fields) = serde_json::to_value(AppConfig::default()).unwrap()
        else {
            panic!("AppConfig must serialize as an object");
        };
        fields
    }

    #[test]
    fn every_appconfig_field_has_a_group() {
        let fields = appconfig_fields();
        let missing: Vec<&String> = fields.keys().filter(|k| !is_classified(k)).collect();
        assert!(
            missing.is_empty(),
            "unclassified AppConfig fields: {missing:?}"
        );
    }

    /// fails if the table keeps a row for a field `AppConfig` no longer
    /// has. `every_appconfig_field_has_a_group` checks the other
    /// direction only, so without this one a removed field's row survives
    /// every run: the count tests iterate the struct, never the table.
    #[test]
    fn every_group_row_names_a_real_field() {
        let fields = appconfig_fields();
        let strays: Vec<&str> = FIELDS
            .iter()
            .map(|row| row.field)
            .filter(|name| !fields.contains_key(*name))
            .collect();
        assert!(strays.is_empty(), "FIELDS rows naming no field: {strays:?}");
    }

    #[test]
    fn kill_signal_reaches_the_next_spawn_not_the_next_kill() {
        assert_eq!(apply_group("kill_signal"), ApplyGroup::NextSpawn);
        assert_eq!(apply_group("kill_timeout"), ApplyGroup::Live);
        assert_eq!(apply_group("graceful_timeout"), ApplyGroup::Live);
    }

    #[test]
    fn dogs_is_live_since_the_daemon_never_reads_it() {
        assert_eq!(apply_group("dogs"), ApplyGroup::Live);
        assert!(reaches_running("dogs"));
    }

    #[test]
    fn shutdown_with_message_is_baked_into_the_child() {
        assert_eq!(
            apply_group("shutdown_with_message"),
            ApplyGroup::NeedsRespawn
        );
    }

    #[test]
    fn depends_on_applies_at_the_next_spawn() {
        // fails if the field is classified Live, which would claim an edit
        // reaches a running flock's order, or Structural, which would route it
        // through handle_scale
        assert_eq!(apply_group("depends_on"), ApplyGroup::NextSpawn);
        assert!(is_classified("depends_on"));
    }

    /// fails if the split drifts unnoticed.
    #[test]
    fn the_split_is_21_5_16_2() {
        let fields = appconfig_fields();
        let count = |want: ApplyGroup| fields.keys().filter(|k| apply_group(k) == want).count();
        assert_eq!(count(ApplyGroup::Live), 21, "Live");
        assert_eq!(count(ApplyGroup::NextSpawn), 5, "NextSpawn");
        assert_eq!(count(ApplyGroup::NeedsRespawn), 16, "NeedsRespawn");
        assert_eq!(count(ApplyGroup::Structural), 2, "Structural");
    }

    #[test]
    fn a_live_field_reaches_a_running_child() {
        assert!(reaches_running("max_restarts"));
    }

    /// The two `NextSpawn` fields nothing spawns to read: `restorable()`
    /// reads one and `plan_for_names` the other, off the stored spec the
    /// moment it lands.
    #[test]
    fn autostart_and_depends_on_reach_one_without_a_respawn() {
        assert!(reaches_running("autostart"));
        assert!(reaches_running("depends_on"));
    }

    /// The other three `NextSpawn` fields come off the per-sheep task's
    /// `ResolvedApp`, so a respawn is what applies them.
    #[test]
    fn the_other_next_spawn_fields_do_not() {
        assert!(!reaches_running("kill_signal"));
        assert!(!reaches_running("listen_timeout"));
        assert!(!reaches_running("readiness_probe"));
    }

    #[test]
    fn a_needs_respawn_field_does_not() {
        assert!(!reaches_running("cwd"));
    }

    /// An unknown name answers like `apply_group` does, conservatively.
    #[test]
    fn an_unknown_field_does_not_reach_a_running_child() {
        assert!(!reaches_running("a_field_from_a_later_shep"));
    }

    /// The claim the hoist rests on: every row's own `reaches_running` entry
    /// is what its group implies, and `reaches_running` answers with that
    /// entry. A field added to `FIELDS` without a thought about which side of
    /// the line it falls on fails here. The match below carries no wildcard
    /// arm, so a group a later shep-core adds stops this test at compile time
    /// instead of inheriting an answer.
    #[test]
    fn reaches_running_agrees_with_the_group_table_for_every_field() {
        let mut next_spawn_reaching = 0;
        for row in FIELDS {
            assert_eq!(
                reaches_running(row.field),
                row.reaches_running,
                "{field} is classified {group:?}",
                field = row.field,
                group = row.group,
            );
            match row.group {
                ApplyGroup::Live => assert!(
                    row.reaches_running,
                    "{field} is Live but its row says a respawn applies it",
                    field = row.field
                ),
                ApplyGroup::NeedsRespawn | ApplyGroup::Structural => assert!(
                    !row.reaches_running,
                    "{field} is {group:?} but its row says a running child sees it",
                    field = row.field,
                    group = row.group
                ),
                ApplyGroup::NextSpawn => next_spawn_reaching += usize::from(row.reaches_running),
            }
        }
        // Which two `NextSpawn` rows those are is pinned by
        // `autostart_and_depends_on_reach_one_without_a_respawn`. This pins
        // that only two fields read the stored spec off the flock they are
        // already part of.
        assert_eq!(
            next_spawn_reaching, 2,
            "NextSpawn rows claiming a running child sees them"
        );
    }
}
