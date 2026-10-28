//! A new sheep cannot take a dog's name, built-in or adopted, whether the
//! dog runs or sits at rest.

use super::*;
use std::collections::BTreeMap;

fn dog(name: &str, source: DogSource) -> crate::dogs::DogSpec {
    crate::dogs::DogSpec {
        name: name.to_string(),
        source,
        env: BTreeMap::new(),
    }
}

async fn start_sheep(
    ctx: &RpcContext,
    name: &str,
) -> Result<Response, shep_core::protocol::RpcError> {
    reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal(name, "./srv")],
                },
            ),
            ctx,
        )
        .await,
    )
    .result
}

#[tokio::test(start_paused = true)]
async fn starting_a_sheep_named_after_a_built_in_dog_at_rest_is_refused() {
    let h = harness(vec![ProcScript::never_exits()]);
    crate::dogs::register_dogs_at_rest(
        &[dog("metrics", DogSource::BuiltIn { grants: None })],
        &[],
        &h.ctx.paths,
        &h.ctx.supervisor,
    )
    .await;

    let refused = start_sheep(&h.ctx, "metrics").await;

    assert!(refused.is_err(), "{refused:?}");
    let flock = list_flock(&h.ctx, 2).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert_eq!(
        flock[0].dog,
        Some(DogSource::BuiltIn { grants: None }),
        "{flock:?}"
    );
    assert_eq!(flock[0].status, ProcStatus::Stopped, "{flock:?}");
}

#[tokio::test(start_paused = true)]
async fn starting_a_sheep_named_after_a_running_dog_is_refused() {
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    enable_dog(&h.ctx, 1, "bark").await;

    let refused = start_sheep(&h.ctx, "bark").await;

    assert!(refused.is_err(), "{refused:?}");
    let flock = list_flock(&h.ctx, 3).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert_eq!(
        flock[0].dog,
        Some(DogSource::BuiltIn { grants: None }),
        "{flock:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn starting_a_sheep_named_after_an_adopted_dog_is_refused() {
    let h = harness(vec![ProcScript::never_exits()]);
    crate::dogs::register_dogs_at_rest(
        &[dog(
            "otel",
            DogSource::Adopted {
                grants: None,
                path: "/usr/local/bin/shep-otel".to_string(),
                channel: true,
            },
        )],
        &[],
        &h.ctx.paths,
        &h.ctx.supervisor,
    )
    .await;

    let refused = start_sheep(&h.ctx, "otel").await;

    assert!(refused.is_err(), "{refused:?}");
    let flock = list_flock(&h.ctx, 2).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert!(flock[0].dog.is_some(), "{flock:?}");
}
