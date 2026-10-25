//! `shep delete`: the one way to remove a sheep or a dog, and it sticks.
//!
//! The verb takes selectors, as it always has. What it now asks depends on
//! what each selector names in this home's `shep.toml`: a dog goes through
//! [`crate::commands::dogs::remove_dog`], which strikes the adoption as well
//! as asking the shepherd to deregister it, so the next boot does not bring it
//! back. Everything else goes through the shepherd, unchanged.
//!
//! A built-in dog's name is deliberately left out of the dog half. It is in
//! neither `enabled_dogs` nor `adopted_dogs` unless an operator wrote it
//! there, and the question "is this name a dog or a sheep that kept the name"
//! has one honest answerer: the shepherd that is holding the entry. So the
//! name travels the sheep path and the shepherd either refuses it or deletes
//! the sheep, whichever it is.
//!
//! A `shep.toml` that exists and cannot be read or parsed answers that same
//! question for no name, so a name selector is refused there rather than sent
//! off to be guessed at. The shepherd would delete the live entry, the
//! adoption would survive in the file the next boot reads, and a verb that
//! promises "for good" would exit 0 saying `deleted 1 sheep`. A sweep needs no
//! answer and is untouched.

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, SelectorSpec};
use shep_core::selector::ProcessSelector;

use crate::cli::SelectorArgs;
use crate::commands::dogs::{self, Removal, fail_config};
use crate::commands::lifecycle::operational::delete_sheep;
use crate::commands::lifecycle::selectors::{flock_matches, parse_selectors};
use crate::commands::rpc::client_error;
use crate::commands::shep_toml::{ShepToml, ShepTomlError};
use crate::exit::ExitCode;
use crate::output::Streams;
use crate::remote::{Link, ssh_hint};
use crate::version_guard::{VersionGuard, connect_client};

/// `shep delete <selectors...>`
///
/// Dogs first, then sheep: a dog's removal is the one that edits `shep.toml`,
/// and a sheep's failure says nothing about whether that edit landed.
///
/// Each half renders its own outcome, so `--format json` on a command that
/// names both gets one envelope per half. A mixed `delete` is rare enough that
/// the alternative, one payload holding a dog's row beside a sheep's ids, was
/// not worth a row type neither verb asked for.
///
/// Over a [`Link::Host`] there is no dog half. The `shep.toml` here is this
/// machine's, not the host's, and the host's shepherd never edits its own, so
/// a dog it has registered could be deregistered but not struck: the next boot
/// there would bring it back. The host's flock is listed first, and an exact
/// selector that names an adopted or unrecognized dog refuses the whole
/// command with the `ssh` line that runs `shep delete` on the host. Every
/// other selector goes to the host's shepherd, which answers `NotFound` for a
/// name it holds no entry under and refuses a built-in dog itself.
///
/// # Errors
/// [`ExitCode::InvalidConfig`] or [`ExitCode::Failure`] after writing what
/// went wrong, when this home's `shep.toml` exists and cannot be read or
/// parsed and a selector is an exact name. Nothing is sent and nothing is
/// deleted on that arm.
///
/// [`ExitCode::Usage`] when, over a host, an exact selector names a dog the
/// host adopted, and the code of a failed listing when the host's flock
/// cannot be read. Nothing is deleted on either arm.
pub(crate) async fn delete(
    streams: &mut Streams<'_>,
    link: &Link<'_>,
    guard: VersionGuard,
    args: &SelectorArgs,
) -> ExitCode {
    let (local_dogs, host) = match link {
        Link::Local(paths) => (Some(*paths), None),
        Link::Host { dest, .. } => (None, Some(*dest)),
    };
    delete_with(
        streams,
        local_dogs,
        host,
        async |streams| connect_client(streams, link, guard).await,
        args,
    )
    .await
}

/// [`delete`] with the dog half's home and the connection decided by the
/// caller: `local_dogs` is `None` when there is no dog half, `host` names the
/// host the selectors are sent to when there is one, and `connect` is asked
/// only if a sheep selector is left to send.
async fn delete_with(
    streams: &mut Streams<'_>,
    local_dogs: Option<&ShepPaths>,
    host: Option<&str>,
    connect: impl AsyncFnOnce(&mut Streams<'_>) -> Result<Client, ExitCode>,
    args: &SelectorArgs,
) -> ExitCode {
    let selectors = match parse_selectors(streams, &args.selectors) {
        Ok(selectors) => selectors,
        Err(code) => return code,
    };
    let mut dog_names: Vec<String> = Vec::new();
    let mut sheep_tokens: Vec<String> = Vec::new();
    for (raw, selector) in args.selectors.iter().zip(&selectors) {
        let dog = match local_dogs {
            Some(paths) => dog_in_config(paths, selector),
            None => Ok(None),
        };
        match dog {
            Ok(Some(name)) => dog_names.push(name),
            Ok(None) => sheep_tokens.push(raw.clone()),
            Err(err) => return fail_config(streams, &err),
        }
    }

    let mut failure: Option<ExitCode> = None;
    if let Some(paths) = local_dogs {
        for name in &dog_names {
            let code = dogs::remove_dog(streams, paths, name, Removal::Delete).await;
            if code != ExitCode::Success {
                failure = failure.or(Some(code));
            }
        }
    }
    if sheep_tokens.is_empty() {
        return failure.unwrap_or(ExitCode::Success);
    }
    let client = match connect(streams).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    if let Some(dest) = host
        && let Err(code) = refuse_adopted_dogs(streams, &client, dest, &selectors).await
    {
        return code;
    }
    let sheep = SelectorArgs {
        selectors: sheep_tokens,
    };
    match delete_sheep(&client, streams, &sheep).await {
        ExitCode::Success => failure.unwrap_or(ExitCode::Success),
        code => failure.unwrap_or(code),
    }
}

/// Refuses `selectors` when an exact one names a dog `dest` adopted, or one
/// whose source this build does not recognize
///
/// The host's flock is listed only if a selector is exact: a sweep passes dogs
/// by, so it needs no answer. A built-in dog is left to the host's shepherd,
/// which refuses it with `shep disable` as the advice. One message names every
/// dog, so a command that names several is refused once.
///
/// # Errors
/// [`ExitCode::Usage`] after writing a line per dog, and the code of a failed
/// listing after writing it. Nothing has been sent but `ListFlock`.
async fn refuse_adopted_dogs(
    streams: &mut Streams<'_>,
    client: &Client,
    dest: &str,
    selectors: &[SelectorSpec],
) -> Result<(), ExitCode> {
    let exact: Vec<ProcessSelector> = selectors
        .iter()
        .filter_map(|spec| ProcessSelector::try_from(spec.clone()).ok())
        .filter(ProcessSelector::is_exact)
        .collect();
    if exact.is_empty() {
        return Ok(());
    }
    let flock = match client.list_flock().await {
        Ok(flock) => flock,
        Err(err) => return Err(client_error(streams, &err)),
    };
    let mut names: Vec<String> = Vec::new();
    for selector in &exact {
        for row in flock_matches(selector, &flock) {
            let adopted = matches!(
                row.dog,
                Some(DogSource::Adopted { .. } | DogSource::Unrecognized)
            );
            if adopted && !names.contains(&row.name) {
                names.push(row.name);
            }
        }
    }
    if names.is_empty() {
        return Ok(());
    }
    let lines: Vec<String> = names
        .iter()
        .map(|name| {
            let hint = ssh_hint(
                dest,
                &["shep".to_string(), "delete".to_string(), name.clone()],
            );
            format!(
                "{name} is a dog on {dest}, and its adoption lives in that host's \
                 shep.toml, which would bring it back at the next boot; run `{hint}` \
                 to delete it there"
            )
        })
        .collect();
    Err(streams.fail(ExitCode::Usage, &lines.join("\n")))
}

/// The dog `selector` names, or `None` when this verb should send it to the
/// shepherd as a sheep
///
/// Only an exact name reaches a dog: `all`, a fold and a regex are the flock,
/// and the shepherd already passes dogs by for an acting verb. The name is
/// read off `shep.toml`, the one place this CLI knows what it adopted, and a
/// built-in name is passed by for the reason this module's header gives.
///
/// # Errors
/// [`ShepTomlError::Io`] or [`ShepTomlError::Parse`] when `shep.toml` exists
/// and this cannot read it. A missing file is an empty document, which answers
/// `None` for every name, and a sweep is `Ok(None)` without reading at all.
fn dog_in_config(
    paths: &ShepPaths,
    selector: &SelectorSpec,
) -> Result<Option<String>, ShepTomlError> {
    let SelectorSpec::Name(name) = selector else {
        return Ok(None);
    };
    if crate::dog::BUILT_IN_DOGS.contains(&name.as_str()) {
        return Ok(None);
    }
    let cfg = ShepToml::read_only(&paths.daemon_config)?;
    let known = cfg
        .adopted_dog_names()
        .iter()
        .chain(cfg.enabled_dog_names().iter())
        .any(|dog| dog == name);
    Ok(known.then(|| name.clone()))
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use shep_core::protocol::{
        DogSource, Envelope, HelloAck, PROTOCOL_VERSION, ProcessInfo, Request, Response,
    };
    use shep_core::status::ProcStatus;

    use super::*;
    use crate::cli::Format;

    fn args(selectors: &[&str]) -> SelectorArgs {
        SelectorArgs {
            selectors: selectors.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// A `$SHEP_HOME` holding one adopted dog named `name` with a section of
    /// its own, with its `run` directory there so a fake shepherd can bind
    /// `paths.socket`
    fn home_with_a_dog(dir: &Path, name: &str) -> ShepPaths {
        let paths = ShepPaths::resolve(&|_| None, dir);
        std::fs::create_dir_all(&paths.run).unwrap();
        ShepToml::edit(&paths.daemon_config, |cfg| {
            cfg.adopt_dog(name, Path::new("/usr/local/bin/shep-otel"), false)
                .unwrap();
        })
        .unwrap();
        std::fs::write(&paths.dogs_config, "[log-rotate]\ndebounce = \"30s\"\n").unwrap();
        paths
    }

    /// A shepherd that answers every `Delete` with one id, announcing this
    /// build's own version so the version guard lets the verb through
    async fn fake_shepherd(paths: &ShepPaths) -> tokio::sync::mpsc::UnboundedReceiver<Envelope> {
        shep_client::testing::fake_daemon_answering_with_ack(
            &paths.socket,
            HelloAck {
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: PROTOCOL_VERSION,
                pid: 4242,
                min_supported: None,
            },
            |request| match request {
                Request::Delete { .. } => Response::Deleted(vec![7]),
                Request::ListFlock => Response::Flock(Vec::new()),
                _ => Response::Pong,
            },
        )
        .await
    }

    /// Runs `delete` against `paths`, handing back its code, stdout and stderr
    ///
    /// `envelopes` is the fake's capture, drained afterwards: `None` stands
    /// for no shepherd running, which is the one connect failure this verb's
    /// dog half tolerates.
    async fn delete_against(
        paths: &ShepPaths,
        mut envelopes: Option<&mut tokio::sync::mpsc::UnboundedReceiver<Envelope>>,
        selectors: &[&str],
        fmt: Format,
    ) -> (ExitCode, String, String, Vec<Request>) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams::bare(&mut out, &mut err, fmt);
        let code = delete(
            &mut streams,
            &Link::Local(paths),
            VersionGuard::Enforce,
            &args(selectors),
        )
        .await;
        let mut sent = Vec::new();
        if let Some(rx) = envelopes.as_mut() {
            while let Ok(envelope) = rx.try_recv() {
                sent.push(envelope.body);
            }
        }
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            sent,
        )
    }

    /// The whole point of the ticket: `delete` is the verb that removes a dog,
    /// and it removes it from the config the next boot reads as well as from
    /// the flock.
    #[tokio::test]
    async fn deleting_an_adopted_dog_forgets_the_adoption_and_asks_the_shepherd() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");
        let mut envelopes = fake_shepherd(&paths).await;

        let (code, out, err, sent) =
            delete_against(&paths, Some(&mut envelopes), &["log-rotate"], Format::Table).await;

        assert_eq!(code, ExitCode::Success, "{err}");
        assert_eq!(
            sent,
            vec![Request::Delete {
                selector: SelectorSpec::Name("log-rotate".to_string()),
            }],
            "the dog half still travels the shepherd's own delete"
        );
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(
            !written.contains("log-rotate"),
            "a dog left in enabled_dogs comes back at the next boot: {written}"
        );
        assert!(
            err.contains("kept [log-rotate] in dogs.toml"),
            "the settings that survive a dog's removal are named: {err}"
        );
        let kept = std::fs::read_to_string(&paths.dogs_config).unwrap();
        assert!(
            kept.contains("[log-rotate]"),
            "the section an operator wrote is theirs, not the adoption's: {kept}"
        );
        assert!(
            !out.contains("deleted 1 sheep"),
            "a dog is not a sheep, and its ids are not what this verb printed: {out}"
        );
    }

    /// `--format json` answers with `delete`'s own reply, not the hidden
    /// verb's: the `command` key is what a script already filtering on
    /// `delete` reads.
    #[tokio::test]
    async fn deleting_a_dog_answers_under_delete() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");
        let mut envelopes = fake_shepherd(&paths).await;

        let (code, out, err, _) =
            delete_against(&paths, Some(&mut envelopes), &["log-rotate"], Format::Json).await;

        assert_eq!(code, ExitCode::Success, "{err}");
        let json: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
        assert_eq!(json["command"], "delete", "{json}");
        assert_eq!(json["data"]["name"], "log-rotate", "{json}");
        assert_eq!(json["data"]["status"], "stopped", "{json}");
    }

    /// A sheep's name reaches the shepherd exactly as it did before this verb
    /// learned about dogs: same request, same reply, same stderr line, and no
    /// dog config spent on the way.
    #[tokio::test]
    async fn deleting_a_sheep_is_untouched_by_the_dog_half() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");
        let mut envelopes = fake_shepherd(&paths).await;

        let (code, _, err, sent) =
            delete_against(&paths, Some(&mut envelopes), &["web"], Format::Table).await;

        assert_eq!(code, ExitCode::Success, "{err}");
        assert_eq!(
            sent,
            vec![
                Request::Delete {
                    selector: SelectorSpec::Name("web".to_string()),
                },
                Request::ListFlock,
            ],
            "{sent:?}"
        );
        assert!(
            err.contains("deleted 1 sheep, id 7"),
            "the sheep half keeps its own stderr line: {err}"
        );
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(
            written.contains("log-rotate"),
            "deleting a sheep spends no dog config: {written}"
        );
    }

    /// A built-in dog's name is not this CLI's call to make. It goes out as a
    /// selector and comes back as whatever the shepherd holding the entry
    /// says, which is a refusal for a dog and a delete for a sheep that kept
    /// the name.
    #[tokio::test]
    async fn a_built_in_dogs_name_travels_the_sheep_path() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");
        ShepToml::try_edit(&paths.daemon_config, |cfg| cfg.enable_dog("metrics")).unwrap();
        let mut envelopes = fake_shepherd(&paths).await;

        let (code, _, _, sent) =
            delete_against(&paths, Some(&mut envelopes), &["metrics"], Format::Json).await;

        assert_eq!(code, ExitCode::Success);
        assert_eq!(
            sent,
            vec![Request::Delete {
                selector: SelectorSpec::Name("metrics".to_string()),
            }],
            "the shepherd is the one that knows whether this name is a dog"
        );
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(
            written.contains("metrics"),
            "a name this CLI could not judge spends no config edit: {written}"
        );
    }

    /// A dog's name over a host is a selector like any other: the dog half
    /// reads and edits this machine's `shep.toml`, and the host's dogs are not
    /// in it. The local adoption of the same name must come through untouched.
    #[tokio::test]
    async fn delete_over_a_host_sends_a_dog_name_to_the_shepherd_and_edits_no_local_shep_toml() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");
        let config_before = std::fs::read(&paths.daemon_config).unwrap();
        let dogs_before = std::fs::read(&paths.dogs_config).unwrap();
        let mut envelopes = fake_shepherd(&paths).await;
        let client = shep_client::Client::connect(&paths.socket).await.unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();

        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            delete_with(
                &mut streams,
                None,
                None,
                async |_| Ok(client),
                &args(&["log-rotate"]),
            )
            .await
        };

        let err = String::from_utf8(err).unwrap();
        assert_eq!(code, ExitCode::Success, "{err}");
        let mut sent = Vec::new();
        while let Ok(envelope) = envelopes.try_recv() {
            sent.push(envelope.body);
        }
        assert_eq!(
            sent,
            vec![
                Request::Delete {
                    selector: SelectorSpec::Name("log-rotate".to_string()),
                },
                Request::ListFlock,
            ],
            "the host's shepherd is the one that knows whether this name is a dog"
        );
        assert_eq!(std::fs::read(&paths.daemon_config).unwrap(), config_before);
        assert_eq!(std::fs::read(&paths.dogs_config).unwrap(), dogs_before);
        assert!(
            err.contains("deleted 1 sheep, id 7") && !err.contains("kept"),
            "the shepherd's answer is reported and no dog is kept: {err}"
        );
    }

    /// A sweep is the flock, and a dog is not in it: `all` must not be the
    /// command that quietly forgets an adoption.
    #[tokio::test]
    async fn a_wildcard_never_takes_the_dog_half() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");
        let mut envelopes = fake_shepherd(&paths).await;

        let (code, _, _, sent) =
            delete_against(&paths, Some(&mut envelopes), &["all"], Format::Json).await;

        assert_eq!(code, ExitCode::Success);
        assert_eq!(
            sent,
            vec![Request::Delete {
                selector: SelectorSpec::All,
            }],
            "{sent:?}"
        );
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(
            written.contains("log-rotate"),
            "`shep delete all` left the adoption exactly where it was: {written}"
        );
    }

    /// The two halves each answer, so a command naming both does both, and the
    /// dog half's config edit is not held hostage to the sheep half's round
    /// trip.
    #[tokio::test]
    async fn a_command_naming_a_dog_and_a_sheep_does_both() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");
        let mut envelopes = fake_shepherd(&paths).await;

        let (code, out, _, sent) = delete_against(
            &paths,
            Some(&mut envelopes),
            &["log-rotate", "web"],
            Format::Json,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        assert_eq!(
            sent,
            vec![
                Request::Delete {
                    selector: SelectorSpec::Name("log-rotate".to_string()),
                },
                Request::Delete {
                    selector: SelectorSpec::Name("web".to_string()),
                },
            ],
            "{sent:?}"
        );
        let json: Vec<serde_json::Value> = serde_json::Deserializer::from_str(&out)
            .into_iter::<serde_json::Value>()
            .map(|value| value.unwrap())
            .collect();
        assert_eq!(json.len(), 2, "{out}");
        assert_eq!(json[0]["command"], "delete", "{json:?}");
        assert_eq!(json[0]["data"]["name"], "log-rotate", "{json:?}");
        assert_eq!(json[1]["data"], serde_json::json!([7]), "{json:?}");
    }

    /// With no shepherd to deregister the entry, the config half still runs,
    /// which is the half that decides whether the dog comes back.
    #[tokio::test]
    async fn deleting_a_dog_with_no_shepherd_still_forgets_the_adoption() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");

        let (code, out, err, _) = delete_against(&paths, None, &["log-rotate"], Format::Json).await;

        assert_eq!(code, ExitCode::Success, "{err}");
        let json: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
        assert_eq!(json["command"], "delete", "{json}");
        assert_eq!(
            json["data"]["shepherd_acted"],
            serde_json::json!(false),
            "{json}"
        );
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(
            !written.contains("log-rotate"),
            "the next boot reads this file: {written}"
        );
    }

    /// A `shep.toml` this CLI cannot read answers "is this name a dog?" for no
    /// name, which is not a licence to guess. Sent off, the shepherd deletes
    /// the live entry, the adoption survives in the file the next boot reads,
    /// and a verb that promises "for good" exits 0 saying `deleted 1 sheep`.
    #[tokio::test]
    async fn a_shep_toml_that_cannot_be_parsed_refuses_a_name_and_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");
        let broken = "enabled_dogs = [\"log-rotate\"\n";
        std::fs::write(&paths.daemon_config, broken).unwrap();
        let mut envelopes = fake_shepherd(&paths).await;

        let (code, out, err, sent) =
            delete_against(&paths, Some(&mut envelopes), &["log-rotate"], Format::Table).await;

        assert_eq!(code, ExitCode::InvalidConfig, "{err}");
        assert!(err.contains("shep.toml"), "{err}");
        assert!(out.is_empty(), "a refusal prints no deletion: {out}");
        assert!(sent.is_empty(), "nothing reached the shepherd: {sent:?}");
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert_eq!(written, broken, "a refusal spends no config edit");
    }

    /// The half the refusal rests on, held with no socket and no shepherd: an
    /// unreadable `shep.toml` answers the dog question with the failure, not
    /// with `None`, and a missing file is still "no dog here".
    #[test]
    fn dog_in_config_reports_an_unreadable_config_instead_of_answering_none() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let name = SelectorSpec::Name("log-rotate".to_string());

        assert!(
            matches!(dog_in_config(&paths, &name), Ok(None)),
            "a home with no shep.toml has no adopted dog"
        );

        std::fs::create_dir_all(&paths.run).unwrap();
        std::fs::write(&paths.daemon_config, "enabled_dogs = [\"log-rotate\"\n").unwrap();
        assert!(
            matches!(
                dog_in_config(&paths, &name),
                Err(ShepTomlError::Parse { .. })
            ),
            "a file that exists and does not parse is a failure this verb reports"
        );

        assert!(
            matches!(dog_in_config(&paths, &SelectorSpec::All), Ok(None)),
            "a sweep is answered without opening the file at all"
        );
    }

    /// The refusal is scoped to the selector that needs the answer. `all` is
    /// the flock, a dog is not in it, and a broken `shep.toml` costs that
    /// command nothing it did not already pay.
    #[tokio::test]
    async fn a_sweep_still_deletes_the_flock_over_an_unreadable_shep_toml() {
        let dir = tempfile::tempdir().unwrap();
        let paths = home_with_a_dog(dir.path(), "log-rotate");
        std::fs::write(&paths.daemon_config, "not toml at all").unwrap();
        let mut envelopes = fake_shepherd(&paths).await;

        let (code, _, err, sent) =
            delete_against(&paths, Some(&mut envelopes), &["all"], Format::Json).await;

        assert_eq!(code, ExitCode::Success, "{err}");
        assert_eq!(
            sent,
            vec![Request::Delete {
                selector: SelectorSpec::All
            }],
            "{sent:?}"
        );
    }

    const HOST: &str = "amp2";

    fn a_sheep(id: u32, name: &str) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online).build()
    }

    fn a_dog(id: u32, name: &str, source: DogSource) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online)
            .dog(Some(source))
            .build()
    }

    fn adopted() -> DogSource {
        DogSource::Adopted {
            path: "/usr/local/bin/shep-otel".to_string(),
            channel: false,
            grants: None,
        }
    }

    /// A host's shepherd that lists `listing` and answers every `Delete` with
    /// one id
    async fn fake_host(
        paths: &ShepPaths,
        listing: Response,
    ) -> tokio::sync::mpsc::UnboundedReceiver<Envelope> {
        shep_client::testing::fake_daemon_answering_with_ack(
            &paths.socket,
            HelloAck {
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: PROTOCOL_VERSION,
                pid: 4242,
                min_supported: None,
            },
            move |request| match request {
                Request::Delete { .. } => Response::Deleted(vec![7]),
                Request::ListFlock => listing.clone(),
                _ => Response::Pong,
            },
        )
        .await
    }

    /// Runs `delete_with` as `delete` does over `--host amp2`: no dog half,
    /// the host's name given, and a client dialled to the fake host
    async fn delete_over_a_host(
        flock: Response,
        selectors: &[&str],
    ) -> (ExitCode, String, Vec<Request>) {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.run).unwrap();
        let mut envelopes = fake_host(&paths, flock).await;
        let client = shep_client::Client::connect(&paths.socket).await.unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            delete_with(
                &mut streams,
                None,
                Some(HOST),
                async |_| Ok(client),
                &args(selectors),
            )
            .await
        };
        let mut sent = Vec::new();
        while let Ok(envelope) = envelopes.try_recv() {
            sent.push(envelope.body);
        }
        (code, String::from_utf8(err).unwrap(), sent)
    }

    /// The host's `shep.toml` holds the adoption and the shepherd never
    /// writes it, so a delete that went through would print success and have
    /// the dog back at the next boot.
    #[tokio::test]
    async fn deleting_a_dog_over_a_host_is_refused_with_the_ssh_line_that_strikes_it() {
        let flock = Response::Flock(vec![a_dog(3, "log-rotate", adopted())]);

        let (code, err, sent) = delete_over_a_host(flock, &["log-rotate"]).await;

        assert_eq!(code, ExitCode::Usage, "{err}");
        assert_eq!(
            sent,
            vec![Request::ListFlock],
            "nothing is deleted: {sent:?}"
        );
        assert!(
            err.contains("ssh amp2 shep delete log-rotate"),
            "the refusal names the line that does strike it: {err}"
        );
    }

    /// One refusal for the whole command, so the sheep named beside the dogs
    /// is not deleted either.
    #[tokio::test]
    async fn a_mixed_delete_over_a_host_refuses_both_names_and_deletes_neither() {
        let flock = Response::Flock(vec![
            a_dog(3, "log-rotate", adopted()),
            a_dog(4, "mystery", DogSource::Unrecognized),
            a_sheep(5, "web"),
        ]);

        let (code, err, sent) = delete_over_a_host(flock, &["log-rotate", "web", "mystery"]).await;

        assert_eq!(code, ExitCode::Usage, "{err}");
        assert_eq!(sent, vec![Request::ListFlock], "{sent:?}");
        for line in [
            "ssh amp2 shep delete log-rotate",
            "ssh amp2 shep delete mystery",
        ] {
            assert!(err.contains(line), "{line} missing: {err}");
        }
    }

    #[tokio::test]
    async fn deleting_a_sheep_over_a_host_still_sends_one_delete() {
        let flock = Response::Flock(vec![a_dog(3, "log-rotate", adopted()), a_sheep(5, "web")]);

        let (code, err, sent) = delete_over_a_host(flock, &["web"]).await;

        assert_eq!(code, ExitCode::Success, "{err}");
        assert_eq!(
            sent,
            vec![
                Request::ListFlock,
                Request::Delete {
                    selector: SelectorSpec::Name("web".to_string()),
                },
                Request::ListFlock,
            ],
            "{sent:?}"
        );
    }

    /// A sweep passes dogs by, so it has no dog to refuse and no reason to
    /// list the host's flock first.
    #[tokio::test]
    async fn a_sweep_over_a_host_sends_no_listing_and_one_delete() {
        let flock = Response::Flock(vec![a_dog(3, "log-rotate", adopted())]);

        let (code, err, sent) = delete_over_a_host(flock, &["all"]).await;

        assert_eq!(code, ExitCode::Success, "{err}");
        assert_eq!(
            sent,
            vec![
                Request::Delete {
                    selector: SelectorSpec::All,
                },
                // `delete_sheep`'s own reread after the delete, not a check.
                Request::ListFlock,
            ],
            "the delete goes first, with no listing ahead of it: {sent:?}"
        );
    }

    /// The shepherd already refuses a built-in dog with `shep disable` as
    /// the advice, and `ssh ... shep delete` would be refused the same way.
    #[tokio::test]
    async fn a_built_in_dog_over_a_host_is_left_to_the_shepherd() {
        let flock = Response::Flock(vec![a_dog(
            3,
            "metrics",
            DogSource::BuiltIn { grants: None },
        )]);

        let (code, err, sent) = delete_over_a_host(flock, &["metrics"]).await;

        assert_eq!(code, ExitCode::Success, "{err}");
        assert!(
            sent.contains(&Request::Delete {
                selector: SelectorSpec::Name("metrics".to_string()),
            }),
            "{sent:?}"
        );
    }

    /// A listing that failed says nothing about which names are dogs, so it
    /// must not pass for a flock with none.
    #[tokio::test]
    async fn a_failed_listing_over_a_host_sends_no_delete() {
        let (code, err, sent) = delete_over_a_host(Response::Pong, &["log-rotate"]).await;

        assert_ne!(code, ExitCode::Success, "{err}");
        assert_ne!(
            code,
            ExitCode::Usage,
            "a failed listing is not a refusal: {err}"
        );
        assert_eq!(sent, vec![Request::ListFlock], "{sent:?}");
    }
}
