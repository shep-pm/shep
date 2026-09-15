//! Applying a Flockfile to a running flock.
//!
//! `apply_one` is the centre: for one declared app it works out what the
//! running instances already match, what can change underneath them, and
//! what has to be parked until a restart. `rearm_specs` then hands the
//! settled spec back to the subsystems that were armed against the old one.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// Merges each declared app into the sheep of the same name, one app at a
    /// time, refusing whole any app whose merge does not normalize.
    ///
    /// A load never registers an app the flock does not have, and never prunes
    /// one the file omits: the loop walks the declared apps, never the flock.
    /// Under [`ResetDepth::None`] it never kills either: a field the running
    /// child holds parks in [`ProcessEntry::pending`] for its next spawn, while
    /// `spec` goes on describing what the child was spawned from. A reset can,
    /// since `instances` routes through [`Self::handle_scale`], whose
    /// `Ordering::Less` arm deletes the instances above the new count.
    /// [`ResetDepth::Env`] and `shutting_down` never reach that function.
    pub(super) fn handle_apply_config(&mut self, apps: Vec<DeclaredApp>, reset: ResetDepth) -> Vec<Applied> {
        // One locked read for the whole file and one locked write at the end:
        // the store is rewritten whole on every write, so a per-app pair costs
        // an eleven-app Flockfile 22 lock acquisitions on the thread that
        // supervises the flock.
        let store = match overrides::all(&self.paths.overrides) {
            Ok(store) => store,
            Err(err) => {
                // The one refusal that covers the whole file: the merge cannot
                // tell what an operator has set without this store, and those
                // edits would be silently overwritten.
                let message = format!("overrides could not be read: {err}");
                return apps
                    .iter()
                    .map(|incoming| Applied {
                        name: incoming.config.name.clone(),
                        applied: Vec::new(),
                        pending: Vec::new(),
                        refused: Some(message.clone()),
                        app: None,
                    })
                    .collect();
            }
        };

        let mut changes = BTreeMap::new();
        let mut report = Vec::with_capacity(apps.len());
        for incoming in &apps {
            let overrides = store
                .get(&incoming.config.name)
                .cloned()
                .unwrap_or_default();
            report.push(self.apply_one(incoming, reset, &overrides, &mut changes));
        }

        if let Err(err) = overrides::update(&self.paths.overrides, &changes) {
            // Reported next to what landed rather than as a refusal: the
            // flock has already been changed by the time this runs, and an
            // `Applied` claiming nothing happened would be untrue.
            let note = format!("overrides could not be written: {err}");
            for applied in &mut report {
                if !changes.contains_key(&applied.name) {
                    continue;
                }
                applied.refused = Some(match applied.refused.take() {
                    Some(existing) => format!("{existing}; {note}"),
                    None => note.clone(),
                });
            }
        }
        report
    }

    /// The slot that stands in for `name`'s whole app, for a command that
    /// holds a name rather than an id.
    ///
    /// Not `ids_of_name(name).first()`: during a reload the drainee holds
    /// the lower id, so the first id is the instance on its way out, and a
    /// config read off it describes what the app is leaving behind rather
    /// than what it is becoming. Falls back to the first id when every slot
    /// is draining, so a command arriving inside a serial reload's drain
    /// window still finds its app rather than being told it is not
    /// registered.
    pub(super) fn representative_id(&self, name: &str) -> Option<u32> {
        let ids = self.ids_of_name(name);
        ids.iter()
            .copied()
            .find(|id| {
                self.sheep
                    .get(id)
                    .is_some_and(|slot| !matches!(slot.entry.reload, ReloadState::Drainee { .. }))
            })
            .or_else(|| ids.first().copied())
    }

    /// One app's half of [`Self::handle_apply_config`].
    ///
    /// `overrides` is this app's record as the one store read found it, and
    /// `changes` is where its replacement goes: the write is the caller's, so
    /// a file of eleven apps costs one lock rather than eleven.
    ///
    /// Every refusal that answers "this app was not touched" is raised before
    /// the instance count is routed: a refusal after the scale would report an
    /// untouched app while the flock had already been reshaped.
    pub(super) fn apply_one(
        &mut self,
        incoming: &DeclaredApp,
        reset: ResetDepth,
        overrides: &AppOverrides,
        changes: &mut BTreeMap<String, Option<AppOverrides>>,
    ) -> Applied {
        let name = incoming.config.name.clone();
        let refuse = |message: String| Applied {
            name: name.clone(),
            applied: Vec::new(),
            pending: Vec::new(),
            refused: Some(message),
            app: None,
        };

        let ids = self.ids_of_name(&name);
        // The app's stand-in, and not `ids.first()`: during a reload the
        // drainee holds the lower id, and the `next_spec` derived from this
        // slot is written onto every slot of the name.
        let Some(slot) = self
            .representative_id(&name)
            .and_then(|id| self.sheep.get(&id))
        else {
            return refuse(format!(
                "{name} is not registered; `shep start` it before a config can be applied to it"
            ));
        };
        // Mirroring `handle_scale`'s guard, for a sharper reason: a dog is
        // never in the override store, so every key a file declares for one is
        // unestablished forever and the additive rule never engages.
        if slot.entry.dog.is_some() {
            return refuse(dog_config_refusal(&name));
        }
        // `running` is what the child was spawned from, `intended` what the app
        // is meant to be. The merge builds on `intended`, or a second load of
        // the same file would find its own key established, skip it, and merge
        // the running value over the parked one.
        let running = slot.entry.spec.config().clone();
        let intended = slot
            .entry
            .pending
            .as_ref()
            .map_or_else(|| running.clone(), |parked| parked.config().clone());

        let (merged, mut next_overrides) =
            match merge_declared(&intended, incoming, overrides, reset) {
                Ok(merged) => merged,
                Err(message) => return refuse(message),
            };
        let merged = match normalize(merged) {
            Ok(merged) => merged,
            Err(err) => return refuse(err.to_string()),
        };

        let mut live = Vec::new();
        let mut next_spawn = Vec::new();
        let mut respawn = Vec::new();
        let mut instances = false;
        for field in intended.drifted_fields(merged.config()) {
            match apply_group(&field) {
                ApplyGroup::Live => live.push(field),
                ApplyGroup::NextSpawn => next_spawn.push(field),
                ApplyGroup::NeedsRespawn => respawn.push(field),
                // `name` cannot drift, the app having been found by it, so
                // `instances` is the only structural field that reaches here.
                ApplyGroup::Structural => instances |= field == "instances",
                // A group a later shep-core adds. Treated as the table's own
                // fallback treats an unknown field: the conservative answer
                // is that the running process does not have the new value.
                _ => respawn.push(field),
            }
        }

        let mut refusals = Vec::new();
        let mut count = running.instances;
        // A plain load never reshapes a flock, so `merge_declared` keeps
        // `instances` out of the merge under `None` and `Env`, leaving only a
        // note. `File` takes a count the template declares and leaves it alone
        // when the template is silent.
        if !matches!(
            reset,
            ResetDepth::Policy | ResetDepth::All | ResetDepth::File
        ) && incoming.declared.contains("instances")
            && incoming.config.instances != running.instances
        {
            // Names the scope, not just the remedy: the guard fires under
            // `--reset=env` too.
            refusals.push(format!(
                "instances: this load never reshapes a flock; no mode scales without also \
                 putting back every setting the file declares, and `--reset=file` is the \
                 narrowest that does, taking the file's count of {}",
                incoming.config.instances
            ));
        }
        if instances {
            if self.shutting_down {
                refusals.push(
                    "instances: the daemon is shutting down and cannot reshape a flock".to_string(),
                );
            } else {
                let (reply, mut answer) = oneshot::channel();
                self.handle_scale(&name, merged.config().instances, reply);
                match answer
                    .try_recv()
                    .expect("handle_scale answers before it returns")
                {
                    Ok(scaled) => {
                        count = scaled.achieved();
                        if let Some(shortfall) = scaled.shortfall {
                            refusals.push(format!("instances: {shortfall}"));
                        }
                    }
                    // Refused whole: every `Err` `handle_scale` answers with is
                    // raised before it spawns or removes anything, so the flock
                    // really is as it was, and a scale is refused exactly when
                    // something else is already reshaping the app.
                    Err(err) => return refuse(err.to_string()),
                }
            }
        }

        // Built from the running config, plus only the fields that can reach a
        // running process: a `NeedsRespawn` field written here would erase the
        // record of what the child was actually spawned from.
        let reaching: Vec<String> = live.iter().chain(next_spawn.iter()).cloned().collect();
        // A subset of a config that normalized can still fail, since normalize
        // checks fields against each other: `watch` needs a `cwd`, and a file
        // declaring both leaves the `cwd` behind. That parks rather than
        // refuses.
        let next_spec = reached_spec(&running, merged.config(), &reaching, count).ok();
        let park_all = next_spec.is_none();

        // The whole merge, for a next spawn to pick up. Recomputed whenever an
        // earlier load left one parked: that config predates this load's Live
        // changes, and promoting it later would put them back.
        let parked_wanted = park_all
            || !respawn.is_empty()
            || ids.iter().any(|id| {
                self.sheep
                    .get(id)
                    .is_some_and(|slot| slot.entry.pending.is_some())
            });
        let parked = if parked_wanted {
            with_count(&merged, count)
        } else {
            None
        };
        // Nothing could be parked, so nothing may be reported as parked, and
        // the previous parked config is left rather than cleared: it is an
        // earlier load's change and still the one a respawn should pick up.
        let parked_failed = parked_wanted && parked.is_none();

        // Re-read by `get_mut` rather than by index: a scale registered ids
        // this app did not have a moment ago and deregistered some it did, so
        // the pre-scale list can name a slot that is already gone.
        for id in self.ids_of_name(&name) {
            let Some(slot) = self.sheep.get_mut(&id) else {
                continue;
            };
            // Before the spec is overwritten below, and against this slot's own
            // spec rather than the one `next_spec` came from: writing that
            // first would make every sibling look like instance 0. `|=` never
            // `=`, so a change nobody has promoted is not forgotten.
            if let Some(parked) = &parked {
                let running = slot.entry.spec.config();
                slot.entry.pending_reidentifies |=
                    running.user != parked.config().user || running.group != parked.config().group;
            }
            if let Some(next_spec) = next_spec.clone() {
                slot.entry.spec = next_spec;
            }
            if let Some(parked) = parked.clone() {
                slot.entry.pending = Some(parked);
            }
        }

        if !park_all
            && live
                .iter()
                .any(|field| EXTRAS_FIELDS.contains(&field.as_str()))
        {
            self.rearm_name(&name);
        }

        // `autostart` and `depends_on` are the two `NextSpawn` fields that
        // report as applied: neither is read at a spawn. `restorable()` reads
        // `autostart` at a muster or a boot, and `plan_for_names` reads
        // `depends_on` whenever a batch is ordered, so both are in force the
        // moment they land on the stored spec.
        let (already_in_force, later): (Vec<String>, Vec<String>) = next_spawn
            .into_iter()
            .partition(|field| matches!(field.as_str(), "autostart" | "depends_on"));
        // What could not be parked, for the refusal below to name. Under
        // `park_all` that is every field; otherwise the `NeedsRespawn` ones
        // alone, since a `NextSpawn` field is already on the stored spec.
        let unparked: Vec<String> = if !parked_failed {
            Vec::new()
        } else if park_all {
            live.iter()
                .cloned()
                .chain(already_in_force.iter().cloned())
                .chain(later.iter().cloned())
                .chain(respawn.iter().cloned())
                .collect()
        } else {
            respawn.clone()
        };
        // Unconditional on `parked_failed`, never on `unparked` being
        // non-empty: `parked_wanted` is also set by an earlier load's parked
        // config, so a load whose only drift is Live can fail to rebuild it
        // with no field of its own to name.
        if parked_failed {
            let what = if unparked.is_empty() {
                "an earlier load's parked config could not be rebuilt, so a respawn will put \
                 its values back"
                    .to_string()
            } else {
                format!(
                    "{} could not be parked for a next spawn",
                    unparked.join(", ")
                )
            };
            refusals.push(format!(
                "{what}: the merged config does not hold at {count} instance(s)"
            ));
        }
        // A field that went nowhere was established by nobody, so this app's
        // record goes back to what it was. Without this the high-water mark
        // absorbs the refused key, and an operator retrying the identical file
        // gets silence.
        for field in &unparked {
            match overrides.fields.get(field) {
                Some(previous) => next_overrides
                    .fields
                    .insert(field.clone(), previous.clone()),
                None => next_overrides.fields.remove(field),
            };
            if !overrides.declared.contains(field) {
                next_overrides.declared.remove(field);
            }
            if field == "env" {
                next_overrides
                    .declared_env
                    .clone_from(&overrides.declared_env);
            }
        }

        // Cached on every instance so `to_info` reads it with no file access:
        // the store is read once per load and `to_info` runs once per sheep on
        // every listing. `All` alone drops the record, since the record is what
        // holds a later plain load off a key an operator set.
        let overridden_names: Vec<String> = if matches!(reset, ResetDepth::All) {
            Vec::new()
        } else {
            next_overrides.fields.keys().cloned().collect()
        };
        for id in self.ids_of_name(&name) {
            if let Some(slot) = self.sheep.get_mut(&id) {
                slot.entry.overridden.clone_from(&overridden_names);
            }
        }

        // Handed to the caller rather than written here, so one file costs one
        // lock. Recorded only for an app that got this far: a load that refused
        // established nothing. The `Option` is the drop.
        changes.insert(
            name.clone(),
            (!matches!(reset, ResetDepth::All)).then_some(next_overrides),
        );
        let (mut applied, mut pending): (Vec<String>, Vec<String>) = if park_all {
            // Nothing reached the running instances at all, so nothing may be
            // reported as applied.
            (
                Vec::new(),
                if parked_failed {
                    Vec::new()
                } else {
                    live.iter()
                        .cloned()
                        .chain(already_in_force)
                        .chain(later)
                        .chain(respawn)
                        .collect()
                },
            )
        } else {
            (
                live.iter().cloned().chain(already_in_force).collect(),
                if parked_failed {
                    later
                } else {
                    later.into_iter().chain(respawn).collect()
                },
            )
        };
        if instances && count != running.instances {
            applied.push("instances".to_string());
        }
        applied.sort_unstable();
        pending.sort_unstable();

        let app = if count == merged.config().instances {
            Some(merged)
        } else {
            with_count(&merged, count)
        };
        Applied {
            name,
            applied,
            pending,
            refused: (!refusals.is_empty()).then(|| refusals.join("; ")),
            app,
        }
    }

    /// Rebuilds every lifecycle extra armed for `name`, so a changed
    /// [`EXTRAS_FIELDS`] value reaches the worker enforcing it.
    ///
    /// [`ExtrasRegistry::rearm_name`] rather than [`Self::arm_extras`] per id:
    /// `arm` preserves a live cron or watch task, which is right for a reload's
    /// overlap and wrong here.
    ///
    /// Gated on `Online` with a live pid, as every other arming site is:
    /// [`ExtrasRegistry::arm`] decides group membership from the configuration,
    /// so arming a stopped instance puts it in a group whose cron occurrence
    /// would start a process the operator had stopped. One prober per instance,
    /// since [`describe`] bakes `SHEP_INSTANCE` into a prober's environment.
    pub(super) fn rearm_name(&mut self, name: &str) {
        let Some(extras) = self.extras.as_ref() else {
            return;
        };
        // Constructed inline, as `arm_extras` does, so the registry can be
        // borrowed mutably while the flock is read.
        let supervisor = SupervisorHandle {
            tx: self.tx.clone(),
        };
        let mut armable: Vec<(u32, &ProcessEntry)> = self
            .sheep
            .iter()
            .filter(|(_, slot)| {
                slot.entry.spec.config().name == name
                    && slot.entry.status == ProcStatus::Online
                    && slot.entry.pid.is_some()
            })
            .map(|(id, slot)| (*id, &slot.entry))
            .collect();
        // No early return on an empty list: a name whose instances are all
        // momentarily non-`Online` still has to reach `rearm_name`, which is
        // what aborts the group holding the old config. Sorted, so the rebuild
        // order does not depend on a `HashMap`'s iteration order.
        armable.sort_unstable_by_key(|(id, _)| *id);
        let entries: Vec<&ProcessEntry> = armable.into_iter().map(|(_, entry)| entry).collect();
        let specs = self.rearm_specs(&entries);
        self.registry.rearm_name(
            name,
            &entries,
            |entry| {
                spec_prober(
                    specs
                        .get(&entry.id)
                        .expect("rearm_name: `rearm_specs` described every entry passed to it"),
                )
            },
            extras,
            &supervisor,
        );
    }

    /// The [`SpawnSpec`] each armable entry's prober is built from, by id.
    ///
    /// One [`SecretView`] per distinct environment across `entries`, not one
    /// for the group: `environment` is a `NeedsRespawn` field and
    /// [`Self::promote_pending`] rewrites a single slot, so two instances of
    /// a name can hold different environments at once and each resolves
    /// against its own.
    ///
    /// [`describe`] rather than [`assemble`]: this sheep is up, and a probe
    /// it can no longer build an environment for must not cost it its watch
    /// and its cron worker too.
    pub(super) fn rearm_specs(&self, entries: &[&ProcessEntry]) -> HashMap<u32, SpawnSpec> {
        let mut views: HashMap<&str, SecretView> = HashMap::new();
        let mut specs = HashMap::with_capacity(entries.len());
        for entry in entries {
            let environment = entry
                .spec
                .config()
                .environment
                .as_deref()
                .unwrap_or(&self.host_environment);
            let secrets = views
                .entry(environment)
                .or_insert_with(|| self.secret_view(&entry.spec));
            // `Credentials` is `Copy`, so this needs no clone. The unresolved
            // arm costs nothing: an entry reached here is running and
            // resolved its identity before it started.
            let credentials = match entry.credentials {
                SpawnIdentity::Resolved(credentials) => credentials,
                SpawnIdentity::Unresolved => None,
            };
            specs.insert(
                entry.id,
                describe(
                    &entry.spec,
                    entry.instance,
                    &self.paths,
                    credentials,
                    secrets,
                ),
            );
        }
        specs
    }
}
