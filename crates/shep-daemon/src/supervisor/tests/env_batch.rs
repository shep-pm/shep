//! Tests for writing a batch of environment variables.
//!
//! A batch is all or nothing under one lock: a collision without force writes
//! nothing at all. A dry run has to refuse exactly what the real send would
//! refuse, or it is not telling the truth.

use super::*;

/// A started `web` and a registered dog, for the batch tests below. Two
/// scripts because the dog is a spawn of its own.
async fn env_batch_harness() -> Harness {
    let h = harness(vec![ProcScript::never_exits(); 2]);
    start_app(&h, AppConfig::minimal("web", "./srv")).await;
    h.ctx
        .supervisor
        .start_dog(dog_app("bark"), DogSource::BuiltIn)
        .await
        .unwrap();
    h
}

/// The stored env of `name`, or a panic naming what was there instead.
fn stored_env(h: &Harness, name: &str) -> serde_json::Map<String, serde_json::Value> {
    let record = shep_core::overrides::get(&h.ctx.paths.overrides, name)
        .unwrap()
        .expect("an override record");
    record.fields["env"]
        .as_object()
        .expect("a flat env object")
        .clone()
}

#[tokio::test(start_paused = true)]
async fn a_batch_writes_every_key_under_one_lock() {
    let h = env_batch_harness().await;
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
            ]),
            false,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.set, ["A", "B"]);
    assert!(batch.collisions.is_empty());
    let env = stored_env(&h, "web");
    assert_eq!(env["A"], "1");
    assert_eq!(env["B"], "2");
}

#[tokio::test(start_paused = true)]
async fn an_identical_value_is_unchanged_rather_than_a_collision() {
    let h = env_batch_harness().await;
    let entries = BTreeMap::from([("A".to_string(), "1".to_string())]);
    h.ctx
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries.clone(), false, false)
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries, false, false)
        .await
        .unwrap()
        .expect("web exists");
    assert!(batch.set.is_empty());
    assert_eq!(batch.unchanged, ["A"]);
    assert!(batch.collisions.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_collision_without_force_writes_nothing_at_all() {
    let h = env_batch_harness().await;
    h.ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            false,
        )
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([
                ("A".to_string(), "2".to_string()),
                ("B".to_string(), "9".to_string()),
            ]),
            false,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.collisions, ["A"]);
    assert!(batch.set.is_empty());
    assert!(batch.app.is_none());
    let env = stored_env(&h, "web");
    assert_eq!(env["A"], "1", "the colliding key kept its value");
    assert!(
        !env.contains_key("B"),
        "the clean key was not written either"
    );
}

#[tokio::test(start_paused = true)]
async fn force_overwrites_and_reports_the_collision_in_both_lists() {
    let h = env_batch_harness().await;
    h.ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            false,
        )
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "2".to_string())]),
            true,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.set, ["A"]);
    assert_eq!(batch.collisions, ["A"]);
    assert_eq!(stored_env(&h, "web")["A"], "2");
}

#[tokio::test(start_paused = true)]
async fn a_dry_run_answers_and_writes_nothing() {
    let h = env_batch_harness().await;
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            true,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.set, ["A"]);
    assert!(batch.app.is_none());
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "a dry run left a store behind"
    );
}

/// A preview that does not match the outcome is worse than no preview:
/// `normalize` is this door's only validation, so a dry run that skipped
/// it would report `SHEP_NAME` as `set` and then fail on the real send,
/// after the caller had acted on the preview.
#[tokio::test(start_paused = true)]
async fn a_dry_run_refuses_what_the_real_send_would_refuse() {
    let h = env_batch_harness().await;
    let err = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("SHEP_NAME".to_string(), "nope".to_string())]),
            false,
            true,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisorError::InvalidEnv(_)), "{err:?}");
    assert!(
        shep_core::overrides::get(&h.ctx.paths.overrides, "web")
            .unwrap()
            .is_none(),
        "a refused dry run left a store behind"
    );
}

/// A refused batch changes nothing, so validating the merged config is
/// moot and the collision report is the whole answer.
#[tokio::test(start_paused = true)]
async fn a_refused_collision_reports_rather_than_validating() {
    let h = env_batch_harness().await;
    h.ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            false,
        )
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([
                ("A".to_string(), "2".to_string()),
                ("SHEP_NAME".to_string(), "nope".to_string()),
            ]),
            false,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.collisions, ["A"]);
    assert!(batch.set.is_empty());
    assert!(batch.app.is_none());
}

/// The contract says `app` is `Some` only when something was written.
/// A batch every key of which is already held writes nothing, so
/// `rpc.rs` must not record a no-op and rewrite the muster roll for it.
#[tokio::test(start_paused = true)]
async fn a_batch_that_changes_nothing_parks_nothing() {
    let h = env_batch_harness().await;
    let entries = BTreeMap::from([("A".to_string(), "1".to_string())]);
    h.ctx
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries.clone(), false, false)
        .await
        .unwrap();
    let batch = h
        .ctx
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries, false, false)
        .await
        .unwrap()
        .expect("web exists");
    assert!(batch.set.is_empty());
    assert_eq!(batch.unchanged, ["A"]);
    assert!(batch.app.is_none(), "nothing was written to record");
}

#[tokio::test(start_paused = true)]
async fn a_batch_refuses_a_dog_and_an_unknown_name() {
    let h = env_batch_harness().await;
    let entries = BTreeMap::from([("A".to_string(), "1".to_string())]);
    assert!(
        h.ctx
            .supervisor
            .set_sheep_env_batch("absent".to_string(), entries.clone(), false, false)
            .await
            .unwrap()
            .is_none()
    );
    let err = h
        .ctx
        .supervisor
        .set_sheep_env_batch("bark".to_string(), entries, false, false)
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisorError::IsADog(_)));
}
