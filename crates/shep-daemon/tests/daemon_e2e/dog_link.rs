//! A dog spawned ahead of the muster restore reaches the shepherd on the
//! connection it was spawned holding, with the control socket never served
//! (#360).

use shep_core::protocol::DogSource;
use shep_daemon::dogs::DogSpec;
use std::collections::BTreeMap;

use super::*;

/// One frame as a `printf` that writes it to the dog's fd 4: four length
/// bytes, then `json`.
fn send_frame(json: &str) -> String {
    assert!(json.len() < 256, "the script writes one length byte");
    format!(
        "printf '\\000\\000\\000\\{:03o}%s' '{json}' >&4",
        json.len()
    )
}

/// A shell dog that handshakes on fd 4, pings, records both answers under
/// `$SHEP_HOME`, and stays up.
fn dog_script() -> String {
    let hello = format!(r#"{{"client_version":"test","protocol":{PROTOCOL_VERSION}}}"#);
    let ping = r#"{"id":1,"deadline_ms":null,"body":{"kind":"ping"}}"#;
    format!(
        r#"#!/bin/sh
read_frame() {{
  n=$(dd bs=1 count=4 <&4 2>/dev/null | od -An -tu1 | awk '{{print $4}}')
  dd bs=1 count="$n" <&4 2>/dev/null
}}
{hello_send}
read_frame > "$SHEP_HOME/ack.json.tmp" && mv "$SHEP_HOME/ack.json.tmp" "$SHEP_HOME/ack.json"
{ping_send}
read_frame > "$SHEP_HOME/pong.json.tmp" && mv "$SHEP_HOME/pong.json.tmp" "$SHEP_HOME/pong.json"
exec sleep 30
"#,
        hello_send = send_frame(&hello),
        ping_send = send_frame(ping),
    )
}

/// Polls `path` until it exists, failing at [`RECV_TIMEOUT`].
pub(super) async fn await_file(path: &std::path::Path) -> String {
    tokio::time::timeout(RECV_TIMEOUT, async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{} never appeared", path.display()))
}

/// fails if a boot-first dog needs the control socket's listener to reach the
/// shepherd. `run` is never called until the end, so nothing accepts on the
/// socket: the handshake can only have been served on the dog's own
/// connection, and the dog is not restarted for the want of one.
#[tokio::test]
async fn a_boot_first_dog_handshakes_while_the_socket_is_not_served() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let script = home.join("linkdog.sh");
    std::fs::write(&script, dog_script()).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let paths = ShepPaths::resolve(
        &|key| (key == "SHEP_HOME").then(|| home.display().to_string()),
        std::path::Path::new("/nonexistent"),
    );
    let dog = DogSpec {
        name: "linkdog".to_string(),
        source: DogSource::Adopted {
            grants: None,
            path: script.display().to_string(),
            channel: false,
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
                boot_first_dogs: vec!["linkdog".to_string()],
                ..BootOptions::default()
            },
        ),
    )
    .await
    .expect("boot must not hang")
    .expect("the daemon must boot");
    let ctx = daemon.context();

    let ack = await_file(&home.join("ack.json")).await;
    assert!(ack.contains("daemon_version"), "{ack}");
    let pong = await_file(&home.join("pong.json")).await;
    assert!(pong.contains("Pong") || pong.contains("pong"), "{pong}");

    // Only now is the socket served: reading the flock needs it, and the
    // assertions above already proved it was not needed for the dog.
    let run = tokio::spawn(daemon.run());
    let fixture = Fixture {
        dir: Some(dir),
        paths,
        ctx,
        run: Some(run),
        spawned: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let mut operator = fixture.connect().await;
    let Ok(Response::Flock(flock)) = operator.request(Request::ListFlock).await.result else {
        panic!("ListFlock must answer with the flock")
    };
    let row = flock
        .into_iter()
        .find(|info| info.name == "linkdog")
        .expect("the dog is in the flock");
    assert_eq!(row.restarts, 0, "the dog was not restarted: {row:?}");

    let log = row.err_file.clone().expect("a dog has an err_file");
    let attributed = tokio::time::timeout(RECV_TIMEOUT, async {
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains("[shep] this dog sent shep a `ping` request") {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        attributed.is_ok(),
        "the dog's log never attributed its request: {:?}",
        std::fs::read_to_string(&log)
    );

    fixture.shutdown().await;
}
