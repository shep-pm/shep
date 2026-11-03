//! Which requests would hand a shepherd an `AppConfig` key it would drop.

use serde_json::Value;

use super::Request;
use crate::config::AppConfig;
use crate::protocol::{LISTEN_SINCE, MEMORY_ENFORCE_SINCE, SANDBOX_DOG_SINCE};

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
    /// The `SetSheepField` key that sets it, if one does.
    field: Option<&'static str>,
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
/// The old `sandbox` key is not here: a shepherd refuses it, and a client no
/// longer serializes it.
const GATES: [Gate; 3] = [
    Gate {
        key: GatedKey {
            key: "dogs.sandbox",
            since: SANDBOX_DOG_SINCE,
        },
        field: None,
        dog: Some("sandbox"),
        in_app: |app| app.dogs.contains_key("sandbox"),
        sets: |_| false,
    },
    Gate {
        key: GatedKey {
            key: "listen",
            since: LISTEN_SINCE,
        },
        field: Some("listen"),
        dog: None,
        in_app: |app| !app.listen.is_empty(),
        sets: |value| !value.is_null() && value.as_object().is_none_or(|table| !table.is_empty()),
    },
    Gate {
        key: GatedKey {
            key: "memory_enforce",
            since: MEMORY_ENFORCE_SINCE,
        },
        field: Some("memory_enforce"),
        dog: None,
        in_app: |app| app.memory_enforce.is_kernel(),
        sets: |value| value == "kernel",
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
            Self::SetSheepField { key, value, .. } => {
                gate.field == Some(key.as_str()) && (gate.sets)(value)
            }
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

    fn listening() -> AppConfig {
        let mut app = AppConfig::minimal("web", "./srv");
        app.listen
            .insert("http".to_string(), "0.0.0.0:8080".to_string());
        app
    }

    fn kernel_enforced() -> AppConfig {
        let mut app = AppConfig::minimal("web", "./srv");
        app.max_memory = Some("512M".parse().unwrap());
        app.memory_enforce = crate::config::MemoryEnforce::Kernel;
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

    const MEMORY_ENFORCE: GatedKey = GatedKey {
        key: "memory_enforce",
        since: MEMORY_ENFORCE_SINCE,
    };

    #[test]
    fn a_load_of_a_gated_app_names_its_key() {
        for (app, key) in [
            (sandboxed(), SANDBOX),
            (listening(), LISTEN),
            (kernel_enforced(), MEMORY_ENFORCE),
        ] {
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
    fn a_merge_of_a_gated_app_names_its_key() {
        for (app, key) in [
            (sandboxed(), SANDBOX),
            (listening(), LISTEN),
            (kernel_enforced(), MEMORY_ENFORCE),
        ] {
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
    fn setting_memory_enforce_to_kernel_names_it_and_setting_poll_does_not() {
        let set = |value| Request::SetSheepField {
            name: "web".to_string(),
            key: "memory_enforce".to_string(),
            value,
        };
        assert_eq!(
            set(json!("kernel")).gated_key_dropped_at(0),
            Some(MEMORY_ENFORCE)
        );
        assert_eq!(set(json!("poll")).gated_key_dropped_at(0), None);
    }

    #[test]
    fn a_poll_app_names_no_memory_enforce_key() {
        let request = Request::Start {
            apps: vec![AppConfig::minimal("web", "./srv")],
        };
        assert_eq!(request.gated_key_dropped_at(MEMORY_ENFORCE_SINCE - 1), None);
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
