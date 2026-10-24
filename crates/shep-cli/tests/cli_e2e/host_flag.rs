//! `--host`: the verbs that reach a host act on its flock through a stand-in
//! `ssh`, and every other verb refuses before it acts on this machine.
//!
//! Unix only, like [`super::ssh_stand_in`], whose script this reuses.

use super::*;

#[cfg(unix)]
use super::ssh_stand_in::write_ssh_stand_in;

#[cfg(unix)]
/// `PATH` with `dir` first, so the child finds the stand-in as `ssh`.
fn path_with_first(dir: &Path) -> std::ffi::OsString {
    let rest = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(dir.to_owned()).chain(std::env::split_paths(&rest)))
        .unwrap()
}

#[cfg(unix)]
/// Starts a sheep named `name` on `home`, adopting the home into `guard`.
fn start_sheep(guard: &mut DaemonGuard, home: &TempDir, name: &str) {
    let script = write_script(
        home,
        &format!("{name}.sh"),
        &format!("{}{}", script_header(), sleep_line(SCRIPT_SLEEP_SECS)),
    );
    let started = shep(home.path())
        .arg("start")
        .arg(&script)
        .output()
        .unwrap();
    guard.adopt_home(home.path());
    assert_success(&started);
}

#[cfg(unix)]
/// The sheep names in a `--format json` flock envelope, dogs left out: every
/// shepherd lists its built-in dogs beside the flock.
fn json_names(output: &Output) -> Vec<String> {
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON: {e}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("no data array: {envelope}"))
        .iter()
        .filter(|info| info["dog"].is_null())
        .map(|info| info["name"].as_str().unwrap().to_owned())
        .collect()
}

/// Each home runs a different sheep, so the listing says which shepherd
/// answered. Both formats, because the JSON path has its own failure shape.
#[cfg(unix)]
#[test]
fn flock_over_a_stand_in_ssh_lists_the_remote_flock_not_the_local_one() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    start_sheep(&mut guard, &local, "local-sheep");
    start_sheep(&mut guard, &remote, "remote-sheep");
    write_ssh_stand_in(bin.path(), remote.path());
    let path = path_with_first(bin.path());

    let table = shep(local.path())
        .args(["--host", "amp2", "flock"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_success(&table);
    let text = String::from_utf8_lossy(&table.stdout);
    assert!(text.contains("remote-sheep"), "{text}");
    assert!(!text.contains("local-sheep"), "{text}");

    let json = shep(local.path())
        .args(["--format", "json", "--host", "amp2", "flock"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_success(&json);
    assert_eq!(json_names(&json), ["remote-sheep"]);

    graceful_kill(local.path());
    graceful_kill(remote.path());
}

/// The local home holds a roll. A dead host that printed it would list this
/// machine's flock under a host's name.
#[cfg(unix)]
#[test]
fn a_dead_remote_exits_5_and_prints_no_local_roll() {
    let local = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let roll =
        shep_daemon::snapshot::FlockSnapshot::with_apps(vec![shep_daemon::snapshot::SavedApp {
            app: shep_core::config::AppConfig::minimal("local-sheep", "./srv"),
            instances_running: 1,
            disabled: false,
        }]);
    std::fs::write(
        local.path().join("flock.json"),
        serde_json::to_vec(&roll).unwrap(),
    )
    .unwrap();
    let ssh = bin.path().join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\necho 'ssh: connect to host amp2 port 22: Connection refused' >&2\nexit 255\n",
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = path_with_first(bin.path());

    let table = shep(local.path())
        .args(["--host", "amp2", "flock"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(table.status.code(), Some(5));
    assert!(
        table.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&table.stdout)
    );
    let err = String::from_utf8_lossy(&table.stderr);
    assert!(err.contains("amp2"), "{err}");
    assert!(
        !err.contains("local-sheep") && !err.contains("saved roll"),
        "{err}"
    );

    let json = shep(local.path())
        .args(["--format", "json", "--host", "amp2", "flock"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_json_error(&json, 5, "daemon_unreachable");
}

/// The dangerous failure: a verb that ignores `--host` and acts here. The
/// local home has no sheep by that name, so acting here would exit 3, and
/// the remote sheep would still be up either way. Exit 2 and a remote flock
/// that still lists it online pin both halves.
#[cfg(unix)]
#[test]
fn another_verb_with_host_exits_2_and_the_remote_sheep_is_untouched() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    start_sheep(&mut guard, &local, "local-sheep");
    start_sheep(&mut guard, &remote, "remote-sheep");
    write_ssh_stand_in(bin.path(), remote.path());

    let refused = shep(local.path())
        .args(["--host", "amp2", "flush", "remote-sheep"])
        .env("PATH", path_with_first(bin.path()))
        .output()
        .unwrap();
    assert_eq!(
        refused.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(
        err.contains("`--host` is not supported by this command yet"),
        "{err}"
    );

    let remote_flock = shep(remote.path())
        .args(["--format", "json", "flock"])
        .output()
        .unwrap();
    assert_success(&remote_flock);
    let envelope: serde_json::Value = serde_json::from_slice(&remote_flock.stdout).unwrap();
    let sheep = envelope["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|info| info["name"] == "remote-sheep")
        .unwrap_or_else(|| panic!("the remote sheep is gone: {envelope}"));
    assert_eq!(sheep["status"], "online", "{envelope}");

    let local_flock = shep(local.path())
        .args(["--format", "json", "flock"])
        .output()
        .unwrap();
    assert_success(&local_flock);
    assert_eq!(json_names(&local_flock), ["local-sheep"]);

    graceful_kill(local.path());
    graceful_kill(remote.path());
}

/// A fresh machine has no `~/.shep`, and the listing is the host's. Creating
/// one here would leave a home (and, on a terminal, a first-run banner)
/// behind a command that read nothing of this machine's.
#[cfg(unix)]
#[test]
fn flock_over_a_host_creates_no_home_on_this_machine() {
    let fake_user_home = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    start_sheep(&mut guard, &remote, "remote-sheep");
    write_ssh_stand_in(bin.path(), remote.path());

    let listed = Command::cargo_bin("shep")
        .unwrap()
        .env("HOME", fake_user_home.path())
        .env_remove("SHEP_HOME")
        .env("PATH", path_with_first(bin.path()))
        .args(["--format", "json", "--host", "amp2", "flock"])
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_success(&listed);
    assert_eq!(json_names(&listed), ["remote-sheep"]);
    assert!(
        !fake_user_home.path().join(".shep").exists(),
        "the listing created a home on this machine"
    );

    graceful_kill(remote.path());
}

/// An adopted dog is refused behind `--host`, and a JSON consumer gets the
/// JSON envelope: clap never finished parsing this argv, so the refusal reads
/// `--format` off it by hand.
#[cfg(unix)]
#[test]
fn an_adopted_dog_behind_host_is_refused_in_the_format_asked_for() {
    let home = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    let binary = write_script(&bin, "mydog", "#!/bin/sh\nexit 0\n");
    assert_success(
        &shep(home.path())
            .arg("adopt")
            .arg("--yes")
            .arg(&binary)
            .args(["--name", "mydog"])
            .output()
            .unwrap(),
    );

    let json = shep(home.path())
        .args(["--format", "json", "--host", "amp2", "mydog"])
        .output()
        .unwrap();
    assert_json_error(&json, 2, "usage");

    let table = shep(home.path())
        .args(["--host=amp2", "mydog"])
        .output()
        .unwrap();
    assert_eq!(table.status.code(), Some(2));
    let err = String::from_utf8_lossy(&table.stderr);
    assert!(
        err.contains("`--host` is not supported by this command yet"),
        "{err}"
    );
    assert!(!err.trim_start().starts_with('{'), "{err}");
}

/// The local home holds a roll that names a secret and a store that will not
/// read. A `describe` that resolved them against the remote's sheep would
/// warn about the store and draw a verdict for the reference.
#[cfg(unix)]
#[test]
fn describe_over_a_stand_in_ssh_shows_the_remote_sheep_and_no_secrets() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    start_sheep(&mut guard, &remote, "remote-sheep");
    write_ssh_stand_in(bin.path(), remote.path());
    let mut config = shep_core::config::AppConfig::minimal("remote-sheep", "./srv");
    config
        .env
        .insert("A".into(), "{{secret:DB_PASSWORD}}".into());
    let roll =
        shep_daemon::snapshot::FlockSnapshot::with_apps(vec![shep_daemon::snapshot::SavedApp {
            app: config,
            instances_running: 1,
            disabled: false,
        }]);
    std::fs::write(
        local.path().join("flock.json"),
        serde_json::to_vec(&roll).unwrap(),
    )
    .unwrap();
    std::fs::write(local.path().join("secrets.json"), b"{not json").unwrap();

    let described = shep(local.path())
        .args([
            "--format",
            "json",
            "--host",
            "amp2",
            "describe",
            "remote-sheep",
        ])
        .env("PATH", path_with_first(bin.path()))
        .output()
        .unwrap();

    assert_success(&described);
    let envelope: serde_json::Value = serde_json::from_slice(&described.stdout).unwrap();
    assert_eq!(envelope["data"][0]["name"], "remote-sheep", "{envelope}");
    assert!(
        envelope["secrets"].as_array().is_none_or(Vec::is_empty),
        "{envelope}"
    );
    let err = String::from_utf8_lossy(&described.stderr);
    assert!(err.contains("secrets are not shown over --host"), "{err}");
    assert!(!err.contains("secret_store_unreadable"), "{err}");

    graceful_kill(remote.path());
}

#[cfg(unix)]
/// Sends each line of `stdout` down a channel from a background thread, so a
/// test can wait for one with a bound.
fn lines_from(stdout: std::process::ChildStdout) -> std::sync::mpsc::Receiver<String> {
    use std::io::BufRead;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// A follow over a host prints what the remote sheep says after it
/// subscribes, and Ctrl+C ends it at exit 0.
#[cfg(unix)]
#[test]
fn bleats_over_a_stand_in_ssh_follows_a_remote_sheep_and_exits_0_on_sigint() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    let script = write_script(
        &remote,
        "chatty.sh",
        "#!/bin/sh\nwhile :; do echo remote-line; sleep 1; done\n",
    );
    let started = shep(remote.path())
        .arg("start")
        .arg(&script)
        .output()
        .unwrap();
    guard.adopt_home(remote.path());
    assert_success(&started);
    write_ssh_stand_in(bin.path(), remote.path());

    use std::os::unix::process::CommandExt;

    // Its own process group, so the SIGINT below reaches the stand-in ssh as
    // well as `shep`, as a terminal's Ctrl+C reaches both.
    let mut child = std::process::Command::cargo_bin("shep")
        .unwrap()
        .arg("--home")
        .arg(local.path())
        .args(["--host", "amp2", "bleats"])
        .process_group(0)
        .env("PATH", path_with_first(bin.path()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    discard_in_background(child.stderr.take().unwrap());
    let lines = lines_from(child.stdout.take().unwrap());

    let seen = lines.recv_timeout(CMD_TIMEOUT);
    nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let deadline = Instant::now() + CMD_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = child.kill();
            let _ = child.wait();
            panic!("bleats did not exit after SIGINT");
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    let seen = seen.expect("the remote sheep's line never arrived");
    assert!(seen.contains("remote-line"), "{seen}");
    assert_eq!(status.code(), Some(0), "{status:?}");

    graceful_kill(remote.path());
}

/// The same name runs on both machines, so a verb that fell back to this one
/// would show as a changed local pid. Each verb is waited out on the remote
/// before the local sheep is read, so a late local change cannot hide.
#[cfg(unix)]
#[test]
fn stop_restart_reload_and_delete_act_on_the_remote_flock_only() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    start_sheep(&mut guard, &local, "web");
    start_sheep(&mut guard, &remote, "web");
    write_ssh_stand_in(bin.path(), remote.path());
    let path = path_with_first(bin.path());
    let online = |sheep: &serde_json::Value| sheep["status"] == "online";
    let local_sheep = poll_flock(local.path(), online);
    let mut remote_pid = poll_flock(remote.path(), online)["pid"].clone();
    let over_host = |verb: &str| {
        let output = shep(local.path())
            .args(["--host", "amp2", verb, "web"])
            .env("PATH", &path)
            .output()
            .unwrap();
        assert_success(&output);
    };
    let local_is_untouched = |verb: &str| {
        let here = poll_flock(local.path(), |_| true);
        assert_eq!(
            (&here["pid"], &here["status"]),
            (&local_sheep["pid"], &local_sheep["status"]),
            "`{verb}` over a host changed the local sheep"
        );
    };

    for verb in ["restart", "reload"] {
        over_host(verb);
        let before = remote_pid.clone();
        let moved = poll_flock(remote.path(), |sheep| {
            online(sheep) && sheep["pid"] != before
        });
        assert_ne!(
            moved["pid"], remote_pid,
            "`{verb}` left the remote sheep on its pid"
        );
        remote_pid = moved["pid"].clone();
        local_is_untouched(verb);
    }

    over_host("stop");
    let stopped = poll_flock(remote.path(), |sheep| sheep["status"] == "stopped");
    assert_eq!(stopped["status"], "stopped", "{stopped}");
    local_is_untouched("stop");

    over_host("delete");
    let left = poll_flock_data(remote.path(), FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(Vec::is_empty)
    });
    assert_eq!(left, serde_json::json!([]), "the remote sheep survived");
    local_is_untouched("delete");

    graceful_kill(local.path());
    graceful_kill(remote.path());
}

#[cfg(unix)]
/// Starts a sheep named `web` on `home` that writes down a `SIGUSR1` in
/// `signalled` and every whispered line in `heard`, both inside `home`.
fn start_listening_sheep(guard: &mut DaemonGuard, home: &TempDir) {
    let script = write_script(
        home,
        "web.sh",
        &format!(
            "{}trap 'echo usr1 >> \"{dir}/signalled\"' USR1\n\
             while :; do\n  if read -r line; then echo \"$line\" >> \"{dir}/heard\"; else sleep 1; fi\ndone\n",
            script_header(),
            dir = home.path().display(),
        ),
    );
    let flockfile = write_flockfile(
        home,
        &format!(
            "[[app]]\nname = \"web\"\nscript = '{}'\nstdin = true\n",
            script.display()
        ),
    );
    let started = shep(home.path())
        .arg("start")
        .arg(&flockfile)
        .output()
        .unwrap();
    guard.adopt_home(home.path());
    assert_success(&started);
}

#[cfg(unix)]
/// Waits for `path` to hold `want` on a line, or panics.
fn await_line(path: &Path, want: &str) {
    let start = Instant::now();
    while !std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .any(|l| l == want)
    {
        assert!(
            start.elapsed() < FLOCK_DEADLINE,
            "`{want}` never reached {}",
            path.display()
        );
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

#[cfg(unix)]
/// The `data` rows named `name` in a `--format json` flock.
fn sheep_named_in(home: &Path, name: &str) -> Vec<serde_json::Value> {
    poll_flock_data(home, FLOCK_DEADLINE, |_| true)
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["name"] == name)
        .cloned()
        .collect()
}

/// The same name runs on both machines and each writes where it lives, so a
/// verb that fell back to this machine would leave a mark in the local home.
/// `solo` exists only on the host: this machine's shepherd would answer
/// `trigger solo` with exit 3.
#[cfg(unix)]
#[test]
fn signal_whisper_trigger_and_stock_reach_the_remote_sheep_only() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    start_listening_sheep(&mut guard, &local);
    start_listening_sheep(&mut guard, &remote);
    start_sheep(&mut guard, &remote, "solo");
    write_ssh_stand_in(bin.path(), remote.path());
    let path = path_with_first(bin.path());
    let over_host = |args: &[&str]| {
        let output = shep(local.path())
            .args(["--format", "json", "--host", "amp2"])
            .args(args)
            .env("PATH", &path)
            .output()
            .unwrap();
        assert_success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };

    let signalled = over_host(&["signal", "web", "USR1"]);
    assert_eq!(
        signalled["data"][0]["outcome"]["kind"], "delivered",
        "{signalled}"
    );
    await_line(&remote.path().join("signalled"), "usr1");

    over_host(&["whisper", "web", "hello"]);
    await_line(&remote.path().join("heard"), "hello");

    let triggered = over_host(&["trigger", "solo", "ping"]);
    assert_eq!(triggered["data"][0]["name"], "solo", "{triggered}");
    assert_eq!(
        triggered["data"][0]["outcome"]["kind"], "no_channel",
        "{triggered}"
    );

    over_host(&["stock", "solo", "2"]);
    poll_flock_data(remote.path(), FLOCK_DEADLINE, |data| {
        data.as_array()
            .is_some_and(|rows| rows.iter().filter(|row| row["name"] == "solo").count() == 2)
    });

    assert!(
        !local.path().join("signalled").exists() && !local.path().join("heard").exists(),
        "a verb over a host reached the local sheep"
    );
    assert_eq!(sheep_named_in(local.path(), "web").len(), 1);
    assert!(sheep_named_in(local.path(), "solo").is_empty());

    graceful_kill(local.path());
    graceful_kill(remote.path());
}

/// Both machines run `web`, so the local sheep staying unmarked is what shows
/// the verb went to the host.
#[cfg(unix)]
#[test]
fn disable_then_enable_over_a_host_marks_the_remote_sheep() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    start_sheep(&mut guard, &local, "web");
    start_sheep(&mut guard, &remote, "web");
    write_ssh_stand_in(bin.path(), remote.path());
    let path = path_with_first(bin.path());
    let over_host = |verb: &str| {
        let output = shep(local.path())
            .args(["--host", "amp2", verb, "web"])
            .env("PATH", &path)
            .output()
            .unwrap();
        assert_success(&output);
    };
    let disabled = |home: &Path| sheep_named_in(home, "web")[0]["disabled"] == true;

    over_host("disable");
    poll_flock(remote.path(), |sheep| sheep["disabled"] == true);
    assert!(disabled(remote.path()));
    assert!(
        !disabled(local.path()),
        "`disable` over a host marked the local sheep"
    );

    over_host("enable");
    poll_flock(remote.path(), |sheep| sheep["status"] == "online");
    assert!(!disabled(remote.path()));
    assert!(!disabled(local.path()));

    graceful_kill(local.path());
    graceful_kill(remote.path());
}

/// Both machines run `web`, and the remote one is stopped, so only a start
/// that reached the host brings it back. A second host with no shepherd shows
/// the connect never boots one: exit 5, and no socket appears there.
#[cfg(unix)]
#[test]
fn start_over_a_host_resumes_a_stopped_remote_sheep_and_never_boots_a_remote_shepherd() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let dead = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let dead_bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    start_sheep(&mut guard, &local, "web");
    start_sheep(&mut guard, &remote, "web");
    write_ssh_stand_in(bin.path(), remote.path());
    write_ssh_stand_in(dead_bin.path(), dead.path());
    let local_sheep = poll_flock(local.path(), |sheep| sheep["status"] == "online");
    assert_success(&shep(remote.path()).args(["stop", "web"]).output().unwrap());
    poll_flock(remote.path(), |sheep| sheep["status"] == "stopped");

    let resumed = shep(local.path())
        .args(["--host", "amp2", "start", "web"])
        .env("PATH", path_with_first(bin.path()))
        .output()
        .unwrap();
    assert_success(&resumed);
    let back = poll_flock(remote.path(), |sheep| sheep["status"] == "online");
    assert_eq!(back["status"], "online", "{back}");
    let here = poll_flock(local.path(), |_| true);
    assert_eq!(
        (&here["pid"], &here["status"]),
        (&local_sheep["pid"], &local_sheep["status"]),
        "`start` over a host touched the local sheep"
    );

    let refused = shep(local.path())
        .args(["--host", "amp2", "start", "./api"])
        .env("PATH", path_with_first(bin.path()))
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains("ssh amp2 shep start ./api"), "{said}");

    let unreachable = shep(local.path())
        .args(["--host", "amp2", "start", "web"])
        .env("PATH", path_with_first(dead_bin.path()))
        .output()
        .unwrap();
    assert_eq!(unreachable.status.code(), Some(5));
    let bare = shep(local.path())
        .args(["--host", "amp2", "start"])
        .env("PATH", path_with_first(dead_bin.path()))
        .output()
        .unwrap();
    assert_eq!(bare.status.code(), Some(2));
    assert!(
        !shep_client::shep_core::paths::ShepPaths::at(dead.path().to_owned())
            .socket
            .exists(),
        "`start` over a host booted a shepherd there"
    );

    graceful_kill(local.path());
    graceful_kill(remote.path());
}

/// A full lookout needs a terminal, which a test does not have. What it can
/// pin is that `--host` no longer refuses the verb, and that the gate is the
/// local one: it answers before any ssh is run.
#[cfg(unix)]
#[test]
fn lookout_over_a_host_without_a_terminal_exits_2_like_a_local_one() {
    let local = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let ran = bin.path().join("ssh-ran");
    let ssh = bin.path().join("ssh");
    std::fs::write(
        &ssh,
        format!("#!/bin/sh\ntouch '{}'\nexit 255\n", ran.display()),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = shep(local.path())
        .args(["--host", "amp2", "lookout"])
        .env("PATH", path_with_first(bin.path()))
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("needs a terminal"), "{said}");
    assert!(!ran.exists(), "ssh ran before the terminal gate");
}

#[cfg(unix)]
/// Waits for `path` to stop existing, for a shepherd's socket after `kill`.
fn wait_until_gone(path: &Path) {
    let deadline = Instant::now() + CMD_TIMEOUT;
    while path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never went away",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// After a reboot there is no remote shepherd to dial. `muster` is the one
/// verb that boots it, from the roll on the host's own home, and nothing of
/// the host's is booted on this machine.
#[cfg(unix)]
#[test]
fn muster_over_a_host_boots_the_remote_shepherd_from_its_roll_and_leaves_the_local_one_down() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    start_sheep(&mut guard, &remote, "remote-sheep");
    assert_success(&shep(remote.path()).arg("save").output().unwrap());
    graceful_kill(remote.path());
    let remote_socket = remote.path().join("run").join("shep.sock");
    wait_until_gone(&remote_socket);
    write_ssh_stand_in(bin.path(), remote.path());

    let mustered = shep(local.path())
        .args(["--format", "json", "--host", "amp2", "muster"])
        .env("PATH", path_with_first(bin.path()))
        .output()
        .unwrap();

    assert_success(&mustered);
    let envelope: serde_json::Value = serde_json::from_slice(&mustered.stdout).unwrap();
    assert_eq!(envelope["command"], "muster", "{envelope}");
    let remote_flock = shep(remote.path())
        .args(["--format", "json", "flock"])
        .output()
        .unwrap();
    assert_success(&remote_flock);
    assert_eq!(json_names(&remote_flock), ["remote-sheep"]);
    assert!(
        !local.path().join("run").join("shep.sock").exists(),
        "a shepherd came up on this machine"
    );

    graceful_kill(remote.path());
}

/// The dial that every other verb makes never asks for a boot, so a host that
/// is down stays down and the exit code says so.
#[cfg(unix)]
#[test]
fn flock_over_a_host_never_boots_the_remote() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    write_ssh_stand_in(bin.path(), remote.path());

    let listed = shep(local.path())
        .args(["--host", "amp2", "flock"])
        .env("PATH", path_with_first(bin.path()))
        .output()
        .unwrap();

    assert_eq!(listed.status.code(), Some(5), "{listed:?}");
    assert!(
        !remote.path().join("run").join("shep.sock").exists(),
        "the remote shepherd was booted"
    );
}

/// ssh ends a session only once every copy of its stdout and stderr pipes has
/// closed. The stand-in does the same: stderr goes down a pipe of its own, and
/// the marker is touched only once both pipes have no writer left. A shepherd
/// booted by `dial-stdio` that kept either would hold the marker back for as
/// long as it lives. Stdin is not watched, since ssh does not wait on it.
#[cfg(unix)]
#[test]
fn the_ssh_session_ends_after_a_remote_muster() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let mut guard = DaemonGuard::default();
    let marker = bin.path().join("session-ended");
    let stand_in = write_ssh_stand_in(bin.path(), remote.path());
    let script = std::fs::read_to_string(&stand_in).unwrap();
    let waiting = format!(
        "{{ \"$@\" 2>&1 1>&3 | cat >&2; }} 3>&1 | {{ cat; touch '{}'; }}\n",
        marker.display()
    );
    std::fs::write(&stand_in, script.replace("exec \"$@\"\n", &waiting)).unwrap();

    let mustered = shep(local.path())
        .args(["--host", "amp2", "muster"])
        .env("PATH", path_with_first(bin.path()))
        .output()
        .unwrap();
    guard.adopt_home(remote.path());
    assert_success(&mustered);

    wait_until_exists(&marker);
    graceful_kill(remote.path());
}

#[cfg(unix)]
/// Waits for `path` to exist.
fn wait_until_exists(path: &Path) {
    let deadline = Instant::now() + CMD_TIMEOUT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared: the session is still open",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
