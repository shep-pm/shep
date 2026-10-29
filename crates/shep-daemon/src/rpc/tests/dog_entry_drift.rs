//! `ConfigDrift` over an adopted dog: the comparison `shep daemon reload` and
//! `shep restart` lean on to say a dog keeps an entry `shep.toml` no longer
//! has.

use super::*;
use crate::dogs::{DogSpec, dog_app};

fn kelpie(path: &str, env: &[(&str, &str)]) -> DogSpec {
    DogSpec {
        name: "kelpie".to_string(),
        source: DogSource::Adopted {
            path: path.to_string(),
            channel: false,
            grants: None,
        },
        env: env
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
    }
}

/// What the shepherd reports for `entry` against a `kelpie` enabled from
/// `spawned`.
async fn drift_of(spawned: &DogSpec, entry: &DogSpec) -> Vec<String> {
    let h = harness(vec![ProcScript::never_exits()]);
    let enabled = reply_of(
        dispatch(
            envelope(
                1,
                Request::EnableDog {
                    name: spawned.name.clone(),
                    env: spawned
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone().into()))
                        .collect(),
                    source: spawned.source.clone(),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(
        matches!(enabled.result, Ok(Response::DogStarted(_))),
        "{:?}",
        enabled.result
    );

    let app = dog_app(entry, &h.ctx.paths).unwrap().into_config();
    let reply = reply_of(
        dispatch(
            envelope(2, Request::ConfigDrift { apps: vec![app] }),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::Drifted(drifted)) = reply.result else {
        panic!("expected Drifted, got {:?}", reply.result)
    };
    drifted.into_iter().flat_map(|drift| drift.fields).collect()
}

#[tokio::test(start_paused = true)]
async fn an_entry_that_gained_an_env_key_since_the_spawn_drifts_in_env_alone() {
    let spawned = kelpie("/usr/local/bin/shep-kelpie", &[]);
    let entry = kelpie(
        "/usr/local/bin/shep-kelpie",
        &[("PADDOCK_KEY", "{{secret:paddock-key}}")],
    );

    assert_eq!(drift_of(&spawned, &entry).await, ["env"]);
}

#[tokio::test(start_paused = true)]
async fn an_entry_that_moved_its_path_since_the_spawn_drifts_in_script_alone() {
    let spawned = kelpie("/usr/local/bin/shep-kelpie", &[]);
    let entry = kelpie("/opt/kelpie/bin/shep-kelpie", &[]);

    assert_eq!(drift_of(&spawned, &entry).await, ["script"]);
}

#[tokio::test(start_paused = true)]
async fn an_entry_the_dog_was_spawned_from_does_not_drift() {
    let spawned = kelpie("/usr/local/bin/shep-kelpie", &[("K", "v")]);

    assert_eq!(drift_of(&spawned, &spawned).await, Vec::<String>::new());
}
