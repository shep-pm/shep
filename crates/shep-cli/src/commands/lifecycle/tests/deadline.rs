use super::*;

#[test]
fn a_staged_start_allows_the_daemon_its_slack_on_every_stage() {
    // fails if the client's slack is flat: the daemon spends
    // `boot_order::STAGE_SLACK` (5s) per stage, so over three stages its
    // worst case is 60s of timeouts plus 15s of slack, and a single 10s
    // made the client abandon at 70s a start the shepherd would still be
    // running at 75s
    let apps: Vec<AppConfig> = ["db", "api", "web"]
        .iter()
        .map(|name| {
            let mut app = AppConfig::minimal(name, "/bin/sleep");
            app.listen_timeout = shep_core::values::UpDuration::from_millis(20_000);
            app
        })
        .collect();

    assert_eq!(
        staged_start_deadline(&apps),
        Duration::from_secs(75),
        "three 20s stages plus 5s of slack each"
    );
}

#[tokio::test]
async fn a_start_asks_for_a_deadline_its_own_stages_fit_inside() {
    // fails if `shep start` sends a fixed budget: the daemon holds every
    // stage for its members' listen_timeout, so a flock whose timeouts
    // sum past `START_DEADLINE` is abandoned by the client while the
    // shepherd is doing exactly what it was asked
    let dir = tempfile::tempdir().unwrap();
    // Two apps at 40s each, so the sum clears `START_DEADLINE` by enough
    // that neither app's timeout alone would.
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"db\"\nscript = \"/bin/sleep\"\nlisten_timeout = \"40s\"\n\
             [[app]]\nname = \"api\"\nscript = \"/bin/sleep\"\nlisten_timeout = \"40s\"\n\
             depends_on = [\"db\"]\n",
    )
    .unwrap();

    let sock = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&sock).await;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = StartArgs {
        targets: vec![flockfile.to_string_lossy().into_owned()],
        name: None,
        fold: None,
        cwd: None,
        interpreter: None,
        flockfile: false,
        reset: None,
    };
    {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let _ = start(&client, &mut streams, &args, None, &BTreeMap::new()).await;
    }

    // The first envelope is the `ListFlock` the bare-name rule needs; the
    // `Start` is the one under test.
    let mut deadlines = Vec::new();
    while let Ok(sent) = envelopes.try_recv() {
        if matches!(sent.body, Request::Start { .. }) {
            deadlines.push(sent.deadline_ms);
        }
    }
    assert_eq!(
        deadlines,
        vec![Some(90_000)],
        "two 40s stages plus STAGED_START_SLACK, not a fixed budget"
    );
}

/// `"/[/"` is one of the only three inputs the selector grammar rejects.
/// A verb skipping the client-side parse would send it and exit
/// `NotFound`.
#[tokio::test]
async fn a_malformed_selector_exits_usage_without_a_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        stop(
            &client,
            &mut streams,
            &SelectorArgs {
                selectors: vec!["/[/".into()],
            },
        )
        .await
    };
    assert_eq!(code, ExitCode::Usage);
    assert!(
        envelopes.try_recv().is_err(),
        "a malformed selector must fail locally"
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
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style: crate::style::Presentation::BARE,
        fmt: Format::Table,
    };
    let code = stop(
        &client,
        &mut streams,
        &SelectorArgs {
            selectors: vec!["ghost".into()],
        },
    )
    .await;
    assert_eq!(code, ExitCode::NotFound);
}

/// Bounded by `timeout`: `start` returns early whenever `resolve_target`
/// fails, before any request is built, so a regression would hang on
/// `envelopes.recv()`. A `.toml` name would not substitute for the
/// fixture, since that extension routes into `Flockfile::parse`.
#[tokio::test]
async fn start_asks_for_the_longer_deadline() {
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

    let _ = start(
        &client,
        &mut streams,
        &start_args(srv.to_str().unwrap()),
        None,
        &BTreeMap::new(),
    )
    .await;

    let sent = next_start(&mut envelopes).await;
    assert_eq!(
        sent.deadline_ms,
        Some(u64::try_from(START_DEADLINE.as_millis()).unwrap())
    );
    // `cwd` comes along: a script started by path runs where the
    // operator stood. The redacted `Debug` does not print it, so a
    // mismatch reads as two identical-looking values.
    let mut expected = AppConfig::minimal("srv", srv.to_str().unwrap());
    expected.cwd = std::env::current_dir()
        .ok()
        .map(|dir| dir.to_string_lossy().into_owned());
    assert_eq!(
        sent.body,
        Request::Start {
            apps: vec![expected]
        }
    );
}
