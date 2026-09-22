//! Fixtures and helpers shared by this module's tests.

use super::format::ALL_ENVIRONMENTS;
use super::provider::ProviderCache;
use std::collections::{BTreeMap, BTreeSet};

/// A cache holding `vercel/PRESENT` for `production`, pushed as that
/// one pair.
pub(super) fn vercel_production() -> ProviderCache {
    ProviderCache::new(
        BTreeMap::from([(
            "vercel".to_string(),
            BTreeMap::from([(
                "PRESENT".to_string(),
                BTreeMap::from([("production".to_string(), "v".to_string())]),
            )]),
        )]),
        BTreeMap::from([(
            "vercel".to_string(),
            BTreeSet::from(["production".to_string()]),
        )]),
    )
}

/// A cache holding `vercel/PRESENT` for [`ALL_ENVIRONMENTS`], pushed as
/// that one pair and no other.
pub(super) fn vercel_all() -> ProviderCache {
    ProviderCache::new(
        BTreeMap::from([(
            "vercel".to_string(),
            BTreeMap::from([(
                "PRESENT".to_string(),
                BTreeMap::from([(ALL_ENVIRONMENTS.to_string(), "v".to_string())]),
            )]),
        )]),
        BTreeMap::from([(
            "vercel".to_string(),
            BTreeSet::from([ALL_ENVIRONMENTS.to_string()]),
        )]),
    )
}
