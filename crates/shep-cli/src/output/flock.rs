//! `emit_flock`'s own envelope and its silence pointer.
//!
//! Split out from `output` because `emit_described` needs the same
//! `Option<Option<HostUsage>>` reasoning right beside it, and the two no
//! longer fit in one file together with the rest of the module.

use std::collections::BTreeMap;
use std::io;

use serde::Serialize;
use shep_core::protocol::{HostUsage, ProcessInfo, sort_flock};

use crate::cli::Format;
use crate::style::Presentation;

use super::{DogRows, FlockRows, SCHEMA_VERSION, rows, table_of, write_json_line};

/// The `--format json` shape [`emit_flock`] writes:
/// [`super::OutputEnvelope`]'s own three fields, plus `host` riding beside
/// `data` rather than inside it.
///
/// `Option<Option<HostUsage>>` has three answers: absent, a shepherd that
/// would not answer; `null`, a platform `sysinfo` cannot read; an object,
/// a reading whose rate fields are `null` before its first window.
#[derive(Serialize)]
#[cfg_attr(windows, allow(dead_code))]
struct FlockEnvelope<'a> {
    schema_version: u32,
    command: &'a str,
    data: FlockRows,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<Option<HostUsage>>,
}

/// One row of a listing that names where it came from: the shepherd's own
/// [`ProcessInfo`], flattened, and `origin`.
///
/// Output-only. The shepherd does not know which host a client reached it
/// through, so the wire stays as it is. `origin` is `null` for this machine's
/// row and the `hosts` entry exactly as written for a remote one, and is
/// never skipped, so a script can read `.origin` on every row.
#[derive(Serialize)]
struct OriginRow<'a> {
    #[serde(flatten)]
    info: &'a ProcessInfo,
    origin: Option<&'a str>,
}

/// The `--format json` shape of a listing whose rows name their `origin`:
/// [`FlockEnvelope`]'s fields with `data` as [`OriginRow`]s, and `hosts`
/// beside `host`.
///
/// `hosts` repeats [`FlockEnvelope`]'s three-way answer per remote host: no
/// entry for a shepherd that would not answer, or for a host that failed.
/// It is absent for a single `--host` listing.
#[derive(Serialize)]
#[cfg_attr(windows, allow(dead_code))]
struct OriginEnvelope<'a> {
    schema_version: u32,
    command: &'a str,
    data: Vec<OriginRow<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<Option<HostUsage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hosts: Option<BTreeMap<&'a str, Option<HostUsage>>>,
}

/// One shepherd's answer to a fleet listing.
pub(crate) struct HostListing {
    /// The host as written in the hosts list, or `None` for this machine.
    pub host: Option<String>,
    pub procs: Vec<ProcessInfo>,
    /// As [`emit_flock`]'s `host`.
    pub usage: Option<Option<HostUsage>>,
}

/// Writes a fleet listing as one JSON envelope: every shepherd's rows in
/// `listings` order under `data`, each tagged with its `origin`, and every
/// remote host's usage under `hosts`.
///
/// Each shepherd's rows go through [`sort_flock`], so `data` reads like the
/// table form. A host that failed is simply not in `listings`.
///
/// # Errors
/// The underlying write failed.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn emit_fleet_flock(
    out: &mut dyn io::Write,
    command: &str,
    mut listings: Vec<HostListing>,
) -> io::Result<()> {
    for listing in &mut listings {
        sort_flock(&mut listing.procs);
    }
    let mut hosts = BTreeMap::new();
    let mut local = None;
    for listing in &listings {
        match (&listing.host, listing.usage) {
            (None, usage) => local = usage,
            (Some(name), Some(usage)) => {
                hosts.insert(name.as_str(), usage);
            }
            (Some(_), None) => {}
        }
    }
    let data = listings
        .iter()
        .flat_map(|listing| {
            listing.procs.iter().map(|info| OriginRow {
                info,
                origin: listing.host.as_deref(),
            })
        })
        .collect();
    write_json_line(
        out,
        &OriginEnvelope {
            schema_version: SCHEMA_VERSION,
            command,
            data,
            host: local,
            hosts: Some(hosts),
        },
    )
}

/// Writes one shepherd's listing as JSON with `origin` on every row: what
/// `--host <name>` prints, `host` being that shepherd's own usage.
///
/// # Errors
/// The underlying write failed.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn emit_origin_flock(
    out: &mut dyn io::Write,
    command: &str,
    origin: &str,
    listing: &[ProcessInfo],
    host: Option<Option<HostUsage>>,
) -> io::Result<()> {
    write_json_line(
        out,
        &OriginEnvelope {
            schema_version: SCHEMA_VERSION,
            command,
            data: listing
                .iter()
                .map(|info| OriginRow {
                    info,
                    origin: Some(origin),
                })
                .collect(),
            host,
            hosts: None,
        },
    )
}

/// Renders one flock listing: the sheep table, then the dogs table
/// beneath it whenever any dog is registered.
///
/// JSON stays one array under `data`, every entry carrying its own `dog`
/// marker, with `host` beside it: see [`FlockEnvelope`]. `host` is `None`
/// for a caller with no host block to carry. Table partitions on
/// [`ProcessInfo::dog`], with a blank line and `Dogs` caption between them
/// only when a dog exists.
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
            write_json_line(&mut *out, &envelope)
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
/// running in a loop. Named rather than counted, since the names are what
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

#[cfg(test)]
mod tests {
    use shep_core::protocol::DogSource;

    use crate::output::rows::tests::dog_info;

    use super::super::tests::{mixed_listing, sheep_info, silent_dog};
    use super::*;

    /// The dogs table needs no flag: a dead bark dog is what an operator
    /// needs to notice, and hiding it means finding out by not being
    /// paged.
    #[test]
    fn a_flock_listing_prints_the_dogs_in_their_own_table() {
        let mut out = Vec::new();
        emit_flock(
            &mut out,
            Format::Table,
            "flock",
            mixed_listing(),
            None,
            Presentation::BARE,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();

        let (sheep_table, dogs_table) = text.split_once("\nDogs\n").expect("a Dogs caption");
        assert!(sheep_table.contains("web"));
        assert!(!sheep_table.contains("bark"), "a dog is not a sheep");
        assert!(dogs_table.contains("bark"));
        assert!(!dogs_table.contains("web"));
        // The dogs table carries an ID column, and its columns line up
        // with the sheep table's for every header the two share.
        assert!(
            dogs_table.starts_with("ID"),
            "the dogs table leads with ID, as the sheep table does: {dogs_table}"
        );
        let shared: Vec<&str> = dogs_table
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .take(9)
            .collect();
        assert_eq!(
            shared,
            [
                "ID", "NAME", "STATUS", "PID", "RESTARTS", "EXIT", "CPU", "MEM", "UPTIME"
            ],
            "the nine shared columns, in the sheep table's own order"
        );
        let header: Vec<&str> = dogs_table
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .collect();
        assert_eq!(
            &header[9..],
            ["SOURCE", "ACCESS"],
            "and this table's own columns last"
        );
    }

    /// `silent` names a relationship, not a state, and an operator cannot
    /// act on it from this table alone: the paragraph lives in `describe`.
    #[test]
    fn a_silent_dog_is_pointed_at_the_view_that_explains_it() {
        let mut out = Vec::new();
        emit_flock(
            &mut out,
            Format::Table,
            "flock",
            vec![sheep_info("web"), silent_dog("log-rotate", Some(true))],
            None,
            Presentation::BARE,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();

        assert!(text.contains("silent"), "the cell still says it: {text}");
        assert!(
            text.contains("shep describe log-rotate"),
            "and the pointer names the dog, so it can be typed: {text}"
        );
    }

    /// Adding a consequence for `silent` must add no column and move no
    /// cell.
    #[test]
    fn the_silence_pointer_sits_below_the_table_and_changes_no_column() {
        // The SAME dog either way, differing only in whether it has
        // answered. A different dog would widen the NAME column on its own
        // and the comparison below would be measuring the fixture rather
        // than the pointer.
        let silent = vec![sheep_info("web"), silent_dog("log-rotate", Some(true))];
        let mut talking = silent.clone();
        talking[1].handshook = Some(true);
        talking[1].dog_stale = Some(false);

        let render = |listing: Vec<ProcessInfo>| {
            let mut out = Vec::new();
            emit_flock(
                &mut out,
                Format::Table,
                "flock",
                listing,
                None,
                Presentation::BARE,
            )
            .unwrap();
            String::from_utf8(out).unwrap()
        };

        let with_pointer = render(silent);
        let header = with_pointer
            .split_once("\nDogs\n")
            .expect("a Dogs caption")
            .1
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert_eq!(
            header,
            render(talking)
                .split_once("\nDogs\n")
                .expect("a Dogs caption")
                .1
                .lines()
                .next()
                .unwrap(),
            "the pointer is prose under the table, never a column in it"
        );
        assert!(
            with_pointer.trim_end().ends_with("what to do about it."),
            "and it comes last, after the table it annotates: {with_pointer}"
        );
    }

    /// The same rule the `Dogs` caption itself follows: a listing with
    /// nothing to report prints nothing extra.
    #[test]
    fn a_flock_with_no_silent_dog_says_nothing_about_silence() {
        let mut out = Vec::new();
        let mut talking = dog_info("bark", DogSource::BuiltIn { grants: None });
        talking.handshook = Some(true);
        talking.dog_stale = Some(false);
        emit_flock(
            &mut out,
            Format::Table,
            "flock",
            vec![sheep_info("web"), talking],
            None,
            Presentation::BARE,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("silent"), "{text}");
        assert!(!text.contains("shep describe"), "{text}");
    }

    /// The machine surface is the single registry: one array, every entry
    /// carrying its own marker, never split to match the tables.
    #[test]
    fn the_json_surface_stays_one_array_of_every_entry() {
        let mut out = Vec::new();
        emit_flock(
            &mut out,
            Format::Json,
            "flock",
            mixed_listing(),
            None,
            Presentation::BARE,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["data"].as_array().unwrap().len(), 2);
        assert_eq!(json["data"][0]["dog"], serde_json::Value::Null);
        assert_eq!(json["data"][1]["dog"]["kind"], "built_in");
    }

    /// An empty table still prints its header row, so a caption here
    /// would surface a bare header line under every dogless listing.
    #[test]
    fn a_flock_with_no_dogs_prints_one_table_and_no_caption() {
        let mut out = Vec::new();
        emit_flock(
            &mut out,
            Format::Table,
            "flock",
            vec![sheep_info("web")],
            None,
            Presentation::BARE,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("Dogs"));
    }

    fn usage(cpu: f32) -> HostUsage {
        HostUsage {
            cpu_percent: Some(cpu),
            memory_used_bytes: 4,
            memory_total_bytes: 8,
            disk_bytes_per_second: None,
            network_bytes_per_second: None,
        }
    }

    fn listing(
        host: Option<&str>,
        names: &[&str],
        usage: Option<Option<HostUsage>>,
    ) -> HostListing {
        HostListing {
            host: host.map(str::to_owned),
            procs: names.iter().map(|name| sheep_info(name)).collect(),
            usage,
        }
    }

    fn fleet_json(listings: Vec<HostListing>) -> serde_json::Value {
        let mut out = Vec::new();
        emit_fleet_flock(&mut out, "flock", listings).unwrap();
        serde_json::from_slice(&out).unwrap()
    }

    #[test]
    fn origin_is_null_for_a_local_row_and_the_entry_as_written_for_a_remote_one() {
        let json = fleet_json(vec![
            listing(None, &["web"], None),
            listing(Some("Deploy@AMP2"), &["api"], None),
        ]);

        assert_eq!(json["data"][0]["name"], "web");
        assert_eq!(json["data"][0]["origin"], serde_json::Value::Null);
        assert!(json["data"][0].as_object().unwrap().contains_key("origin"));
        assert_eq!(json["data"][1]["name"], "api");
        assert_eq!(json["data"][1]["origin"], "Deploy@AMP2");
    }

    #[test]
    fn schema_version_stays_one_and_the_local_envelope_gains_no_key() {
        let mut out = Vec::new();
        emit_flock(
            &mut out,
            Format::Json,
            "flock",
            vec![sheep_info("web")],
            Some(Some(usage(1.0))),
            Presentation::BARE,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();

        assert_eq!(json["schema_version"], 1);
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["command", "data", "host", "schema_version"]);
        assert!(!json["data"][0].as_object().unwrap().contains_key("origin"));
    }

    #[test]
    fn a_failed_host_has_no_rows_and_no_hosts_entry() {
        // A host that failed never reaches `emit_fleet_flock`: the caller
        // lists only the shepherds that answered.
        let json = fleet_json(vec![
            listing(None, &["web"], None),
            listing(Some("amp2"), &["api"], Some(Some(usage(2.0)))),
        ]);

        let rows: Vec<&str> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["origin"].as_str().unwrap_or("local"))
            .collect();
        assert_eq!(rows, ["local", "amp2"]);
        let hosts: Vec<&str> = json["hosts"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(hosts, ["amp2"], "amp1 failed and is nowhere");
    }

    #[test]
    fn hosts_distinguishes_a_reading_from_an_unreadable_platform_from_a_shepherd_that_would_not_answer()
     {
        let json = fleet_json(vec![
            listing(Some("reads"), &[], Some(Some(usage(3.0)))),
            listing(Some("unreadable"), &[], Some(None)),
            listing(Some("silent"), &[], None),
        ]);

        let hosts = json["hosts"].as_object().unwrap();
        assert_eq!(hosts["reads"]["memory_total_bytes"], 8);
        assert_eq!(hosts["unreadable"], serde_json::Value::Null);
        assert!(hosts.contains_key("unreadable"));
        assert!(!hosts.contains_key("silent"));
    }

    #[test]
    fn rows_are_sorted_within_each_host_and_local_leads() {
        let json = fleet_json(vec![
            listing(None, &["web", "db"], None),
            listing(Some("amp2"), &["zed", "api"], None),
        ]);
        let names: Vec<&str> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["db", "web", "api", "zed"],
            "list order, each host sorted"
        );
    }

    #[test]
    fn a_single_host_listing_carries_origin_and_keeps_host_without_hosts() {
        let mut out = Vec::new();
        emit_origin_flock(
            &mut out,
            "flock",
            "amp2",
            &[sheep_info("web")],
            Some(Some(usage(1.0))),
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(json["data"][0]["origin"], "amp2");
        assert_eq!(json["host"]["memory_total_bytes"], 8);
        assert!(!json.as_object().unwrap().contains_key("hosts"));
    }
}
