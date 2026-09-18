use super::*;

/// Wall-clock tests, skipped by every CI job but the serial `slow` one
///
/// The `restart` cases fork and exec a dog's binary inside a budget. A
/// probe that runs out of budget answers unknown, and unknown is silent,
/// so contention turns their subject into thin air. A `/bin/sh` probe
/// takes single-digit milliseconds idle and over a second at
/// `--test-threads=64`. The node case needs a real node to start and
/// exit inside the budget, which is a claim about the machine.
/// The node case climbs a ladder of budgets: the pipe wait spends the
/// whole budget, so one number cannot be both cheap and enough. See
/// `BUDGETS`.
mod slow {
    /// A probe budget no contention can exhaust
    ///
    /// Not a claim about how long a probe should take: far enough above
    /// this suite's contention that timing stops being a variable.
    const PROBE_BUDGET: Duration = Duration::from_secs(30);

    use super::*;

    /// A shepherd that restarts whatever it is asked to and lists an
    /// empty flock afterwards
    ///
    /// Enough for `restart` to reach the end, so a test asserting an
    /// empty stderr is asserting about the dog check.
    #[cfg(unix)]
    fn answering_a_restart(request: &Request) -> Response {
        match request {
            Request::Restart { .. } => Response::Restarted {
                accepted: Vec::new(),
                refused: Vec::new(),
            },
            _ => Response::Flock(Vec::new()),
        }
    }

    /// The answer a dog gives when its binary was built against a
    /// protocol below the floor this shep's handshake still accepts.
    ///
    /// Was `PROTOCOL_VERSION + 1` under the name `stale_answer`, back
    /// when the restart check compared for exact equality. The
    /// handshake now accepts anything at or above
    /// [`shep_core::protocol::MIN_SUPPORTED`], so a newer protocol is
    /// no longer stale; only one below the floor is.
    #[cfg(unix)]
    fn below_floor_answer() -> String {
        format!(
            "echo 'shep-log-rotate 0.1.3'\necho 'shep-protocol: {}'",
            shep_core::protocol::MIN_SUPPORTED.saturating_sub(1)
        )
    }

    /// The ordering is read off stderr rather than off a clock: the
    /// shepherd refuses the restart, so its refusal lands on the same
    /// stream as the warning and the two offsets say which ran first.
    // Unix only because of the fixture: `adopted_dog` writes a
    // `#!/bin/sh` script, so on Windows the probe answers unknown and the
    // three `restarts_in_silence` tests would pass vacuously.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dog_whose_disk_binary_cannot_connect_is_warned_about_before_the_restart() {
        let dir = tempfile::tempdir().unwrap();
        let paths = adopted_dog(dir.path(), "log-rotate", &below_floor_answer());
        let sock = shep_client::testing::control_address(dir.path());
        let (client, _daemon) =
            fake_client_replying_err(&sock, RpcErrorCode::NotFound, "no such sheep").await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let args = SelectorArgs {
                selectors: vec!["log-rotate".into()],
            };
            let _ = restart_within(&client, &mut streams, &paths, &args, PROBE_BUDGET).await;
        }

        let text = String::from_utf8(err).unwrap();
        let warning = text
            .find("dog_binary_skew")
            .unwrap_or_else(|| panic!("the restart must warn first: {text}"));
        let refusal = text
            .find("no such sheep")
            .unwrap_or_else(|| panic!("the restart must still be attempted: {text}"));
        assert!(
            warning < refusal,
            "the warning must reach the operator before the restart does: {text}"
        );
    }

    /// The operator knows which of the two fixes they meant, so the
    /// message names both and tells them what the restart just did.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_restart_warning_names_both_numbers_and_both_ways_out() {
        let dir = tempfile::tempdir().unwrap();
        let paths = adopted_dog(dir.path(), "log-rotate", &below_floor_answer());
        let sock = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = fake_client_answering(&sock, answering_a_restart).await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let args = SelectorArgs {
                selectors: vec!["log-rotate".into()],
            };
            let _ = restart_within(&client, &mut streams, &paths, &args, PROBE_BUDGET).await;
        }

        let text = String::from_utf8(err).unwrap();
        let disk = shep_core::protocol::MIN_SUPPORTED
            .saturating_sub(1)
            .to_string();
        let floor = shep_core::protocol::MIN_SUPPORTED.to_string();
        assert!(
            text.contains(&disk) && text.contains(&floor),
            "the warning names both numbers: {text}"
        );
        assert!(
            text.contains("Run a shep that accepts protocol") && text.contains("reinstall the dog"),
            "the warning names both ways out, and picks neither: {text}"
        );
        assert!(
            text.contains("log-rotate"),
            "the warning names the dog it is about: {text}"
        );
    }

    /// Every restart of every working flock is in this case, so a line
    /// here is a line an operator learns to skip.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dog_whose_disk_binary_is_current_restarts_in_silence() {
        let dir = tempfile::tempdir().unwrap();
        let answer = format!(
            "echo 'shep-log-rotate 0.1.3'\necho 'shep-protocol: {}'",
            shep_client::PROTOCOL_VERSION
        );
        let paths = adopted_dog(dir.path(), "log-rotate", &answer);
        let sock = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_answering(&sock, answering_a_restart).await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let args = SelectorArgs {
                selectors: vec!["log-rotate".into()],
            };
            let _ = restart_within(&client, &mut streams, &paths, &args, PROBE_BUDGET).await;
        }

        assert_eq!(
            String::from_utf8(err).unwrap(),
            "",
            "a healthy dog restarts with nothing said about it"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), envelopes.recv())
                .await
                .expect("restart must reach the wire; it hung instead of sending a request")
                .unwrap()
                .body,
            Request::Restart {
                selector: SelectorSpec::Name("log-rotate".into()),
            },
            "and it is still restarted"
        );
    }

    /// `docs/dogs.md` promises that not answering is never held against
    /// a dog. Unknown is not stale: it is the state `adopt` records.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dog_that_answers_nothing_restarts_in_silence() {
        let dir = tempfile::tempdir().unwrap();
        let paths = adopted_dog(
            dir.path(),
            "log-rotate",
            "echo 'shep-log-rotate does not understand --version' >&2\nexit 2",
        );
        let sock = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_answering(&sock, answering_a_restart).await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let args = SelectorArgs {
                selectors: vec!["log-rotate".into()],
            };
            let _ = restart_within(&client, &mut streams, &paths, &args, PROBE_BUDGET).await;
        }

        assert_eq!(
            String::from_utf8(err).unwrap(),
            "",
            "a dog that does not answer is not a dog that is stale"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), envelopes.recv())
                .await
                .expect("restart must reach the wire; it hung instead of sending a request")
                .unwrap()
                .body,
            Request::Restart {
                selector: SelectorSpec::Name("log-rotate".into()),
            },
            "and it is still restarted"
        );
    }

    /// The second shape of unknown, reaching a different line from the
    /// dog that answers nothing at all: there is an answer here, it just
    /// does not say.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dog_that_names_no_protocol_restarts_in_silence() {
        let dir = tempfile::tempdir().unwrap();
        let paths = adopted_dog(dir.path(), "log-rotate", "echo 'shep-log-rotate 0.1.3'");
        let sock = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_answering(&sock, answering_a_restart).await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let args = SelectorArgs {
                selectors: vec!["log-rotate".into()],
            };
            let _ = restart_within(&client, &mut streams, &paths, &args, PROBE_BUDGET).await;
        }

        assert_eq!(
            String::from_utf8(err).unwrap(),
            "",
            "an unstated protocol is unknown, and unknown is not stale"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), envelopes.recv())
                .await
                .expect("restart must reach the wire; it hung instead of sending a request")
                .unwrap()
                .body,
            Request::Restart {
                selector: SelectorSpec::Name("log-rotate".into()),
            },
            "and it is still restarted"
        );
    }

    /// A built-in dog's binary is the shepherd's own, so there is no
    /// second thing to drift. The config here has a stale adopted dog in
    /// it too, so the silence is about this name rather than about an
    /// empty `adopted_dogs`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_built_in_dog_is_never_asked_what_its_binary_speaks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = adopted_dog(dir.path(), "log-rotate", &below_floor_answer());
        crate::commands::shep_toml::ShepToml::edit(&paths.daemon_config, |cfg| {
            cfg.enable_dog("metrics").unwrap();
        })
        .unwrap();
        let sock = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_answering(&sock, answering_a_restart).await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let args = SelectorArgs {
                selectors: vec!["metrics".into()],
            };
            let _ = restart_within(&client, &mut streams, &paths, &args, PROBE_BUDGET).await;
        }

        assert_eq!(
            String::from_utf8(err).unwrap(),
            "",
            "a built-in dog has no binary of its own to be stale"
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), envelopes.recv())
                .await
                .expect("restart must reach the wire; it hung instead of sending a request")
                .unwrap()
                .body,
            Request::Restart {
                selector: SelectorSpec::Name("metrics".into()),
            },
            "and it is still restarted"
        );
    }

    /// A module that exports its config and leaves a detached node
    /// holding the pipes shep reads. The holder outlives `budget`
    /// threefold, or the reads would finish and the case pass for the
    /// wrong reason.
    fn held_pipe_module(budget: Duration) -> String {
        format!(
            "require('child_process')\
                 .spawn(process.execPath, ['-e', 'setTimeout(()=>{{}},{})'], \
                 {{ detached: true, stdio: 'inherit' }})\
                 .unref(); \
                 module.exports = {{ app: [] }};",
            (budget * 3).as_millis()
        )
    }

    /// node itself exits here: `detached` plus `unref` takes the child
    /// off node's event loop, and `stdio: inherit` hands it the pipes
    /// shep is reading, so only the reads run out of budget.
    #[test]
    fn a_js_flockfile_leaving_a_process_on_the_pipe_says_that_instead() {
        if !node_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flock.js");

        // A ladder, not one number: `run_bounded` waits for node and then
        // spends the rest on the held pipe, so the budget is this case's
        // runtime. A `Killed` verdict is read as a slow machine and retried
        // with more room; the last rung falls through.
        const BUDGETS: [Duration; 3] = [
            Duration::from_secs(5),
            Duration::from_secs(20),
            Duration::from_secs(80),
        ];

        for (attempt, budget) in BUDGETS.iter().copied().enumerate() {
            std::fs::write(&path, held_pipe_module(budget)).unwrap();
            let err = evaluate_js_flockfile(&path, budget).unwrap_err();
            let message = err.to_string();

            // node never got as far as exiting, so there was nothing
            // holding a pipe yet to say anything about. Give it more
            // room. The last rung falls through instead, so a machine
            // that cannot do it in 80s fails with the message it earned.
            if message.contains("still running") && attempt + 1 < BUDGETS.len() {
                continue;
            }

            assert_eq!(target_exit_code(&err), ExitCode::InvalidConfig);
            assert!(
                message.contains("left behind still holds the output"),
                "got: {message}"
            );
            assert!(
                !message.contains("killed"),
                "node exited on its own, so nothing was killed: {message}"
            );
            return;
        }
    }
}
