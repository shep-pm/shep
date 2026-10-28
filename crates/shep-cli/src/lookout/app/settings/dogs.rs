//! One dog's row and what hangs under it: the tree its probe opened, the state
//! a read leaves it in, and the two `[daemon] keys` this screen can write.

use super::*;

impl Settings {
    /// The tree the dog at `index` was opened onto, or [`None`] while its row
    /// is closed or has never been opened.
    #[must_use]
    pub fn dog_tree(&self, index: usize) -> Option<&DogTree> {
        self.snapshot
            .dogs
            .get(index)
            .and_then(|dog| self.dog_trees.get(&dog.name))
    }

    /// Whether the row of the dog named `name` is mid-read: this screen asked
    /// for that dog's schema or section and has not heard back.
    ///
    /// What routes a dog read to the settings screen rather than to a full
    /// dog pane, so a read asked for by `e` from the flock is never answered
    /// here.
    #[must_use]
    pub fn is_reading_a_dog(&self, name: &str) -> bool {
        self.dog_trees
            .get(name)
            .is_some_and(|tree| tree.is_reading())
    }

    /// The tree the row of the dog named `name` holds, by name rather than by
    /// index, for a reply that carries a name and no index.
    #[must_use]
    pub fn dog_tree_named(&self, name: &str) -> Option<&DogTree> {
        self.dog_trees.get(name)
    }

    /// Opens the dog at `index` if its row is closed, closes it if open.
    /// Answers what the caller has to do next: a dog opened for the first
    /// time has to be probed, and a dog whose probe was refused is probed
    /// again rather than left holding the refusal.
    pub(in crate::lookout::app) fn toggle_dog(&mut self, index: usize) -> DogFold {
        let Some(dog) = self.snapshot.dogs.get(index) else {
            return DogFold::Nothing;
        };
        let name = dog.name.clone();
        let adopted_path = dog.adopted_path.clone();
        let held = self.dog_trees.remove(&name);
        let (tree, fold) = match held {
            None | Some(DogTree::Refused { .. }) => (
                DogTree::Probing,
                DogFold::Probe {
                    name: name.clone(),
                    adopted_path,
                },
            ),
            Some(DogTree::Probing) => (DogTree::Probing, DogFold::Nothing),
            Some(DogTree::Reading { schema }) => (DogTree::Reading { schema }, DogFold::Nothing),
            Some(DogTree::Open { pane, schema }) => {
                (DogTree::Shut { pane, schema }, DogFold::Closed)
            }
            Some(DogTree::Shut { pane, schema }) => {
                (DogTree::Open { pane, schema }, DogFold::Opened)
            }
        };
        self.dog_trees.insert(name, tree);
        fold
    }

    /// Parks the schema the probe answered with. The row is still waiting on
    /// the section, which is what [`Self::is_reading_a_dog`] says until
    /// [`Self::open_dog`] replaces it.
    pub(in crate::lookout::app) fn dog_schema(&mut self, name: &str, schema: serde_json::Value) {
        self.dog_trees
            .insert(name.to_owned(), DogTree::Reading { schema });
    }

    /// Opens the dog at `index` onto `pane`, replacing whatever state its row
    /// was in.
    pub(in crate::lookout::app) fn open_dog(
        &mut self,
        index: usize,
        pane: ConfigPane,
        schema: serde_json::Value,
    ) {
        let Some(name) = self.snapshot.dogs.get(index).map(|dog| dog.name.clone()) else {
            return;
        };
        self.dog_trees.insert(
            name,
            DogTree::Open {
                pane: Box::new(pane),
                schema,
            },
        );
    }

    /// Puts the row of the dog named `name` back to waiting on its section,
    /// keeping the schema its probe already answered with, and answers whether
    /// this screen has a tree for it at all.
    ///
    /// What a landed section write raises: the same `Request::DogConfig` a dog
    /// pane re-reads with, and the same reason for not re-probing.
    pub(in crate::lookout::app) fn reload_dog(&mut self, name: &str) -> bool {
        let Some(tree) = self.dog_trees.get_mut(name) else {
            return false;
        };
        let schema = match tree {
            DogTree::Open { schema, .. } | DogTree::Shut { schema, .. } => schema.clone(),
            DogTree::Probing | DogTree::Reading { .. } | DogTree::Refused { .. } => {
                return true;
            }
        };
        *tree = DogTree::Reading { schema };
        true
    }

    /// Says on the dog at `index`'s own row that its config could not be
    /// read, and leaves the row closed.
    pub(in crate::lookout::app) fn refuse_dog(&mut self, index: usize, why: String) {
        let Some(name) = self.snapshot.dogs.get(index).map(|dog| dog.name.clone()) else {
            return;
        };
        self.dog_trees.insert(name, DogTree::Refused { why });
    }

    /// The dog at `index`'s snapshot row.
    #[must_use]
    pub fn dog(&self, index: usize) -> Option<&DogView> {
        self.snapshot.dogs.get(index)
    }

    /// The snapshot row of the dog named `name`, or `None` when this read no
    /// longer lists it.
    ///
    /// What a reply about a dog checks the screen against: a re-read can drop
    /// a dog, and a sentence about a dog the operator can no longer see has to
    /// say something true about a dog that is not on screen.
    #[must_use]
    pub fn dog_named(&self, name: &str) -> Option<&DogView> {
        self.snapshot.dogs.iter().find(|dog| dog.name == name)
    }

    /// What `space` on the dog at `dog`'s `boot first` row would write: the
    /// opposite of the bit the file has.
    #[must_use]
    pub fn next_boot_first(&self, dog: usize) -> Option<bool> {
        self.dog(dog).map(|dog| !dog.boot_first)
    }

    /// What `space` on item `index` of the dog at `dog`'s grants checklist
    /// would write: the permission, and whether it would be held or taken
    /// away. `None` for an index the checklist does not have.
    #[must_use]
    pub fn next_grant(&self, dog: usize, index: usize) -> Option<(String, bool)> {
        let dog = self.dog(dog)?;
        let permission = dog.offered.get(index)?;
        Some((permission.clone(), !dog.grants.grants(permission)))
    }

    /// The dog `name`'s whole section with `row` of its own tree set to `text`,
    /// or why there is none to send.
    ///
    /// Runs through a copy of the dog's own pane rather than a settings-screen
    /// writer of its own, so the gate is [`crate::dog_settings::refusal`] and
    /// the text that reaches `Request::SetDogConfig` is the operator's section
    /// with one line changed. `Ok(None)` for a value it already carries.
    pub(in crate::lookout::app) fn dog_section_edit(
        &self,
        name: &str,
        row: usize,
        text: &str,
    ) -> Result<Option<DogSectionToml>, String> {
        let Some(pane) = self.dog_trees.get(name).and_then(DogTree::pane) else {
            return Err(format!("{name} has no open config tree on this screen"));
        };
        let mut copy = pane.clone();
        copy.move_to_index(row);
        copy.type_over(text);
        if let Some(refusal) = copy.apply_typing() {
            return Err(refusal.text);
        }
        let edits = copy.close();
        match copy.edited_section_with(&edits) {
            SectionWrite::Written(toml) => Ok(Some(toml)),
            SectionWrite::Nothing => Ok(None),
            SectionWrite::Refused => Err(format!(
                "{name}'s section holds something this screen would have to rewrite to change it"
            )),
        }
    }
}
