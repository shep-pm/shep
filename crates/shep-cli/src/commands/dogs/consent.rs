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

use crate::commands::copy::confirm::{Answer, Context, Go, decide, read_answer};
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
pub(crate) struct Listed {
    pub(super) name: String,
    pub(super) required: bool,
    pub(super) reason: String,
}

/// What a dog's manifest comes to once read by this shep.
#[derive(Debug)]
pub(crate) enum Review {
    /// No manifest: the dog declares nothing and keeps full access.
    Undeclared,
    /// A manifest, and the permissions in it this shep knows.
    Declared {
        listed: Vec<Listed>,
        /// Optional permissions this shep does not know, dropped.
        dropped: Vec<String>,
        /// The event kinds the manifest declares it publishes. Not asked
        /// about: they are not permissions, and the dog may publish only
        /// these, under its own name.
        events: Vec<String>,
    },
}

impl Review {
    /// The event kinds the manifest declares; none for an undeclared dog.
    pub(super) fn events(&self) -> Vec<String> {
        match self {
            Self::Undeclared => Vec::new(),
            Self::Declared { events, .. } => events.clone(),
        }
    }
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
        let events = manifest
            .events
            .iter()
            .map(|event| event.kind.clone())
            .collect();
        Ok(Review::Declared {
            listed,
            dropped,
            events,
        })
    } else {
        Err(UnknownRequired(unknown_required))
    }
}

/// A declared review of `entries`, each a permission name, whether it is
/// required, and its reason, as `review` would list them from a manifest.
/// For tests outside this directory, which cannot build a [`Listed`].
#[cfg(test)]
pub(crate) fn declared_review(entries: &[(&str, bool, &str)]) -> Review {
    Review::Declared {
        listed: entries
            .iter()
            .map(|&(name, required, reason)| Listed {
                name: name.to_owned(),
                required,
                reason: reason.to_owned(),
            })
            .collect(),
        dropped: Vec::new(),
        events: Vec::new(),
    }
}

/// What the operator is shown and asked before a dog is granted anything.
///
/// Built apart from reading the answer, so the CLI and lookout's modal show
/// the same words. The text comes from a manifest the operator is about to
/// review, so it is not secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prompt {
    listing: String,
    question: String,
    grants: Option<Grants>,
}

impl Prompt {
    /// A prompt from text already built.
    pub(super) fn new(listing: String, question: String, grants: Option<Grants>) -> Self {
        Self {
            listing,
            question,
            grants,
        }
    }

    /// What `review` asks the operator, or `None` when nothing is asked: a
    /// dog that declares no permissions.
    ///
    /// `verb` is what the operator ran. `note` ends the listing, directly
    /// above the question.
    pub(crate) fn for_review(
        name: &str,
        verb: &str,
        review: &Review,
        note: Option<&str>,
    ) -> Option<Self> {
        let (grants, listing, question) = match review {
            Review::Undeclared => (
                None,
                format!(
                    "{name} declares nothing, runs with full access. {}",
                    not_confined(name)
                ),
                format!("{verb} {name} with full access?"),
            ),
            Review::Declared { listed, .. } if listed.is_empty() => return None,
            Review::Declared { listed, .. } => {
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
                    Some(Grants::from_names(
                        listed.iter().map(|entry| entry.name.clone()),
                    )),
                    format!(
                        "{name} declares these permissions:\n{lines}\n{}",
                        not_confined(name)
                    ),
                    format!("{verb} {name} and grant it these?"),
                )
            }
        };
        let listing = match note {
            Some(note) => format!("{listing}\n{note}"),
            None => listing,
        };
        Some(Self::new(listing, question, grants))
    }

    /// The permissions with their reasons, then the note if there is one.
    pub(crate) fn listing(&self) -> &str {
        &self.listing
    }

    /// The question, without the `[y/N] ` the CLI appends.
    pub(crate) fn question(&self) -> &str {
        &self.question
    }

    /// What a yes grants; `None` for a dog that declared nothing.
    pub(crate) fn grants(&self) -> Option<&Grants> {
        self.grants.as_ref()
    }
}

/// Shows the operator what `name` declares and asks, or applies `--yes`, or
/// refuses for want of a terminal.
///
/// `verb` is what the operator ran. The question goes to stderr with the
/// list. A dog declaring no permissions is granted nothing without a question.
/// `note` ends the listing, directly above the question, and is printed alone
/// when no question is asked.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`: [`ExitCode::Usage`] with
/// no terminal and no `--yes`, [`ExitCode::Failure`] when the answer could
/// not be read.
#[cfg(test)]
pub(super) fn consent(
    streams: &mut Streams<'_>,
    name: &str,
    verb: &str,
    review: &Review,
    note: Option<&str>,
    mode: Mode,
    reader: &mut dyn BufRead,
) -> Result<Verdict, ExitCode> {
    consent_asking(
        streams,
        name,
        verb,
        review,
        note,
        mode,
        &mut |streams, prompt| ask_aloud(streams, prompt, reader),
    )
}

/// Prints `prompt`'s question with the `[y/N] ` suffix, then reads a line.
fn ask_aloud(streams: &mut Streams<'_>, prompt: &Prompt, reader: &mut dyn BufRead) -> Answer {
    streams.aside("confirm", &format!("{} [y/N] ", prompt.question()));
    read_answer(reader)
}

/// Shows `name`'s listing and gets the answer from `ask`, as `consent` does
/// for a reader.
fn consent_asking(
    streams: &mut Streams<'_>,
    name: &str,
    verb: &str,
    review: &Review,
    note: Option<&str>,
    mode: Mode,
    ask: &mut dyn FnMut(&mut Streams<'_>, &Prompt) -> Answer,
) -> Result<Verdict, ExitCode> {
    if let Review::Declared { dropped, .. } = review {
        for dropped in dropped {
            streams.aside(
                PERMISSION_DROPPED_NOTICE,
                &format!(
                    "{name} asks for the optional permission `{dropped}`, which this shep \
                     does not know; it is dropped and not granted"
                ),
            );
        }
    }
    let Some(prompt) = Prompt::for_review(name, verb, review, note) else {
        if let Some(note) = note {
            streams.aside(PERMISSIONS_NOTICE, note);
        }
        return Ok(Verdict::Grant(Some(Grants::default())));
    };
    let go = decide(Context {
        dry_run: false,
        yes: mode.yes,
        stdin_is_terminal: mode.stdin_is_terminal,
        has_changes: true,
    });
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
    streams.aside(PERMISSIONS_NOTICE, prompt.listing());
    if go == Go::Ask {
        match ask(streams, &prompt) {
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
    Ok(Verdict::Grant(prompt.grants().cloned()))
}

/// Where the answer comes from: a terminal's stdin in production, a canned
/// reader in a test, or a callback a modal answers through.
pub(crate) struct Input(Source);

enum Source {
    Reading {
        is_terminal: bool,
        reader: Box<dyn BufRead + Send>,
    },
    Answering(Box<dyn FnMut(&Prompt) -> bool + Send>),
}

impl Input {
    /// This process's stdin.
    pub(super) fn stdin() -> Self {
        Self::reading(
            std::io::stdin().is_terminal(),
            std::io::BufReader::new(std::io::stdin()),
        )
    }

    /// Answers read as a line from `reader`; `is_terminal` is whether there
    /// is someone at it.
    pub(super) fn reading(is_terminal: bool, reader: impl BufRead + Send + 'static) -> Self {
        Self(Source::Reading {
            is_terminal,
            reader: Box::new(reader),
        })
    }

    /// Answers by calling `answer` with the prompt: true is a yes, false a
    /// no. `answer` blocks, so a caller on an async runtime runs the install
    /// on a blocking thread.
    pub(crate) fn answering(answer: impl FnMut(&Prompt) -> bool + Send + 'static) -> Self {
        Self(Source::Answering(Box::new(answer)))
    }

    /// Whether there is someone to ask.
    pub(super) fn is_terminal(&self) -> bool {
        match &self.0 {
            Source::Reading { is_terminal, .. } => *is_terminal,
            Source::Answering(_) => true,
        }
    }

    /// Asks `prompt`'s question. A reader is shown it on `streams` first; a
    /// callback is handed the prompt and shows it itself.
    pub(super) fn ask(&mut self, streams: &mut Streams<'_>, prompt: &Prompt) -> Answer {
        match &mut self.0 {
            Source::Reading { reader, .. } => ask_aloud(streams, prompt, reader),
            Source::Answering(answer) => {
                if answer(prompt) {
                    Answer::Yes
                } else {
                    Answer::No
                }
            }
        }
    }
}

/// [`consent_asking`], asking at `input` with `--yes` as `yes`.
///
/// # Errors
/// As [`consent_asking`].
pub(super) fn consent_from(
    streams: &mut Streams<'_>,
    name: &str,
    verb: &str,
    review: &Review,
    note: Option<&str>,
    yes: bool,
    input: &mut Input,
) -> Result<Verdict, ExitCode> {
    let mode = Mode {
        yes,
        stdin_is_terminal: input.is_terminal(),
    };
    consent_asking(
        streams,
        name,
        verb,
        review,
        note,
        mode,
        &mut |streams, prompt| input.ask(streams, prompt),
    )
}

/// The sentence every prompt carries.
pub(super) fn not_confined(name: &str) -> String {
    format!(
        "Grants keep {name}'s requests in bounds and do not confine it: it runs as you and \
         can do anything you can."
    )
}
