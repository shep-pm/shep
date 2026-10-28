//! [`Vcs`], the revision a sheep's directory is checked out on.

use serde::{Deserialize, Serialize};

/// The revision a sheep's own directory is checked out on, as its row reports it
///
/// `revision` is the commit's full hash in lowercase hex: 40 characters for
/// SHA-1, 64 for SHA-256, never an abbreviated hash or a name a branch could
/// be moved off, so two rows' revisions compare directly. `branch` names the
/// branch pointing at it, and is `None` for a detached head. No dirty flag:
/// whether the tree differs from the commit means reading the index.
// wire format: changing field names is a breaking change
// `Debug` is derived: a commit hash names no secret.
// `#[non_exhaustive]`: a third field would otherwise break an out-of-tree
// consumer's struct literal with no version bump (IR-20). Build one with [`Vcs::new`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vcs {
    /// The commit's full hash, lowercase hex.
    pub revision: String,
    /// The branch the checkout is on. `None` for a detached head.
    pub branch: Option<String>,
}

impl Vcs {
    /// A revision with no branch to name, which is what a detached head has.
    #[must_use]
    pub fn new(revision: impl Into<String>) -> Self {
        Self {
            revision: revision.into(),
            branch: None,
        }
    }

    /// This revision, naming the branch it is on.
    #[must_use]
    pub fn with_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }
}
