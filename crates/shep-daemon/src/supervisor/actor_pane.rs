//! Reading and writing one sheep's config from a pane.
//!
//! An operator editing a single sheep is not the same as loading a
//! Flockfile: the change is theirs, it outlives the next load, and it is
//! stored as an override. These four answer a pane's read and take its
//! writes, one field or a batch of environment variables at a time.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// One sheep's effective config for a pane, or `Ok(None)` when no sheep
    /// has that name.
    ///
    /// Reads and writes nothing. `env` is emptied on the way out by
    /// [`SheepConfigView::new`], which is the only constructor, so no path
    /// out of here can carry a value.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::IsADog`] - the name is a dog's. No other
    ///   request hands a client a whole `AppConfig`, so serving one here
    ///   would be a read surface that exists for dogs and nothing else.
    pub(super) fn handle_sheep_config(
        &self,
        name: &str,
    ) -> Result<Option<SheepConfigView>, SupervisorError> {
        let Some(id) = self.representative_id(name) else {
            return Ok(None);
        };
        let Some(slot) = self.sheep.get(&id) else {
            return Ok(None);
        };
        if slot.entry.dog.is_some() {
            return Err(SupervisorError::IsADog(dog_config_refusal(name)));
        }
        // [`Self::intended_spec`], not the running `spec`: a pane has to
        // show what the sheep is meant to be, or an operator's own parked
        // edit reads as never having landed. `pending` beside it is what
        // says the running child does not have it yet, and it is computed
        // off the same helper the listing uses, so the pane and the flock
        // table never disagree about the same sheep.
        let Some(config) = self.intended_spec(id).map(|spec| spec.config().clone()) else {
            return Ok(None);
        };
        Ok(Some(SheepConfigView::new(
            config,
            self.overridden_for(name),
            pending_fields(&slot.entry),
        )))
    }

    /// Records `key` on `name`'s env as an operator override, or removes it
    /// with `value: None`, and parks the result for that sheep's next spawn.
    ///
    /// `Ok(false)` when no sheep has that name.
    ///
    /// # Ordering
    ///
    /// Validate, then write the store, then park. A refusal is therefore
    /// raised before anything is touched, so an operator whose key
    /// `normalize` will not take is left with the env they already had
    /// rather than a store that disagrees with the flock.
    ///
    /// # Why it parks rather than applies
    ///
    /// The running child was handed its environment at `execve` and cannot
    /// be handed another one, so `env` is a `NeedsRespawn` field wherever it
    /// appears (`config::apply`'s table) and this takes the same route
    /// [`Self::apply_one`] takes for one: onto every slot's
    /// [`ProcessEntry::pending`], for a `shep reload` or `shep restart` to
    /// promote.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::IsADog`] - the name is a dog's. Raised before
    ///   the store is read, so nothing was written.
    /// - [`SupervisorError::InvalidEnv`] - the resulting config is one
    ///   `normalize` refuses. Nothing was written.
    /// - [`SupervisorError::Overrides`] - the override store could not be
    ///   read or written. Nothing was parked.
    pub(super) fn handle_set_sheep_env(
        &mut self,
        name: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<Option<ResolvedApp>, SupervisorError> {
        let Some(id) = self.representative_id(name) else {
            return Ok(None);
        };
        // Checked before the store is read and long before it is written.
        // A dog runs at the daemon's own trust level and its binary is
        // what `shep adopt` vetted, so a `PATH`, an `LD_PRELOAD` or a
        // `DYLD_INSERT_LIBRARIES` parked for its next respawn is arbitrary
        // code at that level, and a dog is never in the override store, so
        // nothing further down this function would have caught it.
        // `Self::apply_one` refuses a Flockfile that names a dog for the
        // same reason and says the same sentence.
        if self
            .sheep
            .get(&id)
            .is_some_and(|slot| slot.entry.dog.is_some())
        {
            return Err(SupervisorError::IsADog(dog_config_refusal(name)));
        }
        // The config an unchanged next spawn would use, which is a parked
        // one when an earlier load left one: building on the running config
        // instead would drop that load's change on the floor, which is
        // `apply_one`'s own reason for reading `intended` rather than
        // `running`.
        let Some(mut intended) = self.intended_spec(id).map(|spec| spec.config().clone()) else {
            return Ok(None);
        };
        // Captured before the edit, because the removal branch below needs
        // to know whether there was anything to remove.
        let was_in_config = intended.env.contains_key(key);
        match value {
            Some(value) => intended.env.insert(key.to_string(), value.to_string()),
            None => intended.env.remove(key),
        };

        let parked =
            normalize(intended).map_err(|err| SupervisorError::InvalidEnv(err.to_string()))?;

        let mut record = overrides::get(&self.paths.overrides, name)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?
            .unwrap_or_default();
        // Read before `env_override_map` borrows the record mutably.
        let file_declares = record.declared_env.contains(key);
        let map = env_override_map(&mut record, name)?;
        let was_overridden = map.contains_key(key);
        // A tombstone is left alone rather than removed and re-inserted,
        // which makes a second removal of the same key a no-op instead of a
        // deletion. Without this, removing an already-removed key drops the
        // tombstone (there is nothing in the config to remove, so the
        // re-insert below does not fire) and the next load of a file that
        // still declares the key puts its value back.
        let tombstoned = map.get(key).is_some_and(serde_json::Value::is_null);
        match value {
            Some(value) => map.insert(
                key.to_string(),
                serde_json::Value::String(value.to_string()),
            ),
            None if tombstoned => None,
            None => map.remove(key),
        };
        // Dropping the operator's own value is only half an answer: for a
        // key the operator never set, `map.remove` is a no-op, the store
        // comes back `{}`, and the edit lives only in
        // `ProcessEntry::pending`, lost to a cold restart and invisible in
        // the CFG column.
        //
        // So a removal leaves a JSON `null` tombstone under the key
        // whenever the key was in the config and something other than
        // this store put it there. Three cases fall out of the condition
        // below:
        //
        // - the key came from the app's config and was never overridden:
        //   tombstone, since the removal is the operator's only record of
        //   it;
        // - the key was an operator override the file also declares:
        //   tombstone, since dropping the override alone would let the
        //   file's value read as current;
        // - the key was an operator override and nothing else supplies
        //   it: plain removal, since the sheep now matches its file. A
        //   tombstone here would mark a sheep forever for a key that
        //   exists nowhere.
        //
        // A null never becomes a config value: `merge_declared`'s env
        // branch reads this map for its keys only, so a null just marks
        // "somebody has spoken for this key" and stops a later plain load
        // from restoring it. `--reset=env` and `--reset=all` clear it
        // through `merge_declared`'s own `next.fields.remove("env")`, not
        // through `establish_env`, which by then has nothing left to find
        // and deliberately preserves a tombstone it does find.
        if value.is_none() && was_in_config && (!was_overridden || file_declares) {
            map.insert(key.to_string(), serde_json::Value::Null);
        }
        let emptied = map.is_empty();
        // A removal that left nothing behind takes the `env` key with it,
        // for the reason `merge_declared` states where it spends an
        // override: a field nobody is holding a value for must stop
        // reporting in `ProcessEntry::overridden`, or the CFG column marks
        // a sheep that no longer differs from its Flockfile.
        if emptied {
            record.fields.remove("env");
        }
        let overridden: Vec<String> = record.fields.keys().cloned().collect();
        let changes = BTreeMap::from([(name.to_string(), Some(record))]);
        overrides::update(&self.paths.overrides, &changes)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?;

        // Every slot of the name, exactly as `apply_one` writes its own
        // parked config, and re-read by `ids_of_name` for its reason too.
        // `pending_reidentifies` is deliberately left alone: it tracks a
        // `user`/`group` move, and an env key cannot make one.
        for id in self.ids_of_name(name) {
            let Some(slot) = self.sheep.get_mut(&id) else {
                continue;
            };
            slot.entry.pending = Some(parked.clone());
            slot.entry.overridden.clone_from(&overridden);
        }
        Ok(Some(parked))
    }

    /// Records several env keys on `name` as operator overrides in one
    /// write, and parks them for the next spawn.
    ///
    /// `Ok(None)` when no sheep has that name.
    ///
    /// # Why this is not a loop over [`Self::handle_set_sheep_env`]
    ///
    /// That function writes the store once per key. Twenty keys would be
    /// twenty read-modify-writes, and a failure at the eleventh would leave
    /// half an import applied with no record of which half. This validates
    /// every key against the intended config first, then writes once.
    ///
    /// # Collisions
    ///
    /// A key already holding a different value in the intended config
    /// collides. Without `force`, one collision refuses the whole batch and
    /// nothing is written. A key holding the same value is `unchanged` and
    /// is not rewritten, so a repeated identical batch is a no-op.
    ///
    /// # What a dry run answers
    ///
    /// Everything the real send would, refusals included: `normalize` runs
    /// on the merged config before this returns, so a preview that reports
    /// a key as `set` is one the real send takes. `app` is `None` here, as
    /// it is for a refused batch and for one whose keys were all
    /// `unchanged`: nothing was written for the caller to record.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::IsADog`] - the name is a dog's. Raised before
    ///   the store is read, for [`Self::handle_set_sheep_env`]'s reason.
    /// - [`SupervisorError::InvalidEnv`] - the resulting config is one
    ///   `normalize` refuses. Nothing was written, on a dry run or a real
    ///   send alike.
    /// - [`SupervisorError::Overrides`] - the store could not be read or
    ///   written. Nothing was parked.
    pub(super) fn handle_set_sheep_env_batch(
        &mut self,
        name: &str,
        entries: &BTreeMap<String, String>,
        force: bool,
        dry_run: bool,
    ) -> Result<Option<EnvBatch>, SupervisorError> {
        let Some(id) = self.representative_id(name) else {
            return Ok(None);
        };
        // Before the store is read, for `handle_set_sheep_env`'s reason: a
        // dog runs at the daemon's own trust level.
        if self
            .sheep
            .get(&id)
            .is_some_and(|slot| slot.entry.dog.is_some())
        {
            return Err(SupervisorError::IsADog(dog_config_refusal(name)));
        }
        let Some(mut intended) = self.intended_spec(id).map(|spec| spec.config().clone()) else {
            return Ok(None);
        };

        let mut set = Vec::new();
        let mut unchanged = Vec::new();
        let mut collisions = Vec::new();
        for (key, value) in entries {
            match intended.env.get(key) {
                Some(current) if current == value => unchanged.push(key.clone()),
                Some(_) => {
                    collisions.push(key.clone());
                    if force {
                        set.push(key.clone());
                    }
                }
                None => set.push(key.clone()),
            }
        }

        // A refused batch changes nothing, so what the merged config would
        // normalize to is moot and the collision report is the whole answer.
        if !collisions.is_empty() && !force {
            return Ok(Some(EnvBatch {
                app: None,
                set: Vec::new(),
                unchanged,
                collisions,
            }));
        }

        // Before the `dry_run` return, not after it: `normalize` is this
        // door's only validation, so a preview that skipped it would report
        // a key as `set` and then fail on the real send.
        for key in &set {
            intended.env.insert(key.clone(), entries[key].clone());
        }
        let parked =
            normalize(intended).map_err(|err| SupervisorError::InvalidEnv(err.to_string()))?;

        // `set` empty means every key was already held at this value, so
        // there is nothing to write and nothing for `rpc.rs` to record:
        // `app` is `Some` only when the store moved.
        if dry_run || set.is_empty() {
            return Ok(Some(EnvBatch {
                app: None,
                set,
                unchanged,
                collisions,
            }));
        }

        let mut record = overrides::get(&self.paths.overrides, name)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?
            .unwrap_or_default();
        let map = env_override_map(&mut record, name)?;
        for key in &set {
            map.insert(key.clone(), serde_json::Value::String(entries[key].clone()));
        }
        // No tombstone handling and no `emptied` branch: this door only
        // ever inserts, so the map cannot come out empty and no key can
        // stop being held.
        let overridden: Vec<String> = record.fields.keys().cloned().collect();
        let changes = BTreeMap::from([(name.to_string(), Some(record))]);
        overrides::update(&self.paths.overrides, &changes)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?;

        for id in self.ids_of_name(name) {
            let Some(slot) = self.sheep.get_mut(&id) else {
                continue;
            };
            slot.entry.pending = Some(parked.clone());
            slot.entry.overridden.clone_from(&overridden);
        }
        Ok(Some(EnvBatch {
            app: Some(parked),
            set,
            unchanged,
            collisions,
        }))
    }

    /// Records `key` on `name` as an operator override, applies what can
    /// reach the running process, and parks the rest for its next spawn.
    ///
    /// `Ok(None)` when no sheep has that name.
    ///
    /// # Why this is not `ApplyConfig`
    ///
    /// One [`DeclaredApp`] declaring one key, at [`ResetDepth::File`],
    /// moves exactly this one field and nothing else, and then
    /// [`merge_declared`] spends the override for it. That reasoning does
    /// not hold here: a key put back to the template is not a key an
    /// operator is still holding a value for, but a pane's value is the
    /// operator's, and the sheep still differs from its file. Routed that
    /// way, an edit would drop out of [`ProcessEntry::overridden`], so the
    /// `*` would never render. This writes the override directly instead
    /// of pretending to be a template.
    ///
    /// # Ordering
    ///
    /// Validate, then write the store, then apply. Every refusal below is
    /// raised before [`overrides::update`], so an operator whose value this
    /// build will not take is left with the config they already had rather
    /// than a store that disagrees with the flock, the same rule
    /// [`Self::handle_set_sheep_env`] states and its own test pins.
    ///
    /// # Which fields reach a running process
    ///
    /// [`apply_group`]'s table, exactly as [`Self::apply_one`] reads it. A
    /// [`ApplyGroup::Live`] field goes onto the stored spec and is in force
    /// at the daemon's next decision; a [`ApplyGroup::NextSpawn`] field
    /// goes onto the stored spec too but is not in force until a spawn
    /// reads it, so it reports as pending, except `autostart`, which
    /// `restorable()` reads at muster rather than at a spawn and so is in
    /// force the moment it lands, the same carve-out `apply_one` makes. A
    /// [`ApplyGroup::NeedsRespawn`] field only parks.
    ///
    /// # `warning`, the one field this can answer besides `pending`
    ///
    /// `cwd`, `script`, `out_file` and `err_file` get a filesystem check
    /// nothing before this ran anywhere: `normalize` cannot see the
    /// filesystem, and a spawn is the daemon's own first look. A problem
    /// here never refuses the write; it names what the next respawn will
    /// otherwise fail on with a bare OS error. See [`FieldSet::warning`].
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::IsADog`] - the name is a dog's. Raised before
    ///   the store is read, so nothing was written.
    /// - [`SupervisorError::InvalidField`] - the key is one this door does
    ///   not own or `AppConfig` does not have, the value will not
    ///   deserialize into it, or `normalize` refuses the result. Nothing
    ///   was written.
    /// - [`SupervisorError::Overrides`] - the override store could not be
    ///   read or written. Nothing was parked.
    pub(super) fn handle_set_sheep_field(
        &mut self,
        name: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        // `env` has its own request, and it needs one: a whole env map is
        // never sent (a pane is not told the values), and this door's
        // wholesale replacement of one field would wipe every key but the
        // one being set. The two Structural fields are identity and flock
        // shape rather than runtime knobs: `handle_scale` owns the count,
        // and a `name` change is a different sheep.
        if key == "env" {
            return Err(SupervisorError::InvalidField(
                "env is set one key at a time; use `SetSheepEnv`".to_string(),
            ));
        }
        if apply_group(key) == ApplyGroup::Structural {
            return Err(SupervisorError::InvalidField(format!(
                "{key} is not a config write; `shep stock` moves an instance count, and a \
                 name change is a different sheep"
            )));
        }
        let Some(id) = self.representative_id(name) else {
            return Ok(None);
        };
        // Checked before the store is read and long before it is written,
        // for the reason `handle_set_sheep_env`'s own guard gives at
        // length: a dog runs at the daemon's own trust level, a dog is
        // never in the override store so nothing further down would catch
        // it, and this door reaches `script` and `args` directly.
        // `apply_one` refuses a dog with this same sentence; `handle_scale`
        // refuses one too, but with its own: a count is not a config
        // write, so it says a dog runs one process, under `InvalidScale`
        // rather than `IsADog`.
        if self
            .sheep
            .get(&id)
            .is_some_and(|slot| slot.entry.dog.is_some())
        {
            return Err(SupervisorError::IsADog(dog_config_refusal(name)));
        }
        let Some(slot) = self.sheep.get(&id) else {
            return Ok(None);
        };
        // `running` is what the child was spawned from; `intended` is what
        // the app is meant to be, which is an earlier edit's parked config
        // when there is one. Building on `running` would drop that edit,
        // which is `apply_one`'s own reason for the same pair.
        let running = slot.entry.spec.config().clone();
        let intended = slot
            .entry
            .pending
            .as_ref()
            .map_or_else(|| running.clone(), |parked| parked.config().clone());

        let Ok(serde_json::Value::Object(mut object)) = serde_json::to_value(&intended) else {
            return Err(SupervisorError::InvalidField(
                "an app config must serialize as an object".to_string(),
            ));
        };
        // Checked rather than inserted blind. `AppConfig` would take an
        // unknown key without complaint or ignore it outright, either way
        // reporting a write that changed nothing, and a pane's key comes
        // off a schema this daemon may not share a version with.
        if !object.contains_key(key) {
            return Err(SupervisorError::InvalidField(format!(
                "no config field named {key}"
            )));
        }
        object.insert(key.to_string(), value.clone());
        let edited: AppConfig = serde_json::from_value(serde_json::Value::Object(object))
            .map_err(|err| SupervisorError::InvalidField(format!("{key}: {err}")))?;
        let merged = normalize(edited)
            .map_err(|err| SupervisorError::InvalidField(format!("{key}: {err}")))?;

        // Advisory, never a second way to refuse the write above: the
        // validation has already accepted the value. Not that the override
        // store has been written, which happens further down; what is
        // settled here is that nothing below will refuse. `normalize` cannot make
        // this call itself (`normalize_with_home`'s own doc gives the
        // reason: the CLI and the daemon can normalize the same config as
        // different users), and the gap between this check and the respawn
        // that actually needs the path is the same one `check_log_ancestry`
        // documents for its own check-then-open window
        // (`docs/specs/deferred.md`). `cwd` and `script` share one spec and
        // one `preflight` call because each one's resolution already
        // depends on the other; `out_file`/`err_file` need neither `cwd`
        // nor one another.
        //
        // The two arms each build their own spec rather than hoisting one
        // above the `match`, which would look tidier and cost more: most
        // keys reach `_ => None`, and `describe` renders every template and
        // resolves every secret reference the config carries. Duplicated
        // lines here buy that work being skipped on every field but these
        // four.
        let warning = match key {
            "cwd" | "script" => {
                let view = self.secret_view(&merged);
                let spec = describe(&merged, 0, &self.paths, None, &view);
                spec.cwd.as_deref().and_then(cwd_advisory).or_else(|| {
                    match self.runner.preflight(&spec) {
                        Preflight::Impossible(reason) | Preflight::Doubtful(reason) => Some(reason),
                        Preflight::Unknown => None,
                    }
                })
            }
            "out_file" | "err_file" => {
                let view = self.secret_view(&merged);
                let spec = describe(&merged, 0, &self.paths, None, &view);
                log_path_advisory(if key == "out_file" {
                    &spec.out_file
                } else {
                    &spec.err_file
                })
            }
            _ => None,
        };

        // The one field that moved, if it moved at all. A value identical
        // to what is already intended still records the override (the
        // operator has spoken for the key, which is the whole point of
        // this door), but nothing needs applying or parking for it.
        let group = apply_group(key);
        let reaches = matches!(group, ApplyGroup::Live | ApplyGroup::NextSpawn);
        let reaching: Vec<String> = if reaches {
            vec![key.to_string()]
        } else {
            Vec::new()
        };
        // A subset of a valid config can still fail to normalize, because
        // `normalize` checks fields against each other: `watch` needs a
        // `cwd`, and a `cwd` left behind as `NeedsRespawn` takes the watch
        // down with it. `apply_one` treats that as "this app needs a
        // restart" rather than as an invalid request, and so does this.
        let next_spec = reached_spec(&running, merged.config(), &reaching, running.instances).ok();
        let park_all = next_spec.is_none();

        let mut record = overrides::get(&self.paths.overrides, name)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?
            .unwrap_or_default();
        record.fields.insert(key.to_string(), value.clone());
        let overridden: Vec<String> = record.fields.keys().cloned().collect();
        let changes = BTreeMap::from([(name.to_string(), Some(record))]);
        overrides::update(&self.paths.overrides, &changes)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?;

        // Parked whenever the field cannot reach a running child, and also
        // whenever an earlier edit already left a config parked: that
        // config predates this one, and promoting it later would put this
        // field back. `apply_one` recomputes it on the same condition.
        let parked_wanted = park_all
            || !reaches
            || self.ids_of_name(name).iter().any(|id| {
                self.sheep
                    .get(id)
                    .is_some_and(|s| s.entry.pending.is_some())
            });
        let parked = parked_wanted.then(|| merged.clone());

        // Every slot of the name, and `ids_of_name` re-read for the reason
        // `handle_set_sheep_env` gives.
        for id in self.ids_of_name(name) {
            let Some(slot) = self.sheep.get_mut(&id) else {
                continue;
            };
            // Against this slot's own spec and before it is overwritten,
            // and `|=` rather than `=`. Both halves are `apply_one`'s and
            // the argument for them is stated there, at the same line in
            // that function, not restated here: a paraphrase of a reason
            // is what goes stale when the reason changes.
            if let Some(parked) = &parked {
                let spawned = slot.entry.spec.config();
                slot.entry.pending_reidentifies |=
                    spawned.user != parked.config().user || spawned.group != parked.config().group;
            }
            if let Some(next_spec) = next_spec.clone() {
                slot.entry.spec = next_spec;
            }
            if let Some(parked) = parked.clone() {
                slot.entry.pending = Some(parked);
            }
            slot.entry.overridden.clone_from(&overridden);
        }

        // A Live field the extras engine reads is re-armed now rather than
        // at the next spawn, the same line `apply_one` runs for the same
        // set of fields.
        if !park_all && group == ApplyGroup::Live && EXTRAS_FIELDS.contains(&key) {
            self.rearm_name(name);
        }

        // `reaches_running` owns the `autostart`/`depends_on` carve-out now,
        // so the pane's prediction and this answer cannot drift.
        let in_force = !park_all && reaches_running(key);
        Ok(Some(FieldSet {
            app: parked.unwrap_or_else(|| next_spec.unwrap_or(merged)),
            pending: !in_force,
            warning,
        }))
    }
}
