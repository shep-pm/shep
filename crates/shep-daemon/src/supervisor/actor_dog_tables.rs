//! A dog's per-sheep tables: the read, the write, and `config.sheep.<dog>`.
//!
//! Every answer comes off one view of the flock, [`Actor::dog_tables`], so
//! the read and the announcement agree about which slot speaks for a name,
//! and about the operator's default for a dog layered under each table.
//! The write is [`Actor::write_sheep_field`] on the whole `dogs` map, built
//! here from the one table that moved. `run` compares the view after every
//! message against the last one it announced.

use super::actor_config::speaker_rank;
use super::*;
use std::borrow::Cow;

/// Sheep name to that sheep's `dogs` map as a dog reads it: borrowed from
/// the flock, or owned where a default was layered in.
type DogView<'a> = BTreeMap<&'a str, Cow<'a, BTreeMap<String, DogTable>>>;

impl<R: ProcessRunner> Actor<R> {
    /// Every non-dog name carrying a table, read off the stored spec of the
    /// slot [`Self::representative_id`] would pick, each table under its
    /// dog's default.
    ///
    /// One pass rather than `representative_id` per name, which would walk
    /// the flock once for every sheep in it.
    fn dog_tables(&self) -> DogView<'_> {
        let mut chosen: BTreeMap<&str, (bool, u32, u32)> = BTreeMap::new();
        for (id, slot) in &self.sheep {
            if slot.entry.dog.is_some() {
                continue;
            }
            let rank = speaker_rank(*id, slot);
            chosen
                .entry(slot.entry.spec.config().name.as_str())
                .and_modify(|held| *held = rank.min(*held))
                .or_insert(rank);
        }
        chosen
            .into_iter()
            .filter_map(|(name, (_, _, id))| {
                let dogs = &self.sheep.get(&id)?.entry.spec.config().dogs;
                (!dogs.is_empty()).then(|| (name, self.dog_defaults.layer(dogs)))
            })
            .collect()
    }

    /// `dog`'s table on every sheep carrying one, by sheep name, under
    /// `dog`'s default.
    ///
    /// Empty when no sheep carries one, never a refusal: a dog asks on its
    /// own name, which cannot be missing the way a sheep's can.
    pub(super) fn handle_dog_sheep_settings(&self, dog: &str) -> BTreeMap<String, DogTable> {
        self.dog_tables()
            .into_iter()
            .filter_map(|(name, dogs)| Some((name.to_string(), dogs.get(dog)?.clone())))
            .collect()
    }

    /// `name`'s table for `dog` under `dog`'s default, and the environment
    /// a reference in it resolves in: the stored spec's `environment`, or the shepherd's when
    /// it sets none, as [`Self::secret_view`] reads it.
    ///
    /// The slot is the one [`Self::dog_tables`] picks, ranked by
    /// [`speaker_rank`], so this and the plain read agree about which slot
    /// speaks for the name. `None` when no sheep has that name.
    pub(super) fn handle_sheep_dog_read(&self, name: &str, dog: &str) -> Option<SheepDogRead> {
        let (_, slot) = self
            .sheep
            .iter()
            .filter(|(_, slot)| slot.entry.dog.is_none())
            .filter(|(_, slot)| slot.entry.spec.config().name == name)
            .min_by_key(|(id, slot)| speaker_rank(**id, slot))?;
        let config = slot.entry.spec.config();
        Some(SheepDogRead {
            table: config
                .dogs
                .get(dog)
                .map(|own| self.dog_defaults.layer_table(dog, own)),
            environment: config
                .environment
                .clone()
                .unwrap_or_else(|| self.host_environment.clone()),
        })
    }

    /// Sets `dog`'s table on `name`, or removes it with `None`, as an
    /// operator override of the whole `dogs` field.
    ///
    /// `Ok(None)` when no sheep has that name. The map is built from the
    /// intended config, so every other dog's table rides along as it is,
    /// and a parked edit is not dropped for `apply_one`'s reason.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::write_sheep_field`] refuses: `IsADog` for a dog's
    /// own name, `InvalidField` for an empty dog name, `Overrides` for a
    /// store that could not be read or written.
    pub(super) fn handle_set_sheep_dog_settings(
        &mut self,
        name: &str,
        dog: &str,
        table: Option<DogTable>,
    ) -> Result<Option<FieldSet>, SupervisorError> {
        let Some(intended) = self
            .representative_id(name)
            .and_then(|id| self.intended_spec(id))
        else {
            return Ok(None);
        };
        let mut dogs = intended.config().dogs.clone();
        match table {
            Some(table) => dogs.insert(dog.to_string(), table),
            None => dogs.remove(dog),
        };
        // No serde message in the refusal: it can quote a value.
        let value = serde_json::to_value(&dogs).map_err(|_| {
            SupervisorError::InvalidField(format!("dogs: {dog}'s table would not serialize"))
        })?;
        self.write_sheep_field(name, "dogs", &value)
    }

    /// `dog`'s default table as stored, `None` when it has none.
    pub(super) fn handle_dog_defaults(&self, dog: &str) -> Option<DogTable> {
        self.dog_defaults.get(dog).cloned()
    }

    /// Sets `dog`'s default table, or removes it with `None`.
    ///
    /// The store is written before the view changes, so a refused write
    /// leaves what every dog reads as it was. The announcement is `run`'s,
    /// off the layered view, one per sheep whose table moved.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::InvalidField`]: `dog` is empty.
    /// - [`SupervisorError::DogDefaults`]: the store could not be read or
    ///   written.
    pub(super) fn handle_set_dog_defaults(
        &mut self,
        dog: &str,
        table: Option<DogTable>,
    ) -> Result<(), SupervisorError> {
        if dog.is_empty() {
            return Err(SupervisorError::InvalidField(
                "a dog's name cannot be empty".to_string(),
            ));
        }
        self.dog_defaults = dog_defaults::put(&self.paths.dog_defaults, dog, table)
            .map_err(|err| SupervisorError::DogDefaults(err.to_string()))?;
        Ok(())
    }

    /// The index `run` starts from, taken off whatever flock the builder
    /// installed, so a handover's carried tables are not news.
    pub(super) fn dog_index(&self) -> DogIndex {
        owned(&self.dog_tables())
    }

    /// Publishes one `DogSheepSettingsChanged` per table that moved since
    /// `announced`, then records what it announced.
    ///
    /// A flock with no dog tables and nothing announced costs one pass over
    /// the flock and no copy. Otherwise the view is built on every call, and
    /// a sheep whose dog has a default pays a copy of its layered tables
    /// there; the announced index is cloned only when a table moved.
    pub(super) fn announce_dog_tables(&self, announced: &mut DogIndex) {
        let no_tables = || {
            self.sheep
                .values()
                .all(|slot| slot.entry.spec.config().dogs.is_empty())
        };
        if announced.is_empty() && no_tables() {
            return;
        }
        let current = self.dog_tables();
        let changed = changed_tables(announced, &current);
        if changed.is_empty() {
            return;
        }
        *announced = owned(&current);
        for (dog, sheep) in changed {
            let _ = self
                .events
                .send(BusEvent::DogSheepSettingsChanged { dog, sheep }.into());
        }
    }
}

/// The last [`DogView`] announced, owned so it outlives the message that
/// built it.
type DogIndex = BTreeMap<String, BTreeMap<String, DogTable>>;

/// `view` with every name and table cloned.
fn owned(view: &DogView<'_>) -> DogIndex {
    view.iter()
        .map(|(name, dogs)| ((*name).to_string(), dogs.as_ref().clone()))
        .collect()
}

/// Each `(dog, sheep)` whose table `before` and `after` disagree on, once
/// each, ordered by dog then sheep.
fn changed_tables(before: &DogIndex, after: &DogView<'_>) -> Vec<(String, String)> {
    let same = before.len() == after.len()
        && before
            .iter()
            .zip(after)
            .all(|((name, dogs), (now, now_dogs))| name == now && dogs == now_dogs.as_ref());
    if same {
        return Vec::new();
    }
    let none = BTreeMap::new();
    let names: BTreeSet<&str> = before
        .keys()
        .map(String::as_str)
        .chain(after.keys().copied())
        .collect();
    let mut changed = Vec::new();
    for sheep in names {
        let was = before.get(sheep).unwrap_or(&none);
        let is = after.get(sheep).map_or(&none, Cow::as_ref);
        let dogs: BTreeSet<&String> = was.keys().chain(is.keys()).collect();
        changed.extend(
            dogs.into_iter()
                .filter(|dog| was.get(*dog) != is.get(*dog))
                .map(|dog| (dog.clone(), sheep.to_string())),
        );
    }
    changed.sort_unstable();
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One index row per `(sheep, [(dog, concurrency)])`, each table
    /// distinct by its `concurrency`.
    fn index(rows: &[(&str, &[(&str, u64)])]) -> DogIndex {
        rows.iter()
            .map(|(sheep, tables)| {
                let dogs = tables
                    .iter()
                    .map(|(dog, n)| {
                        let mut table = serde_json::Map::new();
                        table.insert("concurrency".to_string(), serde_json::json!(n));
                        ((*dog).to_string(), DogTable::from(table))
                    })
                    .collect();
                ((*sheep).to_string(), dogs)
            })
            .collect()
    }

    /// `index` borrowed the way `Actor::dog_tables` hands the flock over.
    fn view(index: &DogIndex) -> DogView<'_> {
        index
            .iter()
            .map(|(name, dogs)| (name.as_str(), Cow::Borrowed(dogs)))
            .collect()
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(dog, sheep)| ((*dog).to_string(), (*sheep).to_string()))
            .collect()
    }

    #[test]
    fn an_unchanged_flock_changes_nothing() {
        let same = index(&[
            ("web", &[("jobs", 2), ("deploy", 1)]),
            ("api", &[("jobs", 1)]),
        ]);
        assert_eq!(changed_tables(&same, &view(&same)), pairs(&[]));
        assert_eq!(
            changed_tables(&DogIndex::new(), &DogView::new()),
            pairs(&[])
        );
    }

    /// `web` gains `deploy`, `api` loses its only table, `db` edits `jobs`
    /// and keeps `audit`, and `worker` arrives carrying two: every change
    /// named once, by dog then sheep, and nothing for the untouched table.
    #[test]
    fn an_added_a_removed_and_an_edited_table_are_each_named_once() {
        let before = index(&[
            ("api", &[("jobs", 1)]),
            ("db", &[("audit", 1), ("jobs", 1)]),
            ("web", &[("jobs", 2)]),
        ]);
        let after = index(&[
            ("db", &[("audit", 1), ("jobs", 5)]),
            ("web", &[("deploy", 1), ("jobs", 2)]),
            ("worker", &[("audit", 3), ("jobs", 3)]),
        ]);
        assert_eq!(
            changed_tables(&before, &view(&after)),
            pairs(&[
                ("audit", "worker"),
                ("deploy", "web"),
                ("jobs", "api"),
                ("jobs", "db"),
                ("jobs", "worker"),
            ])
        );
    }

    /// Every table on a sheep that disappears is named, since each one's
    /// dog loses an answer.
    #[test]
    fn a_sheep_that_goes_names_every_table_it_carried() {
        let before = index(&[("web", &[("deploy", 1), ("jobs", 2)])]);
        assert_eq!(
            changed_tables(&before, &DogView::new()),
            pairs(&[("deploy", "web"), ("jobs", "web")])
        );
    }
}
