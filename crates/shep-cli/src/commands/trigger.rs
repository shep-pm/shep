//! `trigger`: send a named action to matched sheep over the shepherd channel
//! and report what each app answered.
//!
//! Sent with [`TRIGGER_DEADLINE`], not the client's 5s default: the daemon
//! waits on every matched sheep's own `AppConfig::action_timeout`, which can
//! be configured up to 58s.
//!
//! A row's own outcome is never a request failure. `shep trigger` exits
//! non-`Success` only when the RPC itself failed. With no action, it lists
//! a dog's manifest instead, on this machine and without a shepherd.

use shep_client::{Client, TRIGGER_DEADLINE};
use shep_core::dogs::{is_reserved_action, manifest::Action};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{Request, Response};

use crate::cli::{Format, TriggerArgs};
use crate::commands::dogs::{ManifestRead, VERSION_BUDGET, read_manifest};
use crate::commands::rpc::request_and_render;
use crate::commands::selector::parse_selector_spec;
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;
use crate::output::{DogActionList, Streams, TriggeredRows, emit, write_outcome};

/// Sends `args.action` (and `args.params`, if any) to the sheep matching
/// `args.selector`, and renders one row per match.
///
/// `action` and `params` are carried exactly as typed: neither this side nor
/// the daemon parses them. An `args` with no action is refused: listing is
/// [`list_actions`]. A [`shep_core::dogs::RESERVED_ACTION_PREFIX`] action is
/// refused here, before anything goes out: the shepherd sends those itself.
pub async fn trigger(client: &Client, streams: &mut Streams<'_>, args: &TriggerArgs) -> ExitCode {
    let Some(action) = args.action.clone() else {
        return streams.fail(
            ExitCode::Usage,
            "listing a dog's actions reads manifests on this machine; \
             name an action to trigger one on a host",
        );
    };
    if is_reserved_action(&action) {
        return streams.fail(
            ExitCode::Usage,
            &format!("`{action}` is an action shep sends itself; `shep trigger` cannot send it"),
        );
    }
    let selector = match parse_selector_spec(streams, &args.selector) {
        Ok(selector) => selector,
        Err(code) => return code,
    };

    let body = Request::Trigger {
        selector,
        action,
        params: args.params.clone(),
    };

    let deadline = Some(TRIGGER_DEADLINE);
    request_and_render(
        client,
        streams,
        "trigger",
        body,
        deadline,
        |response| match response {
            Response::Triggered(replies) => Some(TriggeredRows(replies)),
            _ => None,
        },
    )
    .await
}

/// Lists the actions the dog `name` declares, read the way `shep adopt` reads
/// a manifest: from the cache, or by asking the dog's binary.
///
/// A dog that declares none says so, in a notice for the table and as an
/// empty array for `--format json`.
pub fn list_actions(streams: &mut Streams<'_>, paths: &ShepPaths, name: &str) -> ExitCode {
    let cfg = match ShepToml::read_only(&paths.daemon_config) {
        Ok(cfg) => cfg,
        Err(err) => return streams.fail(ExitCode::InvalidConfig, &err.to_string()),
    };
    let manifest = match cfg.adopted_dog_path(name) {
        Some(binary) => match read_manifest(&binary, &paths.home, name, VERSION_BUDGET) {
            Ok(ManifestRead::Read(manifest)) => Some(manifest),
            Ok(ManifestRead::Absent) => None,
            Ok(ManifestRead::Unreadable) => {
                return streams.fail(
                    ExitCode::InvalidConfig,
                    &format!(
                        "the manifest recorded for dog `{name}` cannot be read; \
                         `shep adopt {name}` asks the dog again"
                    ),
                );
            }
            Err(refusal) => {
                let message = format!("{}: {refusal}", binary.display());
                return streams.fail(ExitCode::InvalidConfig, &message);
            }
        },
        None => match crate::dog::builtin_manifest(name) {
            Some(manifest) => Some(manifest),
            None => {
                return streams.fail(
                    ExitCode::NotFound,
                    &format!(
                        "`{name}` is not a dog; `shep trigger <dog>` lists a dog's actions, \
                         and `shep trigger <sheep> <action>` triggers one"
                    ),
                );
            }
        },
    };
    let actions: Vec<Action> = manifest
        .map(|manifest| manifest.actions)
        .unwrap_or_default()
        .into_iter()
        .map(|action| {
            let (name, _) = crate::terminal_safe::sanitise(&action.name);
            let (description, _) = crate::terminal_safe::sanitise(&action.description);
            Action::new(name, description)
        })
        .collect();
    if actions.is_empty() && streams.fmt == Format::Table {
        streams.note("no_actions", &format!("dog `{name}` declares no actions"));
        return ExitCode::Success;
    }
    write_outcome(emit(
        &mut *streams.out,
        streams.fmt,
        "trigger",
        DogActionList(actions),
        streams.style,
    ))
}

#[cfg(test)]
mod tests {
    use shep_client::testing::{fake_client_capturing_envelopes, fake_client_replying_err};
    use shep_core::protocol::RpcErrorCode;
    use shep_core::protocol::SelectorSpec;

    use super::*;
    use crate::cli::Format;

    fn args(selector: &str, action: &str, params: Option<&str>) -> TriggerArgs {
        TriggerArgs {
            selector: selector.to_string(),
            action: Some(action.to_string()),
            params: params.map(str::to_string),
        }
    }

    /// Runs the verb against a fake daemon that captures envelopes.
    ///
    /// The same shape as `commands::whisper`'s helper, so the two verbs'
    /// tests read alike.
    async fn run(
        args: &TriggerArgs,
    ) -> (
        ExitCode,
        Vec<u8>,
        Vec<u8>,
        tokio::sync::mpsc::Receiver<shep_core::protocol::Envelope>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            trigger(&client, &mut streams, args).await
        };
        (code, out, err, envelopes)
    }

    /// What a dog's `--shep-manifest` answers, with and without `actions`.
    #[cfg(unix)]
    const DECLARING_BODY: &str = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"shep-janitor","version":"0.1.0","protocol":12,"channel":true,"actions":[{"name":"sweep","description":"Delete finished scratch files"},{"name":"pause","description":"Stop sweeping"}]}'
fi"#;
    #[cfg(unix)]
    const SILENT_ACTIONS_BODY: &str = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"shep-janitor","version":"0.1.0","protocol":12}'
fi"#;

    /// A `$SHEP_HOME` under `dir` with `body` adopted as the dog `janitor`,
    /// probed once so its manifest is cached as `shep adopt` would leave it.
    #[cfg(unix)]
    fn a_home_with_janitor(dir: &std::path::Path, body: &str) -> ShepPaths {
        use std::os::unix::fs::PermissionsExt as _;

        let paths = crate::home::resolve_paths(&crate::cli::GlobalArgs {
            host: None,
            home: Some(dir.join(".shep")),
            format: Format::Table,
            quiet: false,
            style: None,
        })
        .expect("an absolute --home resolves");
        let binary = dir.join("janitor");
        std::fs::write(&binary, format!("#!/bin/sh\n{body}\nexit 0\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        ShepToml::edit(&paths.daemon_config, |cfg| {
            cfg.adopt_dog("janitor", &binary, false).unwrap();
        })
        .unwrap();
        paths
    }

    /// Runs `list_actions` for `name` and returns its exit code, stdout and stderr.
    #[cfg(unix)]
    fn listed(paths: &ShepPaths, name: &str, fmt: Format) -> (ExitCode, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt,
            };
            list_actions(&mut streams, paths, name)
        };
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_dog_with_no_action_lists_the_actions_its_manifest_declares() {
        let dir = tempfile::tempdir().unwrap();
        let paths = a_home_with_janitor(dir.path(), DECLARING_BODY);

        let (code, out, err) = listed(&paths, "janitor", Format::Table);

        assert_eq!(code, ExitCode::Success, "{err}");
        assert!(out.contains("sweep"), "{out}");
        assert!(out.contains("Delete finished scratch files"), "{out}");
        assert!(out.contains("pause"), "{out}");
        assert!(out.contains("Stop sweeping"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn the_json_listing_is_the_manifests_actions() {
        let dir = tempfile::tempdir().unwrap();
        let paths = a_home_with_janitor(dir.path(), DECLARING_BODY);

        let (code, out, _err) = listed(&paths, "janitor", Format::Json);

        assert_eq!(code, ExitCode::Success);
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            value["data"],
            serde_json::json!([
                {"name": "sweep", "description": "Delete finished scratch files"},
                {"name": "pause", "description": "Stop sweeping"},
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dog_that_declares_no_actions_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let paths = a_home_with_janitor(dir.path(), SILENT_ACTIONS_BODY);

        let (code, out, _err) = listed(&paths, "janitor", Format::Table);

        assert_eq!(code, ExitCode::Success);
        assert!(out.contains("dog `janitor` declares no actions"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_a_dog_is_refused_with_nothing_listed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = a_home_with_janitor(dir.path(), DECLARING_BODY);

        let (code, out, err) = listed(&paths, "web", Format::Table);

        assert_eq!(code, ExitCode::NotFound);
        assert!(out.is_empty(), "{out}");
        assert!(err.contains("`web` is not a dog"), "{err}");
    }

    /// `"/[/"` is one of the only three inputs the selector grammar rejects.
    #[tokio::test]
    async fn a_malformed_selector_exits_usage_without_a_round_trip() {
        let (code, _out, _err, mut envelopes) = run(&args("/[/", "ping", None)).await;
        assert_eq!(code, ExitCode::Usage);
        assert!(
            envelopes.try_recv().is_err(),
            "a malformed selector must fail locally, never reach the wire"
        );
    }

    #[tokio::test]
    async fn a_not_found_reply_exits_not_found_rather_than_being_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _served) =
            fake_client_replying_err(&path, RpcErrorCode::NotFound, "no sheep matched").await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            trigger(&client, &mut streams, &args("nonexistent", "ping", None)).await
        };
        assert_eq!(code, ExitCode::NotFound);
        assert!(out.is_empty());
    }

    /// `shep.` names are shep's own to send, and the refusal is local.
    #[tokio::test]
    async fn a_reserved_action_is_refused_before_sending() {
        for action in ["shep.validate", "shep.pane", "shep."] {
            let (code, out, err, mut envelopes) = run(&args("janitor", action, None)).await;
            let err = String::from_utf8(err).unwrap();
            assert_eq!(code, ExitCode::Usage, "{err}");
            assert!(out.is_empty());
            assert!(err.contains(action), "the refusal names `{action}`: {err}");
            assert!(
                err.contains("shep sends itself"),
                "the refusal says why: {err}"
            );
            assert!(
                envelopes.try_recv().is_err(),
                "a reserved action must fail locally, never reach the wire"
            );
        }
    }

    /// The fake daemon always answers `Pong`; only the captured envelope
    /// catches a dropped field.
    #[tokio::test]
    async fn the_request_carries_the_selector_action_params_and_trigger_deadline() {
        let (_code, _out, _err, mut envelopes) = run(&args("web", "gc", Some("--force"))).await;

        let envelope = envelopes.recv().await.unwrap();
        assert_eq!(
            envelope.body,
            Request::Trigger {
                selector: SelectorSpec::Name("web".to_string()),
                action: "gc".to_string(),
                params: Some("--force".to_string()),
            }
        );
        assert_eq!(
            envelope.deadline_ms,
            Some(u64::try_from(TRIGGER_DEADLINE.as_millis()).unwrap()),
            "a trigger sent with the client's plain default would abandon a reply the daemon \
             is still honestly building"
        );
    }

    /// The fake daemon's `Pong` stands in for a `Response` variant this
    /// `match` has no arm for.
    #[tokio::test]
    async fn an_unrecognised_response_exits_internal() {
        let (code, out, err, _envelopes) = run(&args("web", "ping", None)).await;
        assert_eq!(code, ExitCode::Internal);
        assert!(out.is_empty());
        assert!(!err.is_empty());
    }
}
