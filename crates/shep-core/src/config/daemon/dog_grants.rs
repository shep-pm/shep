use serde::{Deserialize, Deserializer};

use crate::permissions::Grants;

/// One dog's entry under `[daemon.dog_grants]`: what the operator granted it.
///
/// A dog with no entry is undeclared and keeps full access. An entry with an
/// empty `granted` is a dog granted nothing. Here rather than in `dogs.toml`
/// for the reason [`super::DaemonSection::adopted_dogs`] gives: that file's
/// `[<name>]` table is the dog's own, and a dog must never be able to
/// grant itself anything.
///
/// Only `granted` is judged by the shepherd. `waiting`, `revoked` and
/// `reviewed` are the CLI's record of the upgrade path, read here so the
/// strict table accepts them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DogGrantsSection {
    /// The granted permission names, such as `flock.read` or
    /// `events:process.`.
    ///
    /// Read strictly: a name this build cannot read is a startup error
    /// naming it, so a typo is caught where it is made rather than silently
    /// granting nothing.
    #[serde(deserialize_with = "strict_grants")]
    #[cfg_attr(feature = "schema", schemars(with = "Vec<String>"))]
    pub granted: Grants,
    /// Permissions a newer binary declared that the operator has not
    /// approved. The dog runs on `granted` alone until they are.
    #[serde(default, deserialize_with = "strict_grants")]
    #[cfg_attr(feature = "schema", schemars(with = "Vec<String>"))]
    pub waiting: Grants,
    /// Permissions the operator took away, so a binary that still declares
    /// them is not read as asking for something new.
    #[serde(default, deserialize_with = "strict_grants")]
    #[cfg_attr(feature = "schema", schemars(with = "Vec<String>"))]
    pub revoked: Grants,
    /// `false` for a list written at upgrade that the operator has not yet
    /// looked at. Absent means reviewed.
    #[serde(default = "reviewed_by_default")]
    pub reviewed: bool,
}

const fn reviewed_by_default() -> bool {
    true
}

fn strict_grants<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Grants, D::Error> {
    let names = Vec::<String>::deserialize(deserializer)?;
    Grants::parse(names).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::super::DaemonConfig;
    use super::super::testing::no_env;
    use crate::permissions::{Grants, Permission};

    #[test]
    fn grants_written_by_hand_are_read_by_dog_name() {
        let src = r#"
[daemon.dog_grants.otel]
granted = ["flock.read", "events:process.", "secrets.write:vault"]
"#;
        let cfg = DaemonConfig::load(Some(src), &no_env).unwrap();
        let otel = cfg.daemon.dog_grants.get("otel").expect("otel has grants");
        assert_eq!(
            otel.granted,
            Grants::new([
                Permission::FlockRead,
                Permission::Events("process.".to_string()),
                Permission::SecretsWrite("vault".to_string()),
            ])
        );
    }

    #[test]
    fn a_file_without_the_table_declares_no_grants() {
        let cfg = DaemonConfig::load(Some("[daemon]\nlog_json = true\n"), &no_env).unwrap();
        assert!(cfg.daemon.dog_grants.is_empty());
    }

    #[test]
    fn an_empty_list_is_a_dog_granted_nothing() {
        let cfg =
            DaemonConfig::load(Some("[daemon.dog_grants.otel]\ngranted = []\n"), &no_env).unwrap();
        assert_eq!(
            cfg.daemon.dog_grants.get("otel").map(|dog| &dog.granted),
            Some(&Grants::default())
        );
    }

    #[test]
    fn a_misspelt_permission_is_a_startup_error_naming_it() {
        let src = "[daemon.dog_grants.otel]\ngranted = [\"flock.reed\"]\n";
        let err = DaemonConfig::load(Some(src), &no_env)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("`flock.reed` is not a permission shep knows"),
            "{err}"
        );
    }

    #[test]
    fn a_key_other_than_granted_is_refused() {
        let src = "[daemon.dog_grants.otel]\ngrant = [\"flock.read\"]\n";
        assert!(DaemonConfig::load(Some(src), &no_env).is_err());
    }
}
