use super::*;

/// The fold has to land on the `AppConfig` that reaches the wire:
/// deleting the `if let Some(fold)` loop leaves every other test here
/// green.
#[tokio::test]
async fn a_fold_flag_lands_on_the_resolved_app() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
    let srv = dir.path().join("srv");
    std::fs::write(&srv, "").unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style: crate::style::Presentation::BARE,
        fmt: Format::Table,
    };
    let mut args = start_args(srv.to_str().unwrap());
    args.fold = Some("backend".to_string());

    let _ = start(&client, &mut streams, &args, None, &BTreeMap::new()).await;

    let sent = next_start(&mut envelopes).await;
    match sent.body {
        Request::Start { apps } => {
            assert_eq!(apps.len(), 1);
            assert_eq!(apps[0].fold.as_deref(), Some("backend"));
        }
        other => panic!("expected Request::Start, got {other:?}"),
    }
}

/// Matched with the dot stripped, absent for a name with no extension,
/// and absent for a dotfile whose one dot leads rather than separates.
#[test]
fn mapped_interpreter_reads_the_extension_without_its_dot() {
    let mut interpreters = BTreeMap::new();
    interpreters.insert("js".to_string(), "node".to_string());

    assert_eq!(
        mapped_interpreter("server.js", &interpreters),
        Some("node".to_string())
    );
    assert_eq!(mapped_interpreter("server", &interpreters), None);
    assert_eq!(mapped_interpreter(".bashrc", &interpreters), None);
    assert_eq!(mapped_interpreter("server.py", &interpreters), None);
}

/// Layer 1. Without it the quick start `welcome.rs` that `--help`
/// advertises fails with `spawn_failed`.
#[tokio::test]
async fn a_shep_toml_mapping_fills_an_unset_interpreter() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
    let srv = dir.path().join("srv.js");
    std::fs::write(&srv, "").unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style: crate::style::Presentation::BARE,
        fmt: Format::Table,
    };
    let mut interpreters = BTreeMap::new();
    interpreters.insert("js".to_string(), "node".to_string());

    let _ = start(
        &client,
        &mut streams,
        &start_args(srv.to_str().unwrap()),
        None,
        &interpreters,
    )
    .await;

    let sent = next_start(&mut envelopes).await;
    match sent.body {
        Request::Start { apps } => {
            assert_eq!(apps.len(), 1);
            assert_eq!(apps[0].interpreter.as_deref(), Some("node"));
        }
        other => panic!("expected Request::Start, got {other:?}"),
    }
}

/// Layer 2: the mapping is a fallback, not a policy.
#[tokio::test]
async fn a_flockfile_interpreter_outranks_the_shep_toml_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"demo\"\nscript = \"server.js\"\ninterpreter = \"bun\"\n",
    )
    .unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style: crate::style::Presentation::BARE,
        fmt: Format::Table,
    };
    let mut interpreters = BTreeMap::new();
    interpreters.insert("js".to_string(), "node".to_string());

    let _ = start(
        &client,
        &mut streams,
        &start_args(flockfile.to_str().unwrap()),
        None,
        &interpreters,
    )
    .await;

    let sent = next_start(&mut envelopes).await;
    match sent.body {
        Request::Start { apps } => {
            assert_eq!(apps.len(), 1);
            assert_eq!(apps[0].interpreter.as_deref(), Some("bun"));
        }
        other => panic!("expected Request::Start, got {other:?}"),
    }
}

/// Layer 3, the one-off override typed on the command line.
#[tokio::test]
async fn the_interpreter_flag_outranks_a_flockfiles_own_field() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"demo\"\nscript = \"server.js\"\ninterpreter = \"bun\"\n",
    )
    .unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style: crate::style::Presentation::BARE,
        fmt: Format::Table,
    };
    let mut args = start_args(flockfile.to_str().unwrap());
    args.interpreter = Some("deno".to_string());
    let mut interpreters = BTreeMap::new();
    interpreters.insert("js".to_string(), "node".to_string());

    let _ = start(&client, &mut streams, &args, None, &interpreters).await;

    let sent = next_start(&mut envelopes).await;
    match sent.body {
        Request::Start { apps } => {
            assert_eq!(apps.len(), 1);
            assert_eq!(apps[0].interpreter.as_deref(), Some("deno"));
        }
        other => panic!("expected Request::Start, got {other:?}"),
    }
}

#[test]
fn any_restart_failed_is_true_only_for_an_errored_row() {
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    let online = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
    let errored = ProcessInfo::builder(2, "worker", ProcStatus::Errored).build();
    assert!(!any_restart_failed(std::slice::from_ref(&online)));
    assert!(any_restart_failed(&[online, errored]));
}

/// `stock` parses no selector, and a copy-pasted `parse_selector` would
/// send a `SelectorSpec::Name("web")` frame the daemon has no arm for.
#[tokio::test]
async fn the_request_carries_the_app_name_and_the_count() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style: crate::style::Presentation::BARE,
        fmt: Format::Table,
    };
    let _ = stock(
        &client,
        &mut streams,
        &StockArgs {
            name: "web".to_string(),
            count: 4,
        },
    )
    .await;

    let envelope = envelopes.recv().await.unwrap();
    assert_eq!(
        envelope.body,
        Request::Scale {
            name: "web".to_string(),
            count: 4,
        }
    );
}

/// A count of 0 is the shape an operator will type, and it has to come
/// back as exit 4 with the daemon's own sentence.
#[tokio::test]
async fn an_invalid_stock_exits_invalid_config_and_prints_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _served) = fake_client_replying_err(
        &path,
        RpcErrorCode::InvalidConfig,
        "an app runs at least one instance; use `shep delete web` to remove it",
    )
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
        stock(
            &client,
            &mut streams,
            &StockArgs {
                name: "web".to_string(),
                count: 1,
            },
        )
        .await
    };
    assert_eq!(code, ExitCode::InvalidConfig);
    assert!(
        String::from_utf8(err).unwrap().contains("shep delete web"),
        "the daemon's own sentence has to reach the operator"
    );
}
