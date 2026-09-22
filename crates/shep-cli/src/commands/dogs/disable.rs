//! `shep disable`: removes a dog from the config, and stops it if a
//! shepherd is running.

use std::path::Path;

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, Request, Response};

use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::shep_toml::{ShepToml, ShepTomlError};
use crate::exit::ExitCode;
use crate::output::{DogActionRow, Streams, emit, write_outcome};

use super::{
    DISABLED_STATUS, NO_SHEPHERD_DISABLE_STATUS, connect_or_absent, dog_source, fail_config,
};

/// `disable`'s config half: which [`DogSource`] `name` resolved to, and the
/// removal from `[daemon] enabled_dogs`.
///
/// `disable_dog` leaves `[daemon] adopted_dogs` alone, the difference
/// between `disable` and `rehome`, so the source reads the same before or
/// after the edit. Read for the report only: `DisableDog` carries a name and
/// nothing else.
///
/// # Errors
/// [`ShepTomlError`] if the read-modify-write underneath the edit failed.
pub(crate) fn disable_in_config(path: &Path, name: &str) -> Result<DogSource, ShepTomlError> {
    ShepToml::edit(path, |cfg| {
        let source = dog_source(cfg, name);
        cfg.disable_dog(name);
        source
    })
}

/// `shep disable <name>`: removes it from the config, and stops it if a
/// shepherd is running.
pub async fn disable(streams: &mut Streams<'_>, paths: &ShepPaths, name: &str) -> ExitCode {
    let source = match disable_in_config(&paths.daemon_config, name) {
        Ok(source) => source,
        Err(err) => return fail_config(streams, &err),
    };
    let client = match connect_or_absent(paths, streams).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    disable_after_config(streams, name, &source, client.as_ref()).await
}

/// `disable`'s daemon half; see enable_after_config for the split and
/// for what `client: None` means.
async fn disable_after_config(
    streams: &mut Streams<'_>,
    name: &str,
    source: &DogSource,
    client: Option<&Client>,
) -> ExitCode {
    let Some(client) = client else {
        let row = DogActionRow::new(name, source.clone(), NO_SHEPHERD_DISABLE_STATUS, false);
        return write_outcome(emit(
            &mut *streams.out,
            streams.fmt,
            "disable",
            row,
            streams.style,
        ));
    };
    // `Response::Deleted`, the same reply `Delete` gives: disabling
    // deregisters exactly as `Delete` does.
    match client
        .request(Request::DisableDog {
            name: name.to_string(),
        })
        .await
    {
        Ok(Response::Deleted(_ids)) => {
            let row = DogActionRow::new(name, source.clone(), DISABLED_STATUS, true);
            write_outcome(emit(
                &mut *streams.out,
                streams.fmt,
                "disable",
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
    use shep_client::testing::{fake_client_capturing_envelopes, fake_client_replying_err};
    use shep_core::protocol::RpcErrorCode;

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

    #[test]
    fn disable_in_config_removes_the_name_and_keeps_the_adoption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shep.toml");
        std::fs::write(
            &path,
            "[daemon]\nenabled_dogs = [\"otel\"]\n\n[daemon.adopted_dogs]\notel = \"/usr/local/bin/shep-otel\"\n",
        )
        .unwrap();

        let source = disable_in_config(&path, "otel").unwrap();

        assert!(matches!(source, DogSource::Adopted { .. }));
        let cfg = ShepToml::read_only(&path).unwrap();
        assert!(cfg.enabled_dog_names().is_empty());
        assert_eq!(
            cfg.adopted_dog_names(),
            vec!["otel".to_string()],
            "disable is not rehome, so the adoption survives"
        );
    }

    /// A hand-written `shep.toml` can carry a name that answers to no dog,
    /// and `shep disable <name>` is the only way back out of `enabled_dogs`.
    #[tokio::test]
    async fn disable_still_removes_a_name_enable_would_now_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        std::fs::create_dir_all(paths.daemon_config.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.daemon_config,
            "[daemon]\nenabled_dogs = [\"pydog\", \"metrics\"]\n",
        )
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = disable(&mut streams(&mut out, &mut err), &paths, "pydog").await;

        assert_eq!(code, ExitCode::Success, "{}", String::from_utf8_lossy(&err));
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(
            !written.contains("pydog"),
            "disable is the escape hatch out of a config enable would now \
             refuse to write: {written}"
        );
        assert!(
            written.contains("metrics"),
            "and it touches nothing else: {written}"
        );
    }

    /// `shepherd_acted` is the one field telling a `--format json` consumer
    /// whether a shepherd was reached or only the config changed, and the two
    /// branches here differ in nothing else a test was reading. It shipped as
    /// `false` on both, so this pins the reached branch.
    #[tokio::test]
    async fn disable_reports_that_the_shepherd_acted_when_one_answered() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) =
            shep_client::testing::fake_client_answering(&path, |_| Response::Deleted(vec![7]))
                .await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = streams(&mut out, &mut err);
        streams.fmt = Format::Json;

        let code =
            disable_after_config(&mut streams, "bark", &DogSource::BuiltIn, Some(&client)).await;

        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            json["data"]["shepherd_acted"],
            serde_json::json!(true),
            "a shepherd answered Deleted, so it acted: {}",
            String::from_utf8_lossy(&out)
        );
        assert_eq!(json["data"]["status"], DISABLED_STATUS);
    }

    #[tokio::test]
    async fn disable_asks_the_shepherd_to_stop_that_dog() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = disable_after_config(
            &mut streams(&mut out, &mut err),
            "bark",
            &DogSource::BuiltIn,
            Some(&client),
        )
        .await;

        let sent = envelopes.recv().await.unwrap();
        assert_eq!(
            sent.body,
            Request::DisableDog {
                name: "bark".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn disable_with_no_shepherd_writes_the_config_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        ShepToml::try_edit(&paths.daemon_config, |seed| seed.enable_dog("bark")).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = disable(&mut streams(&mut out, &mut err), &paths, "bark").await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        let cfg = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
        assert!(
            cfg.daemon.enabled_dogs.is_empty(),
            "disable must remove the name from enabled_dogs: {written}"
        );
    }

    /// `disable` reuses `Delete`'s own selector path, so a dog not
    /// registered answers `NotFound` as `shep stop` would.
    #[tokio::test]
    async fn disable_of_a_dog_the_shepherd_does_not_have_reports_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _daemon) =
            fake_client_replying_err(&path, RpcErrorCode::NotFound, "no sheep matched").await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = disable_after_config(
            &mut streams(&mut out, &mut err),
            "ghost",
            &DogSource::BuiltIn,
            Some(&client),
        )
        .await;
        assert_eq!(code, ExitCode::NotFound);
    }
}
