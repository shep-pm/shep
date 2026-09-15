//! `emit_described`'s own envelope: `describe`'s sheep table, lamb trees,
//! and per-name config/secret prose underneath it.

use std::collections::BTreeSet;
use std::io;

use serde::Serialize;
use shep_core::protocol::ProcessInfo;

use crate::cli::Format;
use crate::style::Presentation;

use super::{FlockRows, LambRows, SCHEMA_VERSION, rows, table_of};

/// The `--format json` shape [`emit_described`] writes: [`OutputEnvelope`]'s
/// own three fields, plus `secrets` riding beside `data` rather than inside
/// it.
///
/// A sibling field rather than a new column on [`ProcessInfo`]: `secrets`
/// is derived by the client from local files, never a fact the shepherd
/// reports, and `data` stays exactly the array it always was, so an
/// existing `data[0].name` script sees no shape change. Empty skips the
/// field entirely, matching a `fold` reply, which never computes one.
///
/// Only ever constructed by [`emit_described`]. `#[cfg_attr(windows,
/// allow(dead_code))]` for [`NoticeEnvelope`]'s reason: every caller lives
/// in `commands/` or `lib.rs`'s `#[cfg(unix)]` arms.
#[derive(Serialize)]
#[cfg_attr(windows, allow(dead_code))]
struct DescribedEnvelope<'a> {
    schema_version: u32,
    command: &'a str,
    data: FlockRows,
    #[serde(skip_serializing_if = "<[rows::DescribedSecret]>::is_empty")]
    secrets: &'a [rows::DescribedSecret],
}

/// Renders one `describe` answer: the sheep table, then each sheep's lamb
/// tree beneath it when the reply walked and found any.
///
/// A silent row also gets a paragraph from
/// [`crate::vocabulary::silence_note`] (what [`silence_pointer`] points
/// at). A "Depends on" heading follows, once per name, naming the sheep this
/// one waits for at a staged start; then Pending and Overridden headings,
/// same once-per-name rule, naming `shep reload <name>` as what promotes a
/// parked config. Never shorten the caption to "process tree": the walk
/// follows parent-pid links while the stop ladder acts on the process group,
/// and the two diverge.
///
/// `secrets` is `describe`'s own local read of this machine's secret
/// stores, keyed by sheep name; pass an empty slice for a reply (`fold`,
/// today) that never computes one. Printed once per name, right after
/// Overridden, in the same "prose under the table" shape.
///
/// # Errors
/// The underlying write failed.
#[cfg_attr(windows, allow(dead_code))]
pub fn emit_described(
    out: &mut dyn io::Write,
    fmt: Format,
    command: &str,
    listing: Vec<ProcessInfo>,
    style: Presentation,
    secrets: &[rows::DescribedSecret],
) -> io::Result<()> {
    match fmt {
        Format::Json => {
            let envelope = DescribedEnvelope {
                schema_version: SCHEMA_VERSION,
                command,
                data: FlockRows(listing),
                secrets,
            };
            serde_json::to_writer(&mut *out, &envelope)?;
            writeln!(out)
        }
        Format::Table => {
            let flock = FlockRows(listing);
            write!(out, "{}", table_of(&flock, style))?;
            // Before the lamb trees, because this explains a cell in the
            // table directly above it and a lamb table would put a second
            // table between the two.
            for sheep in &flock.0 {
                if let Some(note) = rows::silence_note(sheep) {
                    writeln!(out, "\n{note}")?;
                }
            }
            for sheep in &flock.0 {
                let Some(lambs) = &sheep.lambs else {
                    continue;
                };
                if lambs.is_empty() {
                    continue;
                }
                writeln!(
                    out,
                    "\nLambs of {} (id {}) — parent-pid descendants of {}, which is not exactly \
                     the set a stop kills",
                    sheep.name,
                    sheep.id,
                    sheep
                        .pid
                        .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
                )?;
                write!(out, "{}", table_of(&LambRows(lambs.clone()), style))?;
            }
            // Once per name, not once per row: a parked or overridden
            // config belongs to the app, and the daemon writes the same
            // entry onto every slot of a name. The rows themselves stay
            // per instance, since that is a claim about the process.
            let mut said: BTreeSet<&str> = BTreeSet::new();
            for sheep in &flock.0 {
                if !said.insert(sheep.name.as_str()) {
                    continue;
                }
                if !sheep.depends_on.is_empty() {
                    writeln!(out, "\nDepends on for {}:", sheep.name)?;
                    for name in &sheep.depends_on {
                        writeln!(out, "  {name}")?;
                    }
                }
                if let Some(fields) = sheep.pending.as_deref().filter(|f| !f.is_empty()) {
                    writeln!(
                        out,
                        "\nPending for {}, parked by a load; `shep reload {}` promotes it:",
                        sheep.name, sheep.name,
                    )?;
                    for field in fields {
                        writeln!(out, "  {field}")?;
                    }
                }
                if let Some(fields) = sheep.overridden.as_deref().filter(|f| !f.is_empty()) {
                    writeln!(
                        out,
                        "\nOverridden for {}, fields its current Flockfile does not declare:",
                        sheep.name,
                    )?;
                    for field in fields {
                        writeln!(out, "  {field}")?;
                    }
                }
                let mine: Vec<&rows::DescribedSecret> = secrets
                    .iter()
                    .filter(|entry| entry.name == sheep.name)
                    .collect();
                if !mine.is_empty() {
                    writeln!(out, "\nSecrets for {}:", sheep.name)?;
                    for entry in mine {
                        writeln!(
                            out,
                            "  {} ({}): {}",
                            entry.reference,
                            entry.environment,
                            entry.status.as_table_word()
                        )?;
                    }
                }
            }
            Ok(())
        }
    }
}
