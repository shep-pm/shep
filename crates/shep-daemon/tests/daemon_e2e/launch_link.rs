//! A sheep launched through the sandbox dog reads its own table over the link
//! it was spawned holding, whoever it runs as (#1443).

use shep_core::protocol::DogSource;
use shep_daemon::dogs::DogSpec;
use std::collections::BTreeMap;

use super::*;

/// The stand-in sandbox dog. Run as a dog it only stays up. Run as
/// `launch web -- ...` it handshakes on fd 4, reads `web`'s `sandbox` table,
/// then dials the control socket at `$SOCK`, and records both under `$REC`.
///
/// The probe exits 3 when the OS refuses the connect, 4 when the daemon
/// accepts it and hangs up without answering, and 0 when the daemon serves it.
const DOG: &str = r#"#!/bin/sh
if [ "$1" != launch ]; then exec sleep 30; fi
read_frame() {
  n=$(dd bs=1 count=4 <&4 2>/dev/null | od -An -tu1 | awk '{print $4}')
  dd bs=1 count="$n" <&4 2>/dev/null
}
send_frame() {
  printf "\\000\\000\\000\\$(printf %03o ${#1})%s" "$1" >&4
}
send_frame '{"client_version":"test","protocol":@PROTOCOL@}'
read_frame > /dev/null
send_frame '{"id":1,"deadline_ms":null,"body":{"kind":"sheep_dog_settings_resolved","name":"web","dog":"sandbox"}}'
read_frame > "$REC/table.tmp" && mv "$REC/table.tmp" "$REC/table.json"
perl -MSocket -e '
  socket(S, AF_UNIX, SOCK_STREAM, 0) or exit 2;
  connect(S, sockaddr_un($ARGV[0])) or exit 3;
  my $hello = q({"client_version":"test","protocol":@PROTOCOL@});
  syswrite(S, pack("N", length $hello) . $hello);
  exit(sysread(S, my $buf, 4) ? 0 : 4);
' "$SOCK"
echo $? > "$REC/control.tmp" && mv "$REC/control.tmp" "$REC/control.txt"
exec sleep 30
"#;

/// A directory every user can read and write, for the dog script and its
/// records: the home is `0700`, so a sheep running as another user reaches
/// nothing under it.
fn shared_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    dir
}

/// Boots a daemon whose sandbox dog is [`DOG`], starts `web` carrying a
/// `sandbox` table as `user`, and returns what the launch recorded: the
/// reply to its table read and the control-socket probe's exit status.
async fn launched_as(user: Option<&str>) -> (String, String) {
    let shared = shared_dir();
    let script = shared.path().join("sandbox-dog.sh");
    std::fs::write(
        &script,
        DOG.replace("@PROTOCOL@", &PROTOCOL_VERSION.to_string()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let paths = ShepPaths::resolve(
        &|key| (key == "SHEP_HOME").then(|| home.display().to_string()),
        std::path::Path::new("/nonexistent"),
    );
    let dog = DogSpec {
        name: "sandbox".to_string(),
        source: DogSource::Adopted {
            grants: None,
            path: script.display().to_string(),
            channel: false,
            events: Vec::new(),
        },
        env: BTreeMap::new(),
    };
    let daemon = tokio::time::timeout(
        RECV_TIMEOUT,
        boot(
            TokioRunner::new(),
            paths.clone(),
            BootOptions {
                dogs: vec![dog.clone()],
                known_dogs: vec![dog],
                ..BootOptions::default()
            },
        ),
    )
    .await
    .expect("boot must not hang")
    .expect("the daemon must boot");
    let ctx = daemon.context();
    let run = tokio::spawn(daemon.run());
    let fixture = Fixture {
        dir: Some(dir),
        paths,
        ctx,
        run: Some(run),
        spawned: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    };

    let mut app = AppConfig::minimal("web", "/bin/sh");
    app.user = user.map(str::to_string);
    // The child changes directory after the drop, so the default under the
    // `0700` home would be unreachable to it.
    app.cwd = Some(shared.path().display().to_string());
    app.env
        .insert("REC".into(), shared.path().display().to_string());
    app.env
        .insert("SOCK".into(), fixture.paths.socket.display().to_string());
    app.dogs.insert(
        "sandbox".to_string(),
        serde_json::from_value(serde_json::json!({ "network": "none" })).unwrap(),
    );
    let mut operator = fixture.connect().await;
    operator.start_first(app).await;

    let table = super::dog_link::await_file(&shared.path().join("table.json")).await;
    let control = super::dog_link::await_file(&shared.path().join("control.txt")).await;
    fixture.shutdown().await;
    (table, control.trim().to_string())
}

/// fails if a launched sheep cannot read its table over the link it was
/// spawned holding, or if the link makes the control socket answer it.
#[tokio::test]
async fn a_launched_sheep_reads_its_table_over_its_link() {
    let (table, _control) = launched_as(None).await;

    assert!(table.contains("sheep_dog_table"), "{table}");
    assert!(table.contains(r#""network":"none""#), "{table}");
}

/// fails if a sheep running as another user cannot read its table over the
/// link, or if the control socket will talk to it.
#[tokio::test]
#[ignore = "needs root: run with `sudo -E cargo test -p shep-daemon --test daemon_e2e -- --ignored`"]
async fn a_launched_sheep_with_a_user_reads_its_table_while_the_control_socket_stays_refused() {
    assert!(
        nix::unistd::geteuid().is_root(),
        "this test only means anything as root"
    );

    let (table, control) = launched_as(Some("nobody")).await;

    assert!(table.contains("sheep_dog_table"), "{table}");
    assert!(table.contains(r#""network":"none""#), "{table}");
    assert!(
        control == "3" || control == "4",
        "the control socket must refuse a sheep running as another user, probe exit: {control}"
    );
}
