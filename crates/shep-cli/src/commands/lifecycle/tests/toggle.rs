//! `shep disable` and `shep enable` on a sheep: what reaches the wire, what
//! is refused before it, and what the operator is told.

use super::*;
use crate::commands::lifecycle::toggle::{Toggle, toggle, toggle_sheep, toggle_with};

fn stopped_api(disabled: bool) -> ProcessInfo {
    ProcessInfo::builder(0, "api", shep_core::status::ProcStatus::Stopped)
        .disabled(disabled.then_some(true))
        .build()
}

/// Runs `verb name` against `client`, handing back the code and both streams.
async fn toggle_against(
    client: &Client,
    verb: Toggle,
    name: &str,
    fmt: Format,
) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt,
        };
        toggle_sheep(client, &mut streams, verb, name, &[]).await
    };
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// fails if the CLI sends anything but the app's exact name, such as a
/// regex or an id, for `disable api`.
#[tokio::test]
async fn disable_sends_the_app_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_answering(&path, |request| match request {
        Request::Disable { .. } => Response::Disabled(vec![stopped_api(true)]),
        Request::ListFlock => Response::Flock(vec![stopped_api(true)]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) =
        toggle_against(&client, Toggle::Disable, "api", Format::Table).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(
        printed.contains("disabled"),
        "STATUS shows the mark: {printed}"
    );
    let sent = envelopes.recv().await.unwrap();
    assert_eq!(
        sent.body,
        Request::Disable {
            selector: SelectorSpec::Name("api".to_string())
        }
    );
}

#[tokio::test]
async fn enable_sends_the_app_name_and_prints_the_started_sheep() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let online = ProcessInfo::builder(0, "api", shep_core::status::ProcStatus::Online).build();
    let listed = online.clone();
    let (client, mut envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Enable { .. } => Response::Enabled(vec![online.clone()]),
        Request::ListFlock => Response::Flock(vec![listed.clone()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = toggle_against(&client, Toggle::Enable, "api", Format::Table).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(printed.contains("online"), "{printed}");
    assert_eq!(
        envelopes.recv().await.unwrap().body,
        Request::Enable {
            selector: SelectorSpec::Name("api".to_string())
        }
    );
}

/// fails if enabling a sheep that is not disabled prints a table as though
/// something started, or exits non-zero.
#[tokio::test]
async fn enabling_a_sheep_that_is_not_disabled_says_so_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, |request| match request {
        Request::Enable { .. } => Response::Enabled(Vec::new()),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = toggle_against(&client, Toggle::Enable, "api", Format::Table).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(
        printed.contains("api") && printed.contains("not disabled"),
        "the operator is told nothing changed: {printed}"
    );
    assert!(
        !printed.contains("online"),
        "no table for a no-op: {printed}"
    );
}

/// fails if `name:slot` reaches the wire, or is refused without naming the
/// app the operator should have typed.
#[tokio::test]
async fn an_instance_is_refused_with_usage_naming_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;

    for verb in [Toggle::Disable, Toggle::Enable] {
        let (code, printed, said) = toggle_against(&client, verb, "api:1", Format::Table).await;

        assert_eq!(code, ExitCode::Usage, "{said}");
        assert!(said.contains("`api`"), "names the app: {said}");
        assert!(printed.is_empty(), "{printed}");
    }
    assert!(
        envelopes.try_recv().is_err(),
        "a refused instance sends nothing"
    );
}

/// fails if a bare id is widened by the CLI or sent, rather than refused
/// with the name of the app that id belongs to.
#[tokio::test]
async fn an_id_is_refused_with_usage_naming_its_app() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let three = ProcessInfo::builder(3, "api", shep_core::status::ProcStatus::Online).build();
    let (client, mut envelopes) = fake_client_answering(&path, move |request| match request {
        Request::ListFlock => Response::Flock(vec![three.clone()]),
        _ => Response::Pong,
    })
    .await;

    let (code, _printed, said) = toggle_against(&client, Toggle::Disable, "3", Format::Table).await;

    assert_eq!(code, ExitCode::Usage, "{said}");
    assert!(said.contains("`api`"), "names the app: {said}");
    while let Ok(envelope) = envelopes.try_recv() {
        assert_eq!(envelope.body, Request::ListFlock, "only a lookup is sent");
    }
}

/// fails if a wildcard is refused client-side, or is widened or narrowed
/// before it reaches the shepherd, which owns what a sweep matches.
#[tokio::test]
async fn a_wildcard_is_sent_to_the_shepherd_as_typed() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_answering(&path, |request| match request {
        Request::Disable { .. } => Response::Disabled(vec![stopped_api(true)]),
        Request::Enable { .. } => Response::Enabled(vec![stopped_api(false)]),
        Request::ListFlock => Response::Flock(vec![stopped_api(true)]),
        _ => Response::Pong,
    })
    .await;

    let targets = [
        ("all", SelectorSpec::All),
        ("/api/", SelectorSpec::Regex("api".to_string())),
        ("fold:backed", SelectorSpec::Fold("backed".to_string())),
    ];
    for (target, selector) in targets {
        for verb in [Toggle::Disable, Toggle::Enable] {
            let (code, _printed, said) = toggle_against(&client, verb, target, Format::Table).await;
            assert_eq!(code, ExitCode::Success, "{target}: {said}");
            let sent = loop {
                let body = envelopes.recv().await.unwrap().body;
                if body != Request::ListFlock {
                    break body;
                }
            };
            let expected = match verb {
                Toggle::Disable => Request::Disable {
                    selector: selector.clone(),
                },
                Toggle::Enable => Request::Enable {
                    selector: selector.clone(),
                },
            };
            assert_eq!(sent, expected, "{target}");
        }
    }
}

/// fails if `enable all` with nothing disabled prints a table as though
/// something started.
#[tokio::test]
async fn enable_all_with_nothing_disabled_says_so_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, |request| match request {
        Request::Enable { .. } => Response::Enabled(Vec::new()),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = toggle_against(&client, Toggle::Enable, "all", Format::Table).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(
        printed.contains("matching `all`") && printed.contains("nothing changed"),
        "{printed}"
    );
}

/// fails if `disable all` with every sheep already disabled prints a table or
/// exits non-zero for work that is already done.
#[tokio::test]
async fn disable_all_with_everything_disabled_says_so_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, |request| match request {
        Request::Disable { .. } => Response::Disabled(Vec::new()),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) =
        toggle_against(&client, Toggle::Disable, "all", Format::Table).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(
        printed.contains("already disabled") && printed.contains("nothing changed"),
        "{printed}"
    );
}

/// fails if an older shepherd's `Unsupported` reaches the operator as a bare
/// protocol error instead of telling them to upgrade it.
#[tokio::test]
async fn an_older_shepherd_is_told_to_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _daemon) =
        fake_client_replying_err(&path, RpcErrorCode::Unsupported, "unknown variant").await;

    let (code, _printed, said) =
        toggle_against(&client, Toggle::Disable, "api", Format::Table).await;

    assert_eq!(code, ExitCode::Unsupported, "{said}");
    assert!(
        said.contains("upgrad") && said.contains("shepherd"),
        "{said}"
    );
}

/// fails if the shepherd's `Disabled` refusal stops exiting 2 or loses the
/// `shep enable` it names.
#[tokio::test]
async fn a_disabled_refusal_exits_usage_and_names_shep_enable() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _daemon) = fake_client_replying_err(
        &path,
        RpcErrorCode::Disabled,
        "api is disabled; run `shep enable api`",
    )
    .await;

    let (code, _printed, said) =
        toggle_against(&client, Toggle::Disable, "api", Format::Table).await;

    assert_eq!(code, ExitCode::Usage, "{said}");
    assert!(said.contains("shep enable api"), "{said}");
}

/// fails if an unknown name loses its own exit code and the shepherd's words.
#[tokio::test]
async fn an_unknown_name_keeps_its_exit_code_and_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _daemon) =
        fake_client_replying_err(&path, RpcErrorCode::NotFound, "no sheep named api").await;

    let (code, _printed, said) =
        toggle_against(&client, Toggle::Enable, "api", Format::Table).await;

    assert_eq!(code, ExitCode::NotFound, "{said}");
    assert!(said.contains("no sheep named api"), "{said}");
}

/// fails if `disable api` with no shepherd falls through to a silent
/// success, or autostarts one.
#[tokio::test]
async fn with_no_shepherd_a_sheep_is_refused_and_the_remedy_is_named() {
    let home = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, home.path());

    for verb in [Toggle::Disable, Toggle::Enable] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            toggle(&mut streams, &paths, verb, "api", true).await
        };
        let said = String::from_utf8(err).unwrap();

        assert_eq!(code, ExitCode::DaemonUnreachable, "{said}");
        assert!(
            said.contains("no shepherd") && said.contains("shep start"),
            "{said}"
        );
        assert!(out.is_empty());
    }
}

/// fails if a dog's name stops going the way it always has: through the
/// config, with no shepherd required.
#[tokio::test]
async fn a_dogs_name_still_edits_the_config_with_no_shepherd() {
    let home = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, home.path());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        toggle(&mut streams, &paths, Toggle::Enable, "metrics", true).await
    };

    assert_eq!(code, ExitCode::Success, "{}", String::from_utf8_lossy(&err));
    let config = std::fs::read_to_string(&paths.daemon_config).unwrap();
    assert!(config.contains("metrics"), "{config}");
}

/// A dog's name over a host is a selector like any other: the dog half reads
/// and edits this machine's `shep.toml`, and the host's dogs are not in it.
/// `mydog` is adopted here, so a local read would send it down the dog path and
/// never to the wire.
#[tokio::test]
async fn enable_over_a_host_treats_a_dog_name_as_a_sheep_selector_and_reads_no_local_shep_toml() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.run).unwrap();
    crate::commands::shep_toml::ShepToml::edit(&paths.daemon_config, |cfg| {
        cfg.adopt_dog("mydog", std::path::Path::new("/usr/local/bin/mydog"), false)
            .unwrap();
    })
    .unwrap();
    let config_before = std::fs::read(&paths.daemon_config).unwrap();
    let (client, mut envelopes) = fake_client_answering(&paths.socket, |request| match request {
        Request::Enable { .. } => Response::Enabled(Vec::new()),
        _ => Response::Pong,
    })
    .await;
    let mut out = Vec::new();
    let mut err = Vec::new();

    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        toggle_with(
            &mut streams,
            None,
            async |_| Ok(client),
            Toggle::Enable,
            "mydog",
            true,
        )
        .await
    };

    let said = String::from_utf8(err).unwrap();
    assert_eq!(code, ExitCode::Success, "{said}");
    assert_eq!(
        envelopes.recv().await.unwrap().body,
        Request::Enable {
            selector: SelectorSpec::Name("mydog".to_string())
        },
        "the host's shepherd is the one that knows whether this name is a dog"
    );
    assert_eq!(std::fs::read(&paths.daemon_config).unwrap(), config_before);
    assert!(!paths.dogs_config.exists(), "no dog verb ran here");
}

/// fails if a missing remote shepherd sends the operator to `shep start` on
/// this machine, which would start the wrong one.
#[cfg(unix)]
#[tokio::test]
async fn disable_over_a_host_with_no_remote_shepherd_names_the_host_not_shep_start() {
    use std::os::unix::fs::PermissionsExt as _;

    use crate::commands::lifecycle::toggle::toggle_on_host;

    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("ssh");
    std::fs::write(
        &program,
        "#!/bin/sh\necho 'no shepherd is running' >&2\nexit 5\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let link = crate::remote::Link::Host {
        dest: "amp2",
        options: shep_client::SshOptions::default().program(program),
    };
    let mut out = Vec::new();
    let mut err = Vec::new();

    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        toggle_on_host(
            &mut streams,
            &link,
            crate::version_guard::VersionGuard::Enforce,
            Toggle::Disable,
            "api",
        )
        .await
    };

    let said = String::from_utf8(err).unwrap();
    assert_eq!(code, ExitCode::DaemonUnreachable, "{said}");
    assert!(said.contains("no shepherd is running on `amp2`"), "{said}");
    assert!(!said.contains("shep start"), "{said}");
    assert!(out.is_empty());
}
