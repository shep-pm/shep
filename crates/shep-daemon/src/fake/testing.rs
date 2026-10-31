//! Fixtures and helpers shared by this module's tests.

use crate::runner::SpawnSpec;

pub(super) fn spec() -> SpawnSpec {
    SpawnSpec {
        channel: true,
        ..Default::default()
    }
}
