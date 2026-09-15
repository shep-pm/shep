use super::store::is_name;
use crate::config::AppConfig;
use crate::config::template;
use core::fmt;
use std::collections::BTreeSet;

/// One `{{secret:...}}` reference: a key, and the namespace it came from.
///
/// A bare reference reads the operator's own store; a namespaced one reads
/// what the provider dog of that name pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretRef<'a> {
    /// The provider dog's name, or `None` for the operator's own store.
    pub namespace: Option<&'a str>,
    /// The key within that store.
    pub key: &'a str,
}

impl<'a> SecretRef<'a> {
    /// Parses the body of a `{{secret:...}}` token, braces and prefix
    /// already stripped.
    ///
    /// Returns `None` for anything outside the grammar, which is how a
    /// config refuses a bad reference before a sheep ever starts.
    #[must_use]
    pub fn parse(body: &'a str) -> Option<Self> {
        match body.split_once('/') {
            None if is_name(body) => Some(Self {
                namespace: None,
                key: body,
            }),
            Some((namespace, key)) if is_name(namespace) && is_name(key) => Some(Self {
                namespace: Some(namespace),
                key,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for SecretRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{{secret:")?;
        if let Some(namespace) = self.namespace {
            f.write_str(namespace)?;
            f.write_str("/")?;
        }
        f.write_str(self.key)?;
        f.write_str("}}")
    }
}

/// Every `{{secret:...}}` reference `config` names, exactly as the operator
/// wrote it (`KEY` or `namespace/KEY`, no braces), deduplicated.
///
/// Walks `env`'s values, `args`, `out_file` and `err_file` through
/// `template`'s own tokenizer, the same one [`template::render`] resolves
/// against at spawn: a value this misses is one `render` would not touch
/// either, and a positional token (`{{instance}}`, `{{name}}`) contributes
/// nothing.
#[must_use]
pub fn references(config: &AppConfig) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut scan = |value: &str| {
        secret_references_in(value, |reference| {
            found.insert(match reference.namespace {
                Some(namespace) => format!("{namespace}/{}", reference.key),
                None => reference.key.to_string(),
            });
        });
    };
    for value in config.env.values() {
        scan(value);
    }
    for value in &config.args {
        scan(value);
    }
    if let Some(value) = &config.out_file {
        scan(value);
    }
    if let Some(value) = &config.err_file {
        scan(value);
    }
    found
}

/// Walks `value` for every `{{secret:...}}` reference through `template`'s
/// own tokenizer, calling `on_reference` for each one found.
///
/// The one tokenizer pass [`references`] and [`sealed_keys`] both build on,
/// so a value either function accepts as naming a secret is one the other
/// agrees with, rather than two spellings of the same check drifting apart.
pub(super) fn secret_references_in(value: &str, mut on_reference: impl FnMut(SecretRef<'_>)) {
    let _ = template::walk::<core::convert::Infallible>(value, |segment| {
        if let template::Segment::Token(token) = segment
            && let Some(reference) = template::secret_reference(token)
        {
            on_reference(reference);
        }
        Ok(())
    });
}

/// The env keys whose value names at least one `{{secret:...}}` reference,
/// in `env`'s own order.
///
/// The keys, never the references: [`references`] answers the other
/// direction, and a pane that wants to mark a row as sealed needs this one.
/// A value that merely embeds a reference counts, since the store is still
/// what fills it in.
///
/// `env` alone. A reference in `args` or `out_file` is real and
/// [`references`] reports it, but it is not an env row and nothing renders
/// it as one.
#[must_use]
pub fn sealed_keys(config: &AppConfig) -> Vec<String> {
    config
        .env
        .iter()
        .filter(|(_, value)| {
            let mut sealed = false;
            secret_references_in(value, |_| sealed = true);
            sealed
        })
        .map(|(key, _)| key.clone())
        .collect()
}

/// The provider namespaces [`references`] names, derived with
/// [`SecretRef::parse`] and kept to the namespaced half.
///
/// No I/O of its own: the seam boot-dependency ordering asks "which
/// provider namespaces does this sheep depend on" through.
#[must_use]
pub fn namespaces_of(config: &AppConfig) -> BTreeSet<String> {
    references(config)
        .iter()
        .filter_map(|reference| SecretRef::parse(reference))
        .filter_map(|reference| reference.namespace.map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {

    use std::collections::BTreeSet;

    use crate::config::AppConfig;

    use super::*;

    #[test]
    fn a_reference_parses_with_and_without_a_namespace() {
        let bare = SecretRef::parse("DB_PASSWORD").unwrap();
        assert_eq!(bare.namespace, None);
        assert_eq!(bare.key, "DB_PASSWORD");

        let scoped = SecretRef::parse("vercel/DB_PASSWORD").unwrap();
        assert_eq!(scoped.namespace, Some("vercel"));
        assert_eq!(scoped.key, "DB_PASSWORD");

        for bad in ["", "/KEY", "ns/", "a/b/c", "ns/bad key", "bad ns/KEY"] {
            assert!(SecretRef::parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn references_finds_every_secret_in_a_config_and_nothing_else() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("A".into(), "{{secret:ONE}}".into());
        config.env.insert("B".into(), "plain".into());
        config
            .env
            .insert("C".into(), "{{name}}-{{secret:vercel/TWO}}".into());
        config.args = vec!["--x={{secret:ONE}}".into()];
        // Both log paths, and for opposite reasons. `err_file` carries a
        // reference nothing else in this config names, so dropping the
        // log-path scan fails here rather than passing quietly; `out_file`
        // carries positional tokens only, so a scan that ran would still
        // return nothing for it.
        config.err_file = Some("{{secret:THREE}}.log".into());
        config.out_file = Some("{{SHEP_HOME}}/logs/{{instance}}.log".into());

        let found = references(&config);
        assert_eq!(
            found,
            BTreeSet::from([
                "ONE".to_string(),
                "THREE".to_string(),
                "vercel/TWO".to_string(),
            ]),
            "every field scanned, deduplicated, and no positional tokens"
        );
    }

    /// The key, not the reference. `references` answers the other direction,
    /// and a pane marking a row needs this one.
    #[test]
    fn a_sealed_key_is_reported_by_its_own_name() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("A".into(), "{{secret:ONE}}".into());
        config.env.insert("B".into(), "literal".into());
        assert_eq!(sealed_keys(&config), vec!["A".to_string()]);
    }

    /// Only `env`. `args` and `out_file` can name a reference too, and
    /// `references` reports those; this function is about env rows.
    #[test]
    fn a_reference_outside_env_seals_no_key() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.args = vec!["--token={{secret:ONE}}".into()];
        assert!(sealed_keys(&config).is_empty());
    }

    #[test]
    fn namespaces_of_a_config_is_the_seam_boot_ordering_will_want() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("A".into(), "{{secret:ONE}}".into());
        config
            .env
            .insert("B".into(), "{{secret:vercel/TWO}}".into());
        assert_eq!(
            namespaces_of(&config),
            BTreeSet::from(["vercel".to_string()])
        );
    }

    #[test]
    fn a_reference_displays_the_way_an_operator_wrote_it() {
        assert_eq!(
            SecretRef {
                namespace: None,
                key: "K"
            }
            .to_string(),
            "{{secret:K}}"
        );
        assert_eq!(
            SecretRef {
                namespace: Some("vercel"),
                key: "K"
            }
            .to_string(),
            "{{secret:vercel/K}}"
        );
    }
}
