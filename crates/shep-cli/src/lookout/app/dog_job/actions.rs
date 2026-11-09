//! What a catalogue entry can do to its dog: install, upgrade or delete, each
//! only where it applies, and each through the one job [`App::start_job`] runs.

use super::*;

/// The question `D` asks before it deletes `name`.
fn delete_question(name: &str) -> String {
    format!("Delete {name}? An adopted dog with no receipt keeps its file.")
}

impl App {
    /// `shep install <adopt_as>`, from `Enter` on a catalogue entry.
    pub(crate) fn start_install(&mut self, adopt_as: &str) -> Effect {
        self.start_job(Job::Install {
            adopt_as: adopt_as.to_owned(),
        })
    }

    /// `shep upgrade <name>`, from `Enter` on an entry behind the index.
    pub(crate) fn start_upgrade(&mut self, name: &str) -> Effect {
        self.start_job(Job::Upgrade {
            name: name.to_owned(),
        })
    }

    /// Asks whether to delete `name`; a yes starts the job.
    pub(crate) fn ask_delete(&mut self, name: &str) -> Effect {
        let tag = self.mint_tag();
        self.dog_delete = Some((tag, name.to_owned()));
        self.open_modal(Modal::confirm(tag, delete_question(name)));
        Effect::None
    }

    /// Whether `row` has an install action here: the index names a source
    /// shep can build, the dog has no install receipt, and the
    /// gate is open.
    pub(crate) fn can_install(&self, row: &CatalogueRow) -> bool {
        matches!(row.state, DogState::NotInstalled | DogState::Adopted)
            && row.dog.is_installable()
            && WriteAuthority::granted(self).is_some()
    }

    /// Whether `row` has an upgrade action here: it is installed, its
    /// VERSION reads `a → b`, and the gate is open.
    pub(crate) fn can_upgrade(&self, row: &CatalogueRow) -> bool {
        matches!(row.state, DogState::Installed | DogState::Enabled)
            && row.is_behind()
            && WriteAuthority::granted(self).is_some()
    }

    /// Whether `row` has a delete action here: it is installed, it is not a
    /// built-in dog, and the gate is open.
    pub(crate) fn can_delete(&self, row: &CatalogueRow) -> bool {
        row.state != DogState::NotInstalled
            && !crate::dog::BUILT_IN_DOGS.contains(&row.dog.adopt_as.as_str())
            && WriteAuthority::granted(self).is_some()
    }

    /// The answer to the delete question `tag` asked, or `None` when `tag` is
    /// not that question.
    pub(super) fn on_delete_answer(&mut self, tag: u64, yes: bool) -> Option<Effect> {
        let (asked, name) = self.dog_delete.take()?;
        if asked != tag {
            self.dog_delete = Some((asked, name));
            return None;
        }
        Some(if yes {
            self.start_job(Job::Delete { name })
        } else {
            Effect::None
        })
    }
}
