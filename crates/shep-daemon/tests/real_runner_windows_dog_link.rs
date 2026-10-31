//! The connection a dog is spawned holding, on a real Windows child's pipe.
//!
//! The Windows twin of `real_runner/dog_link.rs`: a pipe the dog opens by the
//! name in `SHEP_DOG_PIPE` stands where the unix child inherits fd 4.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use shep_daemon::runner::{DogLink, ProcessRunner, RunningProcess, SpawnSpec};
use shep_daemon::tokio_runner::TokioRunner;
use tokio::io::AsyncReadExt as _;
use tokio::sync::mpsc::UnboundedReceiver;

/// How long a wait for the child or the runner takes before failing:
/// generous, since a loaded CI box is slower than a quiet laptop.
const SETTLE: Duration = Duration::from_secs(15);

/// A spec running the `.cmd` script `body` from `dir`, with the floor of
/// environment `cmd` needs because the runner clears the rest.
///
/// A script file, not `cmd /C <line>`: `std::process::Command` escapes inner
/// quotes MSVC-style, which `cmd.exe` reads literally. Lines are joined with
/// CRLF, which a `.cmd` wants and the LF-pinned `.rs` file cannot hold
/// literally.
fn script_spec(dir: &tempfile::TempDir, body: &[&str]) -> SpawnSpec {
    let script = dir.path().join("dog.cmd");
    let mut lines = vec!["@echo off".to_string()];
    lines.extend(body.iter().map(|line| (*line).to_string()));
    lines.push(String::new());
    std::fs::write(&script, lines.join("\r\n")).unwrap();

    let mut env = BTreeMap::new();
    for key in [
        "PATH",
        "SystemRoot",
        "windir",
        "SystemDrive",
        "COMSPEC",
        "PATHEXT",
        "TEMP",
    ] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    SpawnSpec {
        name: "metrics".to_string(),
        program: script.display().to_string(),
        cwd: Some(dir.path().to_path_buf()),
        env,
        out_file: dir.path().join("out.log"),
        err_file: dir.path().join("err.log"),
        ..Default::default()
    }
}

/// The next link the runner hands over, or a named panic.
async fn next_link(links: &mut UnboundedReceiver<DogLink>) -> DogLink {
    tokio::time::timeout(SETTLE, links.recv())
        .await
        .expect("a link should arrive once the child has a pid")
        .expect("the runner dropped its links")
}

/// Reads `path` until it holds a complete line, or fails after [`SETTLE`].
async fn first_line(path: &Path) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE;
    while tokio::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Some(line) = text.lines().next()
        {
            return line.to_string();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{} never held a line", path.display());
}

/// fails if a dog cannot open the pipe named in `SHEP_DOG_PIPE`, or the link
/// does not name the dog and the pid it was spawned under.
#[tokio::test]
async fn a_dog_is_spawned_with_a_pipe_to_the_shepherd() {
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("seen.txt");
    let mut spec = script_spec(
        &dir,
        &[
            &format!("(echo %SHEP_DOG_PIPE%) > \"{}\"", seen.display()),
            "(echo hello) > \"%SHEP_DOG_PIPE%\"",
            "ping -n 30 127.0.0.1 >nul",
        ],
    );
    spec.dog_link = true;
    let runner = TokioRunner::new();
    let mut links = runner.dog_links().expect("the first call takes the links");

    let (mut proc, _io) = runner.spawn(&spec).unwrap();
    let mut link = next_link(&mut links).await;

    assert_eq!(link.name, "metrics");
    assert_eq!(link.pid, proc.pid());
    let mut heard = [0_u8; 5];
    let read = tokio::time::timeout(SETTLE, link.stream.read_exact(&mut heard)).await;
    let named = first_line(&seen).await;
    let _ = proc.kill_tree();
    read.expect("the dog wrote to its pipe").unwrap();
    assert_eq!(&heard, b"hello");
    assert!(
        named.starts_with(r"\\.\pipe\shep-dog-"),
        "the pipe is named for a dog and not for the channel: {named}"
    );
}

/// fails if two dogs share a pipe name, which would hand one the other's link.
#[tokio::test]
async fn each_dog_gets_a_pipe_of_its_own() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut names = Vec::new();
    let runner = TokioRunner::new();
    let _links = runner.dog_links().unwrap();
    for dir in [&dir_a, &dir_b] {
        let seen = dir.path().join("seen.txt");
        let mut spec = script_spec(
            dir,
            &[
                &format!("(echo %SHEP_DOG_PIPE%) > \"{}\"", seen.display()),
                "ping -n 30 127.0.0.1 >nul",
            ],
        );
        spec.dog_link = true;
        let (mut proc, _io) = runner.spawn(&spec).unwrap();
        names.push(first_line(&seen).await);
        let _ = proc.kill_tree();
    }

    assert_ne!(names[0], names[1]);
}

/// fails if the links can be taken twice, which would give one runner two
/// servers.
#[tokio::test]
async fn the_links_are_taken_once() {
    let runner = TokioRunner::new();
    assert!(runner.dog_links().is_some());
    assert!(runner.dog_links().is_none());
}

/// fails if a sheep that asked for no link is told it has one.
#[tokio::test]
async fn a_sheep_is_given_no_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("seen.txt");
    let spec = script_spec(
        &dir,
        &[&format!(
            "if defined SHEP_DOG_PIPE ((echo set) > \"{0}\") else ((echo unset) > \"{0}\")",
            seen.display()
        )],
    );
    assert!(!spec.dog_link);
    let runner = TokioRunner::new();

    let (mut proc, _io) = runner.spawn(&spec).unwrap();
    let named = first_line(&seen).await;
    let _ = tokio::time::timeout(SETTLE, proc.wait()).await;

    assert_eq!(named, "unset");
}
