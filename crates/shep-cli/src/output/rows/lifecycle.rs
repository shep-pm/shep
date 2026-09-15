//! Rows for one-shot verbs that do not report through a `ProcessInfo`:
//! flushed log files, deletes, kills, roll saves and loads, imports, and
//! `shep startup`'s installer steps.

use serde::Serialize;

use crate::output::Render;
use crate::style::Presentation;
use crate::vocabulary::Role;

use super::process::{Paint, paint};

/// One of the shepherd's own log files, and what `shep flush --daemon` made
/// of it.
///
/// Its own payload rather than a `ProcessInfo`: these files belong to no
/// sheep, and the CLI empties them itself without asking the daemon.
#[derive(Debug, Serialize)]
pub struct EmptiedFile {
    /// Which of the shepherd's streams this file takes: `stdout` or `stderr`.
    pub stream: &'static str,
    /// The file's absolute path, as this invocation resolved `$SHEP_HOME`.
    pub file: String,
    /// `emptied` when the file was truncated, `absent` when there was no
    /// such file: already empty, and not created just to say so.
    pub result: &'static str,
}

/// `shep flush --daemon`: one row per file the shepherd logs into.
///
/// `transparent` so the JSON is a plain array, as every list payload here is.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct EmptiedFiles(pub Vec<EmptiedFile>);

impl Render for EmptiedFiles {
    fn headers() -> &'static [&'static str] {
        &["STREAM", "FILE", "RESULT"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|f| vec![f.stream.to_string(), f.file.clone(), f.result.to_string()])
            .collect()
    }

    /// RESULT alone. `absent` is muted rather than marked: no file to
    /// truncate is the state `flush` was asked to produce, not a failure.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        let rows = self.rows();
        paint(
            rows,
            Self::headers(),
            presentation,
            status_word,
            |header, cell, _index| match (header, cell) {
                ("RESULT", "emptied") => Paint::Role(Role::Meadow),
                ("RESULT", _) => Paint::Role(Role::Ink3),
                _ => Paint::Default,
            },
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "STREAM" => "stream",
            "FILE" => "file",
            "RESULT" => "result",
            other => panic!("EmptiedFiles::headers() does not include {other:?}"),
        }
    }

    // Every field is a column: a verb that emptied a file and would not say
    // which one has reported nothing.
    const JSON_ONLY: &'static [&'static str] = &[];

    // Parallel to `headers()`. Three columns is `render_boxed`'s own floor,
    // so this never narrows.
    const PRIORITIES: &'static [u8] = &[0, 6, 0];
}

/// `Response::Deleted(Vec<u32>)`: the ids that were removed.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct DeletedIds(pub Vec<u32>);

/// No colour, and not the muted ID every other table gives that column: here
/// the ID is the only column and is the content, so muting it would fade the
/// whole table.
impl Render for DeletedIds {
    fn headers() -> &'static [&'static str] {
        &["ID"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0.iter().map(|id| vec![id.to_string()]).collect()
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "ID" => "id",
            other => panic!("DeletedIds::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // One column, and it is the row's whole identity.
    const PRIORITIES: &'static [u8] = &[0];
}

/// `kill`: what teardown actually achieved.
#[derive(Debug, Serialize)]
pub struct KillRow {
    /// Daemon pid at the moment of kill, read before the connection dropped.
    pub pid: u32,
    /// Whether the daemon removed its own socket file before exiting.
    pub socket_removed: bool,
}

impl Render for KillRow {
    fn headers() -> &'static [&'static str] {
        &["PID", "SOCKET_REMOVED"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        vec![vec![self.pid.to_string(), self.socket_removed.to_string()]]
    }

    /// SOCKET_REMOVED alone: `false` means the socket file outlived the
    /// daemon and the next boot has to contend with it. `Butter` and not
    /// `Bark`: a leftover to clear is no crash.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        let removed = self.socket_removed;
        paint(
            self.rows(),
            Self::headers(),
            presentation,
            status_word,
            |header, _cell, _index| match header {
                "SOCKET_REMOVED" if removed => Paint::Role(Role::Meadow),
                "SOCKET_REMOVED" => Paint::Role(Role::Butter),
                _ => Paint::Default,
            },
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "PID" => "pid",
            "SOCKET_REMOVED" => "socket_removed",
            other => panic!("KillRow::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Two columns, both the point of the report.
    const PRIORITIES: &'static [u8] = &[0, 0];
}

/// One sheep as the muster roll remembers it, for `shep flock` when no
/// shepherd is running.
///
/// `status` is always `"stopped"`: a roll records what was registered, and
/// with no shepherd answering, nothing from it is up.
#[derive(Debug, Serialize)]
pub struct RolledSheep {
    /// The sheep's name, as saved.
    pub name: String,
    /// How many instances were running when the roll was written.
    pub instances: u32,
    /// Always `"stopped"`.
    pub status: &'static str,
}

/// Every sheep in a muster roll.
#[derive(Debug, Serialize)]
pub struct RolledSheepRows(pub Vec<RolledSheep>);

/// No colour, including on STATUS, the one unpainted STATUS column here:
/// every row carries the same literal `stopped`, and a colour identical on
/// every row distinguishes nothing.
impl Render for RolledSheepRows {
    fn headers() -> &'static [&'static str] {
        &["NAME", "INSTANCES", "STATUS"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|s| vec![s.name.clone(), s.instances.to_string(), s.status.to_owned()])
            .collect()
    }

    /// # Panics
    /// If `header` is not one of [`Self::headers`]'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "NAME" => "name",
            "INSTANCES" => "instances",
            "STATUS" => "status",
            other => panic!("RolledSheepRows has no column {other}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Parallel to `headers()`. Three columns never narrows.
    const PRIORITIES: &'static [u8] = &[0, 6, 0];
}

/// `Response::RollSaved`: where the muster roll landed, and what it recorded.
///
/// Every field is a column, for [`EmptiedFiles`]' reason.
#[derive(Debug, Serialize)]
pub struct SavedRollRow {
    /// The roll's path, exactly as the daemon reported it.
    pub file: String,
    /// How many apps that roll records.
    pub apps: u32,
}

/// No colour: a path and a count are the report itself rather than a reading
/// about it, with no state, threshold or outcome.
impl Render for SavedRollRow {
    fn headers() -> &'static [&'static str] {
        &["FILE", "APPS"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        vec![vec![self.file.clone(), self.apps.to_string()]]
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "FILE" => "file",
            "APPS" => "apps",
            other => panic!("SavedRollRow::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Two columns, both the point of the report.
    const PRIORITIES: &'static [u8] = &[0, 0];
}

/// One app `shep import` read out of a pm2 dump.
///
/// Never from a wire `Response`: this verb asks the daemon nothing. A `true`
/// `REUSE_PORT` is work for the operator, since shep binds nothing and the
/// app must set `SO_REUSEPORT` itself.
#[derive(Debug, Serialize)]
pub struct ImportRow {
    /// The app's name, which is also the key its instance rows were grouped by.
    pub name: String,
    /// The script the app runs.
    pub script: String,
    /// How many instances of it the dump recorded running.
    pub instances: u32,
    /// Whether the app has to set `SO_REUSEPORT` itself (pm2 cluster mode).
    pub reuse_port: bool,
}

/// `shep import`: one row per app the dump was collapsed into.
///
/// `transparent` so the JSON is a plain array.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct ImportRows(pub Vec<ImportRow>);

impl Render for ImportRows {
    fn headers() -> &'static [&'static str] {
        &["NAME", "SCRIPT", "INSTANCES", "REUSE_PORT"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|row| {
                vec![
                    row.name.clone(),
                    row.script.clone(),
                    row.instances.to_string(),
                    row.reuse_port.to_string(),
                ]
            })
            .collect()
    }

    /// REUSE_PORT alone, and only when `true`: work for the operator, so the
    /// same `Butter` a restart count above zero takes.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        let rows = self.rows();
        paint(
            rows,
            Self::headers(),
            presentation,
            status_word,
            |header, cell, _index| match (header, cell) {
                ("REUSE_PORT", "true") => Paint::Role(Role::Butter),
                _ => Paint::Default,
            },
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "NAME" => "name",
            "SCRIPT" => "script",
            "INSTANCES" => "instances",
            "REUSE_PORT" => "reuse_port",
            other => panic!("ImportRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Parallel to `headers()`. Only SCRIPT, an unbounded path, is ever
    // actually lost.
    const PRIORITIES: &'static [u8] = &[0, 8, 7, 6];
}

/// One key `shep import env` wrote, or would write.
///
/// `Debug` is derived and stays that way only because there is no value
/// here: `bytes` is a length. The row exists in this shape precisely so
/// that neither format can print a value (IR-41).
#[derive(Debug, Serialize)]
pub struct ImportEnvRow {
    /// The key.
    pub key: String,
    /// `secret` or `env`.
    pub store: String,
    /// The environment slot, for a secret. `-` for an env key.
    pub slot: String,
    /// The value's length in bytes.
    pub bytes: usize,
}

/// `shep import env`: one row per key.
///
/// `transparent` so the JSON is a plain array.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct ImportEnvRows(pub Vec<ImportEnvRow>);

impl Render for ImportEnvRows {
    fn headers() -> &'static [&'static str] {
        &["KEY", "STORE", "SLOT", "BYTES"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|row| {
                vec![
                    row.key.clone(),
                    row.store.clone(),
                    row.slot.clone(),
                    row.bytes.to_string(),
                ]
            })
            .collect()
    }

    /// STORE alone, and only on `secret`: the row an operator has to think
    /// about, so the same `Butter` `ImportRows` paints its own.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        let rows = self.rows();
        paint(
            rows,
            Self::headers(),
            presentation,
            status_word,
            |header, cell, _index| match (header, cell) {
                ("STORE", "secret") => Paint::Role(Role::Butter),
                _ => Paint::Default,
            },
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "KEY" => "key",
            "STORE" => "store",
            "SLOT" => "slot",
            "BYTES" => "bytes",
            other => panic!("ImportEnvRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[];

    // Parallel to `headers()`. KEY and STORE are the report; SLOT and BYTES
    // are context a narrow terminal can lose.
    const PRIORITIES: &'static [u8] = &[0, 0, 1, 2];
}

/// One step `shep startup` or `shep unstartup` took.
///
/// Never from a wire `Response`: neither verb asks the shepherd anything.
#[derive(Debug, Serialize)]
pub struct StartupStep {
    /// What was done: `wrote`, `removed`, `ran`.
    pub action: &'static str,
    /// The file or command it was done to.
    pub target: String,
    /// `ok`, `absent`, or the failure in one line. `absent` is an
    /// `unstartup` that found no unit to remove, not a failure.
    pub result: String,
}

/// `shep startup`/`shep unstartup`: one row per step, in the order taken.
///
/// Every step is reported even when an earlier one failed: a half-installed
/// unit needs every row to say which half.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct StartupSteps(pub Vec<StartupStep>);

impl Render for StartupSteps {
    fn headers() -> &'static [&'static str] {
        &["ACTION", "TARGET", "RESULT"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|step| {
                vec![
                    step.action.to_string(),
                    step.target.clone(),
                    step.result.clone(),
                ]
            })
            .collect()
    }

    /// RESULT alone. `absent` is muted rather than marked as a failure;
    /// anything but `ok` or `absent` is the failure line, so it takes `Bark`.
    fn rows_for(&self, presentation: Presentation, status_word: bool) -> Vec<Vec<String>> {
        let rows = self.rows();
        paint(
            rows,
            Self::headers(),
            presentation,
            status_word,
            |header, cell, _index| match (header, cell) {
                ("RESULT", "ok") => Paint::Role(Role::Meadow),
                ("RESULT", "absent") => Paint::Role(Role::Ink3),
                ("RESULT", _) => Paint::Role(Role::Bark),
                _ => Paint::Default,
            },
        )
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "ACTION" => "action",
            "TARGET" => "target",
            "RESULT" => "result",
            other => panic!("StartupSteps::headers() does not include {other:?}"),
        }
    }

    // Every field is a column, for `EmptiedFiles`' reason.
    const JSON_ONLY: &'static [&'static str] = &[];

    // Parallel to `headers()`. Three columns never narrows.
    const PRIORITIES: &'static [u8] = &[6, 0, 0];
}
