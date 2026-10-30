//! The env half of `shep copy`.
//!
//! A client is never sent a sheep's env values, so the copy cannot be planned
//! from the two configs the way a field or a dog's table is. The shepherd does
//! both halves: a dry [`Request::CopySheepEnv`] answers with the change list,
//! and the same request without `dry_run` writes it. What comes back carries a
//! whole `{{secret:...}}` reference as itself and every other value as
//! [`shep_core::secrets::REDACTED_ENV`], so nothing here can print a literal.

use serde_json::Value;
use shep_client::Client;
use shep_core::protocol::{EnvCopyChange, EnvCopyKind, Request, Response};

use crate::commands::rpc::{client_error, unexpected_response};
use crate::exit::ExitCode;
use crate::output::Streams;

use super::plan::{Change, ChangeKind, Plan, Sides, Write};

/// The plan for copying `from`'s env onto `to`'s, as the shepherd would write it.
///
/// Sends the request with `dry_run` set, so nothing is written and every
/// refusal the real copy would raise is raised here.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`: a sheep the shepherd does
/// not know, a dog on either side, or a result the sheep's config refuses.
pub(crate) async fn plan_env(
    client: &Client,
    streams: &mut Streams<'_>,
    sides: Sides<'_>,
    except: &[String],
    replace: bool,
) -> Result<Plan, ExitCode> {
    let request = copy_request(sides, except, replace, None);
    match client.request(request).await {
        Ok(Response::SheepEnvCopied { changes, .. }) => {
            Ok(plan_of(sides.source, except, replace, changes))
        }
        Ok(_other) => Err(unexpected_response(streams)),
        Err(err) => Err(client_error(streams, &err)),
    }
}

/// Sends the copy for real.
///
/// The env lands as an operator override and waits for the target's next
/// spawn, so the one pending key is always `env`.
///
/// # Errors
/// The [`ExitCode`] already reported to `streams`.
pub(crate) async fn write_env(
    client: &Client,
    streams: &mut Streams<'_>,
    target: &str,
    from: &str,
    replace: bool,
    except: &[String],
    expect: Vec<EnvCopyChange>,
) -> Result<Vec<String>, ExitCode> {
    let sides = Sides {
        source: from,
        target,
    };
    match client
        .request(copy_request(sides, except, replace, Some(expect)))
        .await
    {
        Ok(Response::SheepEnvCopied { .. }) => Ok(vec!["env".to_string()]),
        Ok(_other) => Err(unexpected_response(streams)),
        Err(err) => Err(client_error(streams, &err)),
    }
}

/// The request both halves send. The dry run carries no `expect`; the write
/// carries the list the dry run answered, so an edit made while the operator
/// was being asked is refused rather than applied unseen.
fn copy_request(
    sides: Sides<'_>,
    except: &[String],
    replace: bool,
    expect: Option<Vec<EnvCopyChange>>,
) -> Request {
    Request::CopySheepEnv {
        from: sides.source.to_string(),
        to: sides.target.to_string(),
        replace,
        except: except.to_vec(),
        dry_run: expect.is_none(),
        expect,
    }
}

/// The plan for the changes the shepherd listed.
///
/// No change means no write: the copy would leave the target as it is.
fn plan_of(from: &str, except: &[String], replace: bool, changes: Vec<EnvCopyChange>) -> Plan {
    let write = (!changes.is_empty()).then(|| Write::Env {
        from: from.to_string(),
        replace,
        except: except.to_vec(),
        expect: changes.clone(),
    });
    Plan {
        changes: changes.into_iter().map(change_of).collect(),
        write,
    }
}

/// One listed change, with its shown values as the strings they are.
fn change_of(change: EnvCopyChange) -> Change {
    let kind = match change.kind {
        EnvCopyKind::Added => ChangeKind::Added,
        EnvCopyKind::Removed => ChangeKind::Removed,
        // `Changed`, and any kind a newer shepherd adds: the key differs.
        _ => ChangeKind::Changed,
    };
    Change {
        key: change.key,
        kind,
        old: change.old.map(Value::String),
        new: change.new.map(Value::String),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::copy::address::{Address, ConfigPath};
    use crate::commands::copy::report::{ReportState, report_for};
    use crate::output::Render as _;

    fn listed(key: &str, kind: EnvCopyKind, old: Option<&str>, new: Option<&str>) -> EnvCopyChange {
        EnvCopyChange::new(key, kind, old.map(str::to_string), new.map(str::to_string))
    }

    #[test]
    fn the_listed_changes_become_the_plan_and_one_write() {
        let listed_changes = vec![
            listed("A", EnvCopyKind::Added, None, Some("{{secret:A}}")),
            listed(
                "B",
                EnvCopyKind::Changed,
                Some("<redacted>"),
                Some("<redacted>"),
            ),
            listed("D", EnvCopyKind::Removed, Some("<redacted>"), None),
        ];
        let plan = plan_of("web", &["PORT".to_string()], true, listed_changes.clone());

        let rows: Vec<_> = plan
            .changes
            .iter()
            .map(|change| {
                (
                    change.key.as_str(),
                    change.kind,
                    change.old.clone(),
                    change.new.clone(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "A",
                    ChangeKind::Added,
                    None,
                    Some(Value::from("{{secret:A}}"))
                ),
                (
                    "B",
                    ChangeKind::Changed,
                    Some(Value::from("<redacted>")),
                    Some(Value::from("<redacted>"))
                ),
                (
                    "D",
                    ChangeKind::Removed,
                    Some(Value::from("<redacted>")),
                    None
                ),
            ]
        );
        assert_eq!(
            plan.write,
            Some(Write::Env {
                from: "web".to_string(),
                replace: true,
                except: vec!["PORT".to_string()],
                expect: listed_changes,
            })
        );
    }

    #[test]
    fn a_copy_that_changes_nothing_has_no_write() {
        assert_eq!(plan_of("web", &[], false, Vec::new()), Plan::default());
    }

    #[test]
    fn the_report_carries_a_reference_as_itself_and_every_other_value_redacted() {
        let side = |sheep: &str| Address {
            sheep: sheep.to_string(),
            path: ConfigPath::Env,
        };
        let plan = plan_of(
            "web",
            &[],
            false,
            vec![
                listed(
                    "PADDOCK_KEY",
                    EnvCopyKind::Added,
                    None,
                    Some("{{secret:PADDOCK_KEY}}"),
                ),
                listed(
                    "B",
                    EnvCopyKind::Changed,
                    Some("<redacted>"),
                    Some("<redacted>"),
                ),
            ],
        );

        let report = report_for(&side("web"), &side("api"), &plan, ReportState::Written);

        let envelope = serde_json::to_value(&report).unwrap();
        assert_eq!(envelope["path"], "env");
        assert_eq!(envelope["changes"][0]["new"], "{{secret:PADDOCK_KEY}}");
        assert_eq!(envelope["changes"][1]["old"], "<redacted>");
        assert_eq!(
            report.rows(),
            vec![
                vec![
                    "PADDOCK_KEY".to_string(),
                    "added".to_string(),
                    "-".to_string(),
                    "{{secret:PADDOCK_KEY}}".to_string(),
                ],
                vec![
                    "B".to_string(),
                    "changed".to_string(),
                    "<redacted>".to_string(),
                    "<redacted>".to_string(),
                ],
            ]
        );
    }

    #[test]
    fn the_write_sends_back_the_list_the_dry_run_answered() {
        let sides = Sides {
            source: "web",
            target: "api",
        };
        let reviewed = vec![listed("A", EnvCopyKind::Added, None, Some("{{secret:A}}"))];

        let Request::CopySheepEnv {
            dry_run, expect, ..
        } = copy_request(sides, &[], false, None)
        else {
            panic!("a copy request");
        };
        assert!(dry_run && expect.is_none());

        let Request::CopySheepEnv {
            dry_run, expect, ..
        } = copy_request(sides, &[], false, Some(reviewed.clone()))
        else {
            panic!("a copy request");
        };
        assert!(!dry_run);
        assert_eq!(expect, Some(reviewed));
    }
}
