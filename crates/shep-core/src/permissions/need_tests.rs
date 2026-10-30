//! Every [`Request`] against the table: what each group holds, pinned, so
//! moving a request between groups is a visible change.
//!
//! The exhaustive match in `need` already fails to compile on a request
//! nobody mapped. What it cannot say is which group a request is in.

use crate::protocol::{DogSectionToml, EnvValue, LogStream, Request, SelectorSpec};
use std::collections::BTreeMap;

use super::{Need, Permission, need};

/// The dog every request below is sent by.
const CALLER: &str = "kelpie";

fn sheep(name: &str) -> SelectorSpec {
    SelectorSpec::Name(name.to_string())
}

/// The one permission `request` needs of `CALLER`, and a failure for any
/// request that needs something else.
fn permission_of(request: &Request) -> Permission {
    match need(request, CALLER) {
        Need::Permission(permission) => permission,
        other => panic!("{request:?} needs a single permission, got {other:?}"),
    }
}

fn is_always(request: &Request) -> bool {
    matches!(need(request, CALLER), Need::Always)
}

#[test]
fn flock_read_holds_every_request_that_only_looks() {
    let reads = [
        Request::ListFlock,
        Request::HostUsage,
        Request::Describe {
            selector: SelectorSpec::All,
        },
        Request::ConfigDrift { apps: Vec::new() },
        Request::SheepConfig { name: "web".into() },
        Request::DogStaleness,
        Request::HandoverFitness,
        Request::DogSheepSettings {
            dog: "jobs".to_string(),
        },
    ];
    for request in &reads {
        assert_eq!(permission_of(request), Permission::FlockRead, "{request:?}");
    }
}

#[test]
fn flock_control_holds_every_request_that_moves_a_sheep() {
    let controls = [
        Request::Start { apps: Vec::new() },
        Request::Stop {
            selector: sheep("web"),
        },
        Request::Restart {
            selector: sheep("web"),
        },
        Request::Reset {
            selector: sheep("web"),
        },
        Request::Reload {
            selector: sheep("web"),
        },
        Request::Signal {
            selector: sheep("web"),
            signal: "HUP".into(),
            lamb: None,
        },
        Request::SendLine {
            selector: sheep("web"),
            line: "x".into(),
        },
        Request::Enable {
            selector: sheep("web"),
        },
        Request::Disable {
            selector: sheep("web"),
        },
        Request::Muster,
        Request::SaveRoll,
    ];
    for request in &controls {
        assert_eq!(
            permission_of(request),
            Permission::FlockControl,
            "{request:?}"
        );
    }
}

#[test]
fn flock_edit_holds_every_request_that_changes_a_sheeps_config() {
    let edits = [
        Request::Add { apps: Vec::new() },
        Request::ApplyConfig {
            apps: Vec::new(),
            reset: crate::config::ResetDepth::None,
        },
        Request::Scale {
            name: "web".into(),
            count: 2,
        },
        Request::SetSheepField {
            name: "web".into(),
            key: "autorestart".into(),
            value: serde_json::Value::Bool(true),
        },
        Request::ResetSheepField {
            name: "web".into(),
            key: "autorestart".into(),
        },
        Request::SetSheepEnv {
            name: "web".into(),
            key: "K".into(),
            value: Some(EnvValue::from("v".to_string())),
        },
        Request::SetSheepEnvBatch {
            name: "web".into(),
            entries: Default::default(),
            force: false,
            dry_run: true,
        },
        Request::CopySheepEnv {
            from: "web".into(),
            to: "api".into(),
            replace: false,
            except: Vec::new(),
            dry_run: true,
            expect: None,
        },
        Request::SetSmit {
            sheep: "web".into(),
            smit: None,
        },
    ];
    for request in &edits {
        assert_eq!(permission_of(request), Permission::FlockEdit, "{request:?}");
    }
}

#[test]
fn logs_split_into_reading_and_managing() {
    let tail = Request::TailLog {
        id: 1,
        stream: LogStream::Out,
        lines: 10,
    };
    assert_eq!(permission_of(&tail), Permission::LogsRead);
    for request in [
        Request::Reopen {
            selector: SelectorSpec::All,
        },
        Request::Flush {
            selector: SelectorSpec::All,
        },
    ] {
        assert_eq!(permission_of(&request), Permission::LogsManage);
    }
}

#[test]
fn a_secrets_push_needs_write_on_the_namespace_it_names() {
    let push = Request::PutSecrets {
        namespace: "vercel".into(),
        environment: "staging".into(),
        entries: Default::default(),
    };
    assert_eq!(
        permission_of(&push),
        Permission::SecretsWrite("vercel".into())
    );
}

#[test]
fn a_resolved_read_of_a_sheeps_table_is_secrets_read_in_its_environment() {
    let read = Request::SheepDogSettingsResolved {
        name: "web".into(),
        dog: CALLER.into(),
    };
    assert!(matches!(
        need(&read, CALLER),
        Need::ResolvedRead {
            sheep: "web",
            dog: "kelpie"
        }
    ));
}

#[test]
fn dog_control_holds_enabling_and_disabling_a_dog() {
    for request in [
        Request::EnableDog {
            name: "jobs".into(),
            env: BTreeMap::default(),
            source: crate::protocol::DogSource::BuiltIn { grants: None },
        },
        Request::DisableDog {
            name: "jobs".into(),
        },
    ] {
        assert_eq!(permission_of(&request), Permission::DogsControl);
    }
}

#[test]
fn the_shepherd_can_be_stopped_only_with_its_own_name() {
    assert_eq!(
        permission_of(&Request::KillDaemon),
        Permission::ShepherdControl
    );
}

#[test]
fn a_ping_needs_nothing() {
    assert!(is_always(&Request::Ping));
    assert!(is_always(&Request::Unrecognized));
}

#[test]
fn a_dogs_own_section_and_tables_need_nothing_and_another_dogs_need_a_group() {
    let toml = || DogSectionToml::from("x = 1\n".to_string());
    let own = [
        Request::DogConfig {
            name: CALLER.into(),
        },
        Request::DogConfigResolved {
            name: CALLER.into(),
        },
        Request::SetDogConfig {
            name: CALLER.into(),
            toml: toml(),
        },
        Request::DogSheepSettings { dog: CALLER.into() },
        Request::DogDefaults { dog: CALLER.into() },
        Request::SetSheepDogSettings {
            name: "web".into(),
            dog: CALLER.into(),
            table: None,
        },
    ];
    for request in &own {
        assert!(is_always(request), "{request:?}");
    }

    let other = |request: Request, permission: Permission| {
        assert_eq!(permission_of(&request), permission, "{request:?}");
    };
    other(
        Request::DogConfig {
            name: "bark".into(),
        },
        Permission::DogsControl,
    );
    other(
        Request::DogConfigResolved {
            name: "bark".into(),
        },
        Permission::DogsControl,
    );
    other(
        Request::SetDogConfig {
            name: "bark".into(),
            toml: toml(),
        },
        Permission::DogsControl,
    );
    other(
        Request::DogSheepSettings { dog: "bark".into() },
        Permission::FlockRead,
    );
    other(
        Request::SetSheepDogSettings {
            name: "web".into(),
            dog: "bark".into(),
            table: None,
        },
        Permission::FlockEdit,
    );
    other(
        Request::DogDefaults { dog: "bark".into() },
        Permission::FlockRead,
    );
    // Operator-set: a dog cannot write its own default.
    other(
        Request::SetDogDefaults {
            dog: CALLER.into(),
            table: None,
        },
        Permission::FlockEdit,
    );
}

#[test]
fn a_trigger_and_a_delete_need_what_the_selector_reaches() {
    let selector = sheep("web");
    let trigger = Request::Trigger {
        selector: selector.clone(),
        action: "go".into(),
        params: None,
    };
    let Need::Selector { sheep, dog, .. } = need(&trigger, CALLER) else {
        panic!("a trigger is aimed by its selector");
    };
    assert_eq!(sheep, Permission::FlockControl);
    assert_eq!(dog("jobs"), Permission::DogsTrigger("jobs".into()));

    let delete = Request::Delete { selector };
    let Need::Selector { sheep, dog, .. } = need(&delete, CALLER) else {
        panic!("a delete is aimed by its selector");
    };
    assert_eq!(sheep, Permission::FlockEdit);
    assert_eq!(dog("jobs"), Permission::DogsControl);
}

#[test]
fn a_subscription_is_checked_by_its_topics() {
    let topics = vec!["process.*".to_string()];
    let subscribe = Request::Subscribe {
        topics: topics.clone(),
    };
    assert!(matches!(need(&subscribe, CALLER), Need::Topics(t) if t == topics));
}
