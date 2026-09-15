//! `emit_flock`'s own envelope and its silence pointer.
//!
//! Split out from `output` because `emit_described` needs the same
//! `Option<Option<HostUsage>>` reasoning right beside it, and the two no
//! longer fit in one file together with the rest of the module.

use std::io;

use serde::Serialize;
use shep_core::protocol::{HostUsage, ProcessInfo};

use crate::cli::Format;
use crate::style::Presentation;

use super::{DogRows, FlockRows, SCHEMA_VERSION, rows, table_of};

/// The `--format json` shape [`emit_flock`] writes: [`OutputEnvelope`]'s own
/// three fields, plus `host` riding beside `data` rather than inside it.
///
/// A sibling field for [`DescribedEnvelope`]'s reason: `data` stays exactly
/// the array it always was, so an existing `data[0].name` script sees no
/// shape change, and [`SCHEMA_VERSION`] does not move for an addition
/// outside it.
///
/// `Option<Option<HostUsage>>` because there are three answers and a reader
/// has to tell them apart. Absent: this shepherd would not answer, which
/// today means one built before `Request::HostUsage` existed. `null`: a
/// platform `sysinfo` cannot read. An object: a reading, whose own rate
/// fields are `null` where no window has passed yet.
///
/// Only ever constructed by [`emit_flock`]. `#[cfg_attr(windows,
/// allow(dead_code))]` for [`DescribedEnvelope`]'s reason.
#[derive(Serialize)]
#[cfg_attr(windows, allow(dead_code))]
struct FlockEnvelope<'a> {
    schema_version: u32,
    command: &'a str,
    data: FlockRows,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<Option<HostUsage>>,
}

/// Renders one flock listing: the sheep table, then the dogs table
/// beneath it whenever any dog is registered.
///
/// JSON stays one array under `data`, every entry carrying its own `dog`
/// marker, with `host` beside it: see [`FlockEnvelope`]. `host` is `None`
/// for a caller with no host block to carry, which is every caller but
/// `shep flock` itself.
/// Table partitions on [`ProcessInfo::dog`], rendering sheep and dogs
/// each through [`table_of`], with a blank line and `Dogs` caption
/// between them only when a dog exists. [`silence_pointer`] adds one
/// line under the dogs table when a dog is silent.
///
/// # Errors
/// The underlying write failed.
#[cfg_attr(windows, allow(dead_code))]
pub fn emit_flock(
    out: &mut dyn io::Write,
    fmt: Format,
    command: &str,
    listing: Vec<ProcessInfo>,
    host: Option<Option<HostUsage>>,
    style: Presentation,
) -> io::Result<()> {
    match fmt {
        Format::Json => {
            let envelope = FlockEnvelope {
                schema_version: SCHEMA_VERSION,
                command,
                data: FlockRows(listing),
                host,
            };
            serde_json::to_writer(&mut *out, &envelope)?;
            writeln!(out)
        }
        Format::Table => {
            let (dogs, sheep): (Vec<ProcessInfo>, Vec<ProcessInfo>) =
                listing.into_iter().partition(|p| p.dog.is_some());
            write!(out, "{}", table_of(&FlockRows(sheep), style))?;
            if dogs.is_empty() {
                return Ok(());
            }
            // Read before `DogRows` takes the rows, which is the only
            // reason it is not read after the table is written.
            let pointer = silence_pointer(&dogs);
            write!(out, "\nDogs\n")?;
            write!(out, "{}", table_of(&DogRows(dogs), style))?;
            match pointer {
                None => Ok(()),
                Some(line) => writeln!(out, "\n{line}"),
            }
        }
    }
}

/// The one line under the dogs table that says where `silent` is
/// explained, or nothing at all when no dog is silent.
///
/// A pointer, not the explanation: that runs to a paragraph per dog
/// (`vocabulary::silence_note`), too much for a table an operator leaves
/// running in a loop. Rendered after the table, outside it, so a long
/// list of names wraps in the terminal rather than squeezing STATUS off
/// the side of it. Named rather than counted, since the names are what
/// the operator types into the next command.
fn silence_pointer(dogs: &[ProcessInfo]) -> Option<String> {
    let silent: Vec<&str> = dogs
        .iter()
        .filter(|dog| rows::silence_note(dog).is_some())
        .map(|dog| dog.name.as_str())
        .collect();
    match silent.as_slice() {
        [] => None,
        [only] => Some(format!(
            "`{only}` is silent -- its process is up and it has never answered this shepherd. \
             Run `shep describe {only}` for what that means and what to do about it."
        )),
        many => Some(format!(
            "these dogs are silent -- their processes are up and they have never answered this \
             shepherd: {}. Run `shep describe <name>` for what that means and what to do about \
             it.",
            many.join(", ")
        )),
    }
}
