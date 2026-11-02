//! Which requests would hand a shepherd an `AppConfig` key it would drop.

use serde_json::Value;

use super::Request;
use crate::config::AppConfig;
use crate::protocol::{LISTEN_SINCE, SANDBOX_SINCE};

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
    /// Whether an app carries the key with a value.
    in_app: fn(&AppConfig) -> bool,
    /// Whether `SetSheepField` sets the key to this value, as opposed to
    /// clearing it.
    sets: fn(&Value) -> bool,
}

/// Every key a client refuses to send to a shepherd that predates it.
const GATES: [Gate; 2] = [
    Gate {
        key: GatedKey {
            key: "sandbox",
            since: SANDBOX_SINCE,
        },
        in_app: |app| app.sandbox.is_some(),
        sets: |value| !value.is_null(),
    },
    Gate {
        key: GatedKey {
            key: "listen",
            since: LISTEN_SINCE,
        },
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
            Self::SetSheepField { key, value, .. } => key == gate.key.key && (gate.sets)(value),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, DeclaredApp};
    use serde_json::json;

    fn sandboxed() -> AppConfig {
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
        key: "sandbox",
        since: SANDBOX_SINCE,
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
    fn setting_a_gated_key_names_it_and_clearing_it_does_not() {
        let set = |key: &str, value| Request::SetSheepField {
            name: "web".to_string(),
            key: key.to_string(),
            value,
        };
        assert_eq!(
            set("sandbox", json!("sandbox.json")).gated_key_dropped_at(0),
            Some(SANDBOX)
        );
        assert_eq!(set("sandbox", json!(null)).gated_key_dropped_at(0), None);
        assert_eq!(
            set("listen", json!({"http": "0.0.0.0:8080"})).gated_key_dropped_at(0),
            Some(LISTEN)
        );
        assert_eq!(set("listen", json!({})).gated_key_dropped_at(0), None);
        assert_eq!(set("listen", json!(null)).gated_key_dropped_at(0), None);
    }

    #[test]
    fn a_shepherd_at_or_above_a_keys_version_drops_nothing() {
        let request = Request::Start {
            apps: vec![sandboxed(), listening()],
        };
        assert_eq!(request.gated_key_dropped_at(LISTEN_SINCE), None);
        // Below `listen` but at `sandbox`'s version, only `listen` is lost.
        assert_eq!(request.gated_key_dropped_at(SANDBOX_SINCE), Some(LISTEN));
        assert_eq!(
            request.gated_key_dropped_at(SANDBOX_SINCE - 1),
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
