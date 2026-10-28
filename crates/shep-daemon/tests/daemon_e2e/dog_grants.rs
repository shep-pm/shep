//! A dog's grants over the connection it was spawned holding: each
//! permission group lets its own requests through and refuses another
//! group's, a dog with no grants keeps full access, and a refusal names the
//! dog, the request and the permission.
//!
//! The dog is a shell script on fd 4 that sends the requests a case lists, in
//! order, once the case says go, and keeps each reply under `$SHEP_HOME`.

use shep_core::permissions::Grants;
use shep_core::protocol::{DogSource, LogStream, ServerFrame};
use shep_daemon::dogs::DogSpec;
use std::collections::BTreeMap;

use super::*;

/// The dog every case runs.
const DOG: &str = "linkdog";

/// A request as the `printf` that writes its frame to fd 4: four length
/// bytes, then the envelope.
fn send(id: u64, body: &Request) -> String {
    let json = serde_json::to_string(&Envelope {
        id,
        deadline_ms: None,
        body: body.clone(),
    })
    .unwrap();
    let len = json.len();
    assert!(len < 256, "the script writes one length byte: {json}");
    format!("printf '\\000\\000\\000\\{len:03o}%s' '{json}' >&4")
}

/// The script: handshake, wait for `go`, then each request and its reply.
fn script(requests: &[Request]) -> String {
    let hello = format!(r#"{{"client_version":"test","protocol":{PROTOCOL_VERSION}}}"#);
    let hello = format!(
        "printf '\\000\\000\\000\\{:03o}%s' '{hello}' >&4",
        hello.len()
    );
    let steps: String = requests
        .iter()
        .enumerate()
        .map(|(i, request)| {
            format!(
                "{}\nread_frame > \"$SHEP_HOME/reply-{i}.tmp\" && mv \"$SHEP_HOME/reply-{i}.tmp\" \"$SHEP_HOME/reply-{i}.json\"\n",
                send(i as u64 + 1, request)
            )
        })
        .collect();
    format!(
        r#"#!/bin/sh
read_frame() {{
  n=$(dd bs=1 count=4 <&4 2>/dev/null | od -An -tu1 | awk 'NR == 1 {{print $3 * 256 + $4}}')
  dd bs=1 count="$n" <&4 2>/dev/null
}}
{hello}
read_frame > /dev/null
while [ ! -e "$SHEP_HOME/go" ]; do sleep 0.05; done
{steps}exec sleep 30
"#
    )
}

/// A running shepherd with the dog on its link, waiting to be told to go.
struct Session {
    fixture: Fixture,
    requests: usize,
}

impl Session {
    /// Boots a shepherd whose dog holds `grants` (`None` for a dog with
    /// none recorded) and will send `requests`.
    async fn boot(grants: Option<&[&str]>, requests: &[Request]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let path = home.join("grantdog.sh");
        std::fs::write(&path, script(requests)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let paths = ShepPaths::resolve(
            &|key| (key == "SHEP_HOME").then(|| home.display().to_string()),
            std::path::Path::new("/nonexistent"),
        );
        let dog = DogSpec {
            name: DOG.to_string(),
            source: DogSource::Adopted {
                path: path.display().to_string(),
                channel: false,
                grants: grants.map(|names| Grants::parse(names).unwrap()),
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
        Self {
            fixture: Fixture {
                dir: Some(dir),
                paths,
                ctx,
                run: Some(run),
                spawned: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            },
            requests: requests.len(),
        }
    }

    /// Lets the dog send its requests.
    fn go(&self) {
        std::fs::write(self.fixture.paths.home.join("go"), "").unwrap();
    }

    /// Lets the dog send its requests, and returns what the shepherd
    /// answered each, in order. Ends the shepherd.
    async fn replies(self) -> Vec<Result<Response, RpcError>> {
        self.go();
        let home = self.fixture.paths.home.clone();
        let mut replies = Vec::new();
        for i in 0..self.requests {
            let text = super::dog_link::await_file(&home.join(format!("reply-{i}.json"))).await;
            match serde_json::from_str::<ServerFrame>(&text) {
                Ok(ServerFrame::Reply(reply)) => replies.push(reply.result),
                other => panic!("reply {i} was not a reply: {other:?} from {text}"),
            }
        }
        self.fixture.shutdown().await;
        replies
    }
}

fn forbidden(reply: &Result<Response, RpcError>) -> Option<&str> {
    match reply {
        Err(err) if err.code == RpcErrorCode::Forbidden => Some(&err.message),
        _ => None,
    }
}

/// A dog granted `grant` and nothing else sends `allowed`, which is let
/// through, and `refused`, which is not. Neither need succeed: a request let
/// through may still fail on its own terms, and only `Forbidden` is the
/// grants' doing.
async fn lets_through_and_refuses(grant: &str, allowed: Request, refused: Request) {
    let replies = Session::boot(Some(&[grant]), &[refused, allowed])
        .await
        .replies()
        .await;
    assert!(
        forbidden(&replies[0]).is_some(),
        "granted only `{grant}`, the second group's request must be refused: {:?}",
        replies[0]
    );
    assert_eq!(
        forbidden(&replies[1]),
        None,
        "granted `{grant}`, its own request must be let through: {:?}",
        replies[1]
    );
}

fn nobody() -> SelectorSpec {
    SelectorSpec::Name("nosuchsheep".to_string())
}

fn stop() -> Request {
    Request::Stop { selector: nobody() }
}

fn delete() -> Request {
    Request::Delete { selector: nobody() }
}

fn tail() -> Request {
    Request::TailLog {
        id: 9999,
        stream: LogStream::Out,
        lines: 1,
    }
}

fn flush() -> Request {
    Request::Flush { selector: nobody() }
}

fn subscribe(topic: &str) -> Request {
    Request::Subscribe {
        topics: vec![topic.to_string()],
    }
}

fn put_secrets(namespace: &str) -> Request {
    Request::PutSecrets {
        namespace: namespace.to_string(),
        environment: "prod".to_string(),
        entries: std::collections::BTreeMap::new(),
    }
}

#[tokio::test]
async fn flock_read_lets_a_dog_look_and_not_move_a_sheep() {
    lets_through_and_refuses("flock.read", Request::ListFlock, stop()).await;
}

#[tokio::test]
async fn flock_control_lets_a_dog_move_a_sheep_and_not_look() {
    lets_through_and_refuses("flock.control", stop(), Request::ListFlock).await;
}

#[tokio::test]
async fn flock_edit_lets_a_dog_delete_and_not_move_a_sheep() {
    lets_through_and_refuses("flock.edit", delete(), stop()).await;
}

#[tokio::test]
async fn logs_read_lets_a_dog_tail_and_not_flush() {
    lets_through_and_refuses("logs.read", tail(), flush()).await;
}

#[tokio::test]
async fn logs_manage_lets_a_dog_flush_and_not_tail() {
    lets_through_and_refuses("logs.manage", flush(), tail()).await;
}

#[tokio::test]
async fn an_events_grant_lets_a_dog_subscribe_inside_its_prefix_only() {
    let replies = Session::boot(
        Some(&["events:process."]),
        &[subscribe("log.*"), subscribe("process.*")],
    )
    .await
    .replies()
    .await;
    assert!(forbidden(&replies[0]).is_some(), "{:?}", replies[0]);
    assert_eq!(replies[1], Ok(Response::Subscribed));
}

#[tokio::test]
async fn secrets_write_is_scoped_to_a_namespace() {
    lets_through_and_refuses(
        "secrets.write:vault",
        put_secrets("vault"),
        put_secrets("other"),
    )
    .await;
}

#[tokio::test]
async fn dogs_control_lets_a_dog_disable_another_and_not_move_a_sheep() {
    lets_through_and_refuses(
        "dogs.control",
        Request::DisableDog {
            name: "other".to_string(),
        },
        stop(),
    )
    .await;
}

#[tokio::test]
async fn dogs_trigger_is_scoped_to_a_dog_and_flock_control_does_not_reach_it() {
    let trigger = |name: &str| Request::Trigger {
        selector: SelectorSpec::Name(name.to_string()),
        action: "ping".to_string(),
        params: None,
    };
    let replies = Session::boot(
        Some(&["dogs.trigger:linkdog"]),
        &[trigger("web"), trigger(DOG)],
    )
    .await
    .replies()
    .await;
    assert!(
        forbidden(&replies[0]).is_some(),
        "a sheep takes flock.control: {:?}",
        replies[0]
    );
    assert_eq!(forbidden(&replies[1]), None, "{:?}", replies[1]);

    let replies = Session::boot(Some(&["flock.control"]), &[trigger(DOG)])
        .await
        .replies()
        .await;
    assert!(
        forbidden(&replies[0]).is_some(),
        "a dog takes dogs.trigger, which flock.control is not: {:?}",
        replies[0]
    );
}

#[tokio::test]
async fn shepherd_control_alone_lets_a_dog_stop_the_shepherd() {
    let replies = Session::boot(Some(&["flock.read"]), &[Request::KillDaemon])
        .await
        .replies()
        .await;
    assert!(forbidden(&replies[0]).is_some(), "{:?}", replies[0]);

    // The reply is not the evidence: the shepherd takes the dog down with
    // it, and the dog may not live to record the reply. The run ending
    // without anyone calling for it is.
    let mut session = Session::boot(Some(&["shepherd.control"]), &[Request::KillDaemon]).await;
    session.go();
    let run = session.fixture.run.take().unwrap();
    tokio::time::timeout(RECV_TIMEOUT, run)
        .await
        .expect("a dog granted shepherd.control must be able to stop the shepherd")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn secrets_read_is_scoped_to_the_environment_a_sheep_resolves_in() {
    let read = |sheep: &str| Request::SheepDogSettingsResolved {
        name: sheep.to_string(),
        dog: DOG.to_string(),
    };
    let session = Session::boot(
        Some(&["secrets.read:prod"]),
        &[read("staged"), read("live")],
    )
    .await;
    let mut operator = session.fixture.connect().await;
    let mut staged = forever_app("staged");
    staged.environment = Some("staging".to_string());
    let mut live = forever_app("live");
    live.environment = Some("prod".to_string());
    let started = operator
        .request(Request::Start {
            apps: vec![staged, live],
        })
        .await;
    assert!(started.result.is_ok(), "{:?}", started.result);

    let replies = session.replies().await;
    assert!(
        forbidden(&replies[0]).is_some_and(|m| m.contains("secrets.read:staging")),
        "{:?}",
        replies[0]
    );
    assert_eq!(forbidden(&replies[1]), None, "{:?}", replies[1]);
}

#[tokio::test]
async fn a_dog_with_no_grants_recorded_keeps_full_access() {
    let replies = Session::boot(
        None,
        &[
            Request::ListFlock,
            stop(),
            delete(),
            tail(),
            put_secrets("any"),
        ],
    )
    .await
    .replies()
    .await;
    for reply in &replies {
        assert_eq!(forbidden(reply), None, "{reply:?}");
    }
}

#[tokio::test]
async fn a_dog_granted_nothing_still_reaches_its_own_section_and_the_handshake() {
    let own = Request::DogConfig {
        name: DOG.to_string(),
    };
    let replies = Session::boot(Some(&[]), &[Request::Ping, own, Request::ListFlock])
        .await
        .replies()
        .await;
    assert_eq!(replies[0], Ok(Response::Pong));
    assert_eq!(forbidden(&replies[1]), None, "{:?}", replies[1]);
    assert!(forbidden(&replies[2]).is_some(), "{:?}", replies[2]);
}

#[tokio::test]
async fn a_refusal_names_the_dog_the_request_and_the_permission() {
    let replies = Session::boot(Some(&["flock.read"]), &[stop()])
        .await
        .replies()
        .await;
    assert_eq!(
        forbidden(&replies[0]),
        Some(
            "dog `linkdog` was refused `stop`: it needs `flock.control`, which was not granted to it"
        )
    );
}
