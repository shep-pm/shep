//! `shep enable` on a dog the shepherd already lists at rest.

use super::*;
use std::collections::BTreeMap;

fn built_in(name: &str) -> crate::dogs::DogSpec {
    crate::dogs::DogSpec {
        name: name.to_string(),
        source: DogSource::BuiltIn { grants: None },
        env: BTreeMap::new(),
    }
}

/// A handler that took the at-rest entry for a running dog would answer
/// `DogStarted` while the listing still read `stopped`.
#[tokio::test(start_paused = true)]
async fn enabling_a_dog_at_rest_starts_it_in_the_row_it_is_listed_in() {
    let h = harness(vec![ProcScript::never_exits()]);
    crate::dogs::register_dogs_at_rest(
        &[built_in("metrics"), built_in("bark")],
        &[],
        &h.ctx.paths,
        &h.ctx.supervisor,
    )
    .await;
    let at_rest = h.ctx.supervisor.list().await;
    assert!(
        at_rest.iter().all(|row| row.status == ProcStatus::Stopped),
        "{at_rest:?}"
    );

    let started = enable_dog(&h.ctx, 1, "bark").await;

    assert_eq!(started.status, ProcStatus::Online);
    assert_eq!(started.id, 1, "bark was listed second");
    let mut after = h.ctx.supervisor.list().await;
    after.sort_by_key(|row| row.id);
    assert_eq!(after.len(), 2, "no second row for the dog: {after:?}");
    assert_eq!(after[0].name, "metrics");
    assert_eq!(after[0].status, ProcStatus::Stopped);
    assert_eq!(after[0].disabled, Some(true));
    assert_eq!(after[1].name, "bark");
    assert_eq!(after[1].status, ProcStatus::Online);
    assert_eq!(after[1].disabled, None);
}
