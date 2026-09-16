//! Detecting a dog that is running and has never once spoken to this
//! shepherd, and laddering it through the same restart-then-stale verdicts a
//! named protocol refusal earns, reached by inference off [`PeerContacts`]
//! rather than by the dog saying who it is.

use core::time::Duration;
use std::collections::BTreeMap;

use tokio::time::Instant;

use shep_core::protocol::ProcessInfo;
use shep_core::status::ProcStatus;

use crate::bus::Bus;
use crate::supervisor::SupervisorHandle;

use super::PeerContacts;
use super::narrate::narrate;
use super::refusals::{DogRefusals, Refusal, restart_refused_dog};
use super::verdict::{Silence, first_rung_evidence, stale_verdict};

/// How long a registered, running dog may stay silent before this shepherd
/// concludes it is never going to talk to it.
///
/// A handshake is one connect and one round trip on a local socket. Five
/// seconds is sized against the slowest legitimate silence: a dog carried
/// across a handover has to notice its connection died and dial back, and a
/// third-party dog is free to sleep a second first.
///
/// Not `shep daemon reload`'s three-second settle wait, which lives in
/// `shep-cli` and answers how long a command holds its output open.
///
/// Not the budget a boot-promoted dog dies to either, though that one is
/// also five seconds. A dog spawned by `[daemon] boot_first_dogs` meets a
/// socket that is bound and not yet served, and `shep-client`'s own
/// `HANDSHAKE_TIMEOUT` ends it while this watch is still unarmed. See
/// `docs/specs/deferred.md`, "A promoted dog cannot handshake during the
/// restore".
pub const DOG_SILENCE_BUDGET: Duration = Duration::from_secs(5);

/// Gap between two of [`spawn_silent_dog_watch`]'s looks.
///
/// Finer than [`DOG_SILENCE_BUDGET`] so a dog's restart is asked for near the
/// moment its budget runs out. One look is one message to the supervisor actor
/// and no syscall per dog.
const DOG_SILENCE_POLL: Duration = Duration::from_secs(1);

/// Every dog the supervisor is running that has never once handshaken with
/// this daemon, sorted.
///
/// [`spawn_silent_dog_watch`] and `rpc::dog_staleness` both read it and must
/// not disagree about the population: a dog in one set but not the other would
/// be reported forever or condemned unreported. Only a dog with a process
/// counts, and a stale one is already answered for.
pub(crate) fn silent_dogs(infos: &[ProcessInfo], refusals: &DogRefusals) -> Vec<String> {
    let stale = refusals.stale();
    let mut names: Vec<String> = infos
        .iter()
        .filter(|info| {
            info.dog.is_some()
                && matches!(info.status, ProcStatus::Starting | ProcStatus::Online)
                && !refusals.has_handshook(&info.name)
                && !stale.contains(&info.name)
        })
        .map(|info| info.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// When each currently-silent dog was first seen silent.
///
/// Why that watch is a task on a clock rather than a branch inside
/// `rpc::dog_staleness`: staleness is a query, and `shep daemon reload` polls
/// it in a loop, so a ladder driven from there would walk a merely slow dog
/// from restart to stale in the time it takes to ask three times.
#[derive(Debug, Default)]
pub(crate) struct SilentDogs {
    /// One instant per dog currently silent. A name absent from the map is a
    /// dog that was talking, stopped, or deleted at the last look.
    first_seen: BTreeMap<String, Instant>,
}

impl SilentDogs {
    /// The dogs that have now been silent for a whole [`DOG_SILENCE_BUDGET`],
    /// given the set observed silent at `now`.
    ///
    /// `now` is a parameter so every dog in one look is judged against the
    /// same instant, and so a test can move the clock.
    fn due(&mut self, silent: &[String], now: Instant) -> Vec<String> {
        // A dog that answered, stopped, or was deleted is not silent any more,
        // and starts a fresh budget if it falls quiet again.
        self.first_seen.retain(|name, _| silent.contains(name));
        let mut due = Vec::new();
        for name in silent {
            let since = self.first_seen.entry(name.clone()).or_insert(now);
            if now.saturating_duration_since(*since) >= DOG_SILENCE_BUDGET {
                // Rearmed rather than forgotten: the next rung costs another
                // whole budget.
                *since = now;
                due.push(name.clone());
            }
        }
        due
    }
}

/// One look: which of this daemon's dogs have now been quiet too long, and
/// what each of them earned.
///
/// Returns what it acted on; the loop that calls it discards the answer.
pub(crate) async fn check_silent_dogs(
    supervisor: &SupervisorHandle,
    refusals: &DogRefusals,
    contacts: &PeerContacts,
    events: &Bus,
    seen: &mut SilentDogs,
    now: Instant,
) -> Vec<(String, Refusal)> {
    // Nothing is judged while attribution is still maturing: the stale rung is
    // spent once, so a wrong answer here is the last answer.
    if contacts.is_warming() {
        return Vec::new();
    }
    // `seen` is left untouched rather than cleared: a look that could not
    // judge has learned nothing, and must not hand every dog a fresh budget.
    let Ok(infos) = supervisor.list_checked().await else {
        return Vec::new();
    };
    let silent = silent_dogs(&infos, refusals);
    let mut acted = Vec::new();
    for name in seen.due(&silent, now) {
        // Off the same listing the silence was judged from, so the pid a
        // message names is the process that was silent.
        let info = infos.iter().find(|info| info.name == name);
        let evidence = Silence::of(info.and_then(|info| info.pid), contacts);
        let verdict = record_silent_dog(&name, info, evidence, refusals, events, supervisor).await;
        acted.push((name, verdict));
    }
    acted
}

/// Enters a dog that has gone quiet into the same ladder a named refusal
/// enters, reached by inference rather than by the dog saying who it is.
///
/// `record_refused_dog` is keyed on `Hello::dog_name`, which a client speaking
/// an older protocol cannot send, so its ladder reaches only dogs new enough to
/// name themselves. The set difference this rides on needs no cooperation from
/// the client; peer credentials are read only to fill in `evidence`.
///
/// A dog that is merely slow to connect is restarted once for nothing, and
/// heals itself: [`DogRefusals::handshook`] clears everything held against a
/// dog the moment it handshakes.
async fn record_silent_dog(
    name: &str,
    info: Option<&ProcessInfo>,
    evidence: Silence,
    refusals: &DogRefusals,
    events: &Bus,
    supervisor: &SupervisorHandle,
) -> Refusal {
    let verdict = refusals.refused(name);
    match verdict {
        Refusal::Restart => {
            let seen = first_rung_evidence(evidence);
            tracing::warn!(
                dog = %name,
                silent_for_secs = DOG_SILENCE_BUDGET.as_secs(),
                evidence = %seen,
                "a dog has been running without ever answering this shepherd; restarting it once from the binary on disk"
            );
            if let Some(info) = info {
                narrate(
                    events,
                    info,
                    &format!(
                        "this dog has been running for {}s without ever answering this shepherd: {seen}. Restarting it once from the binary on disk",
                        DOG_SILENCE_BUDGET.as_secs()
                    ),
                )
                .await;
            }
            // Awaited rather than spawned: this keeps the next look from
            // running while a kill ladder is in flight, so a dog is never
            // judged mid-restart.
            restart_refused_dog(supervisor, name).await;
        }
        Refusal::Stale => {
            let verdict = stale_verdict(name, evidence);
            tracing::error!(dog = %name, "{verdict}");
            // Into the dog's own log as well, because that is the file the
            // verdict tells the operator to read.
            if let Some(info) = info {
                narrate(events, info, &verdict).await;
            }
        }
        // Unreachable here: `silent_dogs` filters a stale dog out before it
        // can be seen quiet again. A real arm, so a caller that stops filtering
        // does not find a `todo!`.
        Refusal::AlreadyStale => tracing::debug!(
            dog = %name,
            "a silent dog that was already reported stale"
        ),
    }
    verdict
}

/// Watches for dogs that are running and have never once spoken to this
/// shepherd, and enters each into the ladder after [`DOG_SILENCE_BUDGET`] of
/// silence: restarted once from the binary on disk, then reported stale,
/// then left alone.
///
/// Anchored to the daemon's boot rather than to a dog's spawn: a handover is
/// an `execve`, so a per-dog timer would die at the exec, and `boot` runs again
/// in the successor.
///
/// Its `JoinHandle` is held by the caller and aborted at teardown: the loop has
/// no end of its own, and nothing may restart a dog during shutdown.
pub fn spawn_silent_dog_watch(
    supervisor: SupervisorHandle,
    refusals: DogRefusals,
    contacts: PeerContacts,
    events: Bus,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(DOG_SILENCE_POLL);
        // A look missed under load is not a look owed: the budget runs off the
        // clock, not off a tick count.
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut seen = SilentDogs::default();
        loop {
            let now = ticks.tick().await;
            check_silent_dogs(&supervisor, &refusals, &contacts, &events, &mut seen, now).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::ProcScript;

    use super::super::contacts::PEER_CONTACT_WARMUP;
    use super::super::test_support::start_test_dog;

    /// How often [`settle_until`] looks while the watch works.
    ///
    /// Finer than [`DOG_SILENCE_POLL`] so a rung is seen inside the poll
    /// period it lands in.
    const SETTLE_STEP: Duration = Duration::from_millis(250);

    /// How long [`settle_until`] gives a rung before it gives up.
    ///
    /// A hang guard, not a timing assertion: a whole warm-up plus both rungs
    /// is fifteen seconds of virtual time. Generosity is free, because the
    /// clock it bounds is virtual.
    const LADDER_BUDGET: Duration = Duration::from_secs(45);

    /// Waits for `settled` to answer true, and answers how much virtual time
    /// that took.
    ///
    /// Sleeping here is a barrier rather than a slower spin: under
    /// `start_paused` the runtime advances the clock only once every task is
    /// idle, and work on the blocking pool holds it there. Each of
    /// `spawn_silent_dog_watch`'s looks writes into the dog's own log, which
    /// `narrate` puts on that pool. A `yield_now` loop keeps a task runnable,
    /// so the runtime never idles and the clock never advances.
    ///
    /// Panics, naming `what`, if `settled` has not answered true within
    /// `within` of virtual time.
    async fn settle_until(
        what: &str,
        within: Duration,
        mut settled: impl FnMut() -> bool,
    ) -> Duration {
        let began = Instant::now();
        tokio::time::timeout(within, async {
            while !settled() {
                tokio::time::sleep(SETTLE_STEP).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what} did not happen within {within:?} of virtual time"));
        began.elapsed()
    }

    /// The production case for the inference: a dog on an older protocol
    /// cannot send `Hello::dog_name`, so the refusal it earns is anonymous and
    /// `record_refused_dog` never runs for it.
    #[tokio::test(start_paused = true)]
    async fn a_dog_that_never_answers_is_restarted_once_and_then_marked_stale() {
        let h = crate::testing::harness(vec![
            ProcScript::never_exits(),
            ProcScript::never_exits(),
            ProcScript::never_exits(),
            ProcScript::never_exits(),
        ]);
        start_test_dog(&h.ctx, "metrics").await;
        // Past the warm-up: the ladder judges nothing while attribution is
        // still maturing, and this case is about the rungs, not the gate.
        tokio::time::advance(PEER_CONTACT_WARMUP * 2).await;
        let refusals = &h.ctx.dog_refusals;
        let contacts = &h.ctx.peer_contacts;
        let events = &h.ctx.events;
        let mut seen = SilentDogs::default();
        let t0 = Instant::now();

        assert!(
            check_silent_dogs(&h.ctx.supervisor, refusals, contacts, events, &mut seen, t0)
                .await
                .is_empty(),
            "a dog seen quiet for the first time has not yet been quiet for any length of time"
        );

        assert_eq!(
            check_silent_dogs(
                &h.ctx.supervisor,
                refusals,
                contacts,
                events,
                &mut seen,
                t0 + DOG_SILENCE_BUDGET
            )
            .await,
            vec![("metrics".to_string(), Refusal::Restart)],
            "a whole budget of silence buys the one restart from disk"
        );
        assert_eq!(refusals.restarting(), vec!["metrics".to_string()]);
        assert!(
            refusals.stale().is_empty(),
            "one silence is a dog to restart, not a dog to give up on"
        );

        assert_eq!(
            check_silent_dogs(
                &h.ctx.supervisor,
                refusals,
                contacts,
                events,
                &mut seen,
                t0 + 2 * DOG_SILENCE_BUDGET
            )
            .await,
            vec![("metrics".to_string(), Refusal::Stale)],
            "the restart ran and the dog still has not spoken, so the ladder ends here"
        );
        assert_eq!(refusals.stale(), vec!["metrics".to_string()]);

        assert!(
            check_silent_dogs(
                &h.ctx.supervisor,
                refusals,
                contacts,
                events,
                &mut seen,
                t0 + 3 * DOG_SILENCE_BUDGET
            )
            .await
            .is_empty(),
            "a dog already given up on is not laddered again, however long it stays quiet"
        );
    }

    /// Written against a clock ten budgets past the point where a silent dog
    /// would have been condemned twice over: this case passes for the wrong
    /// reason if the inference never fires at all.
    #[tokio::test(start_paused = true)]
    async fn a_dog_that_answers_inside_the_budget_is_never_touched() {
        let h = crate::testing::harness(vec![ProcScript::never_exits()]);
        start_test_dog(&h.ctx, "metrics").await;
        let refusals = &h.ctx.dog_refusals;
        let contacts = &h.ctx.peer_contacts;
        let events = &h.ctx.events;
        refusals.handshook("metrics");
        let mut seen = SilentDogs::default();
        let t0 = Instant::now();

        for elapsed in [0, 1, 2, 10] {
            assert!(
                check_silent_dogs(
                    &h.ctx.supervisor,
                    refusals,
                    contacts,
                    events,
                    &mut seen,
                    t0 + elapsed * DOG_SILENCE_BUDGET
                )
                .await
                .is_empty(),
                "a dog this shepherd has heard from is not silent at any point on the clock"
            );
        }
        assert!(refusals.restarting().is_empty());
        assert!(refusals.stale().is_empty());
    }

    /// Re-laddering a stale dog would spend a restart the record already says
    /// was spent, and write the same report once per budget for as long as the
    /// daemon runs.
    #[tokio::test(start_paused = true)]
    async fn a_dog_already_stale_is_not_laddered_again() {
        let h = crate::testing::harness(vec![ProcScript::never_exits()]);
        start_test_dog(&h.ctx, "metrics").await;
        let refusals = &h.ctx.dog_refusals;
        let contacts = &h.ctx.peer_contacts;
        let events = &h.ctx.events;
        refusals.refused("metrics");
        refusals.refused("metrics");
        assert_eq!(refusals.stale(), vec!["metrics".to_string()]);

        let mut seen = SilentDogs::default();
        let t0 = Instant::now();
        for elapsed in [0, 1, 2, 5] {
            assert!(
                check_silent_dogs(
                    &h.ctx.supervisor,
                    refusals,
                    contacts,
                    events,
                    &mut seen,
                    t0 + elapsed * DOG_SILENCE_BUDGET
                )
                .await
                .is_empty(),
                "the ladder ends at stale; there is no rung after it to reach"
            );
        }
    }

    /// `Request::DogStaleness` derives the same set and `shep daemon reload`
    /// polls it every 50ms, so a ladder driven from there would restart a
    /// merely slow dog and report it stale inside a second.
    #[tokio::test(start_paused = true)]
    async fn asking_repeatedly_does_not_advance_the_ladder() {
        let h = crate::testing::harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        start_test_dog(&h.ctx, "metrics").await;
        // Past the warm-up: the ladder judges nothing while attribution is
        // still maturing, and this case is about the rungs, not the gate.
        tokio::time::advance(PEER_CONTACT_WARMUP * 2).await;
        let refusals = &h.ctx.dog_refusals;
        let contacts = &h.ctx.peer_contacts;
        let events = &h.ctx.events;
        let mut seen = SilentDogs::default();
        let t0 = Instant::now();

        for look in 0..20 {
            assert!(
                check_silent_dogs(
                    &h.ctx.supervisor,
                    refusals,
                    contacts,
                    events,
                    &mut seen,
                    t0 + (DOG_SILENCE_BUDGET / 20) * look
                )
                .await
                .is_empty(),
                "look {look} fell inside the budget and must not have moved the dog along"
            );
        }
        assert!(refusals.restarting().is_empty());

        assert_eq!(
            check_silent_dogs(
                &h.ctx.supervisor,
                refusals,
                contacts,
                events,
                &mut seen,
                t0 + DOG_SILENCE_BUDGET
            )
            .await,
            vec![("metrics".to_string(), Refusal::Restart)],
            "the clock is what moves the dog along, and it has now moved"
        );
    }

    /// Fails if the warm-up swallows the one verdict it exists to protect, or
    /// if `spawn_silent_dog_watch`'s own loop stops calling `check_silent_dogs`
    /// at all: every test above calls it directly, and would keep passing with
    /// the watcher's tick path deleted.
    ///
    /// A warm-up wider than the ladder spends the stale rung against a map that
    /// is still cold, and `silent_dogs` then drops the dog, so no later look
    /// reclassifies it. [`settle_until`] drives virtual time here, so this
    /// stays in the fast tier rather than `mod slow`.
    #[tokio::test(start_paused = true)]
    async fn a_dog_that_never_calls_still_earns_its_rebuild_after_the_warm_up() {
        // A lower bound on when `PeerContacts` started warming, taken before
        // the harness that builds it: the map's clock starts inside `harness`
        // and nothing out here can ask it when.
        let map_started_no_earlier_than = Instant::now();
        let h = crate::testing::harness(vec![
            ProcScript::never_exits(),
            ProcScript::never_exits(),
            ProcScript::never_exits(),
        ]);
        start_test_dog(&h.ctx, "metrics").await;
        let refusals = h.ctx.dog_refusals.clone();
        let contacts = h.ctx.peer_contacts.clone();
        assert!(contacts.is_warming(), "a fresh map starts cold");

        let watch = spawn_silent_dog_watch(
            h.ctx.supervisor.clone(),
            refusals.clone(),
            contacts.clone(),
            h.ctx.events.clone(),
        );

        // Waiting for the first rung, rather than walking a fixed number of
        // ticks and asserting nothing happened, is what turns the warm-up gate
        // from assumed into proved: an "assert nothing yet" passes just as
        // happily when the watch's loop never ran at all.
        let restart_rung = settle_until("the silent dog's restart rung", LADDER_BUDGET, || {
            !refusals.restarting().is_empty()
        })
        .await;
        assert_eq!(
            refusals.restarting(),
            vec!["metrics".to_string()],
            "the dog nothing ever connected from is the one that earns the rung"
        );

        // When the rung landed is the whole point: a ladder on a cold map
        // reaches it one budget after the watch spawned, one that waits a
        // budget after the warm-up ends. The `is_warming` assertion above pins
        // the map as cold at spawn, so the two cannot coincide.
        let first_rung_at = map_started_no_earlier_than.elapsed();
        assert!(
            first_rung_at >= PEER_CONTACT_WARMUP + DOG_SILENCE_BUDGET,
            "a cold map must judge nothing: the first rung landed {first_rung_at:?} in, \
             which is inside the {PEER_CONTACT_WARMUP:?} warm-up plus one \
             {DOG_SILENCE_BUDGET:?} budget of silence it has to wait out"
        );
        assert!(
            restart_rung >= DOG_SILENCE_BUDGET,
            "no rung can be earned in less than a whole budget of silence: {restart_rung:?}"
        );

        // The second rung, read off a map that has now been listening for
        // longer than any dog has been quiet.
        settle_until("the silent dog's stale rung", LADDER_BUDGET, || {
            refusals.stale().contains(&"metrics".to_string())
        })
        .await;
        let info = h
            .ctx
            .supervisor
            .list()
            .await
            .into_iter()
            .find(|info| info.name == "metrics")
            .expect("the dog fixture is listed");
        let verdict = stale_verdict("metrics", Silence::of(info.pid, &contacts));
        assert!(
            verdict.contains("cannot reach this shep"),
            "the earned rebuild advice must survive the warm-up: {verdict}"
        );
        watch.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn the_watcher_restarts_a_silent_dog_after_one_budget_of_paused_time() {
        let h = crate::testing::harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        start_test_dog(&h.ctx, "metrics").await;
        // Past the warm-up: the ladder judges nothing while attribution is
        // still maturing, and this case is about the rungs, not the gate.
        tokio::time::advance(PEER_CONTACT_WARMUP * 2).await;
        let refusals = h.ctx.dog_refusals.clone();

        let watch = spawn_silent_dog_watch(
            h.ctx.supervisor.clone(),
            refusals.clone(),
            h.ctx.peer_contacts.clone(),
            h.ctx.events.clone(),
        );

        // The watcher's own interval fires an immediate first tick, which
        // records the dog as seen-silent-since-now. The wait below idles the
        // runtime, and an idle runtime is when the paused clock moves.
        let waited = settle_until("the silent dog's restart", LADDER_BUDGET, || {
            !refusals.restarting().is_empty()
        })
        .await;

        assert_eq!(
            refusals.restarting(),
            vec!["metrics".to_string()],
            "one budget of silence, driven through the watcher's own tick, must earn exactly one restart"
        );
        // The budget is asserted rather than assumed: a watch that judged a
        // dog early would earn the same restart and pass on the line above.
        assert!(
            waited >= DOG_SILENCE_BUDGET,
            "a restart is earned by a whole budget of silence, not by less: {waited:?}"
        );
        assert!(
            refusals.stale().is_empty(),
            "one silence is a dog to restart, not a dog to give up on"
        );

        watch.abort();
    }
}
