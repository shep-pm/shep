//! The part of a write both verbs share: print the change list, ask, check the
//! holder did not move, send, and report.

use std::io::IsTerminal as _;

use serde_json::Value as Json;
use shep_client::Client;
use shep_core::protocol::{DogSectionToml, Request, Response};

use super::plan::schema_refusal;
use super::{Flags, Plan, Verb, Write, WriteRefusal};
use crate::cli::Format;
use crate::commands::copy;
use crate::commands::copy::address;
use crate::commands::copy::confirm::{self, Answer, Go};
use crate::commands::copy::report::shows_changes_first;
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::settings_read::{self, Holder};
use crate::exit::ExitCode;
use crate::output::{CopyChange, SetReport, Streams, emit, write_outcome};

/// Prints the change list, asks, writes, and reports what the write left
/// waiting.
pub(super) async fn run(
    client: &Client,
    streams: &mut Streams<'_>,
    holder: Holder,
    name: &str,
    path: &str,
    plan: &Plan,
    flags: Flags,
) -> ExitCode {
    let verb = flags.verb;
    if let Some(refusal) = schema_refusal(plan, name, path) {
        return refusal.report(streams);
    }
    let mut report = report_for(holder, name, path, plan, false, flags.dry_run);
    if plan.changes.is_empty() {
        let reason = match verb {
            Verb::Set => "already holds that value",
            Verb::Unset => "holds nothing to remove",
        };
        announce(
            streams,
            verb.as_str(),
            &format!("nothing to write: `{}.{}` {reason}", name, report.path),
        );
        return finish(streams, verb, &report);
    }
    let go = confirm::decide(
        flags.dry_run,
        flags.yes,
        std::io::stdin().is_terminal(),
        !plan.changes.is_empty(),
    );

    // The change list prints before anything is asked about it, in every
    // form. Where it prints depends on what that form's stdout carries: the
    // table form's stdout is the change list, and the JSON form's is one
    // envelope that can only be written after the write the question gates.
    if shows_changes_first(streams.fmt, go)
        && let Err(code) = print_changes(streams, verb, &report)
    {
        return code;
    }

    if go == Go::Refuse {
        return streams.fail(
            ExitCode::Usage,
            &format!(
                "nothing was written: `shep {verb}` asks before it writes, and this run has no \
                 terminal to ask on. Pass --yes to write without asking, or --dry-run to see the \
                 changes and write nothing",
                verb = verb.as_str()
            ),
        );
    }
    if go == Go::Ask {
        let mut reader = std::io::stdin().lock();
        match ask(
            streams,
            verb,
            name,
            &report.path,
            plan.changes.len(),
            &mut reader,
        ) {
            Answer::Yes => {}
            Answer::No => {
                announce(streams, verb.as_str(), "nothing was written");
                return finish(streams, verb, &report);
            }
            Answer::Failed(reason) => {
                return streams.fail(
                    ExitCode::Failure,
                    &format!("nothing was written: the question could not be read ({reason})"),
                );
            }
        }
    }
    if go == Go::DryRun {
        announce(streams, "dry_run", "nothing was written");
        return finish(streams, verb, &report);
    }

    // The answer was given, and the holder may have moved while it was being
    // given. A whole-section or whole-table write carries every key it read, so
    // a stale plan overwrites what someone else wrote in the meantime.
    match holder_unchanged(client, streams, name, &plan.write).await {
        Ok(true) => {}
        Ok(false) => {
            return WriteRefusal::Stale(holder_moved(verb, name, &report.path, &plan.write))
                .report(streams);
        }
        Err(code) => return code,
    }

    let pending = match send(client, streams, name, &plan.write).await {
        Ok(pending) => pending,
        Err(code) => return code,
    };
    report = report_for(holder, name, path, plan, true, flags.dry_run);
    report.pending = pending;
    if !report.pending.is_empty() {
        announce(
            streams,
            "parked",
            &format!(
                "{} waiting on the next spawn (`shep reload {name}` promotes them)",
                report.pending.join(", ")
            ),
        );
    }
    announce(
        streams,
        "written",
        &format!(
            "wrote {} change{} onto `{}`",
            plan.changes.len(),
            if plan.changes.len() == 1 { "" } else { "s" },
            name
        ),
    );
    finish(streams, verb, &report)
}

/// Prints a notice, in the form that has room for one.
///
/// The table form's stdout is prose lines beside the change list. The JSON
/// form's stdout is the one envelope this invocation owes, and a notice printed
/// in it is a second object on the same stream, which is what a consumer
/// piping into `jq` chokes on. Everything a notice would have said is a field
/// of that envelope: `written`, `dry_run` and `pending`.
fn announce(streams: &mut Streams<'_>, code: &str, message: &str) {
    if streams.fmt == Format::Table {
        streams.note(code, message);
    }
}

/// Asks the operator, on stderr, beside the change list the question is about.
pub(super) fn ask(
    streams: &mut Streams<'_>,
    verb: Verb,
    name: &str,
    path: &str,
    changes: usize,
    reader: &mut dyn std::io::BufRead,
) -> Answer {
    let question = if changes == 1 {
        format!("{} `{}.{}`? [y/N] ", verb.as_str(), name, path)
    } else {
        format!(
            "{} {changes} keys under `{}.{}`? [y/N] ",
            verb.as_str(),
            name,
            path
        )
    };
    streams.aside("confirm", &question);
    confirm::read_answer(reader)
}

/// Sends the plan's write, and reports which keys wait for a respawn.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
async fn send(
    client: &Client,
    streams: &mut Streams<'_>,
    name: &str,
    write: &Write,
) -> Result<Vec<String>, ExitCode> {
    let request = match write {
        Write::SheepField { key, value } => Request::SetSheepField {
            name: name.to_string(),
            key: key.clone(),
            value: value.clone(),
        },
        Write::SheepDogTable { dog, table, .. } => Request::SetSheepDogSettings {
            name: name.to_string(),
            dog: dog.clone(),
            table: Some(table.clone()),
        },
        Write::DropSheepDogTable { dog, .. } => Request::SetSheepDogSettings {
            name: name.to_string(),
            dog: dog.clone(),
            table: None,
        },
        Write::ResetSheepField { key } => Request::ResetSheepField {
            name: name.to_string(),
            key: key.clone(),
        },
        Write::DogSection { toml, .. } => Request::SetDogConfig {
            name: name.to_string(),
            toml: DogSectionToml::from(toml.clone()),
        },
    };
    let pending_key = match write {
        Write::SheepField { key, .. } | Write::ResetSheepField { key } => key.clone(),
        _ => String::new(),
    };
    match client.request(request).await {
        Ok(Response::SheepFieldSet {
            pending, warning, ..
        }) => {
            if let Some(warning) = warning {
                streams.aside("warning", &warning);
            }
            Ok(pending.then_some(pending_key).into_iter().collect())
        }
        Ok(Response::SheepFieldReset {
            key,
            value,
            pending,
            warning,
            ..
        }) => {
            if let Some(warning) = warning {
                streams.aside("warning", &warning);
            }
            announce(
                streams,
                "reset",
                &format!("`{name}.{key}` holds {value} again"),
            );
            Ok(pending.then_some(pending_key).into_iter().collect())
        }
        Ok(Response::SheepDogSettingsSet { .. }) | Ok(Response::DogConfigSet { .. }) => {
            Ok(Vec::new())
        }
        Ok(_other) => Err(unexpected_response(streams)),
        Err(err) => Err(client_error(streams, &err)),
    }
}

/// The check that the holder still holds what the plan was built from.
///
/// [`Request::SetDogConfig`] replaces a whole section and
/// [`Request::SetSheepDogSettings`] replaces a whole table, so what this sends
/// carries every key it read, not only the one the operator named. Between the
/// read and the answer the lookout config pane can file a write of its own on a
/// different key of the same holder, and the text read before the question would
/// overwrite it with no warning. A sheep's own field is the one write of one key
/// rather than of a whole holder, so it has nothing to re-read, and neither does
/// dropping its override.
///
/// The comparison is on the whole holder rather than on the key the operator
/// named: a holder that moved anywhere is one this plan no longer describes, and
/// re-planning against it here would write a change the operator never saw the
/// old value of.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
pub(super) async fn holder_unchanged(
    client: &Client,
    streams: &mut Streams<'_>,
    name: &str,
    write: &Write,
) -> Result<bool, ExitCode> {
    Ok(match write {
        Write::SheepField { .. } | Write::ResetSheepField { .. } => true,
        Write::DogSection { before, .. } => {
            match settings_read::read_dog_section(client, streams, name).await {
                Ok(now) => !section_moved(before, &now),
                Err(code) => return Err(code),
            }
        }
        Write::SheepDogTable { dog, before, .. } | Write::DropSheepDogTable { dog, before } => {
            let config = match copy::read_config(client, streams, name).await {
                Ok(config) => config,
                Err(code) => return Err(code),
            };
            let now = address::dog_table(&config, dog)
                .and_then(Json::as_object)
                .cloned()
                .unwrap_or_default();
            !table_moved(before, &Json::Object(now))
        }
    })
}

/// `true` when a dog's section is no longer the text this plan was built from.
///
/// The compare ignores the blank line a section may or may not end with: the
/// text is what the shepherd answered both times, and a difference in nothing
/// but trailing whitespace is not a change an operator made.
pub(super) fn section_moved(before: &str, now: &str) -> bool {
    before.trim() != now.trim()
}

/// `true` when a sheep's table for a dog is no longer the table this plan was
/// built from.
pub(super) fn table_moved(before: &Json, now: &Json) -> bool {
    before != now
}

/// The sentence for a holder that moved between the read and the write.
///
/// The remedy is to read again, because the change list the operator answered
/// describes the holder this run read, and nothing here can show them the old
/// value of a key that moved in between.
pub(super) fn holder_moved(verb: Verb, name: &str, path: &str, write: &Write) -> String {
    let address = if path.is_empty() {
        name.to_string()
    } else {
        format!("{name}.{path}")
    };
    let head = match write {
        Write::DogSection { .. } => format!("`{name}`'s section"),
        Write::SheepDogTable { dog, .. } | Write::DropSheepDogTable { dog, .. } => {
            format!("`{name}`'s table for `{dog}`")
        }
        // A sheep's own field is one key and no whole holder, so this sentence
        // is never built for one.
        Write::SheepField { .. } | Write::ResetSheepField { .. } => format!("`{name}`'s field"),
    };
    let verb = verb.as_str();
    format!(
        "nothing was written: {head} changed while `shep {verb}` waited for the answer. `shep \
         get {address}` reads what it holds now, and `shep {verb}` again plans against that"
    )
}

/// The report a plan renders, before or after the write.
pub(super) fn report_for(
    holder: Holder,
    name: &str,
    path: &str,
    plan: &Plan,
    written: bool,
    dry_run: bool,
) -> SetReport {
    SetReport {
        kind: holder.as_str(),
        name: name.to_string(),
        path: path.to_string(),
        written,
        dry_run,
        pending: Vec::new(),
        changes: plan
            .changes
            .iter()
            .map(|change| CopyChange {
                key: change.key.clone(),
                change: change.kind.as_str(),
                old: change.old.clone(),
                new: change.new.clone(),
            })
            .collect(),
    }
}

/// Prints the change list on the stream the operator is reading.
///
/// The table form prints it on stdout. A form whose stdout is a JSON envelope
/// prints it on stderr, so the operator sees what they are being asked to
/// approve while stdout stays the one envelope the `--format json` rule keeps.
///
/// # Errors
/// [`ExitCode::Failure`] if the write itself failed.
fn print_changes(
    streams: &mut Streams<'_>,
    verb: Verb,
    report: &SetReport,
) -> Result<(), ExitCode> {
    let writer: &mut dyn std::io::Write = if streams.fmt == Format::Table {
        &mut *streams.out
    } else {
        &mut *streams.err
    };
    emit(
        writer,
        Format::Table,
        verb.as_str(),
        report.clone(),
        streams.style,
    )
    .map_err(|_| ExitCode::Failure)
}

/// Prints the run's verdict.
///
/// The table form already printed the change list, so this adds nothing to it.
/// The JSON form prints the one envelope this invocation owes, with `written`,
/// `pending` and `dry_run` filled in by what actually happened: a parked key is
/// a field of that envelope rather than a second object on stdout.
fn finish(streams: &mut Streams<'_>, verb: Verb, report: &SetReport) -> ExitCode {
    if streams.fmt != Format::Json {
        return ExitCode::Success;
    }
    write_outcome(emit(
        &mut *streams.out,
        Format::Json,
        verb.as_str(),
        report.clone(),
        streams.style,
    ))
}
