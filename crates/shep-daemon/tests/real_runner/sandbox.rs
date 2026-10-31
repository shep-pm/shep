//! A sandboxed sheep runs `crabpot`, found on the sheep's own `PATH`.

use super::*;

/// A stand-in `crabpot` that prints its argv, one argument per line, and
/// exits. A shim in a tempdir resolves only through the `PATH` the spec
/// carries, so a pass proves the lookup and the wrapped argv together.
#[tokio::test]
async fn a_sandboxed_sheep_runs_the_command_under_crabpot_from_its_path() {
    use shep_core::config::{AppConfig, normalize};
    use shep_core::paths::ShepPaths;
    use shep_core::secrets::SecretView;
    use shep_daemon::assemble::assemble;
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let shim_dir = dir.path().join("bin");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("crabpot");
    fs::write(
        &shim,
        "#!/bin/sh\nfor arg in \"$@\"; do echo \"$arg\"; done\n",
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();

    let mut config = AppConfig::minimal("boxed", "./server");
    config.args = vec!["--port".to_string(), "8080".to_string()];
    config.cwd = Some(dir.path().to_string_lossy().into_owned());
    config.sandbox = Some("sandbox.json".to_string());
    let app = normalize(config).unwrap();
    let mut spec = assemble(
        &app,
        0,
        &ShepPaths::at(dir.path().to_path_buf()),
        None,
        &SecretView::empty("production".to_string()),
    )
    .expect("this fixture carries no secret to resolve");
    spec.env
        .insert("PATH".to_string(), shim_dir.to_string_lossy().into_owned());
    spec.out_file = dir.path().join("out.log");
    spec.err_file = dir.path().join("err.log");

    let (mut proc, mut io) = TokioRunner::new().spawn(&spec).unwrap();
    let _reaper = Reaper::of(&proc);
    let mut argv = Vec::new();
    for _ in 0..6 {
        argv.push(
            recv_log(&mut io, LONG_DEADLINE, "crabpot's argv line")
                .await
                .line,
        );
    }
    let outcome = reaped(&mut proc, LONG_DEADLINE, "the stand-in must exit").await;

    assert_eq!(
        argv,
        [
            "-s",
            &dir.path().join("sandbox.json").to_string_lossy(),
            "--",
            "./server",
            "--port",
            "8080",
        ]
    );
    assert_eq!(outcome.code, Some(0));
}
