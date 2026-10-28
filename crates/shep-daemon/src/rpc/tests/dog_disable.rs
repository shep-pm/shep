//! `DisableDog`: stops the dog and keeps it listed, still answering
//! `Deleted` for the older CLI that only accepts that reply.

use super::*;
use std::collections::BTreeMap;

async fn disable_dog(h: &Harness, id: u64, name: &str) -> Reply {
    reply_of(
        dispatch(
            envelope(
                id,
                Request::DisableDog {
                    name: name.to_string(),
                },
            ),
            &h.ctx,
        )
        .await,
    )
}

/// fails if `DisableDog` goes back to deregistering the dog, which takes
/// `metrics` and `bark` out of `shep flock` until the next boot.
#[tokio::test(start_paused = true)]
async fn disabling_a_dog_stops_it_and_keeps_it_listed_as_disabled() {
    let h = harness(vec![ProcScript::never_exits()]);
    let info = enable_dog(&h.ctx, 1, "bark").await;

    let disabled = disable_dog(&h, 2, "bark").await;

    assert_eq!(disabled.result.unwrap(), Response::Deleted(vec![info.id]));
    let flock = list_flock(&h.ctx, 3).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert_eq!(flock[0].id, info.id);
    assert_eq!(flock[0].dog, Some(DogSource::BuiltIn { grants: None }));
    assert_eq!(flock[0].status, ProcStatus::Stopped);
    assert_eq!(flock[0].pid, None);
    assert_eq!(flock[0].disabled, Some(true));
}

/// fails if a dog that ran and stopped counts as already running, so
/// `enable` answers `DogStarted` while the listing still reads `stopped`.
#[tokio::test(start_paused = true)]
async fn enabling_a_disabled_dog_starts_it_again_in_its_row() {
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let first = enable_dog(&h.ctx, 1, "bark").await;
    disable_dog(&h, 2, "bark").await;

    let again = enable_dog(&h.ctx, 3, "bark").await;

    assert_eq!(again.id, first.id);
    assert_eq!(again.status, ProcStatus::Online);
    assert_eq!(again.disabled, None);
    let flock = list_flock(&h.ctx, 4).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert_eq!(flock[0].status, ProcStatus::Online);
    assert_eq!(flock[0].disabled, None);
}

/// A dog at rest has no process to stop, and the mark is all that changes.
#[tokio::test(start_paused = true)]
async fn disabling_a_dog_that_never_ran_marks_it_and_answers_deleted() {
    let h = harness(vec![]);
    let spec = crate::dogs::DogSpec {
        name: "bark".to_string(),
        source: DogSource::BuiltIn { grants: None },
        env: BTreeMap::new(),
    };
    crate::dogs::register_dogs_at_rest(&[spec], &[], &h.ctx.paths, &h.ctx.supervisor).await;
    let before = list_flock(&h.ctx, 1).await;
    assert_eq!(before[0].disabled, Some(true));

    let disabled = disable_dog(&h, 2, "bark").await;

    assert_eq!(
        disabled.result.unwrap(),
        Response::Deleted(vec![before[0].id])
    );
    let after = list_flock(&h.ctx, 3).await;
    assert_eq!(after, before);
}

/// fails if `DisableDog` reaches a sheep: the old handler went through
/// `delete`, so `shep disable web` from an older CLI deregistered the app.
#[tokio::test(start_paused = true)]
async fn disabling_a_name_a_sheep_holds_touches_nothing() {
    let h = harness(vec![ProcScript::never_exits()]);
    start_web(&h).await;

    let reply = disable_dog(&h, 2, "web").await;

    let Err(err) = reply.result else {
        panic!("a sheep is not a dog")
    };
    assert_eq!(err.code, RpcErrorCode::NotFound, "{err:?}");
    let flock = list_flock(&h.ctx, 3).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert_eq!(flock[0].status, ProcStatus::Online);
    assert_eq!(flock[0].disabled, None);
}

#[tokio::test(start_paused = true)]
async fn disabling_a_name_nothing_holds_is_not_found() {
    let h = harness(vec![]);

    let reply = disable_dog(&h, 1, "ghost").await;

    let Err(err) = reply.result else {
        panic!("nothing to disable")
    };
    assert_eq!(err.code, RpcErrorCode::NotFound, "{err:?}");
}

/// fails if `enable` lands while a disable's stop is still on the kill
/// ladder and spawns a second process beside the one being stopped, or lets
/// the stop finish and leave the dog disabled after the operator asked for it
/// back.
#[tokio::test(start_paused = true)]
async fn enabling_a_dog_mid_stop_cancels_the_disable_without_a_second_process() {
    let h = harness(vec![
        ProcScript::ignores_signals(),
        ProcScript::never_exits(),
    ]);
    let first = enable_dog(&h.ctx, 1, "bark").await;

    let (disabled, again) = tokio::join!(disable_dog(&h, 2, "bark"), async {
        tokio::time::sleep(Duration::from_millis(1)).await;
        enable_dog(&h.ctx, 3, "bark").await
    });

    assert!(disabled.result.is_ok(), "{:?}", disabled.result);
    assert_eq!(again.pid, first.pid, "the old process was still live");
    let flock = list_flock(&h.ctx, 4).await;
    assert_eq!(flock.len(), 1, "{flock:?}");
    assert_eq!(flock[0].id, first.id);
    assert_eq!(flock[0].status, ProcStatus::Online);
    assert_eq!(flock[0].disabled, None);
}
