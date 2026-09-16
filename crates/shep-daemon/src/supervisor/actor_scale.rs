//! Changing how many instances an app runs.
//!
//! Scaling up registers and starts new ids; scaling down picks the highest
//! ones and stops them. Either way the answer waits until every instance it
//! touched is terminal or online, so the reply reflects what actually
//! happened rather than what was asked for.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// Sets `name`'s instance count to `count`.
    ///
    /// Scaling up, [`instance_slots`] hands out the lowest free slots, exactly
    /// as a `Start` does. Scaling down, the highest-numbered slots are
    /// deregistered first, the same thing `Delete` does: a `Stop` would leave
    /// them holding their slots. The reply does not wait for the departures,
    /// which report themselves on the bus as `process.delete`.
    ///
    /// Refused while departures are in flight, since a departing instance stays
    /// registered and [`SheepSlot::pending_delete`] until its exit lands. That
    /// reaches the operator as [`SupervisorError::InvalidScale`].
    pub(super) fn handle_scale(
        &mut self,
        name: &str,
        count: u32,
        reply: oneshot::Sender<Result<Scaled, SupervisorError>>,
    ) {
        let mut slots: Vec<(u32, u32)> = self
            .sheep
            .iter()
            .filter(|(_, slot)| slot.entry.spec.config().name == name)
            .map(|(id, slot)| (slot.entry.instance, *id))
            .collect();
        if slots.is_empty() {
            let _ = reply.send(Err(SupervisorError::NotFound));
            return;
        }
        slots.sort_unstable();

        if count == 0 {
            let _ = reply.send(Err(SupervisorError::InvalidScale(format!(
                "an app runs at least one instance; use `shep delete {name}` to remove it"
            ))));
            return;
        }
        if self
            .sheep
            .get(&slots[0].1)
            .is_some_and(|slot| slot.entry.dog.is_some())
        {
            let _ = reply.send(Err(SupervisorError::InvalidScale(format!(
                "{name} is a dog, and a dog runs one process"
            ))));
            return;
        }
        if self.reloads.contains_key(name) {
            let _ = reply.send(Err(SupervisorError::ReloadInFlight(name.to_string())));
            return;
        }
        // Counted rather than merely detected: the number tells the operator
        // how much of the flock is still moving.
        let leaving = slots
            .iter()
            .filter(|(_, id)| self.sheep.get(id).is_some_and(|slot| slot.pending_delete))
            .count();
        if leaving > 0 {
            let _ = reply.send(Err(SupervisorError::InvalidScale(format!(
                "{name} has {leaving} instance(s) still shutting down from an \
                 earlier command; wait for them to leave `shep flock` and scale \
                 again"
            ))));
            return;
        }

        // Re-normalized rather than mutated in place: holding a `ResolvedApp`
        // proves it passed `normalize`, and editing the field behind that door
        // would be the one place in the tree holding one that had not.
        let mut config = self
            .sheep
            .get(&slots[0].1)
            .expect("handle_scale: id read off this map a moment ago")
            .entry
            .spec
            .config()
            .clone();
        config.instances = count;
        let rescaled = match normalize(config) {
            Ok(app) => app,
            Err(err) => {
                let _ = reply.send(Err(SupervisorError::InvalidScale(err.to_string())));
                return;
            }
        };

        let current = u32::try_from(slots.len()).unwrap_or(u32::MAX);
        // The spawn/remove pass runs first and the config write-back second:
        // writing `rescaled` onto every slot up front and then failing a spawn
        // leaves every survivor claiming `instances = 4` in a flock of three.
        let mut failure = None;
        // The one slot `spawn_fresh` registers on a failed attempt. Kept out of
        // `survivors`, since it is not a running instance, but the config
        // write-back below still has to reach it.
        let mut orphaned_by_failed_spawn = None;
        let survivors: Vec<u32> = match count.cmp(&current) {
            Ordering::Equal => slots.iter().map(|(_, id)| *id).collect(),
            Ordering::Greater => {
                // Inside this arm, since it is the only one that spawns:
                // resolving an identity for `Equal` or `Less` meant a lookup
                // that could refuse a call using no credentials at all.
                // `CannotStart`: nothing has been spawned or removed yet.
                let credentials = match self.credentials_for_spawn(slots[0].1) {
                    Ok(credentials) => credentials,
                    Err(err) => {
                        let _ =
                            reply.send(Err(SupervisorError::CannotStart(format!("{name}: {err}"))));
                        return;
                    }
                };
                let existing: Vec<u32> = slots.iter().map(|(instance, _)| *instance).collect();
                let mut ids: Vec<u32> = slots.iter().map(|(_, id)| *id).collect();
                for instance in instance_slots(&existing, count - current) {
                    let attempted_id = self.next_id;
                    match self.spawn_fresh(&rescaled, instance, credentials, None, &BTreeSet::new())
                    {
                        Ok(info) => ids.push(info.id),
                        Err(message) => {
                            // Partial, and said so: the instances already
                            // spawned are serving real traffic, and unwinding
                            // them would turn one failed spawn into an outage.
                            orphaned_by_failed_spawn = Some(attempted_id);
                            failure = Some(message);
                            break;
                        }
                    }
                }
                ids
            }
            Ordering::Less => {
                let cut = usize::try_from(count).unwrap_or(usize::MAX);
                let (keep, remove) = slots.split_at(cut);
                let removed: Vec<u32> = remove.iter().map(|(_, id)| *id).collect();
                self.begin_manual_ids(
                    removed,
                    ManualKind::Delete,
                    CommandOrigin::Operator,
                    // The removals' own terminal snapshots go nowhere: this
                    // reply is the survivors.
                    ReplyKind::Ids(oneshot::channel().0),
                );
                keep.iter().map(|(_, id)| *id).collect()
            }
        };

        // The count actually achieved: `count` on every path but a partial
        // scale-up. Re-normalized rather than assigned, for the same reason
        // `rescaled` was: a `ResolvedApp` is a proof token.
        let achieved = u32::try_from(survivors.len()).unwrap_or(u32::MAX);
        let stored = if achieved == count {
            rescaled
        } else {
            let mut config = rescaled.config().clone();
            config.instances = achieved;
            match normalize(config) {
                Ok(app) => app,
                Err(err) => {
                    let _ = reply.send(Err(SupervisorError::InvalidScale(err.to_string())));
                    return;
                }
            }
        };

        // The parked config travels onto the slots this call created, which
        // `spawn_fresh` registers with `pending: None`; the loop below also
        // writes `stored` onto the `Errored` slot a failed spawn left, which a
        // later `handle_scale` counts. `with_count` keeps the count agreeing.
        let owed = self.sheep.get(&slots[0].1).and_then(|slot| {
            let parked = slot.entry.pending.clone()?;
            let parked = with_count(&parked, achieved).unwrap_or(parked);
            Some((parked, slot.entry.pending_reidentifies))
        });
        for id in survivors.iter().chain(orphaned_by_failed_spawn.iter()) {
            if let Some(slot) = self.sheep.get_mut(id) {
                // `out_file`/`err_file` need no refresh here: `stored` only
                // moves `instances`, and no token an accepted log path may
                // carry reads that. `normalize` refuses a `{{secret:...}}`
                // in either field (`SecretInLogPath`), which leaves
                // `{{instance}}` and `{{name}}`; a survivor's own `instance`
                // is untouched by a scale and its name cannot move. Note it
                // is `normalize` that narrows this and not `render`, which
                // resolves secret references too.
                slot.entry.spec = stored.clone();
                match &mut slot.entry.pending {
                    // A slot already owed a config keeps it, with the count
                    // brought forward: a parked count of 2 against a spec of 4
                    // is drift that never clears.
                    Some(parked) => {
                        if let Some(recounted) = with_count(parked, achieved) {
                            *parked = recounted;
                        }
                    }
                    None => {
                        if let Some((parked, reidentifies)) = &owed {
                            slot.entry.pending = Some(parked.clone());
                            slot.entry.pending_reidentifies = *reidentifies;
                        }
                    }
                }
            }
        }

        let mut instances: Vec<ProcessInfo> = survivors
            .iter()
            .filter_map(|id| {
                self.sheep
                    .get(id)
                    .map(|slot| to_info(&slot.entry, &self.smits))
            })
            .collect();
        sort_flock(&mut instances);
        // `Ok` even when `failure` is set: the caller records `app`
        // unconditionally and turns `shortfall` into the operator's error,
        // where an `Err` would leave the muster roll on the pre-scale count.
        let _ = reply.send(Ok(Scaled {
            instances,
            app: stored,
            requested: count,
            shortfall: failure,
        }));
    }
}
