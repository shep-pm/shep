//! The check a reload makes before it hands the flock over in place.
//!
//! The `sandbox` key was removed from a sheep. A successor that reads a
//! carried sheep still setting it would refuse that sheep at adoption and
//! strand the flock, and by then the old shepherd is gone. The only place to
//! refuse with the old shepherd still running is here, before the signal.

use shep_client::Client;
use shep_core::protocol::Request;

/// Asks `client`'s shepherd which sheep still set the removed `sandbox` key,
/// and returns the refusal that names them, or `None` when none does.
///
/// Fails closed: a shepherd that cannot answer the question is a refusal too,
/// because a handover that cannot be checked is one that could strand the
/// flock. Dogs are not asked, since a dog's config never carries the key.
pub(super) async fn refusal(client: &Client) -> Option<String> {
    let flock = match client.list_flock().await {
        Ok(flock) => flock,
        Err(err) => return Some(unchecked(&err)),
    };
    let mut setting = Vec::new();
    for info in flock.iter().filter(|info| info.dog.is_none()) {
        let request = Request::SheepConfig {
            name: info.name.clone(),
        };
        match client.request(request).await {
            Ok(shep_core::protocol::Response::SheepConfig(view)) => {
                if view.config.sandbox.is_some() {
                    setting.push(info.name.as_str());
                }
            }
            Ok(other) => return Some(unchecked(&format_args!("answered {other:?}"))),
            Err(err) => return Some(unchecked(&err)),
        }
    }
    (!setting.is_empty()).then(|| naming(&setting))
}

/// The refusal for sheep that set the removed key, one name each.
fn naming(sheep: &[&str]) -> String {
    let names = sheep
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "the flock is still running and nothing was handed over: {names} set{s} sandbox, which \
         the sandbox dog replaced and the new shepherd refuses. Remove the key from each and \
         configure the dog in an [app.dogs.sandbox] table, then reload again",
        s = if sheep.len() == 1 { "s" } else { "" },
    )
}

/// The refusal for a shepherd that could not say.
fn unchecked(cause: &dyn core::fmt::Display) -> String {
    format!(
        "could not check the flock for sheep that set the removed sandbox key ({cause}), so \
         nothing was handed over and the flock is still running"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Format;
    use crate::commands::daemon::reload;
    use crate::exit::ExitCode;
    use crate::output::Streams;
    use crate::version_guard::VersionGuard;
    use shep_core::paths::ShepPaths;
    use shep_core::protocol::Response;

    #[test]
    fn one_sheep_is_named_with_the_move_to_the_dog_table() {
        let text = naming(&["web"]);
        assert!(text.contains("`web` sets sandbox"), "{text}");
        assert!(text.contains("[app.dogs.sandbox]"), "{text}");
    }

    #[test]
    fn several_sheep_are_each_named_and_the_verb_agrees() {
        let text = naming(&["web", "jobs"]);
        assert!(text.contains("`web`, `jobs` set sandbox"), "{text}");
    }

    #[test]
    fn an_unchecked_flock_says_the_flock_is_untouched() {
        let text = unchecked(&"connection reset");
        assert!(text.contains("could not check"), "{text}");
        assert!(text.contains("connection reset"), "{text}");
        assert!(text.contains("still running"), "{text}");
    }

    /// A fake shepherd that can be handed over, holding `flock`: each name
    /// paired with whether that sheep still sets the removed `sandbox` key.
    #[cfg(unix)]
    async fn flock_answering(
        paths: &ShepPaths,
        flock: &[(&str, bool)],
    ) -> tokio::sync::mpsc::UnboundedReceiver<shep_core::protocol::Envelope> {
        use shep_core::config::AppConfig;
        use shep_core::protocol::{ProcessInfo, SheepConfigView};
        use shep_core::status::ProcStatus;

        std::fs::create_dir_all(&paths.run).unwrap();
        std::fs::create_dir_all(&paths.pids).unwrap();
        let flock: Vec<(String, bool)> = flock
            .iter()
            .map(|(name, sandbox)| ((*name).to_string(), *sandbox))
            .collect();
        shep_client::testing::fake_daemon_answering_with_ack(
            &paths.socket,
            ack_naming("9.9.9"),
            move |req| match req {
                Request::HandoverFitness => Response::HandoverFitness { refusal: None },
                Request::ListFlock => Response::Flock(
                    flock
                        .iter()
                        .zip(1..)
                        .map(|((name, _), id)| {
                            ProcessInfo::builder(id, name.clone(), ProcStatus::Online).build()
                        })
                        .collect(),
                ),
                Request::SheepConfig { name } => {
                    let mut config = AppConfig::minimal(name, "./srv");
                    config.sandbox = flock
                        .iter()
                        .find(|(held, _)| held == name)
                        .filter(|(_, sandbox)| *sandbox)
                        .map(|_| "sandbox.json".to_string());
                    Response::SheepConfig(Box::new(SheepConfigView::new(
                        config,
                        Vec::new(),
                        Vec::new(),
                    )))
                }
                other => panic!("a reload asked a question this fake does not hold: {other:?}"),
            },
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_flock_with_a_sheep_that_sets_the_removed_sandbox_key_is_not_handed_over() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let _sent = flock_answering(&paths, &[("api", false), ("web", true), ("jobs", true)]).await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            reload(&mut streams, &paths, VersionGuard::Exempt).await
        };

        let text = format!(
            "{}{}",
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap()
        );
        // The pidfile is absent, so a reload that went on to signal would fail
        // as "no shepherd is running" and `DaemonUnreachable`.
        assert_eq!(code, ExitCode::InvalidConfig, "{text}");
        assert!(
            text.contains("web") && text.contains("jobs") && !text.contains("api"),
            "each offending sheep is named, and no other: {text}"
        );
        assert!(text.contains("[app.dogs.sandbox]"), "{text}");
        assert!(
            !text.contains("stopping and starting") && !text.contains("stopped and started"),
            "the flock is left running, not restarted by the other arm: {text}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_flock_with_no_removed_sandbox_key_goes_on_to_the_handover() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let mut sent = flock_answering(&paths, &[("api", false), ("web", false)]).await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            reload(&mut streams, &paths, VersionGuard::Exempt).await
        };

        // No shepherd owns this home, so reaching the signal step is what
        // fails: proof the check let the handover through.
        assert_eq!(code, ExitCode::DaemonUnreachable);
        let asked: Vec<Request> = std::iter::from_fn(|| sent.try_recv().ok())
            .map(|envelope| envelope.body)
            .collect();
        let configs = asked
            .iter()
            .filter(|req| matches!(req, Request::SheepConfig { .. }))
            .count();
        assert_eq!(configs, 2, "every sheep is checked: {asked:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dog_in_the_flock_is_not_asked_for_its_config() {
        use shep_core::protocol::{DogSource, ProcessInfo};
        use shep_core::status::ProcStatus;

        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.run).unwrap();
        std::fs::create_dir_all(&paths.pids).unwrap();
        let mut sent = shep_client::testing::fake_daemon_answering_with_ack(
            &paths.socket,
            ack_naming("9.9.9"),
            |req| match req {
                Request::ListFlock => Response::Flock(vec![
                    ProcessInfo::builder(1, "metrics", ProcStatus::Online)
                        .dog(Some(DogSource::BuiltIn { grants: None }))
                        .build(),
                ]),
                _ => Response::HandoverFitness { refusal: None },
            },
        )
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            reload(&mut streams, &paths, VersionGuard::Exempt).await;
        }

        let asked: Vec<Request> = std::iter::from_fn(|| sent.try_recv().ok())
            .map(|envelope| envelope.body)
            .collect();
        assert!(
            !asked
                .iter()
                .any(|req| matches!(req, Request::SheepConfig { .. })),
            "a dog never carries the key: {asked:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_flock_that_cannot_be_checked_is_not_handed_over() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.run).unwrap();
        std::fs::create_dir_all(&paths.pids).unwrap();
        let _sent = shep_client::testing::fake_daemon_answering_with_ack(
            &paths.socket,
            ack_naming("9.9.9"),
            |req| match req {
                Request::HandoverFitness => Response::HandoverFitness { refusal: None },
                _ => Response::Pong,
            },
        )
        .await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            reload(&mut streams, &paths, VersionGuard::Exempt).await
        };

        let text = format!(
            "{}{}",
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap()
        );
        assert_eq!(code, ExitCode::InvalidConfig, "{text}");
        assert!(text.contains("could not check"), "{text}");
    }

    /// A `HelloAck` for a shepherd newer than this build, so the handover arm
    /// is the one taken.
    #[cfg(unix)]
    fn ack_naming(version: &str) -> shep_core::protocol::HelloAck {
        shep_core::protocol::HelloAck {
            daemon_version: version.to_string(),
            protocol: shep_core::protocol::PROTOCOL_VERSION,
            pid: 4242,
            min_supported: None,
        }
    }
}
