//! Which requests would hand a shepherd an `AppConfig` key it would drop.

use serde_json::Value;

use super::Request;
use crate::config::AppConfig;
use crate::protocol::{LISTEN_SINCE, SANDBOX_DOG_SINCE};

/// An `AppConfig` key that a shepherd below [`GatedKey::since`] ignores,
/// running the sheep without what the key asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatedKey {
    /// The Flockfile key.
    pub key: &'static str,
    /// The first protocol version whose shepherd acts on it.
    pub since: u32,
}

/// One gated key and how to tell a request sets it.
struct Gate {
    key: GatedKey,
    /// The `SetSheepField` key that sets it.
    field: &'static str,
    /// The dog whose table `SetSheepDogSettings` sets, if a table gates it.
    dog: Option<&'static str>,
    /// Whether an app carries the key with a value.
    in_app: fn(&AppConfig) -> bool,
    /// Whether `SetSheepField` sets the key to this value, as opposed to
    /// clearing it.
    sets: fn(&Value) -> bool,
}

/// Every key a client refuses to send to a shepherd that predates it.
///
/// The old `sandbox` key gates with the `dogs.sandbox` table: a shepherd that
/// does not spawn through the dog starts either unsandboxed.
const GATES: [Gate; 2] = [
    Gate {
        key: GatedKey {
            key: "dogs.sandbox",
            since: SANDBOX_DOG_SINCE,
        },
        field: "sandbox",
        dog: Some("sandbox"),
        in_app: |app| app.sandbox.is_some() || app.dogs.contains_key("sandbox"),
        sets: |value| !value.is_null(),
    },
    Gate {
        key: GatedKey {
            key: "listen",
            since: LISTEN_SINCE,
        },
        field: "listen",
        dog: None,
        in_app: |app| !app.listen.is_empty(),
        sets: |value| !value.is_null() && value.as_object().is_none_or(|table| !table.is_empty()),
    },
];

impl Request {
    /// The first gated key this request carries that a shepherd speaking
    /// `protocol` would drop, or `None` if it would honour all of them.
    ///
    /// A shepherd below a key's version decodes the key as unknown and starts
    /// the sheep without it, so a client checks this before it sends to one.
    #[must_use]
    pub fn gated_key_dropped_at(&self, protocol: u32) -> Option<GatedKey> {
        GATES
            .iter()
            .filter(|gate| protocol < gate.key.since)
            .find(|gate| self.carries(gate))
            .map(|gate| gate.key)
    }

    fn carries(&self, gate: &Gate) -> bool {
        match self {
            Self::Start { apps } | Self::Add { apps } | Self::ConfigDrift { apps } => {
                apps.iter().any(gate.in_app)
            }
            Self::ApplyConfig { apps, .. } => apps.iter().any(|app| (gate.in_app)(&app.config)),
            Self::SetSheepField { key, value, .. } => key == gate.field && (gate.sets)(value),
            Self::SetSheepDogSettings { dog, table, .. } => {
                gate.dog == Some(dog.as_str()) && table.is_some()
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, DeclaredApp, DogTable};
    use serde_json::json;

    fn table() -> DogTable {
        serde_json::from_value(json!({"profile": "strict"})).unwrap()
    }

    fn sandboxed() -> AppConfig {
        let mut app = AppConfig::minimal("web", "./srv");
        app.dogs.insert("sandbox".to_string(), table());
        app
    }

    fn old_sandboxed() -> AppConfig {
        AppConfig {
            sandbox: Some("sandbox.json".to_string()),
            ..AppConfig::minimal("web", "./srv")
        }
    }

    fn listening() -> AppConfig {
        let mut app = AppConfig::minimal("web", "./srv");
        app.listen
            .insert("http".to_string(), "0.0.0.0:8080".to_string());
        app
    }

    fn declared(config: AppConfig) -> DeclaredApp {
        DeclaredApp {
            config,
            declared: Default::default(),
            declared_env: Default::default(),
        }
    }

    const SANDBOX: GatedKey = GatedKey {
        key: "dogs.sandbox",
        since: SANDBOX_DOG_SINCE,
    };
    const LISTEN: GatedKey = GatedKey {
        key: "listen",
        since: LISTEN_SINCE,
    };

    #[test]
    fn a_load_of_a_gated_app_names_its_key() {
        for (app, key) in [(sandboxed(), SANDBOX), (listening(), LISTEN)] {
            for request in [
                Request::Start {
                    apps: vec![app.clone()],
                },
                Request::Add {
                    apps: vec![app.clone()],
                },
                Request::ConfigDrift {
                    apps: vec![app.clone()],
                },
            ] {
                assert_eq!(request.gated_key_dropped_at(key.since - 1), Some(key));
            }
        }
    }

    #[test]
    fn a_load_of_an_app_with_a_sandbox_dog_table_asks_for_one() {
        let app = sandboxed();
        for request in [
            Request::Start {
                apps: vec![app.clone()],
            },
            Request::Add {
                apps: vec![app.clone()],
            },
            Request::ConfigDrift {
                apps: vec![app.clone()],
            },
            Request::ApplyConfig {
                apps: vec![declared(app)],
                reset: Default::default(),
            },
        ] {
            assert_eq!(request.gated_key_dropped_at(0), Some(SANDBOX));
        }
    }

    #[test]
    fn the_old_sandbox_key_still_asks_for_one() {
        let request = Request::Start {
            apps: vec![old_sandboxed()],
        };
        assert_eq!(
            request.gated_key_dropped_at(SANDBOX_DOG_SINCE - 1),
            Some(SANDBOX)
        );
        let merge = Request::ApplyConfig {
            apps: vec![declared(old_sandboxed())],
            reset: Default::default(),
        };
        assert_eq!(
            merge.gated_key_dropped_at(SANDBOX_DOG_SINCE - 1),
            Some(SANDBOX)
        );
        let set = Request::SetSheepField {
            name: "web".to_string(),
            key: "sandbox".to_string(),
            value: json!("sandbox.json"),
        };
        assert_eq!(set.gated_key_dropped_at(0), Some(SANDBOX));
        let cleared = Request::SetSheepField {
            name: "web".to_string(),
            key: "sandbox".to_string(),
            value: json!(null),
        };
        assert_eq!(cleared.gated_key_dropped_at(0), None);
    }

    #[test]
    fn setting_the_sandbox_dog_table_asks_for_one_and_removing_it_does_not() {
        let set = |table| Request::SetSheepDogSettings {
            name: "web".to_string(),
            dog: "sandbox".to_string(),
            table,
        };
        assert_eq!(set(Some(table())).gated_key_dropped_at(0), Some(SANDBOX));
        assert_eq!(set(None).gated_key_dropped_at(0), None);
    }

    #[test]
    fn a_table_for_another_dog_asks_for_none() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.dogs.insert("metrics".to_string(), table());
        assert_eq!(
            Request::Start { apps: vec![app] }.gated_key_dropped_at(0),
            None
        );
        let set = Request::SetSheepDogSettings {
            name: "web".to_string(),
            dog: "metrics".to_string(),
            table: Some(table()),
        };
        assert_eq!(set.gated_key_dropped_at(0), None);
    }

    #[test]
    fn a_merge_of_a_gated_app_names_its_key() {
        for (app, key) in [(sandboxed(), SANDBOX), (listening(), LISTEN)] {
            let request = Request::ApplyConfig {
                apps: vec![declared(app)],
                reset: Default::default(),
            };
            assert_eq!(request.gated_key_dropped_at(key.since - 1), Some(key));
        }
    }

    #[test]
    fn setting_a_listen_names_it_and_clearing_it_does_not() {
        let set = |value| Request::SetSheepField {
            name: "web".to_string(),
            key: "listen".to_string(),
            value,
        };
        assert_eq!(
            set(json!({"http": "0.0.0.0:8080"})).gated_key_dropped_at(0),
            Some(LISTEN)
        );
        assert_eq!(set(json!({})).gated_key_dropped_at(0), None);
        assert_eq!(set(json!(null)).gated_key_dropped_at(0), None);
    }

    #[test]
    fn a_shepherd_at_or_above_a_keys_version_drops_nothing() {
        let request = Request::Start {
            apps: vec![sandboxed(), listening()],
        };
        assert_eq!(request.gated_key_dropped_at(LISTEN_SINCE), None);
        assert_eq!(request.gated_key_dropped_at(SANDBOX_DOG_SINCE), None);
        assert_eq!(
            request.gated_key_dropped_at(SANDBOX_DOG_SINCE - 1),
            Some(SANDBOX)
        );
    }

    #[test]
    fn a_request_with_no_gated_key_names_none() {
        let plain = AppConfig::minimal("web", "./srv");
        assert_eq!(
            Request::Start { apps: vec![plain] }.gated_key_dropped_at(0),
            None
        );
        assert_eq!(Request::ListFlock.gated_key_dropped_at(0), None);
        let other_key = Request::SetSheepField {
            name: "web".to_string(),
            key: "cwd".to_string(),
            value: json!("sandbox.json"),
        };
        assert_eq!(other_key.gated_key_dropped_at(0), None);
    }
}
