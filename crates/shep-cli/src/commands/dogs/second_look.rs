//! What `shep enable` asks of a dog that already holds grants: the ones
//! written at upgrade that nobody has looked at, and the ones a newer build
//! is waiting on.
//!
//! The answer only ever widens. A no, or no terminal to ask on, leaves the
//! dog on what it holds and the question for the next enable.

use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;

use crate::commands::shep_toml::{GrantRecord, ShepToml};
use crate::exit::ExitCode;
use crate::output::Streams;

use super::consent::{Input, Prompt, not_confined};
use super::fail_config;
use super::upgrade::upgrade_one;
use crate::commands::copy::confirm::Answer;

/// [`emit_notice`](crate::output::emit_notice) code for grants left as they
/// were because no one could be asked; not a failure.
const UNREVIEWED_NOTICE: &str = "dog_grants_unreviewed";

/// [`emit_notice`](crate::output::emit_notice) code for the grants a dog
/// holds and the ones it waits on; not a failure.
const GRANTS_NOTICE: &str = "dog_grants";

/// Brings `name`'s record up to its build, then puts what is unreviewed or
/// waiting to the operator.
///
/// `yes` approves without asking. With no terminal and no `--yes` the dog
/// carries on with what it holds and a notice says so.
///
/// The answer is written only if the record is still the one that was shown,
/// so a grant or revoke made meanwhile is never overwritten.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`: `shep.toml` could not be
/// read or written, the answer could not be read, or the record changed
/// while the question was open.
pub(super) fn second_look(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    yes: bool,
    input: &mut Input,
) -> Result<(), ExitCode> {
    upgrade_one(paths, name).map_err(|err| fail_config(streams, &err))?;
    let record = ShepToml::read_only(&paths.daemon_config)
        .and_then(|cfg| cfg.grant_record(name))
        .map_err(|err| fail_config(streams, &err))?;
    let Some(record) = record else {
        return Ok(());
    };
    if record.waiting.names().next().is_none() && record.reviewed {
        return Ok(());
    }
    let listing = listing(name, &record);
    if !yes && !input.is_terminal() {
        streams.aside(
            UNREVIEWED_NOTICE,
            &format!(
                "{listing}\nleft as they are: there is no terminal to ask on. Run `shep enable \
                 {name} --yes` to approve them"
            ),
        );
        return Ok(());
    }
    let shown = format!("{listing}\n{}", not_confined(name));
    streams.aside(GRANTS_NOTICE, &shown);
    let approved = GrantRecord {
        granted: Grants::from_names(
            record
                .granted
                .names()
                .chain(record.waiting.names())
                .map(str::to_owned),
        ),
        waiting: Grants::default(),
        reviewed: true,
        ..record.clone()
    };
    if !yes {
        let question = if record.waiting.names().next().is_some() {
            format!("grant {name} the new permissions too?")
        } else {
            format!("keep these grants for {name}?")
        };
        let prompt = Prompt::new(shown, question, Some(approved.granted.clone()));
        match input.ask(streams, &prompt) {
            Answer::Yes => {}
            Answer::No => return Ok(()),
            Answer::Failed(reason) => {
                return Err(streams.fail(
                    ExitCode::Failure,
                    &format!(
                        "no permission was approved: the question could not be read ({reason})"
                    ),
                ));
            }
        }
    }
    let written = ShepToml::try_edit(&paths.daemon_config, |cfg| {
        if cfg.grant_record(name)?.as_ref() != Some(&record) {
            return Ok(false);
        }
        cfg.set_grant_record(name, &approved).map(|()| true)
    })
    .map_err(|err| fail_config(streams, &err))?;
    if written {
        return Ok(());
    }
    Err(streams.fail(
        ExitCode::Failure,
        &format!(
            "no permission was approved: the grants for {name} changed while the question was \
             open. Run `shep enable {name}` again to see them"
        ),
    ))
}

/// What `name` holds, and what it waits on, as the question shows them.
fn listing(name: &str, record: &GrantRecord) -> String {
    let names = |grants: &Grants| grants.names().collect::<Vec<_>>().join(", ");
    let mut lines = Vec::new();
    if record.granted.names().next().is_some() {
        let mark = if record.reviewed {
            ""
        } else {
            " (granted at upgrade, not reviewed)"
        };
        lines.push(format!("{name} holds: {}{mark}", names(&record.granted)));
    } else if !record.reviewed {
        lines.push(format!(
            "{name} holds no grants (granted at upgrade, not reviewed)"
        ));
    }
    if record.waiting.names().next().is_some() {
        lines.push(format!(
            "{name}'s build now also asks for, and waits on: {}",
            names(&record.waiting)
        ));
    }
    lines.join("\n")
}
