//! Which dogs a change to a secret can reach: the ones whose `dogs.toml`
//! section names a value in the namespace that moved, or in the operator's
//! own store.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use shep_core::config::DogsConfig;

use super::config::dog_section;
use super::secrets::section_references;

/// Every dog whose section in `dogs_config` names `namespace`, with the
/// keys it names there.
///
/// `namespace` is a provider's name, or `None` for the operator's own store,
/// which an un-namespaced `{{secret:KEY}}` reads. Either way the keys come
/// back bare. A reference is read through [`section_references`], so a
/// string that [`super::resolve_section`] would not substitute names nothing.
#[must_use]
pub fn dogs_referencing(
    dogs_config: &Path,
    namespace: Option<&str>,
) -> BTreeMap<String, BTreeSet<String>> {
    let Ok(source) = std::fs::read_to_string(dogs_config) else {
        return BTreeMap::new();
    };
    let Ok(config) = DogsConfig::load(Some(&source)) else {
        return BTreeMap::new();
    };
    config
        .dog
        .keys()
        .filter_map(|dog| {
            let section = dog_section(dogs_config, dog).ok()?;
            let keys: BTreeSet<String> = section_references(&section)
                .into_iter()
                .filter_map(|reference| match (namespace, reference.split_once('/')) {
                    (None, None) => Some(reference),
                    (Some(wanted), Some((found, key))) if wanted == found => Some(key.to_owned()),
                    _ => None,
                })
                .collect();
            (!keys.is_empty()).then(|| (dog.clone(), keys))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(dogs_toml: &str, namespace: Option<&str>) -> BTreeMap<String, BTreeSet<String>> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        std::fs::write(&path, dogs_toml).unwrap();
        dogs_referencing(&path, namespace)
    }

    fn keys(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn each_dog_comes_back_with_the_keys_it_names_in_that_namespace() {
        let found = named(
            "[bark]\nurl = \"{{secret:vercel/URL}}\"\ntoken = \"{{secret:vercel/TOKEN}} {{secret:other/X}}\"\n\
             [metrics.auth]\nkey = \"{{secret:vercel/TOKEN}}\"\n\
             [quiet]\npoll = \"5s\"\n\
             [own]\nk = \"{{secret:TOKEN}}\"\n",
            Some("vercel"),
        );
        assert_eq!(
            found,
            BTreeMap::from([
                ("bark".to_string(), keys(&["TOKEN", "URL"])),
                ("metrics".to_string(), keys(&["TOKEN"])),
            ])
        );
    }

    #[test]
    fn no_namespace_finds_the_dogs_naming_the_operators_own_store() {
        let found = named(
            "[bark]\nurl = \"{{secret:vercel/URL}}\"\ntoken = \"{{secret:TOKEN}} {{secret:OTHER}}\"\n\
             [metrics.auth]\nkey = \"{{secret:TOKEN}}\"\n",
            None,
        );
        assert_eq!(
            found,
            BTreeMap::from([
                ("bark".to_string(), keys(&["OTHER", "TOKEN"])),
                ("metrics".to_string(), keys(&["TOKEN"])),
            ])
        );
    }

    #[test]
    fn a_namespace_nobody_names_finds_nobody() {
        assert!(named("[bark]\nt = \"{{secret:vercel/T}}\"\n", Some("other")).is_empty());
    }

    #[test]
    fn a_missing_or_invalid_file_names_nobody() {
        let dir = tempfile::tempdir().unwrap();
        assert!(dogs_referencing(&dir.path().join("dogs.toml"), Some("vercel")).is_empty());
        assert!(named("[bark\nt = ", Some("vercel")).is_empty());
    }
}
