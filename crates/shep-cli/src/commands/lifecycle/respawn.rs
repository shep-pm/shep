//! Brings stopped sheep back up without touching live ones.
//!
//! The resume/dedupe layer `start::load` calls after registration.

use shep_client::Client;
use shep_core::protocol::{ProcessInfo, Request, Response, SelectorSpec};

use crate::commands::lifecycle::any_restart_failed;
use crate::commands::lifecycle::selectors::request_each;
use crate::exit::ExitCode;
use crate::output::Streams;

/// Whether `info` names a sheep `start` must leave alone rather than bring up
fn is_live(info: &ProcessInfo) -> bool {
    use shep_core::status::ProcStatus;
    matches!(
        info.status,
        ProcStatus::Online | ProcStatus::Starting | ProcStatus::Stopping
    )
}

/// Brings every sheep in `matched` up, reporting the ones already up
///
/// `selector` is the operator's own token, `None` when the match came from
/// a path or Flockfile. Only a token can be quoted back as a remedy.
///
/// Respawns go out per row, by id: a name selector reaches every instance
/// the name has.
pub(crate) async fn resume_all(
    client: &Client,
    streams: &mut Streams<'_>,
    selector: Option<&str>,
    matched: &[ProcessInfo],
    started: &mut Vec<ProcessInfo>,
) -> ExitCode {
    let (live, asleep): (Vec<&ProcessInfo>, Vec<&ProcessInfo>) =
        matched.iter().partition(|info| is_live(info));

    match live.as_slice() {
        [] => {}
        [one] => {
            // The operator's own token, not the sheep's name: `shep restart
            // zam` for a `shep start 0` would replace every instance.
            let retype = selector.unwrap_or(one.name.as_str());
            let message = format!(
                "{} is already {}; `shep restart {retype}` replaces it.",
                one.name, one.status
            );
            streams.aside("start", &message);
        }
        several => {
            let names: Vec<&str> = unique_names(several);
            let retype = selector.map_or_else(|| names.join(" "), str::to_string);
            let message = format!(
                "{} are already running; `shep restart {retype}` replaces them.",
                names.join(", ")
            );
            streams.aside("start", &message);
        }
    }

    // Every row is attempted and the first failure is what the verb
    // returns, as `request_each` does for the selector-taking verbs.
    let mut failure: Option<ExitCode> = None;
    for sheep in asleep {
        let code = resume(client, streams, sheep, started).await;
        if code != ExitCode::Success {
            failure = failure.or(Some(code));
        }
    }
    failure.unwrap_or(ExitCode::Success)
}

/// Drops the disabled rows from `rows` and says which sheep stay down
///
/// A Flockfile or path load is not an operator's request to bring a sheep
/// back: only `shep enable` clears the mark. One aside for the whole set,
/// naming `shep enable` once per sheep. A named `shep start` does not pass
/// through here, since the shepherd's refusal is that verb's answer.
pub(crate) fn leave_disabled(
    streams: &mut Streams<'_>,
    rows: Vec<ProcessInfo>,
) -> Vec<ProcessInfo> {
    let (disabled, kept): (Vec<ProcessInfo>, Vec<ProcessInfo>) = rows
        .into_iter()
        .partition(|info| info.disabled == Some(true));
    let refs: Vec<&ProcessInfo> = disabled.iter().collect();
    let names = unique_names(&refs);
    let message = match names.as_slice() {
        [] => return kept,
        [one] => format!("{one} is disabled and stays down; `shep enable {one}` brings it back."),
        several => {
            let remedies: Vec<String> = several
                .iter()
                .map(|name| format!("`shep enable {name}`"))
                .collect();
            format!(
                "{} are disabled and stay down; {} brings one back.",
                several.join(", "),
                remedies.join(" or ")
            )
        }
    };
    streams.aside("start", &message);
    kept
}

/// Reports the sheep `shep add` found already registered, and changes nothing
///
/// Always [`ExitCode::Success`]: a deploy script running `shep add
/// Flockfile.toml` twice must not fail the second time. No remedy is named,
/// since this arm holds both stopped and running rows.
pub(crate) fn already_registered(streams: &mut Streams<'_>, matched: &[ProcessInfo]) -> ExitCode {
    let refs: Vec<&ProcessInfo> = matched.iter().collect();
    let names = unique_names(&refs);
    let message = match names.as_slice() {
        [] => return ExitCode::Success,
        [one] => format!("{one} is already registered; nothing to add."),
        several => format!(
            "{} are already registered; nothing to add.",
            several.join(", ")
        ),
    };
    streams.aside("add", &message);
    ExitCode::Success
}

/// Every distinct name in `infos`, in the order they first appear
///
/// Feeds the already-running notice, never the respawn targets, which stay
/// per-row and per-id; see [`resume_all`]. Compares against every name kept
/// so far, not the previous one, so an unsorted listing gives the same
/// answer as a sorted one.
pub(crate) fn unique_names<'a>(infos: &[&'a ProcessInfo]) -> Vec<&'a str> {
    let mut names: Vec<&str> = Vec::with_capacity(infos.len());
    for info in infos {
        if !names.contains(&info.name.as_str()) {
            names.push(&info.name);
        }
    }
    names
}

pub(crate) async fn resume(
    client: &Client,
    streams: &mut Streams<'_>,
    sheep: &ProcessInfo,
    started: &mut Vec<shep_core::protocol::ProcessInfo>,
) -> ExitCode {
    let (procs, failure) = request_each(
        client,
        streams,
        &[SelectorSpec::Id(sheep.id)],
        None,
        |selector| Request::Restart { selector },
        // One id, so the shepherd matches it or refuses the whole request;
        // `refused` is a staged walk's field and a walk needs two names.
        |response| match response {
            Response::Restarted { accepted, .. } => Some(accepted),
            _ => None,
        },
    )
    .await;
    // The `Restart` reply has no per-id error slot, so an `Ok` can carry an
    // `errored` sheep, which is a `start` failure. Returned without
    // extending `started`, so a failing verb leaves stdout empty.
    if any_restart_failed(&procs) {
        // By id as well as by name: this reports one row, and four failing
        // instances would otherwise print four identical messages.
        let (name, id) = (&sheep.name, sheep.id);
        let message = format!(
            "{name} (id {id}) could not be started; see `shep bleats {id}` or its log files for why"
        );
        return streams.fail(ExitCode::SpawnFailed, &message);
    }
    started.extend(procs);
    failure.unwrap_or(ExitCode::Success)
}
