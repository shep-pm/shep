//! Which requests would hand a shepherd a `sandbox` file to run.

use super::Request;

impl Request {
    /// Whether this request asks the shepherd to run a sheep under a
    /// `sandbox`.
    ///
    /// A shepherd below [`SANDBOX_SINCE`](crate::protocol::SANDBOX_SINCE)
    /// decodes the key as unknown and starts the sheep bare, so a client
    /// checks this before it sends to one.
    #[must_use]
    pub fn asks_for_sandbox(&self) -> bool {
        match self {
            Self::Start { apps } | Self::Add { apps } | Self::ConfigDrift { apps } => {
                apps.iter().any(|app| app.sandbox.is_some())
            }
            Self::ApplyConfig { apps, .. } => apps.iter().any(|app| app.config.sandbox.is_some()),
            Self::SetSheepField { key, value, .. } => key == "sandbox" && !value.is_null(),
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

    fn declared(config: AppConfig) -> DeclaredApp {
        DeclaredApp {
            config,
            declared: Default::default(),
            declared_env: Default::default(),
        }
    }

    #[test]
    fn a_load_of_a_sandboxed_app_asks_for_one() {
        assert!(
            Request::Start {
                apps: vec![sandboxed()]
            }
            .asks_for_sandbox()
        );
        assert!(
            Request::Add {
                apps: vec![sandboxed()]
            }
            .asks_for_sandbox()
        );
        assert!(
            Request::ConfigDrift {
                apps: vec![sandboxed()]
            }
            .asks_for_sandbox()
        );
    }

    #[test]
    fn a_merge_of_a_sandboxed_app_asks_for_one() {
        let request = Request::ApplyConfig {
            apps: vec![declared(sandboxed())],
            reset: Default::default(),
        };
        assert!(request.asks_for_sandbox());
    }

    #[test]
    fn setting_the_sandbox_key_asks_for_one_and_clearing_it_does_not() {
        let set = |value| Request::SetSheepField {
            name: "web".to_string(),
            key: "sandbox".to_string(),
            value,
        };
        assert!(set(json!("sandbox.json")).asks_for_sandbox());
        assert!(!set(json!(null)).asks_for_sandbox());
    }

    #[test]
    fn a_request_with_no_sandbox_asks_for_none() {
        let plain = AppConfig::minimal("web", "./srv");
        assert!(!Request::Start { apps: vec![plain] }.asks_for_sandbox());
        assert!(!Request::ListFlock.asks_for_sandbox());
        let other_key = Request::SetSheepField {
            name: "web".to_string(),
            key: "cwd".to_string(),
            value: json!("sandbox.json"),
        };
        assert!(!other_key.asks_for_sandbox());
    }
}
