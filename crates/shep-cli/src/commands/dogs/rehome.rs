//! `shep rehome`: stops an adopted dog and forgets where its binary lived,
//! leaving the settings an operator wrote for it.

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, Request, Response};

use crate::commands::dog_migration;
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;
use crate::output::{DogActionRow, Streams, emit, write_outcome};

use super::{DISABLED_STATUS, NO_SHEPHERD_DISABLE_STATUS, connect_or_absent, fail_config};

/// `shep rehome <name>`: stops an adopted dog and forgets where its binary
/// lived, leaving the settings an operator wrote for it.
///
/// One file: [`ShepToml::rehome_dog`] strikes the registration from
/// `shep.toml`. `dogs.toml` is only read, to say whether there was a
/// section to keep, on the argument `disable` already makes for itself.
/// Re-adopting the same dog finds its old configuration waiting; the
/// difference from `disable` is that recovery needs a fresh `shep adopt`.
pub async fn rehome(streams: &mut Streams<'_>, paths: &ShepPaths, name: &str) -> ExitCode {
    let source = match ShepToml::edit(&paths.daemon_config, |cfg| {
        // Read before `rehome_dog` erases it. `None` is legitimate: a name
        // never adopted, or a built-in dog's own.
        let source = cfg.adopted_dog_path(name).map(|path| DogSource::Adopted {
            path: path.display().to_string(),
        });
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
    let code = rehome_after_config(streams, name, source, client.as_ref()).await;
    if kept {
        streams.aside(
            "rehome",
            &format!("kept [{name}] in dogs.toml; adopting {name} again finds those settings"),
        );
    }
    code
}

/// `rehome`'s daemon half; see enable_after_config for the split and
/// for what `client: None` means. Sends the same `DisableDog` request
/// `disable` does: [`rehome`] has already erased the registration `disable`
/// leaves alone.
async fn rehome_after_config(
    streams: &mut Streams<'_>,
    name: &str,
    source: Option<DogSource>,
    client: Option<&Client>,
) -> ExitCode {
    let Some(client) = client else {
        let row = DogActionRow::new(name, source, NO_SHEPHERD_DISABLE_STATUS, false);
        return write_outcome(emit(
            &mut *streams.out,
            streams.fmt,
            "rehome",
            row,
            streams.style,
        ));
    };
    match client
        .request(Request::DisableDog {
            name: name.to_string(),
        })
        .await
    {
        Ok(Response::Deleted(_ids)) => {
            let row = DogActionRow::new(name, source, DISABLED_STATUS, true);
            write_outcome(emit(
                &mut *streams.out,
                streams.fmt,
                "rehome",
                row,
                streams.style,
            ))
        }
        Ok(_unrecognised) => unexpected_response(streams),
        Err(err) => client_error(streams, &err),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use shep_client::testing::fake_client_capturing_envelopes;

    use super::*;
    use crate::cli::Format;

    /// Every test here drives a dog verb under `--format table`.
    fn streams<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>) -> Streams<'a> {
        Streams {
            out,
            err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        }
    }

    /// Both places a dog's own settings can sit: a `[dog.otel]` an
    /// un-migrated `shep.toml` still carries, and the `[otel]` in
    /// `dogs.toml` where one lives now. Neither is the adoption, so
    /// neither goes. `metrics` is beside it to catch a rewrite that
    /// reaches further than it was asked to.
    #[tokio::test]
    async fn rehome_keeps_the_settings_and_forgets_only_the_adoption() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(&paths.home).unwrap();
        // Seeded by hand: no writer creates a `[dog.<name>]` any more, but
        // a `shep.toml` no daemon has booted against since the move still
        // carries one, and that section is the operator's too.
        std::fs::write(&paths.daemon_config, "[dog.otel]\ndebounce = \"30s\"\n").unwrap();
        ShepToml::edit(&paths.daemon_config, |seed| {
            seed.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"))
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
        let code = rehome(&mut streams(&mut out, &mut err), &paths, "otel").await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        let cfg = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
        assert!(
            cfg.daemon.enabled_dogs.is_empty(),
            "rehome must remove the name from enabled_dogs: {written}"
        );
        assert!(
            !cfg.daemon.adopted_dogs.contains_key("otel"),
            "rehome must forget the adopted_dogs entry disable deliberately keeps: {written}"
        );
        assert!(
            cfg.dog.contains_key("otel"),
            "an un-migrated [dog.otel] is the operator's, not the adoption: {written}"
        );
        let dogs = std::fs::read_to_string(&paths.dogs_config).unwrap();
        let dogs = shep_core::config::DogsConfig::load(Some(&dogs)).unwrap();
        assert!(
            dogs.dog.contains_key("otel"),
            "rehome must leave the section in dogs.toml, where a dog's config lives now"
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
    async fn rehoming_with_no_dogs_toml_at_all_writes_none() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        ShepToml::edit(&paths.daemon_config, |seed| {
            seed.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"))
                .unwrap();
        })
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = rehome(&mut streams(&mut out, &mut err), &paths, "otel").await;

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

    /// `shepherd_acted` separates "only the config changed" from "a shepherd
    /// acted", and this verb's two branches differ in little else. The pair
    /// is the guard: either case alone passes while the other branch carries
    /// the wrong flag.
    #[tokio::test]
    async fn rehome_reports_whether_a_shepherd_acted() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) =
            shep_client::testing::fake_client_answering(&path, |_| Response::Deleted(vec![3]))
                .await;

        for (client, expected) in [(Some(&client), true), (None, false)] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut streams = streams(&mut out, &mut err);
            streams.fmt = Format::Json;

            let code =
                rehome_after_config(&mut streams, "otel", Some(DogSource::BuiltIn), client).await;

            assert_eq!(code, ExitCode::Success);
            let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(
                json["data"]["shepherd_acted"],
                serde_json::json!(expected),
                "client present: {}, payload: {}",
                client.is_some(),
                String::from_utf8_lossy(&out)
            );
        }
    }

    #[tokio::test]
    async fn rehome_asks_the_shepherd_to_stop_that_dog() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = rehome_after_config(
            &mut streams(&mut out, &mut err),
            "otel",
            Some(DogSource::Adopted {
                path: "/usr/local/bin/shep-otel".to_string(),
            }),
            Some(&client),
        )
        .await;

        let sent = envelopes.recv().await.unwrap();
        assert_eq!(
            sent.body,
            Request::DisableDog {
                name: "otel".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn rehome_with_no_shepherd_writes_the_config_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        ShepToml::edit(&paths.daemon_config, |seed| {
            seed.adopt_dog("otel", Path::new("/usr/local/bin/shep-otel"))
                .unwrap();
        })
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = rehome(&mut streams(&mut out, &mut err), &paths, "otel").await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        let cfg = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
        assert!(cfg.daemon.enabled_dogs.is_empty());
        assert!(!cfg.daemon.adopted_dogs.contains_key("otel"));
    }
}
