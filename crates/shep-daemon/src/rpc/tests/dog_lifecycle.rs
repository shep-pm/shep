//! Enabling and disabling a dog, reading and writing its config file,
//! an adopted-but-never-started dog staying reachable, and a dog
//! request refused against a sheep's name or a stopped engine.

use super::*;

/// Sends one `SetDogConfig` and hands back the reply.
async fn set_dog_config(
    ctx: &RpcContext,
    id: u64,
    name: &str,
    toml: &str,
) -> Result<Response, RpcError> {
    reply_of(
        dispatch(
            envelope(
                id,
                Request::SetDogConfig {
                    name: name.to_string(),
                    toml: toml.to_string().into(),
                },
            ),
            ctx,
        )
        .await,
    )
    .result
}

/// The file is written after the harness built its context, so a reader
/// that cached at boot answers the empty string here.
#[tokio::test(start_paused = true)]
async fn a_dog_config_request_reads_the_file_as_it_stands_now() {
    let h = harness(vec![]);
    std::fs::write(&h.ctx.dogs_config, "[bark]\ndebounce = \"30s\"\n").unwrap();
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::DogConfig {
                    name: "bark".to_string(),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::DogSection { toml }) = reply.result else {
        panic!("expected DogSection, got {:?}", reply.result)
    };
    assert!(toml.as_str().contains("30s"));
}

/// This door differs from a CLI writing the file directly because it
/// publishes: a running dog subscribed to `config.dog.<name>` is the
/// only reader that finds out a section moved. Asserted on a
/// subscriber rather than the publish call, the way the dog
/// contract's own `bark_subscribes_to_its_own_config_topic` does,
/// since a publisher that ran proves nothing about what a dog hears.
#[tokio::test(start_paused = true)]
async fn set_dog_config_writes_the_file_and_a_subscriber_hears_about_it() {
    let h = harness(vec![ProcScript::never_exits()]);
    enable_dog(&h.ctx, 1, "bark").await;
    let mut sub = h.ctx.events.subscribe();

    let reply = set_dog_config(&h.ctx, 2, "bark", "poll = \"30s\"\n").await;
    assert_eq!(
        reply.unwrap(),
        Response::DogConfigSet {
            name: "bark".to_string()
        }
    );

    let text = std::fs::read_to_string(&h.ctx.dogs_config).unwrap();
    assert!(text.contains("poll = \"30s\""), "{text}");

    // The dog's own spawn narrates itself on the same bus, so the
    // topic wanted here is not necessarily the first frame waiting.
    let mut topics = Vec::new();
    while let Ok(Ok(event)) = tokio::time::timeout(Duration::from_secs(5), sub.recv()).await {
        topics.push(event.to_event().topic().into_owned());
        if topics
            .last()
            .is_some_and(|topic| topic == "config.dog.bark")
        {
            break;
        }
    }
    assert!(
        topics.iter().any(|topic| topic == "config.dog.bark"),
        "{topics:?}"
    );
}

/// The dog most in need of configuring is the one that is switched
/// off: an operator adopts a dog, sets its webhook, and only then
/// enables it. A guard on `supervisor.list()` would refuse exactly
/// that dog, and this one has never been started at all.
#[tokio::test(start_paused = true)]
async fn a_dog_that_is_adopted_and_never_started_can_still_be_configured() {
    let mut h = harness(vec![]);
    h.ctx.known_dogs = KnownDogs::new(["otel".to_string()].into_iter().collect());

    let reply = set_dog_config(&h.ctx, 1, "otel", "endpoint = \"http://127.0.0.1:4317\"\n").await;

    assert_eq!(
        reply.unwrap(),
        Response::DogConfigSet {
            name: "otel".to_string()
        }
    );
    let text = std::fs::read_to_string(&h.ctx.dogs_config).unwrap();
    assert!(text.contains("4317"), "{text}");
}

/// A dog an operator has enabled but that is not up right now
/// (crashed, stopped, or simply not spawned on this boot) is still a
/// dog whose section this shepherd holds. The harness starts nothing,
/// so `bark` is known and absent from the flock.
#[tokio::test(start_paused = true)]
async fn a_dog_that_is_enabled_but_not_running_can_still_be_configured() {
    let h = harness(vec![]);
    assert!(h.ctx.supervisor.list().await.is_empty());

    let reply = set_dog_config(&h.ctx, 1, "bark", "poll = \"30s\"\n").await;

    assert_eq!(
        reply.unwrap(),
        Response::DogConfigSet {
            name: "bark".to_string()
        }
    );
}

/// A dog adopted and enabled since this shepherd started is not in
/// the list the CLI handed over at boot (refreshed only by a `shep
/// daemon reload`), so a guard that stopped there would refuse a dog
/// that is up and answering right now.
#[tokio::test(start_paused = true)]
async fn a_dog_adopted_since_boot_is_reached_through_the_running_flock() {
    let mut h = harness(vec![ProcScript::never_exits()]);
    h.ctx.known_dogs = KnownDogs::new(BTreeSet::new());
    enable_dog(&h.ctx, 1, "bark").await;

    let reply = set_dog_config(&h.ctx, 2, "bark", "poll = \"30s\"\n").await;

    assert_eq!(
        reply.unwrap(),
        Response::DogConfigSet {
            name: "bark".to_string()
        }
    );
}

/// The boot-time list is a snapshot, so a dog adopted against a running
/// shepherd is in neither half of the old guard unless the running flock
/// answers for it, and both of these were refused:
///
/// - adopt, disable, configure, enable
/// - adopt, the dog crashes for want of config, configure
/// A disabled dog stays in the flock, so it is that entry that answers.
#[tokio::test(start_paused = true)]
async fn a_dog_adopted_since_boot_stays_configurable_once_it_is_disabled() {
    let mut h = harness(vec![ProcScript::never_exits()]);
    h.ctx.known_dogs = KnownDogs::new(BTreeSet::new());
    enable_dog(&h.ctx, 1, "bark").await;
    let disabled = reply_of(
        dispatch(
            envelope(
                2,
                Request::DisableDog {
                    name: "bark".to_string(),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(disabled.result.is_ok(), "{:?}", disabled.result);

    let reply = set_dog_config(&h.ctx, 3, "bark", "poll = \"30s\"\n").await;

    assert_eq!(
        reply.unwrap(),
        Response::DogConfigSet {
            name: "bark".to_string()
        }
    );
}

/// This is the inverse of the guard every other config door carries:
/// `dogs.toml` holds dogs' sections and nothing else, so it has to
/// refuse a sheep's name, one nobody registered with it. Refused
/// before the file opens, so a mistyped name leaves no stray table
/// behind for a dog that will never exist.
#[tokio::test(start_paused = true)]
async fn setting_a_dogs_config_over_a_sheeps_name_is_refused_and_writes_nothing() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web(&h).await;
    std::fs::write(&h.ctx.dogs_config, "[bark]\npoll = \"60s\"\n").unwrap();

    for (id, name) in [(2, "web"), (3, "ghost")] {
        let reply = set_dog_config(&h.ctx, id, name, "poll = \"30s\"\n").await;
        let Err(err) = reply else {
            panic!("{name} is not a dog and must be refused")
        };
        assert_eq!(err.code, RpcErrorCode::NotFound, "{err:?}");
        assert!(err.message.contains(name), "{err:?}");
    }

    assert_eq!(
        std::fs::read_to_string(&h.ctx.dogs_config).unwrap(),
        "[bark]\npoll = \"60s\"\n"
    );
}

/// `otel` is outside the harness's built-in dogs, so the guard gets past
/// `known_dogs` and reaches the running-flock widening, which is the half
/// that has to answer on a stopped engine.
#[tokio::test]
async fn setting_a_dogs_config_against_a_stopped_engine_is_refused_not_a_panic() {
    let h = harness(vec![]);
    h.ctx.supervisor.shutdown().await;

    let reply = set_dog_config(&h.ctx, 1, "otel", "poll = \"30s\"\n").await;

    let Err(err) = reply else {
        panic!("otel is not a known dog and must be refused")
    };
    assert_eq!(err.code, RpcErrorCode::NotFound, "{err:?}");
}
