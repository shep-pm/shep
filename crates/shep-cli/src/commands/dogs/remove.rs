//! The one removal an adopted dog has, run by `shep delete <name>` and by
//! the hidden `shep rehome <name>`.
//!
//! Both verbs stop the dog, deregister it from this shepherd, and strike the
//! adoption from `shep.toml`: out of `[daemon] enabled_dogs`,
//! `[daemon] adopted_dogs` and `[daemon] channel_dogs`. What stays is the
//! `[<name>]` section in `dogs.toml`, which is the operator's own writing.
//!
//! A built-in dog is refused here rather than only in the CLI's help text:
//! `disable` is the verb that exists for it.

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, Request, Response, SelectorSpec};

use crate::commands::dog_migration;
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;
use crate::output::Streams;

use super::{
    DISABLED_STATUS, NO_SHEPHERD_DISABLE_STATUS, adopted_source, connect_or_absent,
    emit_dog_action, fail_config,
};

/// Which verb ran the removal
///
/// The two differ in nothing but the name they answer under, and that name is
/// what a script reading `--format json` keys on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    /// `shep delete <name>`: the one way an operator removes a dog
    Delete,
    /// `shep rehome <name>`: hidden, kept for the scripts that already call it
    Rehome,
}

impl Removal {
    /// The verb as the operator typed it, which is also the `command` key its
    /// reply carries
    fn verb(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Rehome => "rehome",
        }
    }
}

/// `shep delete <name>` and `shep rehome <name>`, for a dog
///
/// One file: [`ShepToml::rehome_dog`] strikes the registration from
/// `shep.toml`. `dogs.toml` is only read, to say whether there was a section
/// to keep. Re-adopting the same dog finds its old configuration waiting; the
/// difference from `disable` is that recovery needs a fresh `shep adopt`.
///
/// # Errors
/// [`ExitCode::InvalidConfig`] after writing the refusal, when `name` is a
/// built-in dog. Nothing is read or written on that arm: a name with no
/// adoption must not spend a config edit.
pub(crate) async fn remove_dog(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    name: &str,
    removal: Removal,
) -> ExitCode {
    if crate::dog::BUILT_IN_DOGS.contains(&name) {
        return streams.fail(
            ExitCode::InvalidConfig,
            &shep_daemon::dogs::built_in_dog_refusal(name),
        );
    }
    let source = match ShepToml::edit(&paths.daemon_config, |cfg| {
        // Read before `rehome_dog` erases it. `None` is legitimate: a name
        // never adopted.
        let source = adopted_source(cfg, name, None);
        cfg.rehome_dog(name);
        source
    }) {
        Ok(source) => source,
        Err(err) => return fail_config(streams, &err),
    };
    // This verb does not write `dogs.toml`, so an unreadable one costs
    // the notice below and nothing more.
    let kept = dog_migration::dog_section_exists(&paths.dogs_config, name).unwrap_or(false);
    let client = match connect_or_absent(paths, streams).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    let code = remove_after_config(streams, name, source, client.as_ref(), removal).await;
    if kept {
        streams.aside(
            removal.verb(),
            &format!("kept [{name}] in dogs.toml; adopting {name} again finds those settings"),
        );
    }
    code
}

/// The daemon half of [`remove_dog`]; see enable_after_config for the split
/// and for what `client: None` means. Sends `Delete` by name: `DisableDog`
/// leaves the dog registered at rest, and this verb has erased the
/// registration that entry was listed under.
async fn remove_after_config(
    streams: &mut Streams<'_>,
    name: &str,
    source: Option<DogSource>,
    client: Option<&Client>,
    removal: Removal,
) -> ExitCode {
    let Some(client) = client else {
        return emit_dog_action(
            streams,
            removal.verb(),
            name,
            source,
            NO_SHEPHERD_DISABLE_STATUS,
        );
    };
    match client
        .request(Request::Delete {
            selector: SelectorSpec::Name(name.to_string()),
        })
        .await
    {
        Ok(Response::Deleted(_ids)) => {
            emit_dog_action(streams, removal.verb(), name, source, DISABLED_STATUS)
        }
        Ok(_unrecognised) => unexpected_response(streams),
        Err(err) => client_error(streams, &err),
    }
}

/// `shep rehome <name>`: the removal, answering under its own `command`
///
/// Hidden from `--help` because `delete` is the verb an operator should look
/// for. It stays reachable, and stays byte-identical in what it prints, for
/// the scripts written against it.
pub async fn rehome(streams: &mut Streams<'_>, paths: &ShepPaths, name: &str) -> ExitCode {
    remove_dog(streams, paths, name, Removal::Rehome).await
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use shep_client::testing::fake_client_capturing_envelopes;

    use super::*;
    use crate::cli::Format;
    use crate::commands::dogs::test_support::streams;
    use crate::output::DogActionStatus;

    /// Both places a dog's own settings can sit: a `[dog.otel]` an
    /// un-migrated `shep.toml` still carries, and the `[otel]` in
    /// `dogs.toml` where one lives now. Neither is the adoption, so
    /// neither goes. `metrics` is beside it to catch a rewrite that
    /// reaches further than it was asked to.
    #[tokio::test]
    async fn a_removal_keeps_the_settings_and_forgets_only_the_adoption() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.home).unwrap();
        // Seeded by hand: no writer creates a `[dog.<name>]` any more, but
        // a `shep.toml` no daemon has booted against since the move still
        // carries one, and that section is the operator's too.
        std::fs::write(&paths.daemon_config, "[dog.otel]\ndebounce = \"30s\"\n").unwrap();
        ShepToml::edit(&paths.daemon_config, |seed| {
            seed.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), false)
                .unwrap();
        })
        .unwrap();
        std::fs::write(
            &paths.dogs_config,
            "[otel]\nendpoint = \"127.0.0.1:4317\"\n\n[metrics]\nbind = \"127.0.0.1:9615\"\n",
        )
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = remove_dog(
            &mut streams(&mut out, &mut err),
            &paths,
            "otel",
            Removal::Delete,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        let cfg = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
        assert!(
            cfg.daemon.enabled_dogs.is_empty(),
            "a removal must remove the name from enabled_dogs: {written}"
        );
        assert!(
            !cfg.daemon.adopted_dogs.contains_key("otel"),
            "a removal must forget the adopted_dogs entry disable deliberately keeps: {written}"
        );
        assert!(
            cfg.dog.contains_key("otel"),
            "an un-migrated [dog.otel] is the operator's, not the adoption: {written}"
        );
        let dogs = std::fs::read_to_string(&paths.dogs_config).unwrap();
        let dogs = shep_core::config::DogsConfig::load(Some(&dogs)).unwrap();
        assert!(
            dogs.dog.contains_key("otel"),
            "a removal must leave the section in dogs.toml, where a dog's config lives now"
        );
        assert!(
            dogs.dog.contains_key("metrics"),
            "and must leave every other dog's section exactly where it was"
        );
        assert!(
            String::from_utf8(err).unwrap().contains("kept [otel]"),
            "an operator who asked to forget a dog is told what stayed"
        );
    }

    /// Rehoming a dog nobody ever configured must not invent an empty
    /// `dogs.toml`, fail over its absence, or claim it kept anything.
    #[tokio::test]
    async fn a_removal_with_no_dogs_toml_at_all_writes_none() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        ShepToml::edit(&paths.daemon_config, |seed| {
            seed.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), false)
                .unwrap();
        })
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = remove_dog(
            &mut streams(&mut out, &mut err),
            &paths,
            "otel",
            Removal::Rehome,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        assert!(
            !paths.dogs_config.exists(),
            "nothing to keep, nothing written"
        );
        assert!(
            !String::from_utf8(err).unwrap().contains("kept ["),
            "there was no section, so there is nothing to say was kept"
        );
    }

    /// `shepherd_acted` and `status` together separate "only the config
    /// changed" from "a shepherd acted", and this verb's two branches differ
    /// in little else. The pair is the guard: either field alone passes while
    /// the other branch carries the wrong one.
    #[tokio::test]
    async fn a_removal_reports_whether_a_shepherd_acted() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) =
            shep_client::testing::fake_client_answering(&path, |_| Response::Deleted(vec![3]))
                .await;

        for (client, expected) in [
            (Some(&client), DogActionStatus::Acted("stopped")),
            (None, NO_SHEPHERD_DISABLE_STATUS),
        ] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut streams = streams(&mut out, &mut err);
            streams.fmt = Format::Json;

            let code = remove_after_config(
                &mut streams,
                "otel",
                Some(DogSource::BuiltIn { grants: None }),
                client,
                Removal::Rehome,
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
    async fn a_removal_asks_the_shepherd_to_stop_that_dog() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = remove_after_config(
            &mut streams(&mut out, &mut err),
            "otel",
            Some(DogSource::Adopted {
                grants: None,
                path: "/usr/local/bin/shep-otel".to_string(),
                channel: false,
            }),
            Some(&client),
            Removal::Delete,
        )
        .await;

        let sent = envelopes.recv().await.unwrap();
        assert_eq!(
            sent.body,
            Request::Delete {
                selector: SelectorSpec::Name("otel".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn a_removal_with_no_shepherd_writes_the_config_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        ShepToml::edit(&paths.daemon_config, |seed| {
            seed.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"), false)
                .unwrap();
        })
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = remove_dog(
            &mut streams(&mut out, &mut err),
            &paths,
            "otel",
            Removal::Delete,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        let cfg = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
        assert!(cfg.daemon.enabled_dogs.is_empty());
        assert!(!cfg.daemon.adopted_dogs.contains_key("otel"));
    }

    /// The two verbs are one removal, so the only thing a script can tell them
    /// apart by is the `command` its envelope carries. Asserted as a pair: a
    /// build that spells both `delete` leaves one arm wrong, and a build that
    /// routes `rehome` through `delete`'s row type fails the shape assertion.
    #[tokio::test]
    async fn each_removal_answers_under_its_own_command() {
        for (removal, expected) in [(Removal::Delete, "delete"), (Removal::Rehome, "rehome")] {
            let dir = tempfile::tempdir().unwrap();
            let paths = ShepPaths::resolve(&|_| None, dir.path());
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut streams = streams(&mut out, &mut err);
            streams.fmt = Format::Json;

            let code = remove_dog(&mut streams, &paths, "otel", removal).await;

            assert_eq!(code, ExitCode::Success, "{expected}: {err:?}");
            let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(json["command"], expected, "{json}");
            assert_eq!(json["data"]["name"], "otel", "{json}");
        }
    }

    /// A built-in dog has no adoption to forget, so the refusal is the whole
    /// verb: `shep.toml` is not opened, and the name is not struck from
    /// `enabled_dogs` on the way to saying no.
    #[tokio::test]
    async fn a_removal_refuses_a_built_in_dog_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        ShepToml::try_edit(&paths.daemon_config, |seed| seed.enable_dog("metrics")).unwrap();

        for removal in [Removal::Delete, Removal::Rehome] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let code =
                remove_dog(&mut streams(&mut out, &mut err), &paths, "metrics", removal).await;

            assert_eq!(code, ExitCode::InvalidConfig, "{removal:?}");
            assert!(
                String::from_utf8(err)
                    .unwrap()
                    .contains(&shep_daemon::dogs::built_in_dog_refusal("metrics")),
                "{removal:?} must name the verb that does exist"
            );
            let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
            assert!(
                written.contains("metrics"),
                "{removal:?} refused, so it changed nothing: {written}"
            );
        }
    }
}
