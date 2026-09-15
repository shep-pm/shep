use shep_client::PROTOCOL_VERSION;
use shep_core::protocol::MIN_SUPPORTED;

use super::*;
use crate::cli::AdoptArgs;
use crate::commands::dogs::adopt;

/// Every test here drives a dog verb under `--format table`.
fn streams<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>) -> Streams<'a> {
    Streams {
        out,
        err,
        style: crate::style::Presentation::BARE,
        fmt: crate::cli::Format::Table,
    }
}

fn chmod(path: &Path, mode: u32) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, mode);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Each refusal must name its own cause: "not executable" for a missing
/// path sends an operator to `chmod` a file that is not there.
#[test]
fn a_binary_shep_has_never_seen_is_vetted_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        vet_binary_within(&dir.path().join("nope"), dir.path(), "probe", TEST_BUDGET),
        Err(AdoptRefusal::Missing)
    );
    assert_eq!(
        vet_binary_within(dir.path(), dir.path(), "probe", TEST_BUDGET),
        Err(AdoptRefusal::NotAFile)
    );

    let plain = dir.path().join("plain");
    std::fs::write(&plain, "#!/bin/sh\nexit 0\n").unwrap();
    assert_eq!(
        vet_binary_within(&plain, dir.path(), "probe", TEST_BUDGET),
        Err(AdoptRefusal::NotExecutable)
    );

    // The same file, now executable: only the mode bit changed, so a
    // refusal for another reason fails here.
    chmod(&plain, 0o755);
    let vetted = vet_binary_within(&plain, dir.path(), "probe", TEST_BUDGET).unwrap();
    assert_eq!(vetted.path, plain.canonicalize().unwrap());
    assert!(
        vetted.group_writable.is_empty(),
        "an 0o755 binary in an 0o700 directory has nothing to warn about: {vetted:?}"
    );

    // Executable, and not something this kernel can run.
    let bogus = dir.path().join("bogus");
    std::fs::write(&bogus, b"\x7fELF\x00\x00\x00 not really").unwrap();
    chmod(&bogus, 0o755);
    assert!(matches!(
        vet_binary_within(&bogus, dir.path(), "probe", TEST_BUDGET),
        Err(AdoptRefusal::WillNotExec { .. })
    ));
}

#[test]
fn a_binary_any_user_can_rewrite_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("dog");
    std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
    chmod(&bin, 0o757);
    assert_eq!(
        vet_binary_within(&bin, dir.path(), "probe", TEST_BUDGET),
        Err(AdoptRefusal::WorldWritable {
            path: bin.canonicalize().unwrap(),
        }),
        "a world-writable binary must be refused"
    );

    // The file is now sound; the directory holding it is not.
    chmod(&bin, 0o755);
    chmod(dir.path(), 0o777);
    assert_eq!(
        vet_binary_within(&bin, dir.path(), "probe", TEST_BUDGET),
        Err(AdoptRefusal::WorldWritable {
            path: bin.canonicalize().unwrap().parent().unwrap().to_path_buf(),
        }),
        "a world-writable directory must be refused too"
    );
    // Restored so the tempdir cleans up from a known state.
    chmod(dir.path(), 0o700);
}

/// Writes an executable `/bin/sh` script at `dir/name` that dispatches
/// on `$1`: `version_body` for the version flag, `schema_body` for the
/// schema flag, nothing for any other argument.
///
/// A fixture answering every flag the same way answers the schema flag
/// with unreadable JSON, so every other test would carry that warning.
fn probe_script(dir: &Path, name: &str, version_body: &str, schema_body: &str) -> PathBuf {
    let path = dir.join(name);
    let body = format!(
        "#!/bin/sh\ncase \"$1\" in\n--version)\n{version_body}\n;;\n\
             --schema)\n{schema_body}\n;;\nesac\n"
    );
    std::fs::write(&path, body).unwrap();
    chmod(&path, 0o755);
    path
}

/// A dog that answers the version flag with `body` and the schema flag
/// with nothing.
fn dog_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    probe_script(dir, name, body, "exit 0")
}

/// Renamed from `a_dog_that_speaks_another_protocol_is_refused_at_adopt`,
/// whose name and body asserted a refusal for exactly this input. The
/// daemon's own handshake compares against a floor with no upper bound,
/// so a dog newer than this shep now has to adopt cleanly here too, or
/// `shep adopt` would refuse a dog the shepherd would happily serve.
#[tokio::test]
async fn a_dog_above_this_protocol_is_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let newer = PROTOCOL_VERSION + 1;
    let bin = dog_script(
        dir.path(),
        "shep-otel",
        &format!("echo 'shep-otel 0.1.3'\necho 'shep-protocol: {newer}'"),
    );

    assert!(vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).is_ok());

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::Success, "a newer protocol is not a refusal");
    assert!(
        paths.daemon_config.exists(),
        "an accepted adopt writes shep.toml"
    );
}

/// Adopting one would make an online-and-idle entry whose failure
/// surfaces days later, at a handshake nobody is watching.
#[tokio::test]
async fn a_dog_below_the_floor_is_refused_at_adopt() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let stale = MIN_SUPPORTED.saturating_sub(1);
    let bin = dog_script(
        dir.path(),
        "shep-otel",
        &format!("echo 'shep-otel 0.1.3'\necho 'shep-protocol: {stale}'"),
    );

    assert_eq!(
        vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET),
        Err(AdoptRefusal::ProtocolMismatch {
            dog: stale,
            min: MIN_SUPPORTED,
        })
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::InvalidConfig);
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.contains(&stale.to_string()) && text.contains(&MIN_SUPPORTED.to_string()),
        "the refusal names both numbers: {text}"
    );
    assert!(
        text.contains("--locked") && text.contains("run a shep that accepts protocol"),
        "the refusal names both fixes, and picks neither: {text}"
    );
    assert!(
        !paths.daemon_config.exists(),
        "a refused adopt must not write shep.toml"
    );
}

/// Rewrites the binary a name in `[daemon] adopted_dogs` points at, so
/// `warn_of_a_dog_a_restart_would_break` re-probes it and sees `protocol`
/// rather than whatever `adopt` vetted at adoption time.
fn rewrite_dog_protocol(bin: &Path, protocol: u32) {
    std::fs::write(
        bin,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n--version)\necho 'shep-otel 0.1.3'\n\
                 echo 'shep-protocol: {protocol}'\n;;\nesac\n"
        ),
    )
    .unwrap();
    chmod(bin, 0o755);
}

/// The warning used to fire on every dog after every protocol bump,
/// because it compared for exact equality. A dog inside the accepted
/// window, at the floor or above it, now earns silence: the point of
/// the warning is a dog that genuinely cannot connect, not drift.
#[tokio::test]
async fn no_restart_warning_for_a_dog_inside_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let bin = dog_script(
        dir.path(),
        "shep-otel",
        &format!("echo 'shep-otel 0.1.3'\necho 'shep-protocol: {PROTOCOL_VERSION}'"),
    );
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin.clone(),
    };
    let mut adopt_out = Vec::new();
    let mut adopt_err = Vec::new();
    let code = adopt(&mut streams(&mut adopt_out, &mut adopt_err), &paths, &args).await;
    assert_eq!(code, ExitCode::Success, "the fixture must adopt cleanly");

    for protocol in [PROTOCOL_VERSION + 1, MIN_SUPPORTED] {
        rewrite_dog_protocol(&bin, protocol);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let selectors = vec![SelectorSpec::Name("otel".to_string())];
        warn_of_a_dog_a_restart_would_break(
            &mut streams(&mut out, &mut err),
            &paths,
            &selectors,
            TEST_BUDGET,
        );
        let text = String::from_utf8(err).unwrap();
        assert!(
            !text.contains(DOG_BINARY_SKEW_NOTICE),
            "protocol {protocol} is inside the window, so no warning belongs here: {text}"
        );
    }
}

/// The one case the warning exists for: a binary on disk that would
/// come back unable to connect.
#[tokio::test]
async fn a_restart_warning_fires_for_a_dog_below_the_floor() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let bin = dog_script(
        dir.path(),
        "shep-otel",
        &format!("echo 'shep-otel 0.1.3'\necho 'shep-protocol: {PROTOCOL_VERSION}'"),
    );
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin.clone(),
    };
    let mut adopt_out = Vec::new();
    let mut adopt_err = Vec::new();
    let code = adopt(&mut streams(&mut adopt_out, &mut adopt_err), &paths, &args).await;
    assert_eq!(code, ExitCode::Success, "the fixture must adopt cleanly");

    rewrite_dog_protocol(&bin, MIN_SUPPORTED.saturating_sub(1));
    let mut out = Vec::new();
    let mut err = Vec::new();
    let selectors = vec![SelectorSpec::Name("otel".to_string())];
    warn_of_a_dog_a_restart_would_break(
        &mut streams(&mut out, &mut err),
        &paths,
        &selectors,
        TEST_BUDGET,
    );
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.contains(DOG_BINARY_SKEW_NOTICE),
        "a dog below the floor is exactly what this warning is for: {text}"
    );
    assert!(
        text.contains(&MIN_SUPPORTED.to_string()),
        "the warning names the floor, not one exact version: {text}"
    );
}

/// A third-party dog's crate version has no relationship to shep's own,
/// so it is reported and never compared.
#[tokio::test]
async fn a_dog_whose_version_is_nothing_like_sheps_is_still_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let bin = dog_script(
        dir.path(),
        "shep-otel",
        &format!("echo 'shep-otel 9.9.9-rc1'\necho 'shep-protocol: {PROTOCOL_VERSION}'"),
    );

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    assert_eq!(
        vetted.answer,
        Some(DogVersion {
            version: "9.9.9-rc1".to_string(),
            protocol: Some(PROTOCOL_VERSION),
        })
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(
        code,
        ExitCode::Success,
        "a version difference is not a refusal"
    );
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.contains("9.9.9-rc1"),
        "the version it answered is reported: {text}"
    );
}

/// Refusing silence would break every dog that predates the contract,
/// so detection falls back to the handshake for that dog alone.
#[tokio::test]
async fn a_dog_that_does_not_answer_is_adopted_with_an_unknown_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let bin = dog_script(dir.path(), "shep-otel", "exit 0");

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    assert_eq!(vetted.answer, None, "silence is an unknown, not an answer");

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(
        code,
        ExitCode::Success,
        "a dog predating the contract is adoptable"
    );
    let cfg = std::fs::read_to_string(&paths.daemon_config).unwrap();
    assert!(cfg.contains("otel"), "and it is recorded: {cfg}");
}

/// A clap-built dog prints `<name> <version>` and never mentions
/// `shep-protocol`: unknown, not mismatched.
#[tokio::test]
async fn a_dog_that_names_no_protocol_is_adopted_with_the_version_it_gave() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let bin = dog_script(dir.path(), "shep-otel", "echo 'shep-otel 0.1.3'");

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    assert_eq!(
        vetted.answer,
        Some(DogVersion {
            version: "0.1.3".to_string(),
            protocol: None,
        })
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::Success);
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.contains("0.1.3") && text.contains("unknown"),
        "the operator hears the version and that the protocol is unknown: {text}"
    );
}

/// A budget for tests that are not about the budget.
///
/// The probe spawns a real child, so every test reaching `vet_binary`
/// inherits a wall-clock bound, and at the production one second a test
/// asking whether a version string parses fails on a busy machine.
const TEST_BUDGET: Duration = Duration::from_secs(30);

/// `CARGO_PKG_NAME` is the sentinel: cargo sets it for this process and
/// it is not on the daemon's allowlist, so a leak shows up as the child
/// seeing a variable this test never gave it, with no environment
/// mutated.
#[cfg(unix)]
#[test]
fn a_probe_runs_with_the_daemons_environment_and_not_the_operators() {
    assert!(
        std::env::var("CARGO_PKG_NAME").is_ok(),
        "the sentinel has to be in this process for its absence downstream to mean anything"
    );
    let dir = tempfile::tempdir().unwrap();
    // The sentinel must be the last field of line 1, the only part
    // `parse_version_answer` keeps; anywhere else and the test passes
    // with `env_clear` removed.
    let bin = dog_script(
        dir.path(),
        "shep-otel",
        "echo \"shep-otel ${CARGO_PKG_NAME:-clean}\"",
    );

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    let version = vetted.answer.expect("the candidate answered").version;

    assert_eq!(
        version, "clean",
        "the operator's environment reached a candidate: it saw CARGO_PKG_NAME"
    );
}

/// The assertion on elapsed time is what turns a blocking `child.wait()`
/// into a reported failure rather than a hang.
#[test]
fn a_candidate_that_never_exits_does_not_hang_the_vet() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dog_script(dir.path(), "shep-otel", "sleep 30");

    let started = std::time::Instant::now();
    // The real budget, not `TEST_BUDGET`: this test is the bound.
    let vetted = vet_binary_within(&bin, dir.path(), "otel", VERSION_BUDGET).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(
        vetted.answer, None,
        "a candidate that never answered is unknown"
    );
    // Absolute, not a multiple of `VERSION_BUDGET`, so the bound stays
    // wrong when the budget is what got mutated. Ten seconds against a
    // candidate that sleeps thirty.
    assert!(
        elapsed < Duration::from_secs(10),
        "the vet is bounded by its own budget, not by the candidate: {elapsed:?}"
    );
}

/// Twice the cap is written, so a read that stopped anywhere else shows
/// up in the length. `trap '' PIPE` lets the script reach its own `exit
/// 0` after shep closes the pipe: a candidate killed by SIGPIPE answers
/// `None` for its exit status and says nothing about where the read
/// stopped.
#[test]
fn a_candidate_that_will_not_stop_talking_is_read_no_further_than_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let chunk = "a".repeat(1024);
    let chunks = PROBE_OUTPUT_LIMIT / 1024 * 2;
    let bin = probe_script(
        dir.path(),
        "shep-otel",
        &format!(
            "trap '' PIPE\ni=0\nwhile [ $i -lt {chunks} ]; do \
                 printf '%s' '{chunk}'; i=$((i+1)); done\nexit 0"
        ),
        "exit 0",
    );

    let answer = ask(&bin, VERSION_FLAG, dir.path(), "otel", TEST_BUDGET)
        .expect("what a candidate prints is never a refusal")
        .expect("it exited 0, so it answered");

    assert_eq!(
        answer.len() as u64,
        PROBE_OUTPUT_LIMIT,
        "the read stops at the cap, whatever the candidate does after it"
    );
}

/// `docs/dogs.md` asks a dog to answer on stdout and exit 0, so lines
/// from a run that then failed cannot refuse an adopt.
#[test]
fn an_answer_from_a_failed_run_is_not_an_answer() {
    let dir = tempfile::tempdir().unwrap();
    // The value itself is irrelevant here: a failed exit discards the
    // answer whether the protocol named is above the floor or below it.
    let named = PROTOCOL_VERSION + 1;
    let bin = dog_script(
        dir.path(),
        "shep-otel",
        &format!("echo 'shep-otel 0.1.3'\necho 'shep-protocol: {named}'\nexit 3"),
    );

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    assert_eq!(vetted.answer, None);
}

/// shep has no standing to refuse a dog over the shape of text it never
/// promised to print.
#[test]
fn output_that_answers_nothing_shep_asked_is_not_a_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dog_script(
        dir.path(),
        "shep-otel",
        "echo 'error: unrecognized option --version'",
    );

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    assert_eq!(
        vetted.answer,
        Some(DogVersion {
            version: "--version".to_string(),
            protocol: None,
        }),
        "the last field of line 1 is taken as the version, whatever it is"
    );
}

/// What `docs/dogs.md` says the parser tolerates: an ignored name on
/// line 1, blank lines, key order, unknown keys, and a reserved `shep-`
/// key that does not exist yet. A shep that predates a third number must
/// ignore its line rather than refuse the dog.
#[test]
fn a_future_shep_key_is_ignored_rather_than_breaking_the_parser() {
    let answer = parse_version_answer(
        "some-other-crate-name 0.4.0\n\nshep-channel: 7\nother: whatever\n\
             shep-protocol: 2\nshep-lambs: 9\n",
    );
    assert_eq!(
        answer,
        Some(DogVersion {
            version: "0.4.0".to_string(),
            protocol: Some(2),
        })
    );

    assert_eq!(parse_version_answer(""), None, "no output is no answer");
    assert_eq!(
        parse_version_answer("shep-otel 0.1.3\nshep-protocol: two\n"),
        Some(DogVersion {
            version: "0.1.3".to_string(),
            protocol: None,
        }),
        "a protocol that is not a decimal is unknown, not a refusal"
    );
}

/// A dog whose schema half is what the test is about; the version half
/// always names this shep's own protocol.
fn two_flag_dog(dir: &Path, schema_body: &str) -> PathBuf {
    probe_script(
        dir,
        "shep-otel",
        &format!("echo 'shep-otel 0.1.3'\necho 'shep-protocol: {PROTOCOL_VERSION}'"),
        schema_body,
    )
}

#[test]
fn a_dog_that_answers_a_schema_has_it_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let bin = two_flag_dog(
        dir.path(),
        "echo '{\"title\":\"otel\",\"properties\":{\"endpoint\":{\"type\":\"string\"}}}'",
    );

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();

    let DogSchema::Published(schema) = &vetted.schema else {
        panic!("a dog that printed valid JSON Schema has one: {vetted:?}");
    };
    assert_eq!(
        schema["properties"]["endpoint"]["type"], "string",
        "the schema is kept as the dog wrote it, not summarised"
    );
}

/// How long a fork the probe left behind would go on running.
const PROBE_FORK_LIFETIME_SECS: u64 = 30;

/// How long [`waited_out`] gives a SIGKILLed fork to leave the process
/// table.
///
/// The signal is unblockable, so this covers the reaping parent's
/// scheduling and nothing else. Five seconds against the sub-millisecond
/// it takes idle, sized for a loaded runner rather than for this machine.
const FORK_EXIT_DEADLINE: Duration = Duration::from_secs(5);

/// Gap between [`FORK_EXIT_DEADLINE`]'s retries, as
/// `cli_e2e.rs`'s own pid guard does it.
const FORK_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Whether `pid` left the process table inside [`FORK_EXIT_DEADLINE`].
///
/// A fork the probe abandons is reparented to init, which reaps it, so
/// `ESRCH` is what being gone looks like from here. Polled rather than
/// awaited: the fork is nobody's child in this process, so there is no
/// exit status to wait on.
fn waited_out(pid: nix::unistd::Pid) -> bool {
    let started = Instant::now();
    while started.elapsed() < FORK_EXIT_DEADLINE {
        if nix::sys::signal::kill(pid, None).is_err() {
            return true;
        }
        std::thread::sleep(FORK_EXIT_POLL_INTERVAL);
    }
    false
}

/// fails if a fork the probe left behind outlives the probe
///
/// Opening a dog's config pane runs that dog's binary, and a dog that
/// does not recognise the flag starts its ordinary job instead. Killing
/// the leader alone leaves what it forked running against the live
/// `SHEP_HOME`, once per keystroke.
#[test]
fn a_fork_the_probe_left_behind_is_killed_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("fork.pid");
    // The fork writes its pid before the script answers, so an answer is
    // proof the file is there to read. Its stdout goes to `/dev/null`: a
    // fork holding the inherited pipe open spends the whole budget and
    // leaves no answer to wait on.
    let bin = two_flag_dog(
        dir.path(),
        &format!(
            "sh -c 'echo $$ > {pid}.tmp; mv {pid}.tmp {pid}; exec sleep {secs}' \
                 >/dev/null 2>&1 &\n\
                 while [ ! -f {pid} ]; do sleep 0.01; done\n\
                 echo '{{}}'",
            pid = pid_file.display(),
            secs = PROBE_FORK_LIFETIME_SECS,
        ),
    );

    let schema = ask_schema(&bin, dir.path(), "otel", TEST_BUDGET);
    assert!(
        matches!(schema, DogSchema::Published(_)),
        "the probe must answer, or the fork never wrote its pid: {schema:?}"
    );
    let pid = nix::unistd::Pid::from_raw(
        std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
    );

    let gone = waited_out(pid);
    // Before the assertion: a red run must not leave the fork behind for
    // the rest of its lifetime.
    if !gone {
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
    }
    assert!(gone, "a fork the probe left behind outlived it");
}

/// A dog with a broken schema flag may still do its job.
#[tokio::test]
async fn a_dog_whose_schema_run_exits_non_zero_has_no_schema_and_no_warning() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let bin = two_flag_dog(dir.path(), "echo '{}'; exit 3");

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    assert_eq!(
        vetted.schema,
        DogSchema::Silent,
        "a failed run printed no answer, whatever reached stdout"
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::Success, "no schema is not a refusal");
    let text = String::from_utf8(err).unwrap();
    assert!(
        !text.contains(DOG_SCHEMA_UNREADABLE_NOTICE),
        "only unreadable output earns the warning, not a failed run: {text}"
    );
}

/// Every dog written before this contract is silent, and a warning for
/// each is a line about the ordinary case.
#[tokio::test]
async fn a_dog_that_prints_no_schema_has_no_schema_and_no_warning() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let bin = two_flag_dog(dir.path(), "exit 0");

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    assert_eq!(vetted.schema, DogSchema::Silent, "silence is no schema");

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::Success, "silence is not a refusal");
    let text = String::from_utf8(err).unwrap();
    assert!(
        !text.contains(DOG_SCHEMA_UNREADABLE_NOTICE),
        "a dog that answered nothing is the ordinary case, not a warning: {text}"
    );
}

/// The dog meant to answer and its answer cannot be read: a bug its
/// author can fix. Exactly one warning, because the count is what an
/// operator reads.
#[tokio::test]
async fn a_dog_that_prints_invalid_json_is_adopted_with_one_warning() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let bin = two_flag_dog(dir.path(), "echo 'error: unrecognized option --schema'");

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    assert_eq!(
        vetted.schema,
        DogSchema::Unreadable,
        "output that is not JSON is unreadable, not absent"
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::Success, "a broken schema is not a refusal");
    let text = String::from_utf8(err).unwrap();
    assert_eq!(
        text.matches(&format!("notice[{DOG_SCHEMA_UNREADABLE_NOTICE}]"))
            .count(),
        1,
        "one warning, and only one: {text}"
    );
    assert!(
        paths.daemon_config.exists(),
        "the dog is adopted despite the warning"
    );
}

/// Every file under `dir`, recursively, as text. A non-UTF-8 file is
/// skipped rather than failing the walk.
fn every_file_under(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(every_file_under(&path));
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            found.push((path, text));
        }
    }
    found
}

/// `cargo install` replaces a dog's binary with nothing watching, and a
/// stale schema mislabels which field is a credential. Asserts on the
/// files, walking the whole home; the dog's binary lives in its own
/// directory because the fixture script contains the schema it prints.
#[tokio::test]
async fn a_published_schema_reaches_no_file_shep_writes() {
    let home = tempfile::tempdir().unwrap();
    let binaries = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, home.path());
    let marker = "only-ever-in-the-schema";
    let bin = two_flag_dog(
        binaries.path(),
        &format!("echo '{{\"title\":\"{marker}\",\"properties\":{{}}}}'"),
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;
    assert_eq!(code, ExitCode::Success);

    let written = every_file_under(home.path());
    assert!(
        written.iter().any(|(_, text)| text.contains("otel")),
        "the adopt has to have written the dog somewhere for this to mean anything: {written:?}"
    );
    for (path, text) in &written {
        assert!(
            !text.contains(marker),
            "the schema was stored in {}: {text}",
            path.display()
        );
    }
}

/// A `default` in the schema carries the dog author's own `Default`.
/// Exact string: the failure mode is a derive replacing the impl.
#[test]
fn debug_reports_that_there_is_a_schema_and_never_what_is_in_it() {
    let schema = DogSchema::Published(serde_json::json!({"token": "hunter2"}));
    assert_eq!(format!("{schema:?}"), "Published(..)");
    assert_eq!(format!("{:?}", DogSchema::Silent), "Silent");
    assert_eq!(format!("{:?}", DogSchema::Unreadable), "Unreadable");
}

/// A deployment directory owned by a trusted deploy group is legitimate,
/// and this command is the operator's only chance to hear about it.
#[tokio::test]
async fn a_group_writable_binary_is_adopted_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let deploy = dir.path().join("deploy");
    std::fs::create_dir(&deploy).unwrap();
    let bin = deploy.join("shep-otel");
    std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
    chmod(&bin, 0o775);
    chmod(&deploy, 0o775);

    let vetted = vet_binary_within(&bin, dir.path(), "otel", TEST_BUDGET).unwrap();
    assert_eq!(
        vetted.group_writable,
        vec![bin.canonicalize().unwrap(), deploy.canonicalize().unwrap()],
        "both the binary and its directory are group-writable"
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: bin.clone(),
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::Success, "group-writable is a warning");
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.contains(&bin.canonicalize().unwrap().display().to_string()),
        "the warning names the path: {text}"
    );
    assert!(
        text.contains("group"),
        "the warning says what the risk is: {text}"
    );
}

/// The vetting is worth nothing if the config records the binary anyway.
#[tokio::test]
async fn a_refused_adopt_leaves_the_config_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        name: Some("otel".to_string()),
        path: dir.path().join("nope"),
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::InvalidConfig);
    assert!(
        !paths.daemon_config.exists(),
        "a refused adopt must never touch shep.toml: {}",
        paths.daemon_config.display()
    );
}
