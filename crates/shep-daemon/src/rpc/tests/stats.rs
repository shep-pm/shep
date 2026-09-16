//! Live CPU and memory: reading them through `ListFlock` and
//! `Describe`, sampled from the periodic baseline rather than the
//! previous listing, a no-pid sheep answering with no stats, and a
//! lifecycle reply carrying none at all.

use super::*;

/// Without a live sample the fields come back `None` for a running sheep,
/// which a reader renders as `-` and an operator reads as "shep cannot
/// see it".
#[tokio::test]
async fn list_flock_carries_a_live_memory_reading_for_a_running_sheep() {
    // The harness's sampler is scripted, so the number below is the
    // fixture's and not the machine's; this asserts the plumbing, not
    // sysinfo. `ScriptedRunner` hands out `FIRST_SCRIPTED_PID`, and the
    // scripted reading describes a tree rooted at that same pid.
    let h = harness_with_stats(vec![ProcScript::never_exits()]);
    start_web(&h).await;

    let infos = list_flock(&h.ctx, 2).await;
    assert_eq!(infos[0].pid, Some(FIRST_SCRIPTED_PID));
    assert_eq!(infos[0].memory_bytes, Some(SCRIPTED_TREE_BYTES));
    assert_eq!(
        infos[0].cpu_percent, None,
        "no periodic baseline has been recorded, and a number invented \
         from the read's own window is worse than an empty cell"
    );
}

/// A baseline exists here, so a real number has to come back, and the
/// second listing says which window produced it: 1500 CPU-ms over the
/// 15 s since the baseline is 10%, while the same counter over the
/// millisecond since the previous listing is hundreds of percent.
#[tokio::test]
async fn list_flock_measures_cpu_from_the_periodic_baseline_not_from_the_previous_listing() {
    let h = harness_with_stats(vec![ProcScript::never_exits()]);
    start_web(&h).await;
    // A baseline dated one poll interval back, which is what the tick
    // would have left behind had one fired: the clock here is real, so a
    // test that waited for the enforcer's own tick would wait 15 s.
    let last_tick = Instant::now()
        .checked_sub(MEMORY_POLL_INTERVAL)
        .expect("the monotonic clock is older than one poll interval");
    h.stats.record_baseline_now(last_tick);

    let first = list_flock(&h.ctx, 2).await[0]
        .cpu_percent
        .expect("a baseline exists, so a running sheep has a CPU figure");
    let second = list_flock(&h.ctx, 3).await[0]
        .cpu_percent
        .expect("a baseline exists, so a running sheep has a CPU figure");

    assert!(
        (5.0..=10.05).contains(&first),
        "1500 CPU-ms over the 15 s since the baseline is 10%; got {first}"
    );
    assert!(
        (second - first).abs() < 1.0,
        "the second listing divided by the gap between the two LISTINGS \
         rather than by the window since the tick: {first} then {second}"
    );
}

/// `Describe` is the second of the two verbs an operator reads resource
/// usage from, and an implementation wired into `ListFlock` alone passes
/// every other case here.
#[tokio::test]
async fn describe_carries_a_live_reading_too() {
    let h = harness_with_stats(vec![ProcScript::never_exits()]);
    start_web(&h).await;

    let described = reply_of(
        dispatch(
            envelope(
                2,
                Request::Describe {
                    selector: SelectorSpec::Name("web".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::Described(infos)) = described.result else {
        panic!("expected Described, got {:?}", described.result)
    };
    assert_eq!(infos[0].memory_bytes, Some(SCRIPTED_TREE_BYTES));
}

/// The join is keyed on the pid a reading was taken against; one falling
/// back to the id, or to the first reading in the sample, would print one
/// sheep's resource use against another.
///
/// Two sheep, and both are needed: stopping a sheep unwatches it, so a
/// listing holding only the stopped one leaves the sample empty and every
/// join misses.
#[tokio::test]
async fn a_sheep_with_no_pid_reports_no_stats() {
    let h = harness_with_stats(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    start_web(&h).await;
    reply_of(
        dispatch(
            envelope(
                2,
                Request::Start {
                    apps: vec![AppConfig::minimal("worker", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    reply_of(
        dispatch(
            envelope(
                3,
                Request::Stop {
                    selector: SelectorSpec::Name("worker".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );

    let infos = list_flock(&h.ctx, 4).await;
    let named = |name: &str| {
        infos
            .iter()
            .find(|info| info.name == name)
            .unwrap_or_else(|| panic!("{name} is missing from the listing"))
    };
    // The scripted table describes the first spawn's pid and no other,
    // so this is the one row carrying a reading, and the one a fallback
    // join would hand to its neighbour.
    assert_eq!(named("web").pid, Some(FIRST_SCRIPTED_PID));
    assert_eq!(named("web").memory_bytes, Some(SCRIPTED_TREE_BYTES));

    assert_eq!(named("worker").pid, None);
    assert_eq!(named("worker").memory_bytes, None);
    assert_eq!(named("worker").cpu_percent, None);
}

/// A 5.77 ms syscall walk over the host's whole process table, on every
/// `start`, buys a reading nobody reads there.
///
/// Asserted on `Started` rather than on `Stopped`: a stopped sheep has no
/// pid, so its row comes back empty whether or not the verb sampled and
/// the assertion would hold for either implementation.
#[tokio::test]
async fn a_lifecycle_reply_carries_no_stats() {
    let h = harness_with_stats(vec![ProcScript::never_exits()]);
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal("web", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::Started(infos)) = started.result else {
        panic!("expected Started, got {:?}", started.result)
    };
    assert_eq!(
        infos[0].pid,
        Some(FIRST_SCRIPTED_PID),
        "a row with no pid would report no stats however the verb behaved"
    );
    assert_eq!(
        infos[0].memory_bytes, None,
        "only `flock` and `describe` take a live sample"
    );
    assert_eq!(infos[0].cpu_percent, None);
}
