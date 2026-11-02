//! A sheep with a `dogs.sandbox` table runs its command through the dog's `launch`.

use super::*;

/// A stand-in dog that records its argv and `SHEP_HOME`, then runs the command
/// after `--`. The real dog's `launch` does not exist yet, so this pins only
/// the contract the shepherd spawns against.
#[tokio::test]
async fn a_sheep_runs_its_command_through_the_stub_dog_with_shep_home_set() {
    use shep_core::config::{AppConfig, normalize};
    use shep_core::paths::ShepPaths;
    use shep_core::secrets::SecretView;
    use shep_daemon::assemble::{assemble, through_dog};
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("record.txt");
    let stub = dir.path().join("stub-dog");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\n\
             for arg in \"$@\"; do echo \"arg=$arg\"; done > '{record}'\n\
             echo \"SHEP_HOME=$SHEP_HOME\" >> '{record}'\n\
             while [ \"$1\" != -- ]; do shift; done\n\
             shift\n\
             exec \"$@\"\n",
            record = record.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

    let mut config = AppConfig::minimal("jailed", "/bin/echo");
    config.args = vec!["from the sheep".to_string()];
    config.dogs.insert(
        "sandbox".to_string(),
        serde_json::from_value(serde_json::json!({})).unwrap(),
    );
    let app = normalize(config).unwrap();
    let paths = ShepPaths::at(dir.path().to_path_buf());
    let spec = assemble(
        &app,
        0,
        &paths,
        None,
        &SecretView::empty("production".to_string()),
    )
    .expect("this fixture carries no secret to resolve");
    let mut spec = through_dog(spec, &stub.to_string_lossy(), &paths.home, "jailed");
    spec.out_file = dir.path().join("out.log");
    spec.err_file = dir.path().join("err.log");

    let (mut proc, mut io) = TokioRunner::new().spawn(&spec).unwrap();
    let _reaper = Reaper::of(&proc);
    let line = recv_log(&mut io, LONG_DEADLINE, "the command's output").await;
    let outcome = reaped(&mut proc, LONG_DEADLINE, "the command must exit").await;

    assert_eq!(line.line, "from the sheep");
    assert_eq!(outcome.code, Some(0));
    assert_eq!(
        fs::read_to_string(&record).unwrap(),
        format!(
            "arg=launch\narg=jailed\narg=--\narg=/bin/echo\narg=from the sheep\nSHEP_HOME={}\n",
            dir.path().display()
        )
    );
}
