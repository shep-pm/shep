//! What a real child inherits: a seeded `PATH`, and the pipe on fd 0 it
//! asked for or did not.

use super::*;

/// RAII guard: sets `PATH` for a test and restores it on drop, including on
/// panic, so a failing assertion never leaks a mutated `PATH` into another
/// test running concurrently.
///
/// `set_var`/`remove_var` are `unsafe fn` in edition 2024: the hazard is
/// unsynchronized env reads from another thread. This binary is its own
/// crate root, separate from `shep-daemon`'s `#![deny(unsafe_code)]`; no
/// other test here touches `PATH`, and `Command::spawn` reads env through
/// std's own synchronized path, so nothing here can race it.
struct PathGuard {
    original: Option<String>,
}

impl PathGuard {
    fn set(new_path: &std::path::Path) -> Self {
        let original = std::env::var("PATH").ok();
        // SAFETY: no other test writes `PATH`; `Command::spawn`'s concurrent
        // reads go through std's synchronized path, not a raw `getenv`.
        unsafe { std::env::set_var("PATH", new_path) };
        Self { original }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.original {
            // SAFETY: no other test writes `PATH`; `Command::spawn`'s
            // concurrent reads go through std's synchronized path.
            Some(value) => unsafe { std::env::set_var("PATH", value) },
            // SAFETY: no other test writes `PATH`; `Command::spawn`'s
            // concurrent reads go through std's synchronized path.
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

#[tokio::test]
async fn a_bare_interpreter_resolves_via_the_seeded_path() {
    // Isolates assemble()+TokioRunner from the full e2e stack. A shim, not
    // `/bin/sh`: `/bin/sh` falls back to the OS's default PATH even with
    // an empty env, so it would pass without a real seed. A shim in a
    // tempdir can only resolve through the seed.
    use shep_core::config::{AppConfig, normalize};
    use shep_core::paths::ShepPaths;
    use shep_core::secrets::SecretView;
    use shep_daemon::assemble::assemble;
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let shim_dir = dir.path().join("bin");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim_path = shim_dir.join("shep-test-interp");
    fs::write(&shim_path, "#!/bin/sh\necho shim-exec-ok\n").unwrap();
    let mut perms = fs::metadata(&shim_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&shim_path, perms).unwrap();

    // Points this process's own PATH at only the shim's directory, so
    // inherited_env() has exactly one place to find "shep-test-interp".
    let _path_guard = PathGuard::set(&shim_dir);

    let paths = ShepPaths {
        home: dir.path().to_path_buf(),
        daemon_config: dir.path().join("shep.toml"),
        dogs_config: dir.path().join("dogs.toml"),
        snapshot: dir.path().join("flock.json"),
        logs: dir.path().join("logs"),
        pids: dir.path().join("pids"),
        run: dir.path().join("run"),
        socket: dir.path().join("run/shep.sock"),
        barks: dir.path().join("barks.jsonl"),
        kv: dir.path().join("kv.json"),
        overrides: dir.path().join("overrides.json"),
        secrets: dir.path().join("secrets.json"),
        secrets_cache: dir.path().join("secrets-cache.json"),
    };
    let app_config = AppConfig {
        name: "bare".to_string(),
        script: "unused".to_string(),
        args: vec![],
        interpreter: Some("shep-test-interp".to_string()), // bare: only found via seeded PATH
        ..Default::default()
    };
    let app = normalize(app_config).unwrap();
    let spec = assemble(
        &app,
        0,
        &paths,
        None,
        &SecretView::empty("production".to_string()),
    )
    .expect("this fixture carries no secret to resolve");
    assert_eq!(
        spec.program, "shep-test-interp",
        "sanity: genuinely bare, not accidentally absolute"
    );

    let runner = TokioRunner::new();
    let (mut proc, mut io) = runner.spawn(&spec).unwrap();
    let line = recv_log(
        &mut io,
        LOG_WRITE_DEADLINE,
        "the shim must resolve via the seeded PATH and produce output",
    )
    .await;
    assert_eq!(line.line, "shim-exec-ok");
    assert!(!line.err);
    let outcome = proc.wait().await;
    assert_eq!(outcome.code, Some(0));
}

/// A real pipe on a real fd 0. `cat` echoes what is written to it, so a
/// line on stdout proves the whole path worked: created, mapped to fd 0,
/// written, flushed, and read by the child.
///
/// Bounded: a line that never arrives must fail this test, not hang it.
#[tokio::test]
async fn a_real_child_reads_a_line_written_to_its_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec_for(&dir, "/bin/cat", &[]);
    spec.stdin = true;
    let runner = TokioRunner::new();
    let (_proc, mut io) = runner.spawn(&spec).unwrap();

    let (done, ack) = tokio::sync::oneshot::channel();
    io.to_stdin
        .send(StdinWrite {
            line: "hello sheep".to_string(),
            done,
        })
        .await
        .unwrap();
    ack.await.unwrap().unwrap();

    let line = recv_log(&mut io, LONG_DEADLINE, "no stdout line within 10s").await;
    assert!(!line.err);
    assert_eq!(line.line, "hello sheep");
}

/// fails if a spec that did not ask for a pipe gets one anyway. `/dev/null`
/// on fd 0 is the default; `cat` reading EOF immediately, rather than
/// waiting, is the observable difference.
#[tokio::test]
async fn a_child_that_did_not_ask_for_stdin_gets_eof_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = spec_for(&dir, "/bin/cat", &[]);
    spec.stdin = false;
    let runner = TokioRunner::new();
    let (mut proc, io) = runner.spawn(&spec).unwrap();

    assert!(io.to_stdin.is_closed());
    let outcome = reaped(
        &mut proc,
        LONG_DEADLINE,
        "cat did not exit on EOF within 10s",
    )
    .await;
    assert_eq!(outcome.code, Some(0));
}
