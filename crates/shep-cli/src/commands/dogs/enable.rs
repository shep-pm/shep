//! `shep enable`: turns a registered dog on, writing the config and
//! starting it if a shepherd is running.

use std::path::Path;

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;
use shep_core::protocol::{DogSource, Request, Response};

use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::shep_toml::{ShepToml, ShepTomlError};
use crate::exit::ExitCode;
use crate::output::{DogActionStatus, Streams};

use super::consent::{Input, Verdict, consent_from, review};
use super::depends::warn_of_unmet_depends;
use super::grants::warn_if_unenforced;
use super::vet::{VERSION_BUDGET, fail_adopt, read_manifest};
use super::{
    DECLINED_STATUS, NO_SHEPHERD_ENABLE_STATUS, adopted_source, connect_or_absent, dog_source,
    emit_dog_action, fail_config,
};

/// [`enable`]'s own failure, so its `try_edit` closure can refuse from
/// inside the lock: either the config layer's error, or a name that answers
/// to no dog at all.
///
/// `Debug` is derived, not redacted: [`Self::Config`] forwards to
/// [`ShepTomlError`]'s own redacted `Debug`, and [`Self::UnknownDog`]
/// carries dog names the refusal already prints.
#[derive(Debug)]
pub(crate) enum EnableRefusal {
    /// The read-modify-write underneath the closure failed.
    Config(ShepTomlError),
    /// The name is neither one of [`crate::dog::BUILT_IN_DOGS`] nor a key of
    /// `[daemon] adopted_dogs`. `adopted` is what that map held, read under
    /// the same lock, so a concurrent `shep adopt` cannot invalidate the
    /// alternatives the refusal names.
    UnknownDog { adopted: Vec<String> },
}

impl From<ShepTomlError> for EnableRefusal {
    fn from(err: ShepTomlError) -> Self {
        Self::Config(err)
    }
}

/// `enable`'s config half: which [`DogSource`] `name` resolves to, whether
/// it names a dog at all, and the write itself.
///
/// [`dog_source`] reads built-in-ness as an absence, so without the
/// unknown-name check a typo lands in `enabled_dogs` as a built-in and the
/// shepherd spawns `shep dog <typo>` on a ladder that cannot succeed. The
/// check sits inside the `try_edit` closure so it skips `save` and reads the
/// adopted names under the lock a concurrent `shep adopt` would race.
///
/// # Errors
/// [`EnableRefusal::Config`] if the read-modify-write failed.
/// [`EnableRefusal::UnknownDog`] if `name` names no dog.
// Windows only, for the same reason as `commands::shep_toml`'s module-wide
// allow: `EnableRefusal::Config` carries `ShepTomlError`, and `Err` crosses
// clippy's 128-byte threshold there.
#[cfg_attr(windows, allow(clippy::result_large_err))]
pub(crate) fn enable_in_config(path: &Path, name: &str) -> Result<DogSource, EnableRefusal> {
    enable_granting(path, name, None)
}

/// [`enable_in_config`], recording `grants` for the dog in the same write
/// when there are any, so the source it returns carries them.
///
/// # Errors
/// As [`enable_in_config`].
#[cfg_attr(windows, allow(clippy::result_large_err))]
fn enable_granting(
    path: &Path,
    name: &str,
    grants: Option<&Grants>,
) -> Result<DogSource, EnableRefusal> {
    ShepToml::try_edit(path, |cfg| {
        // `grants` answers a question asked against a read taken before this
        // write. A grant another shep wrote since stands.
        if let Some(grants) = grants
            && cfg.granted(name)?.is_none()
        {
            cfg.set_granted(name, grants)?;
        }
        let source = dog_source(cfg, name, cfg.granted(name)?);
        if matches!(source, DogSource::BuiltIn { .. }) && !crate::dog::BUILT_IN_DOGS.contains(&name)
        {
            return Err(EnableRefusal::UnknownDog {
                adopted: cfg.adopted_dog_names(),
            });
        }
        cfg.enable_dog(name)?;
        Ok(source)
    })
}

/// `shep enable <name>`: writes the config, and starts the dog if a
/// shepherd is running.
///
/// An adopted dog that has never been granted anything and is not yet
/// enabled is asked about first ([`ask_before_first_enable`]); `yes` is
/// `--yes`, which answers that question.
pub async fn enable(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    yes: bool,
) -> ExitCode {
    enable_asking(streams, paths, name, yes, &mut Input::stdin()).await
}

/// [`enable`], asking at `input` rather than at this process's stdin.
pub(super) async fn enable_asking(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    yes: bool,
    input: &mut Input,
) -> ExitCode {
    let grants = match ask_before_first_enable(streams, paths, name, yes, input) {
        Ok(grants) => grants,
        Err(code) => return code,
    };
    let source = match enable_granting(&paths.daemon_config, name, grants.as_ref()) {
        Ok(source) => source,
        Err(EnableRefusal::Config(err)) => return fail_config(streams, &err),
        Err(EnableRefusal::UnknownDog { adopted }) => {
            return fail_enable_unknown_dog(streams, name, &adopted);
        }
    };
    warn_of_unmet_depends_of(streams, paths, name, &source);
    let client = match connect_or_absent(paths, streams).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    enable_after_config(streams, name, &source, client.as_ref()).await
}

/// The first enable of an adopted dog: shows what it declares and asks
/// whether to grant it.
///
/// "First" is a dog with no `dog_grants` entry that is not already enabled:
/// one an operator declined at `shep adopt`, or hand-wrote into
/// `[daemon.adopted_dogs]`. A built-in dog has no manifest to show yet, and a
/// name that is no dog is left for [`enable_in_config`] to refuse. A config
/// that cannot be read is reported here: carrying on would enable the dog
/// without the question.
///
/// `Ok` holds the grants to record with the enable; `Ok(None)` is nothing to
/// record, either because no question was due or because the dog is
/// undeclared and keeps full access.
///
/// # Errors
/// The [`ExitCode`] to stop with, already reported: a refusal, or a no, which
/// leaves the dog disabled with nothing granted.
fn ask_before_first_enable(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    yes: bool,
    input: &mut Input,
) -> Result<Option<Grants>, ExitCode> {
    let cfg =
        ShepToml::read_only(&paths.daemon_config).map_err(|err| fail_config(streams, &err))?;
    let Some(binary) = cfg.adopted_dog_path(name) else {
        return Ok(None);
    };
    let already_decided = !matches!(cfg.granted(name), Ok(None));
    if already_decided || cfg.enabled_dog_names().iter().any(|dog| dog == name) {
        return Ok(None);
    }
    let manifest = read_manifest(&binary, &paths.home, name, VERSION_BUDGET)
        .map_err(|refusal| fail_adopt(streams, &binary, &refusal))?;
    let reviewed =
        review(manifest.as_ref()).map_err(|refusal| refusal.report(streams, name, "enabled"))?;
    match consent_from(streams, name, "enable", &reviewed, yes, input)? {
        Verdict::Grant(grants) => Ok(grants),
        Verdict::Declined => {
            let source = adopted_source(&cfg, name, None);
            Err(emit_dog_action(
                streams,
                "enable",
                name,
                source,
                DECLINED_STATUS,
            ))
        }
    }
}

/// Warns, for an adopted dog, of what its manifest needs and is missing.
///
/// A built-in dog has no binary to read. The manifest comes from the cache
/// when this binary has been read before, so a dog adopted by an older shep,
/// or rebuilt since, is asked once more. A dog with no readable manifest
/// earns nothing here.
fn warn_of_unmet_depends_of(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    source: &DogSource,
) {
    let DogSource::Adopted { path, .. } = source else {
        return;
    };
    let Ok(Some(manifest)) = read_manifest(Path::new(path), &paths.home, name, VERSION_BUDGET)
    else {
        return;
    };
    let adopted = ShepToml::read_only(&paths.daemon_config)
        .map(|cfg| cfg.adopted_dog_names())
        .unwrap_or_default();
    warn_of_unmet_depends(streams, name, &manifest, &adopted);
}

/// Renders [`enable`]'s refusal of a name that names no dog.
///
/// `adopted` is every key of `[daemon] adopted_dogs`, read under the lock
/// the refusal was decided under, and empty where nothing has been adopted.
/// [`ExitCode::InvalidConfig`] rather than [`ExitCode::Usage`]: the name is
/// one the daemon config cannot resolve, not a malformed argument.
fn fail_enable_unknown_dog(streams: &mut Streams<'_>, name: &str, adopted: &[String]) -> ExitCode {
    let valid: Vec<String> = crate::dog::BUILT_IN_DOGS
        .iter()
        .map(|built_in| format!("{built_in:?}"))
        .chain(adopted.iter().map(|dog| format!("{dog:?}")))
        .collect();
    let message = format!(
        "`{name}` is not a dog; valid names are {} -- if you meant a third-party dog, \
         run `shep adopt {name}` first",
        join_with_and(&valid)
    );
    streams.fail(ExitCode::InvalidConfig, &message)
}

/// Joins `items` as an English list: `a`, `a and b`, `a, b, and c`.
///
/// The empty slice answers with the empty string, and no caller reaches it.
pub(crate) fn join_with_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// `enable`'s daemon half, split out so a test can drive it against a
/// `shep_client::testing` fake rather than a second, real connection.
///
/// `client: None` is [`connect_or_absent`] reporting a genuine absence. A
/// stale socket file and a daemon that was never started are not
/// distinguished, so a provisioning script can configure a host before
/// starting anything.
pub(super) async fn enable_after_config(
    streams: &mut Streams<'_>,
    name: &str,
    source: &DogSource,
    client: Option<&Client>,
) -> ExitCode {
    let Some(client) = client else {
        return emit_dog_action(
            streams,
            "enable",
            name,
            source.clone(),
            NO_SHEPHERD_ENABLE_STATUS,
        );
    };
    // A name a sheep already holds comes back as
    // `RpcErrorCode::InvalidConfig`; the `Err` arm surfaces it verbatim.
    let request = Request::EnableDog {
        name: name.to_string(),
        source: source.clone(),
    };
    match client.request(request).await {
        Ok(Response::DogStarted(info)) => {
            warn_if_unenforced(streams, name, source, client.daemon().protocol);
            emit_dog_action(
                streams,
                "enable",
                name,
                source.clone(),
                DogActionStatus::Acted(info.status.into()),
            )
        }
        Ok(_unrecognised) => unexpected_response(streams),
        Err(err) => client_error(streams, &err),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use shep_client::testing::{
        fake_client_capturing_envelopes, fake_client_replying_err, sample_ack, sample_info,
        serve_one_request,
    };
    use shep_core::protocol::RpcErrorCode;

    use super::*;
    use crate::cli::Format;
    use crate::commands::dogs::test_support::streams;

    /// Exact strings, not a "contains no secret" probe: the output is half
    /// the derive and half [`ShepTomlError`]'s manual impl, and either alone
    /// could start printing the document.
    #[test]
    fn enable_refusal_debug_never_prints_the_document() {
        let path = std::path::PathBuf::from("/home/ada/.shep/shep.toml");
        let secret = "https://hooks.example.com/services/T00/B00/super-secret-token";
        let broken = format!("[dog.bark]\nwebhook = \"{secret}\"\n[daemon\n");
        let source = broken.parse::<toml_edit::DocumentMut>().unwrap_err();

        let wrong_shape = EnableRefusal::Config(ShepTomlError::WrongShape {
            path: path.clone(),
            key: "style",
            expected: "a table",
            found: "string",
        });
        assert_eq!(
            format!("{wrong_shape:?}"),
            "Config(WrongShape { path: \"/home/ada/.shep/shep.toml\", key: \"style\", \
             expected: \"a table\", found: \"string\" })"
        );

        let parse = EnableRefusal::Config(ShepTomlError::Parse { path, source });
        let debug = format!("{parse:?}");
        assert!(
            !debug.contains(secret),
            "the document must never reach Debug: {debug}"
        );
        assert!(!debug.contains("webhook"), "{debug}");
        assert_eq!(
            debug,
            "Config(Parse { path: \"/home/ada/.shep/shep.toml\", message: \"invalid table \
             header\\nexpected `.`, `]`\" })"
        );

        let unknown = EnableRefusal::UnknownDog {
            adopted: vec!["otel".to_string()],
        };
        assert_eq!(format!("{unknown:?}"), "UnknownDog { adopted: [\"otel\"] }");
    }

    #[test]
    fn enable_in_config_writes_the_name_and_reports_a_built_in_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shep.toml");

        let source = enable_in_config(&path, "metrics").unwrap();

        assert!(matches!(source, DogSource::BuiltIn { .. }));
        assert!(
            ShepToml::read_only(&path)
                .unwrap()
                .enabled_dog_names()
                .contains(&"metrics".to_string())
        );
    }

    #[test]
    fn enable_in_config_refuses_a_name_that_is_no_dog_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shep.toml");
        std::fs::write(&path, "").unwrap();

        let refusal = enable_in_config(&path, "nonsense");

        assert!(matches!(refusal, Err(EnableRefusal::UnknownDog { .. })));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "",
            "a refused enable leaves shep.toml untouched"
        );
    }

    /// `shepherd_acted` and `status` together separate "only the config
    /// changed" from "a shepherd acted", and this verb's two branches differ
    /// in little else. The pair is the guard: either field alone passes while
    /// the other branch carries the wrong one.
    #[tokio::test]
    async fn enable_reports_whether_a_shepherd_acted() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = shep_client::testing::fake_client_answering(&path, |_| {
            Response::DogStarted(sample_info())
        })
        .await;

        for (client, expected) in [
            (Some(&client), DogActionStatus::Acted("online")),
            (None, NO_SHEPHERD_ENABLE_STATUS),
        ] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut streams = streams(&mut out, &mut err);
            streams.fmt = Format::Json;

            let code = enable_after_config(
                &mut streams,
                "bark",
                &DogSource::BuiltIn { grants: None },
                client,
            )
            .await;

            assert_eq!(code, ExitCode::Success);
            let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(
                json["data"]["shepherd_acted"],
                serde_json::json!(expected.shepherd_acted()),
                "client present: {}, payload: {}",
                client.is_some(),
                String::from_utf8_lossy(&out)
            );
            assert_eq!(
                json["data"]["status"],
                expected.text(),
                "payload: {}",
                String::from_utf8_lossy(&out)
            );
        }
    }

    #[tokio::test]
    async fn enable_asks_the_shepherd_to_start_that_dog_as_a_built_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = enable_after_config(
            &mut streams(&mut out, &mut err),
            "metrics",
            &DogSource::BuiltIn { grants: None },
            Some(&client),
        )
        .await;

        let sent = envelopes.recv().await.unwrap();
        assert_eq!(
            sent.body,
            Request::EnableDog {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn { grants: None },
            }
        );
    }

    /// End to end through `enable`, since the lookup lives in the config
    /// half: [`serve_one_request`] only binds the socket, so `enable` does
    /// its own `Client::connect`.
    ///
    /// Run with the channel ask recorded and without: a re-enable that
    /// dropped it would start a dog that asked for the channel without one.
    #[tokio::test]
    async fn enable_of_an_adopted_dog_sends_its_recorded_path_and_channel_ask() {
        for channel in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let paths = ShepPaths::resolve(&|_| None, dir.path());
            std::fs::create_dir_all(&paths.run).unwrap();
            ShepToml::edit(&paths.daemon_config, |seed| {
                seed.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), channel)
                    .unwrap();
            })
            .unwrap();
            let handle = serve_one_request(
                &paths.socket,
                sample_ack(),
                Response::DogStarted(sample_info()),
            )
            .await;

            let mut out = Vec::new();
            let mut err = Vec::new();
            let code = enable(&mut streams(&mut out, &mut err), &paths, "otel", true).await;

            assert_eq!(code, ExitCode::Success);
            let envelope = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
                .await
                .expect("enable must reach the wire; it hung instead of connecting")
                .unwrap();
            assert_eq!(
                envelope.body,
                Request::EnableDog {
                    name: "otel".to_string(),
                    source: DogSource::Adopted {
                        grants: None,
                        path: "/usr/local/bin/shep-otel".to_string(),
                        channel,
                    },
                }
            );
            let text = String::from_utf8(out).unwrap();
            assert!(
                text.contains("adopted"),
                "the row must render an adopted dog as adopted: {text}"
            );
        }
    }

    /// A name holding values in both `shep.toml` and `dogs.toml` makes the
    /// migration refuse and the daemon exit 4 with the flock unsupervised.
    #[tokio::test]
    async fn enabling_a_dog_does_not_leave_a_section_that_refuses_the_next_boot() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = enable(&mut streams(&mut out, &mut err), &paths, "metrics", true).await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(
            !written.contains("[dog"),
            "enable must write no dog section into shep.toml: {written}"
        );

        // The operator configures the dog where `docs/dogs.md` says to,
        // and the next `shep muster` runs the migration.
        std::fs::write(&paths.dogs_config, "[metrics]\nbind = \"127.0.0.1:9615\"\n").unwrap();
        crate::commands::dog_migration::migrate_dog_sections(&paths)
            .expect("a boot after an enable must not refuse over a section enable wrote");
    }

    #[tokio::test]
    async fn enable_reports_a_refusal_as_a_refusal_not_as_no_shepherd() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.run).unwrap();
        let refusal = shep_core::protocol::RpcError {
            code: RpcErrorCode::ProtocolMismatch,
            message: "this daemon speaks protocol 1, this client speaks 2".to_string(),
            daemon_version: Some("0.1.8".to_string()),
        };
        let _daemon = shep_client::testing::fake_daemon(&paths.socket, Err(refusal)).await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = enable(&mut streams(&mut out, &mut err), &paths, "metrics", true).await;

        assert_ne!(code, ExitCode::Success);
        let text = String::from_utf8(err).unwrap();
        assert!(
            !text.contains(NO_SHEPHERD_ENABLE_STATUS.text()),
            "a refusal is not an absence: {text}"
        );
        assert!(text.contains("shep daemon reload"), "{text}");
    }

    /// A non-zero exit would make `shep enable` unusable in a provisioning
    /// script that configures a host before starting anything.
    #[tokio::test]
    async fn enable_with_no_shepherd_writes_the_config_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = enable(&mut streams(&mut out, &mut err), &paths, "metrics", true).await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(
            written.contains("metrics"),
            "the config edit must still land: {written}"
        );
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("next shepherd"),
            "the operator needs to know the dog is not running yet: {text}"
        );
    }

    #[tokio::test]
    async fn enable_refuses_a_name_that_is_neither_built_in_nor_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = enable(&mut streams(&mut out, &mut err), &paths, "pydog", true).await;

        assert_eq!(code, ExitCode::InvalidConfig);
        let text = String::from_utf8(err).unwrap();
        assert!(
            text.contains("pydog"),
            "the refusal must name the name: {text}"
        );
        assert!(
            text.contains("shep adopt pydog"),
            "the refusal must name the way out, the way `shep adopt`'s own \
             name-collision refusal does: {text}"
        );
        assert!(
            !paths.daemon_config.exists(),
            "a refused enable must leave the config untouched -- `try_edit` \
             skips `save`, so a `$SHEP_HOME` that had no `shep.toml` still \
             has none"
        );
    }

    /// Read under `enable`'s own lock, so a concurrent `shep adopt` cannot
    /// make the message name a set the refusal was never decided against.
    #[tokio::test]
    async fn enable_refusal_names_the_adopted_dogs_alongside_the_built_ins() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        ShepToml::edit(&paths.daemon_config, |cfg| {
            cfg.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), false)
                .unwrap();
        })
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = enable(&mut streams(&mut out, &mut err), &paths, "pydog", true).await;

        assert_eq!(code, ExitCode::InvalidConfig);
        let text = String::from_utf8(err).unwrap();
        for expected in ["\"metrics\"", "\"bark\"", "\"otel\""] {
            assert!(
                text.contains(expected),
                "the refusal must name {expected} among the valid names: {text}"
            );
        }
    }

    #[tokio::test]
    async fn enable_still_accepts_a_name_adopt_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        ShepToml::edit(&paths.daemon_config, |cfg| {
            cfg.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), false)
                .unwrap();
        })
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = enable(&mut streams(&mut out, &mut err), &paths, "otel", true).await;

        assert_eq!(code, ExitCode::Success, "{}", String::from_utf8_lossy(&err));
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(written.contains("otel"), "{written}");
    }

    #[test]
    fn join_with_and_reads_as_an_english_list() {
        let one = ["a".to_string()];
        let two = ["a".to_string(), "b".to_string()];
        let three = ["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(join_with_and(&[]), "");
        assert_eq!(join_with_and(&one), "a");
        assert_eq!(join_with_and(&two), "a and b");
        assert_eq!(join_with_and(&three), "a, b, and c");
    }

    /// `start_dog` is idempotent by name, so an unmarked entry coming back
    /// means a sheep already holds `name`. The operator must see the
    /// daemon's message verbatim, not a bare code.
    #[tokio::test]
    async fn enable_reports_a_name_collision_with_the_daemons_own_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let message =
            "a sheep is already registered as `bark`; rename it or give the dog another name";
        let (client, _daemon) =
            fake_client_replying_err(&path, RpcErrorCode::InvalidConfig, message).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = enable_after_config(
            &mut streams(&mut out, &mut err),
            "bark",
            &DogSource::BuiltIn { grants: None },
            Some(&client),
        )
        .await;

        assert_eq!(code, ExitCode::InvalidConfig);
        let text = String::from_utf8(err).unwrap();
        assert!(
            text.contains(message),
            "the daemon's own message must reach the operator: {text}"
        );
    }
}
