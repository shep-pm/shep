//! `shep adopt` of a dog under the name `sandbox`.
//!
//! A dog adopted as `sandbox` runs sheep through
//! `<path> launch <sheep> -- <cmd>`, so adopt asks the manifest whether the
//! binary can, before it records the path. The name checked is the one the
//! dog is adopted under, not the binary's own.

use std::path::{Path, PathBuf};
use std::time::Duration;

use shep_core::paths::ShepPaths;

use super::*;
use crate::cli::AdoptArgs;
use crate::commands::dogs::adopt;
use crate::commands::dogs::test_support::streams;

/// A budget for tests that are not about the budget: the probe spawns a real
/// child and bounds its wait on a wall clock.
const BUDGET: Duration = Duration::from_secs(30);

/// A dog that answers [`MANIFEST_FLAG`](shep_core::dogs::MANIFEST_FLAG) with
/// `manifest`, or with nothing when `manifest` is `None`, and records every
/// flag it was run with in `asked` beside itself.
fn dog(dir: &Path, manifest: Option<&str>) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("shep-box");
    let answer = manifest.map_or_else(String::new, |manifest| {
        format!(
            "{flag})\necho '{manifest}'\n;;\n",
            flag = shep_core::dogs::MANIFEST_FLAG
        )
    });
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$1\" >> '{asked}'\ncase \"$1\" in\n{answer}esac\n",
            asked = dir.join("asked").display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

fn declared(launch: bool) -> String {
    serde_json::json!({
        "manifest": 1,
        "name": "box",
        "version": "0.2.0",
        "protocol": shep_core::protocol::PROTOCOL_VERSION,
        "launch": launch,
    })
    .to_string()
}

fn flags_asked(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("asked"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_dog_adopted_as_sandbox_without_launch_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dog(dir.path(), Some(&declared(false)));

    assert_eq!(
        vet_binary_within(&bin, dir.path(), "sandbox", BUDGET),
        Err(AdoptRefusal::SandboxWithoutLaunch)
    );
    assert_eq!(
        flags_asked(dir.path()),
        [shep_core::dogs::MANIFEST_FLAG],
        "a refused candidate is not run a second time"
    );
}

#[test]
fn a_dog_adopted_as_sandbox_with_no_manifest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dog(dir.path(), None);

    assert_eq!(
        vet_binary_within(&bin, dir.path(), "sandbox", BUDGET),
        Err(AdoptRefusal::SandboxWithoutLaunch)
    );
    assert_eq!(flags_asked(dir.path()), [shep_core::dogs::MANIFEST_FLAG]);
}

#[test]
fn a_dog_adopted_as_sandbox_that_declares_launch_is_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dog(dir.path(), Some(&declared(true)));

    let vetted = vet_binary_within(&bin, dir.path(), "sandbox", BUDGET).expect("adopted");

    assert!(vetted.manifest.expect("a manifest").launch);
}

#[test]
fn a_dog_adopted_under_another_name_is_not_asked_for_launch() {
    let dir = tempfile::tempdir().unwrap();
    let without = dog(dir.path(), Some(&declared(false)));
    assert!(vet_binary_within(&without, dir.path(), "kelpie", BUDGET).is_ok());
    let none = dog(dir.path(), None);
    assert!(vet_binary_within(&none, dir.path(), "kelpie", BUDGET).is_ok());
    let with = dog(dir.path(), Some(&declared(true)));
    assert!(vet_binary_within(&with, dir.path(), "kelpie", BUDGET).is_ok());
}

#[tokio::test]
async fn adopt_refuses_a_sandbox_dog_without_launch_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let args = AdoptArgs {
        yes: true,
        name: Some("sandbox".to_string()),
        path: dog(dir.path(), Some(&declared(false))),
    };

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    let text = String::from_utf8(err).unwrap();
    assert_eq!(code, ExitCode::InvalidConfig, "{text}");
    assert!(
        text.contains("`sandbox`") && text.contains("`launch`") && text.contains("another name"),
        "the refusal names the dog and both ways out: {text}"
    );
    assert!(
        !paths.daemon_config.exists(),
        "a refused adopt writes no `shep.toml`"
    );
}
