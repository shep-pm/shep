//! The answer to copying one sheep's env onto another: what each key does.

use serde::{Deserialize, Serialize};

/// What one env key ends up as on the sheep a copy writes to.
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EnvCopyKind {
    /// The target had no such key.
    Added,
    /// The target had one, holding something else.
    Changed,
    /// The target had one and a `replace` copy leaves it out.
    Removed,
}

/// One env key a copy touches.
///
/// `old` and `new` are what [`shown_env_value`](crate::secrets::shown_env_value)
/// lets a client read: exactly one `{{secret:...}}` reference reads as
/// written, and every other value reads as `<redacted>`. The values
/// themselves never leave the shepherd. `seal` tells two redacted values
/// apart. `Debug` is derived.
// wire format: changing field names is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EnvCopyChange {
    /// The env key.
    pub key: String,
    /// What happens to it.
    pub kind: EnvCopyKind,
    /// What the target held, `None` for an added key.
    pub old: Option<String>,
    /// What the target holds afterwards, `None` for a removed key.
    pub new: Option<String>,
    /// A fingerprint of the real `old` and `new`, keyed by the shepherd that
    /// answered and meaningful only to it.
    ///
    /// Two redacted values look alike. A copy sent back with the list it
    /// reviewed would not notice one literal swapped for another. The key is
    /// fresh for each shepherd process, so the seal reveals nothing.
    #[serde(default)]
    pub seal: u64,
}

impl EnvCopyChange {
    /// Builds one key's change.
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        kind: EnvCopyKind,
        old: Option<String>,
        new: Option<String>,
    ) -> Self {
        Self {
            key: key.into(),
            kind,
            old,
            new,
            seal: 0,
        }
    }

    /// This change with `seal` as its fingerprint.
    #[must_use]
    pub fn sealed(mut self, seal: u64) -> Self {
        self.seal = seal;
        self
    }
}
