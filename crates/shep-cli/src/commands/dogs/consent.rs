//! What `adopt` and the first `enable` of a dog ask the operator: the
//! permissions its manifest declares, each with its reason, and whether to
//! grant them.
//!
//! The question is the operator's whole say. A yes grants what the manifest
//! declares and the shepherd's grants never exceed it; a no leaves nothing
//! granted. Grants keep a dog's requests in bounds and do not confine it, and
//! the prompt says so.

use std::io::{BufRead, IsTerminal as _};
use std::str::FromStr as _;

use shep_core::dogs::manifest::Manifest;
use shep_core::permissions::{Grants, Permission};

use crate::commands::copy::confirm::{Answer, Go, decide, read_answer};
use crate::exit::ExitCode;
use crate::output::Streams;

/// [`emit_notice`](crate::output::emit_notice) code for the list a dog's
/// declared permissions are shown in; not a failure.
const PERMISSIONS_NOTICE: &str = "dog_permissions";

/// [`emit_notice`](crate::output::emit_notice) code for an optional
/// permission this shep dropped; not a failure.
const PERMISSION_DROPPED_NOTICE: &str = "dog_permission_dropped";

/// What the operator typed on the command line, and the terminal they typed
/// it at.
#[derive(Debug, Clone, Copy)]
pub(super) struct Mode {
    /// `--yes`: grant everything declared without asking.
    pub(super) yes: bool,
    /// Whether stdin is a terminal to ask on.
    pub(super) stdin_is_terminal: bool,
}

/// One permission a dog declared that this shep knows.
#[derive(Debug)]
pub(super) struct Listed {
    name: String,
    required: bool,
    reason: String,
}

/// What a dog's manifest comes to once read by this shep.
#[derive(Debug)]
pub(super) enum Review {
    /// No manifest: the dog declares nothing and keeps full access.
    Undeclared,
    /// A manifest, and the permissions in it this shep knows.
    Declared {
        listed: Vec<Listed>,
        /// Optional permissions this shep does not know, dropped.
        dropped: Vec<String>,
    },
}

/// A dog that requires permissions this shep does not know.
#[derive(Debug)]
pub(super) struct UnknownRequired(Vec<String>);

impl UnknownRequired {
    /// Renders the refusal. `done` is the verb in the past tense, `adopted`
    /// or `enabled`.
    pub(super) fn report(&self, streams: &mut Streams<'_>, name: &str, done: &str) -> ExitCode {
        let names = self
            .0
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "{name} requires {names}, which this shep does not know, so it was not {done}: \
             it would run half-working. Upgrade shep, or use a version of {name} that does \
             not require it"
        );
        streams.fail(ExitCode::InvalidConfig, &message)
    }
}

/// What the operator's answer comes to.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Go ahead. `Some` is the list to record in `shep.toml`; `None` is a
    /// dog that declared nothing, which has nothing to record and keeps full
    /// access.
    Grant(Option<Grants>),
    /// The operator said no. Nothing is granted.
    Declined,
}

/// Reads `manifest` the way this shep can.
///
/// An optional permission this shep does not know is set aside, to be
/// warned of; it is never granted.
///
/// # Errors
/// [`UnknownRequired`] when the dog requires a permission this shep does not
/// know.
pub(super) fn review(manifest: Option<&Manifest>) -> Result<Review, UnknownRequired> {
    let Some(manifest) = manifest else {
        return Ok(Review::Undeclared);
    };
    let mut listed: Vec<Listed> = Vec::new();
    let mut dropped = Vec::new();
    let mut unknown_required = Vec::new();
    for declared in &manifest.permissions {
        match (Permission::from_str(&declared.name), declared.required) {
            (Ok(_), _) if listed.iter().any(|seen| seen.name == declared.name) => {}
            (Ok(_), required) => listed.push(Listed {
                name: declared.name.clone(),
                required,
                reason: declared.reason.clone(),
            }),
            (Err(_), true) => unknown_required.push(declared.name.clone()),
            (Err(_), false) => dropped.push(declared.name.clone()),
        }
    }
    if unknown_required.is_empty() {
        Ok(Review::Declared { listed, dropped })
    } else {
        Err(UnknownRequired(unknown_required))
    }
}

/// Shows the operator what `name` declares and asks, or applies `--yes`, or
/// refuses for want of a terminal.
///
/// `verb` is what the operator ran, `adopt` or `enable`. The question goes to
/// stderr with the list, so a piped stdout stays what it was. A dog whose
/// manifest declares no permissions is granted nothing without a question:
/// there is nothing to decide.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`: [`ExitCode::Usage`] with
/// no terminal and no `--yes`, [`ExitCode::Failure`] when the answer could
/// not be read.
pub(super) fn consent(
    streams: &mut Streams<'_>,
    name: &str,
    verb: &str,
    review: &Review,
    mode: Mode,
    reader: &mut dyn BufRead,
) -> Result<Verdict, ExitCode> {
    let (granted, listing, question) = match review {
        Review::Undeclared => (
            None,
            format!(
                "{name} declares nothing, runs with full access. {}",
                not_confined(name)
            ),
            format!("{verb} {name} with full access? [y/N] "),
        ),
        Review::Declared { listed, dropped } => {
            for dropped in dropped {
                streams.aside(
                    PERMISSION_DROPPED_NOTICE,
                    &format!(
                        "{name} asks for the optional permission `{dropped}`, which this shep \
                         does not know; it is dropped and not granted"
                    ),
                );
            }
            let grants = Grants::from_names(listed.iter().map(|entry| entry.name.clone()));
            if listed.is_empty() {
                return Ok(Verdict::Grant(Some(grants)));
            }
            let lines = listed
                .iter()
                .map(|entry| {
                    let kind = if entry.required {
                        "required"
                    } else {
                        "optional"
                    };
                    format!("  {} ({kind}): {}", entry.name, entry.reason)
                })
                .collect::<Vec<_>>()
                .join("\n");
            (
                Some(grants),
                format!(
                    "{name} declares these permissions:\n{lines}\n{}",
                    not_confined(name)
                ),
                format!("{verb} {name} and grant it these? [y/N] "),
            )
        }
    };
    let go = decide(false, mode.yes, mode.stdin_is_terminal, true);
    if go == Go::Refuse {
        return Err(streams.fail(
            ExitCode::Usage,
            &format!(
                "nothing was changed: `shep {verb}` asks before it grants a dog its \
                 permissions, and this run has no terminal to ask on. Pass --yes to grant \
                 everything {name} declares"
            ),
        ));
    }
    streams.aside(PERMISSIONS_NOTICE, &listing);
    if go == Go::Ask {
        streams.aside("confirm", &question);
        match read_answer(reader) {
            Answer::Yes => {}
            Answer::No => return Ok(Verdict::Declined),
            Answer::Failed(reason) => {
                return Err(streams.fail(
                    ExitCode::Failure,
                    &format!("nothing was changed: the question could not be read ({reason})"),
                ));
            }
        }
    }
    Ok(Verdict::Grant(granted))
}

/// Where the answer is read from: a terminal's stdin in production, a canned
/// reader in a test.
pub(super) struct Input {
    /// Whether the reader is a terminal there is someone at.
    pub(super) is_terminal: bool,
    /// The line the operator types.
    pub(super) reader: Box<dyn BufRead + Send>,
}

impl Input {
    /// This process's stdin.
    pub(super) fn stdin() -> Self {
        Self {
            is_terminal: std::io::stdin().is_terminal(),
            reader: Box::new(std::io::BufReader::new(std::io::stdin())),
        }
    }
}

/// [`consent`], asking at `input` with `--yes` as `yes`.
///
/// # Errors
/// As [`consent`].
pub(super) fn consent_from(
    streams: &mut Streams<'_>,
    name: &str,
    verb: &str,
    review: &Review,
    yes: bool,
    input: &mut Input,
) -> Result<Verdict, ExitCode> {
    let mode = Mode {
        yes,
        stdin_is_terminal: input.is_terminal,
    };
    consent(streams, name, verb, review, mode, &mut input.reader)
}

/// The sentence every prompt carries.
fn not_confined(name: &str) -> String {
    format!(
        "Grants keep {name}'s requests in bounds and do not confine it: it runs as you and \
         can do anything you can."
    )
}
