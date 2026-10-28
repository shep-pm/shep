//! Reading and writing one sheep's config from a pane.
//!
//! An operator editing a single sheep is not the same as loading a
//! Flockfile: the change is theirs, it outlives the next load, and it is
//! stored as an override. The handlers here answer a pane's read and take its
//! writes, one field or a batch of environment variables at a time.

use super::*;

/// Records one env edit in `record`: `value` under `key`, or with `None` the
/// removal of `key`, leaving a tombstone where the removal is the only record
/// of it.
///
/// `was_in_config` is whether the intended config held `key` before the edit,
/// which a removal needs to know whether there was anything to remove.
///
/// # Errors
///
/// [`SupervisorError::Overrides`] - the stored `env` override is not an
/// object.
pub(super) fn record_env_edit(
    record: &mut AppOverrides,
    name: &str,
    key: &str,
    value: Option<&str>,
    was_in_config: bool,
) -> Result<(), SupervisorError> {
    // Read before `env_override_map` borrows the record mutably.
    let file_declares = record.declared_env.contains(key);
    let map = env_override_map(&mut *record, name)?;
    let was_overridden = map.contains_key(key);
    // A tombstone is left alone rather than removed and re-inserted, which
    // makes a second removal of the same key a no-op instead of a deletion.
    // Without it, removing an already-removed key drops the tombstone and
    // the next load of a file that still declares the key puts its value back.
    let tombstoned = map.get(key).is_some_and(serde_json::Value::is_null);
    match value {
        Some(value) => map.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        ),
        None if tombstoned => None,
        None => map.remove(key),
    };
    // Dropping the operator's own value is half an answer: for a key the
    // operator never set, `map.remove` is a no-op and the edit lives only in
    // `ProcessEntry::pending`, lost to a cold restart. A `null` tombstone
    // marks "somebody spoke for this key" and never becomes a config value.
    if value.is_none() && was_in_config && (!was_overridden || file_declares) {
        map.insert(key.to_string(), serde_json::Value::Null);
    }
    let emptied = map.is_empty();
    // A removal that left nothing behind takes the `env` key with it: a
    // field nobody is holding a value for must stop reporting in
    // `ProcessEntry::overridden`, or the CFG column marks a sheep that no
    // longer differs from its Flockfile.
    if emptied {
        record.fields.remove("env");
    }
    Ok(())
}

impl<R: ProcessRunner> Actor<R> {
    /// Refuse `name` when the slot at `id` is a dog's.
    ///
    /// A dog runs at the daemon's own trust level and its binary is what
    /// `shep adopt` vetted, so a `PATH`, an `LD_PRELOAD` or a
    /// `DYLD_INSERT_LIBRARIES` parked for its next respawn is arbitrary code
    /// at that level. A dog is never in the override store, so nothing further
    /// down any of these doors would have caught it, which is why every caller
    /// asks before the store is read.
    ///
    /// # Errors
    ///
    /// [`SupervisorError::IsADog`] - the slot at `id` is a dog's.
    pub(super) fn ensure_not_dog(&self, name: &str, id: u32) -> Result<(), SupervisorError> {
        if self
            .sheep
            .get(&id)
            .is_some_and(|slot| slot.entry.dog.is_some())
        {
            return Err(SupervisorError::IsADog(dog_config_refusal(name)));
        }
        Ok(())
    }

    /// One read-modify-write of `name`'s record in the override store, and
    /// the field names it leaves overridden.
    ///
    /// `f` is the mutation, which is the only part of a pane write that is
    /// not this: the store read, the empty record a first edit needs, the
    /// field names an operator has spoken for, and the one-record write back.
    /// A refusal out of `f` lands before the store is written.
    ///
    /// # Errors
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
    /// - Whatever `f` refuses.
    pub(super) fn mutate_override(
        &self,
        name: &str,
        f: impl FnOnce(&mut AppOverrides) -> Result<(), SupervisorError>,
    ) -> Result<Vec<String>, SupervisorError> {
        let mut record = overrides::get(&self.paths.overrides, name)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?
            .unwrap_or_default();
        f(&mut record)?;
        let overridden: Vec<String> = record.fields.keys().cloned().collect();
        let changes = BTreeMap::from([(name.to_string(), Some(record))]);
        overrides::update(&self.paths.overrides, &changes)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?;
        Ok(overridden)
    }

    /// Park `parked` on every slot of `name`, with `overridden` beside it.
    ///
    /// Exactly what [`Self::apply_one`] writes for one, and `ids_of_name`
    /// re-read for its reason too. A caller that also moves `spec` or
    /// `pending_reidentifies` writes its own loop: an env key never makes a
    /// `user`/`group` move, and a config field can.
    pub(super) fn park_slots(&mut self, name: &str, parked: &ResolvedApp, overridden: &[String]) {
        for id in self.ids_of_name(name) {
            let Some(slot) = self.sheep.get_mut(&id) else {
                continue;
            };
            slot.entry.pending = Some(parked.clone());
            slot.entry.overridden = overridden.to_vec();
        }
    }

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
        self.ensure_not_dog(name, id)?;
        let Some(slot) = self.sheep.get(&id) else {
            return Ok(None);
        };
        // [`Self::intended_spec`], not the running `spec`: a pane has to show
        // what the sheep is meant to be, or an operator's own parked edit
        // reads as never having landed. `pending` beside it is what says the
        // running child does not have it yet.
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
    /// with `value: None`, parks it for the next spawn, and answers `false`
    /// when no sheep has that name.
    ///
    /// Validate, then write the store, then park: a refusal is raised before
    /// anything is touched. A running child was handed its environment at
    /// `execve` and cannot be handed another, so this parks onto every slot's
    /// [`ProcessEntry::pending`] for a `shep reload` to promote.
    ///
    /// # Errors
    /// - [`SupervisorError::IsADog`] - the name is a dog's.
    /// - [`SupervisorError::InvalidEnv`] - `normalize` refuses the result.
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
    pub(super) fn handle_set_sheep_env(
        &mut self,
        name: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<Option<ResolvedApp>, SupervisorError> {
        let Some(id) = self.representative_id(name) else {
            return Ok(None);
        };
        // Before the store is read, and long before it is written:
        // [`Self::ensure_not_dog`] is why, and [`Self::apply_one`] refuses a
        // Flockfile that names a dog with the same sentence.
        self.ensure_not_dog(name, id)?;
        // The config an unchanged next spawn would use, which is a parked one
        // when an earlier load left one: building on the running config
        // instead would drop that load's change on the floor, which is
        // `apply_one`'s own reason for reading `intended` rather than `running`.
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

        let overridden = self.mutate_override(name, |record| {
            record_env_edit(record, name, key, value, was_in_config)
        })?;

        self.park_slots(name, &parked, &overridden);
        Ok(Some(parked))
    }

    /// Records several env keys on `name` as operator overrides in one write,
    /// parks them for the next spawn, and answers `None` for no such name.
    ///
    /// Not a loop over [`Self::handle_set_sheep_env`]: that writes the store
    /// once per key, so a failure part-way would leave half an import applied
    /// with no record of which half. A key already holding a different value
    /// collides; without `force` one collision refuses the whole batch. A dry
    /// run answers everything the real send would, refusals included.
    ///
    /// # Errors
    /// - [`SupervisorError::IsADog`] - the name is a dog's.
    /// - [`SupervisorError::InvalidEnv`] - `normalize` refuses the result.
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
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
        // Before the store is read, for [`Self::ensure_not_dog`]'s reason.
        self.ensure_not_dog(name, id)?;
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
        // there is nothing to write and nothing for `rpc/dispatch.rs` to record:
        // `app` is `Some` only when the store moved.
        if dry_run || set.is_empty() {
            return Ok(Some(EnvBatch {
                app: None,
                set,
                unchanged,
                collisions,
            }));
        }

        let overridden = self.mutate_override(name, |record| {
            let map = env_override_map(&mut *record, name)?;
            for key in &set {
                map.insert(key.clone(), serde_json::Value::String(entries[key].clone()));
            }
            // No tombstone handling and no `emptied` branch: this door only
            // ever inserts, so the map cannot come out empty and no key can
            // stop being held.
            Ok(())
        })?;

        self.park_slots(name, &parked, &overridden);
        Ok(Some(EnvBatch {
            app: Some(parked),
            set,
            unchanged,
            collisions,
        }))
    }

    /// A pane's door to [`Self::write_sheep_field`]: refuses the keys
    /// another request owns, then writes.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::InvalidField`] - `env`, `dogs`, or a
    ///   Structural key. Nothing was read or written.
    /// - Whatever [`Self::write_sheep_field`] refuses.
    pub(super) fn handle_set_sheep_field(
        &mut self,
        name: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        refuse_unowned_key(key)?;
        self.write_sheep_field(name, key, value)
    }

    /// Drops the operator's override of `key` on `name`, so the value it held
    /// before comes back, and applies or parks that value as
    /// [`Self::write_sheep_field`] does. `Ok(None)` for no such name.
    ///
    /// The value that returns is the override record's `template` for the key:
    /// the Flockfile's value as of the last load that named it, or the value
    /// the sheep held when the operator first spoke for it. A record with no
    /// template goes back to the built-in default.
    ///
    /// # Errors
    /// - [`SupervisorError::InvalidField`] - an unknown key, one not overridden,
    ///   or a value that will not normalize.
    /// - [`SupervisorError::IsADog`] and [`SupervisorError::Overrides`].
    pub(super) fn handle_reset_sheep_field(
        &mut self,
        name: &str,
        key: &str,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        refuse_unowned_key(key)?;
        self.apply_sheep_field(name, key, FieldSource::Reset)
    }

    /// Records `key` on `name` as an operator override, applies what can reach
    /// the running process, and parks the rest for its next spawn.
    ///
    /// Validate, then write the store, then apply: every refusal is raised
    /// before [`Self::mutate_override`]. Which fields reach a running process
    /// is [`apply_group`]'s table, read as [`Self::apply_one`] reads it.
    /// `cwd`, `script`, `out_file` and `err_file` get a filesystem check in
    /// `warning` that `normalize` cannot make.
    ///
    /// # Errors
    /// - [`SupervisorError::IsADog`] - the name is a dog's.
    /// - [`SupervisorError::InvalidField`] - no such key, or `normalize` refuses.
    /// - [`SupervisorError::Overrides`] - the store could not be read or written.
    pub(super) fn write_sheep_field(
        &mut self,
        name: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        self.apply_sheep_field(name, key, FieldSource::Set(value))
    }

    /// [`Self::write_sheep_field`] and [`Self::handle_reset_sheep_field`]:
    /// the one path that validates a field's value, writes the override
    /// store, and applies or parks the result. `source` is the only
    /// difference between the two, which is why they cannot drift.
    fn apply_sheep_field(
        &mut self,
        name: &str,
        key: &str,
        source: FieldSource<'_>,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        let Some(id) = self.representative_id(name) else {
            return Ok(None);
        };
        // Before the store is read, for [`Self::ensure_not_dog`]'s reason:
        // this door reaches `script` and `args` directly.
        self.ensure_not_dog(name, id)?;
        let value = match source {
            FieldSource::Set(value) => value.clone(),
            FieldSource::Reset => self.template_value(name, key)?,
        };
        let value = &value;
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
        let before = object.insert(key.to_string(), value.clone());
        let edited: AppConfig = serde_json::from_value(serde_json::Value::Object(object))
            .map_err(|err| SupervisorError::InvalidField(format!("{key}: {err}")))?;
        let merged = normalize(edited)
            .map_err(|err| SupervisorError::InvalidField(format!("{key}: {err}")))?;

        // Advisory, never a second refusal: `normalize` cannot see the
        // filesystem (`normalize_with_home` says why). `cwd` and `script`
        // share one `preflight` since each resolves against the other. Each
        // arm builds its own spec so every other key skips `describe`.
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
        // `normalize` checks fields against each other: `watch` needs a `cwd`,
        // and a `cwd` left behind as `NeedsRespawn` takes the watch down with
        // it. That parks rather than refuses, as it does in `apply_one`.
        let next_spec = reached_spec(&running, merged.config(), &reaching, running.instances).ok();
        let park_all = next_spec.is_none();

        let overridden = self.mutate_override(name, |record| {
            match source {
                // The value the key held before this write is the one a
                // reset returns to, unless the operator already holds the
                // key and the record remembers an older one.
                FieldSource::Set(_) => {
                    if !record.fields.contains_key(key)
                        && let Some(before) = before
                    {
                        record.template.insert(key.to_string(), before);
                    }
                    record.fields.insert(key.to_string(), value.clone());
                }
                FieldSource::Reset => {
                    record.fields.remove(key);
                    record.template.remove(key);
                }
            }
            Ok(())
        })?;

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
            // Against this slot's own spec before it is overwritten, and
            // `|=`, for the reasons `apply_one` gives at the same line.
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
            value: value.clone(),
            pending: !in_force,
            warning,
        }))
    }

    /// The value a reset puts `key` back to, from `name`'s override record.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::InvalidField`] - the operator holds no override of
    ///   `key`, so there is nothing to drop and a no-op would read as success.
    /// - [`SupervisorError::Overrides`] - the store could not be read.
    fn template_value(&self, name: &str, key: &str) -> Result<serde_json::Value, SupervisorError> {
        let record = overrides::get(&self.paths.overrides, name)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?
            .unwrap_or_default();
        if !record.fields.contains_key(key) {
            return Err(SupervisorError::InvalidField(format!(
                "{key} has no override to drop: it already holds the Flockfile's value"
            )));
        }
        if let Some(template) = record.template.get(key) {
            return Ok(template.clone());
        }
        let defaults = serde_json::to_value(AppConfig::default())
            .map_err(|err| SupervisorError::InvalidField(format!("{key}: {err}")))?;
        defaults
            .get(key)
            .cloned()
            .ok_or_else(|| SupervisorError::InvalidField(format!("no config field named {key}")))
    }
}

/// Where [`Actor::apply_sheep_field`] gets the value it applies.
#[derive(Clone, Copy)]
enum FieldSource<'a> {
    /// The operator's value, recorded as an override.
    Set(&'a serde_json::Value),
    /// The value the key held before the operator spoke for it, with the
    /// override dropped.
    Reset,
}

/// The refusal for the keys `SetSheepField` and `ResetSheepField` leave to
/// another request.
///
/// A pane is never sent a whole `env` or `dogs` map, and a wholesale write of
/// either would wipe every env key, or every other dog's table, but the one
/// being set. The Structural fields are identity and flock shape:
/// `handle_scale` owns the count.
///
/// # Errors
///
/// [`SupervisorError::InvalidField`] - `env`, `dogs`, or a Structural key.
fn refuse_unowned_key(key: &str) -> Result<(), SupervisorError> {
    let owner = match key {
        "env" => Some("env is set one key at a time; use `SetSheepEnv`"),
        "dogs" => Some("dogs is set one dog's table at a time; use `SetSheepDogSettings`"),
        _ => None,
    };
    if let Some(refusal) = owner {
        return Err(SupervisorError::InvalidField(refusal.to_string()));
    }
    if apply_group(key) == ApplyGroup::Structural {
        return Err(SupervisorError::InvalidField(format!(
            "{key} is not a config write; `shep stock` moves an instance count, and a \
             name change is a different sheep"
        )));
    }
    Ok(())
}
