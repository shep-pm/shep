//! Which requests ask a shepherd to sandbox a sheep, by the `dogs.sandbox` table or the old key.

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::config::{AppConfig, DeclaredApp, DogTable};
    use crate::protocol::{GatedKey, Request, SANDBOX_DOG_SINCE};

    const DOG: GatedKey = GatedKey {
        key: "dogs.sandbox",
        since: SANDBOX_DOG_SINCE,
    };
    const KEY: GatedKey = GatedKey {
        key: "sandbox",
        since: SANDBOX_DOG_SINCE,
    };

    fn table() -> DogTable {
        serde_json::from_value(json!({"profile": "strict"})).unwrap()
    }

    fn with_table(dog: &str) -> AppConfig {
        let mut app = AppConfig::minimal("web", "./srv");
        app.dogs.insert(dog.to_string(), table());
        app
    }

    fn declared(config: AppConfig) -> DeclaredApp {
        DeclaredApp {
            config,
            declared: Default::default(),
            declared_env: Default::default(),
        }
    }

    #[test]
    fn a_load_of_an_app_with_a_sandbox_dog_table_asks_for_one() {
        let app = with_table("sandbox");
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
            assert_eq!(request.gated_key_dropped_at(0), Some(DOG));
        }
    }

    #[test]
    fn setting_the_sandbox_dog_table_asks_for_one_and_removing_it_does_not() {
        let set = |table| Request::SetSheepDogSettings {
            name: "web".to_string(),
            dog: "sandbox".to_string(),
            table,
        };
        assert_eq!(set(Some(table())).gated_key_dropped_at(0), Some(DOG));
        assert_eq!(set(None).gated_key_dropped_at(0), None);
    }

    #[test]
    fn a_table_for_another_dog_asks_for_none() {
        let app = with_table("metrics");
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
    fn the_old_sandbox_key_still_asks_for_one() {
        let old = AppConfig {
            sandbox: Some("sandbox.json".to_string()),
            ..AppConfig::minimal("web", "./srv")
        };
        let load = Request::Start {
            apps: vec![old.clone()],
        };
        assert_eq!(load.gated_key_dropped_at(SANDBOX_DOG_SINCE - 1), Some(KEY));
        assert_eq!(load.gated_key_dropped_at(SANDBOX_DOG_SINCE), None);
        let merge = Request::ApplyConfig {
            apps: vec![declared(old)],
            reset: Default::default(),
        };
        assert_eq!(merge.gated_key_dropped_at(0), Some(KEY));
        let set = |value| Request::SetSheepField {
            name: "web".to_string(),
            key: "sandbox".to_string(),
            value,
        };
        assert_eq!(
            set(json!("sandbox.json")).gated_key_dropped_at(0),
            Some(KEY)
        );
        assert_eq!(set(json!(null)).gated_key_dropped_at(0), None);
    }
}
