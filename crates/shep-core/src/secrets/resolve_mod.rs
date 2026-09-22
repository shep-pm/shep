use super::format::ALL_ENVIRONMENTS;
use super::provider::ProviderCache;
use super::refs::SecretRef;
use core::fmt;
use std::collections::BTreeMap;

/// What one environment can see: the operator's store plus every provider
/// dog's, resolved against a single environment name.
///
/// Built once per resolution pass and read many times, so the maps are owned
/// rather than borrowed.
///
/// Debug does not leak a value: it prints the environment and two counts.
pub struct SecretView {
    environment: String,
    store: BTreeMap<String, BTreeMap<String, String>>,
    providers: ProviderCache,
}

/// Redacted (IR-41): `store` and the provider cache hold secret values.
impl fmt::Debug for SecretView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretView")
            .field("environment", &self.environment)
            .field("keys", &self.store.len())
            .field("namespaces", &self.providers.namespace_count())
            .finish()
    }
}

/// The outcome of resolving one [`SecretRef`].
///
/// [`Self::MissingKey`] and [`Self::MissingNamespace`] are kept apart
/// because they need different remedies: a pair no dog has pushed is a dog
/// that has not reported yet, which a retry fixes, while a missing key is a
/// person's to set.
///
/// Debug does not leak a value: [`Self::Found`] prints as `Found(..)`.
pub enum Resolution<'a> {
    /// The resolved value.
    Found(&'a str),
    /// The operator's store holds nothing for this key, or a provider has
    /// pushed this namespace for this environment and that push lacks the
    /// key.
    MissingKey,
    /// No provider dog has pushed this namespace for this environment yet.
    MissingNamespace,
}

/// Redacted (IR-41): [`Resolution::Found`] carries a secret's value.
impl fmt::Debug for Resolution<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Found(_) => "Found(..)",
            Self::MissingKey => "MissingKey",
            Self::MissingNamespace => "MissingNamespace",
        })
    }
}

impl SecretView {
    /// A view over `store` and `providers`, resolved for `environment`.
    #[must_use]
    pub fn new(
        environment: String,
        store: BTreeMap<String, BTreeMap<String, String>>,
        providers: ProviderCache,
    ) -> Self {
        Self {
            environment,
            store,
            providers,
        }
    }

    /// A view holding nothing, so a bare reference resolves to
    /// [`Resolution::MissingKey`] and a namespaced one to
    /// [`Resolution::MissingNamespace`].
    #[must_use]
    pub fn empty(environment: String) -> Self {
        Self::new(environment, BTreeMap::new(), ProviderCache::default())
    }

    /// The environment this view resolves against.
    #[must_use]
    pub fn environment(&self) -> &str {
        &self.environment
    }

    /// The value `reference` resolves to in this view's environment.
    ///
    /// Exact environment, then [`ALL_ENVIRONMENTS`], then nothing. There is
    /// deliberately no fallback to another named environment: filling an
    /// empty `staging` slot from `production` would hand a live credential
    /// to staging the first time somebody forgot to set one.
    ///
    /// A miss on a namespaced reference is [`Resolution::MissingNamespace`]
    /// unless a provider has pushed this view's own environment for that
    /// namespace: a push carries one `(namespace, environment)` pair, so a
    /// dog part way through `production` then `staging` has said nothing
    /// about staging yet, and calling that a missing key would `Errored` a
    /// staging sheep permanently for a value arriving a second later.
    #[must_use]
    pub fn resolve(&self, reference: &SecretRef<'_>) -> Resolution<'_> {
        let table = match reference.namespace {
            None => Some(&self.store),
            Some(namespace) => self.providers.namespace(namespace),
        };
        if let Some(value) =
            table
                .and_then(|table| table.get(reference.key))
                .and_then(|by_environment| {
                    by_environment
                        .get(&self.environment)
                        .or_else(|| by_environment.get(ALL_ENVIRONMENTS))
                })
        {
            return Resolution::Found(value.as_str());
        }
        match reference.namespace {
            None => Resolution::MissingKey,
            Some(namespace) if self.is_pushed(namespace) => Resolution::MissingKey,
            Some(_) => Resolution::MissingNamespace,
        }
    }

    /// Whether a provider has pushed `namespace` for this view's own
    /// environment or for [`ALL_ENVIRONMENTS`].
    ///
    /// A push to [`ALL_ENVIRONMENTS`] populates the namespace for every
    /// environment, [`resolve`](Self::resolve)'s value lookup included, so
    /// the pair check has to agree: a namespace pushed only under `all`
    /// counts as pushed here too, or a key genuinely absent from that push
    /// would read as the transient `MissingNamespace` instead of the
    /// permanent `MissingKey` it actually is.
    fn is_pushed(&self, namespace: &str) -> bool {
        self.providers
            .pushed
            .get(namespace)
            .is_some_and(|environments| {
                environments.contains(&self.environment) || environments.contains(ALL_ENVIRONMENTS)
            })
    }
}

#[cfg(test)]
mod tests {

    use super::super::provider::ProviderCache;
    use super::super::refs::SecretRef;

    use std::collections::{BTreeMap, BTreeSet};

    use super::super::testing::*;
    use super::*;

    #[test]
    fn resolution_prefers_the_exact_environment_then_all_then_gives_up() {
        let mut store = BTreeMap::new();
        store.insert(
            "K".to_string(),
            BTreeMap::from([
                ("production".to_string(), "prod".to_string()),
                ("all".to_string(), "fallback".to_string()),
            ]),
        );
        store.insert(
            "ONLY_ALL".to_string(),
            BTreeMap::from([("all".to_string(), "everywhere".to_string())]),
        );
        store.insert(
            "ONLY_PROD".to_string(),
            BTreeMap::from([("production".to_string(), "prod".to_string())]),
        );

        let view = SecretView::new("staging".to_string(), store, ProviderCache::default());
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: None,
                key: "K"
            }),
            Resolution::Found("fallback")
        ));
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: None,
                key: "ONLY_ALL"
            }),
            Resolution::Found("everywhere")
        ));
        // The whole point: staging never falls back to production's value.
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: None,
                key: "ONLY_PROD"
            }),
            Resolution::MissingKey
        ));
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: None,
                key: "ABSENT"
            }),
            Resolution::MissingKey
        ));
    }

    #[test]
    fn an_unpopulated_namespace_is_told_apart_from_a_missing_key() {
        let view = SecretView::new(
            "production".to_string(),
            BTreeMap::new(),
            vercel_production(),
        );

        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "PRESENT"
            }),
            Resolution::Found("v")
        ));
        // The dog is up and simply does not have this key: a person's problem.
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "ABSENT"
            }),
            Resolution::MissingKey
        ));
        // No dog has ever pushed under this name: transient, retry.
        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vault"),
                key: "ANY"
            }),
            Resolution::MissingNamespace
        ));
    }

    /// The case this pair set exists for. A provider pushes `production`
    /// and then `staging`, which is the ordinary shape, and a staging sheep
    /// spawning between the two must wait on the restart ladder rather than
    /// `Errored` for good. Keying on the namespace alone calls the second
    /// push's keys permanently missing the moment the first push lands.
    #[test]
    fn a_namespace_pushed_for_another_environment_is_not_populated_for_this_one() {
        let view = SecretView::new("staging".to_string(), BTreeMap::new(), vercel_production());

        assert!(
            matches!(
                view.resolve(&SecretRef {
                    namespace: Some("vercel"),
                    key: "PRESENT"
                }),
                Resolution::MissingNamespace
            ),
            "staging has had no push, so waiting is what fixes this"
        );
    }

    /// The other half, and the one that must stay permanent: the pair has
    /// been pushed and the key is not in it, so the provider genuinely does
    /// not have it and no amount of waiting will produce one.
    #[test]
    fn a_pushed_pair_missing_a_key_stays_permanent() {
        let view = SecretView::new(
            "production".to_string(),
            BTreeMap::new(),
            vercel_production(),
        );

        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "ABSENT"
            }),
            Resolution::MissingKey
        ));
    }

    /// A push to [`ALL_ENVIRONMENTS`] populates the namespace for every
    /// environment, not only the literal string `"all"`, so a key that push
    /// genuinely lacks is permanent for a `staging` view exactly as it
    /// would be for a `production` one.
    #[test]
    fn an_all_slot_push_makes_a_genuinely_missing_key_permanent() {
        let view = SecretView::new("staging".to_string(), BTreeMap::new(), vercel_all());

        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "ABSENT"
            }),
            Resolution::MissingKey
        ));
    }

    /// The value half of the same all-only push, from a pair-aware view:
    /// the namespace resolves the key it does carry for an environment that
    /// never received its own push.
    #[test]
    fn an_all_slot_push_resolves_its_key_for_every_environment() {
        let view = SecretView::new("staging".to_string(), BTreeMap::new(), vercel_all());

        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "PRESENT"
            }),
            Resolution::Found("v")
        ));
    }

    /// An empty push is a dog saying it holds nothing for that pair, which
    /// is an answer. Deriving the pairs from the values would lose it and
    /// leave such a sheep retrying against a dog that has already spoken.
    #[test]
    fn an_empty_push_populates_the_pair_it_carried() {
        let view = SecretView::new(
            "production".to_string(),
            BTreeMap::new(),
            ProviderCache::new(
                BTreeMap::from([("vercel".to_string(), BTreeMap::new())]),
                BTreeMap::from([(
                    "vercel".to_string(),
                    BTreeSet::from(["production".to_string()]),
                )]),
            ),
        );

        assert!(matches!(
            view.resolve(&SecretRef {
                namespace: Some("vercel"),
                key: "ANY"
            }),
            Resolution::MissingKey
        ));
    }

    /// IR-41. Fails the moment somebody replaces the hand-written impl with
    /// a derive, which is the only way this leak comes back.
    #[test]
    fn debug_never_prints_a_value() {
        let store = BTreeMap::from([(
            "K".to_string(),
            BTreeMap::from([("production".to_string(), "hunter2".to_string())]),
        )]);
        let view = SecretView::new("production".to_string(), store, ProviderCache::default());
        let rendered = format!("{view:?}");
        assert_eq!(
            rendered,
            "SecretView { environment: \"production\", keys: 1, namespaces: 0 }"
        );
        assert!(!rendered.contains("hunter2"));
    }

    /// IR-41, the same guard for the type a resolved value travels in.
    #[test]
    fn a_resolution_debug_never_prints_the_value_it_found() {
        assert_eq!(format!("{:?}", Resolution::Found("hunter2")), "Found(..)");
        assert_eq!(format!("{:?}", Resolution::MissingKey), "MissingKey");
        assert_eq!(
            format!("{:?}", Resolution::MissingNamespace),
            "MissingNamespace"
        );
    }
}
