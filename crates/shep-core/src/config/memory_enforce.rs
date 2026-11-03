//! Who enforces `max_memory`: shep by sampling, or the kernel by cgroup.

use serde::{Deserialize, Serialize};

/// How `max_memory` is enforced.
///
/// `Kernel` writes the limit to the sheep's cgroup `memory.max`, so the
/// kernel caps the whole tree at the moment of allocation. It needs Linux with
/// a delegated cgroup v2 subtree, and an OOM kill counts against
/// `max_restarts`. `Poll` samples the tree and restarts it above the limit,
/// which does not count against `max_restarts`.
// wire format: changing these strings is a breaking change
// `#[non_exhaustive]`: a newer peer may name a third strategy, and this type
// rides the wire inside `AppConfig`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemoryEnforce {
    /// The shepherd samples the tree and restarts it above `max_memory`
    #[default]
    Poll,
    /// The kernel caps the tree through the cgroup's `memory.max`
    Kernel,
}

impl MemoryEnforce {
    /// Whether the kernel enforces the limit, so the polling enforcer stays
    /// disarmed.
    #[must_use]
    pub fn is_kernel(self) -> bool {
        self == Self::Kernel
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryEnforce;

    #[test]
    fn it_spells_the_two_strategies_in_lowercase() {
        assert_eq!(
            serde_json::to_string(&[MemoryEnforce::Poll, MemoryEnforce::Kernel]).unwrap(),
            r#"["poll","kernel"]"#
        );
    }

    #[test]
    fn it_refuses_a_spelling_it_does_not_define() {
        assert!(serde_json::from_str::<MemoryEnforce>(r#""cgroup""#).is_err());
    }
}
