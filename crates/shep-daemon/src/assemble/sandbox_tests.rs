use super::assemble;
use super::testing::*;
use shep_core::config::{AppConfig, normalize};

/// `normalize` refuses `sandbox` on Windows, so the tests that set it are Unix only.
fn sandboxed(edit: impl FnOnce(&mut AppConfig)) -> crate::runner::SpawnSpec {
    let mut config = AppConfig::minimal("app", "/opt/bin/server");
    config.args = vec!["--port".to_string(), "8080".to_string()];
    edit(&mut config);
    let app = normalize(config).unwrap();
    assemble(&app, 0, &test_paths(), None, &no_secrets()).unwrap()
}

#[test]
#[cfg(unix)]
fn a_sandbox_wraps_the_command_in_crabpot() {
    let spec = sandboxed(|c| c.sandbox = Some("/etc/box.json".to_string()));

    assert_eq!(spec.program, "crabpot");
    assert_eq!(
        spec.args,
        vec![
            "-s",
            "/etc/box.json",
            "--",
            "/opt/bin/server",
            "--port",
            "8080"
        ]
    );
    assert!(spec.leader_only_stop);
}

#[test]
#[cfg(unix)]
fn a_sandbox_wraps_the_interpreter_after_it_has_wrapped_the_script() {
    let spec = sandboxed(|c| {
        c.script = "app.js".to_string();
        c.interpreter = Some("node".to_string());
        c.sandbox = Some("/etc/box.json".to_string());
    });

    assert_eq!(spec.program, "crabpot");
    assert_eq!(
        spec.args,
        vec![
            "-s",
            "/etc/box.json",
            "--",
            "node",
            "app.js",
            "--port",
            "8080"
        ]
    );
}

#[test]
#[cfg(unix)]
fn a_relative_sandbox_file_is_anchored_at_the_app_cwd() {
    let spec = sandboxed(|c| {
        c.cwd = Some("/srv/app".to_string());
        c.sandbox = Some("sandbox.json".to_string());
    });

    assert_eq!(&spec.args[..2], ["-s", "/srv/app/sandbox.json"]);
}

#[test]
#[cfg(unix)]
fn an_absolute_sandbox_file_ignores_the_app_cwd() {
    let spec = sandboxed(|c| {
        c.cwd = Some("/srv/app".to_string());
        c.sandbox = Some("/etc/box.json".to_string());
    });

    assert_eq!(&spec.args[..2], ["-s", "/etc/box.json"]);
}

#[test]
#[cfg(unix)]
fn a_relative_sandbox_file_without_a_cwd_is_left_alone() {
    let spec = sandboxed(|c| c.sandbox = Some("sandbox.json".to_string()));

    assert_eq!(&spec.args[..2], ["-s", "sandbox.json"]);
}

#[test]
fn no_sandbox_leaves_the_command_and_the_stop_signal_alone() {
    let spec = sandboxed(|_| {});

    assert_eq!(spec.program, "/opt/bin/server");
    assert_eq!(spec.args, vec!["--port", "8080"]);
    assert!(!spec.leader_only_stop);
}
