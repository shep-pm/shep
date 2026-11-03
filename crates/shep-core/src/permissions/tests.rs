use super::{Grants, Permission};

#[test]
fn every_permission_reads_back_as_it_is_written() {
    for name in [
        "flock.read",
        "flock.control",
        "flock.edit",
        "logs.read",
        "logs.manage",
        "events:process.",
        "events:",
        "secrets.read:staging",
        "secrets.write:vercel",
        "dogs.control",
        "dogs.trigger:jobs",
        "shepherd.control",
    ] {
        let parsed: Permission = name.parse().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(parsed.to_string(), name);
    }
}

#[test]
fn a_name_outside_the_list_or_with_the_wrong_scope_is_refused() {
    for name in [
        "",
        "flock",
        "flock.watch",
        "flock.read:web",
        "secrets.read",
        "secrets.read:",
        "secrets.write:a/b",
        "dogs.trigger",
        "dogs.control:jobs",
    ] {
        assert!(name.parse::<Permission>().is_err(), "{name:?}");
    }
}

#[test]
fn parsing_grants_names_the_first_unknown_permission() {
    let err = Grants::parse(["flock.read", "flock.watch"]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "`flock.watch` is not a permission shep knows"
    );
}

#[test]
fn a_grant_allows_exactly_the_permission_it_names() {
    let grants = Grants::parse(["flock.read", "secrets.read:staging"]).unwrap();
    assert!(grants.allows(&Permission::FlockRead));
    assert!(!grants.allows(&Permission::FlockControl));
    assert!(grants.allows(&Permission::SecretsRead("staging".into())));
    assert!(!grants.allows(&Permission::SecretsRead("production".into())));
}

#[test]
fn a_name_this_build_cannot_read_is_kept_and_matches_nothing() {
    let grants = Grants::from_names(["flock.read".to_string(), "from.the.future".to_string()]);
    assert!(grants.allows(&Permission::FlockRead));
    assert_eq!(grants.names().count(), 2);
}

#[test]
fn a_topic_pattern_must_stay_inside_a_granted_prefix() {
    let grants = Grants::parse(["events:process.", "events:log.out"]).unwrap();
    assert!(grants.allows_topic("process.exit"));
    assert!(grants.allows_topic("process.*"));
    assert!(grants.allows_topic("log.out"));
    assert!(!grants.allows_topic("log.*"), "could also reach log.err");
    assert!(!grants.allows_topic("*"));
    assert!(!grants.allows_topic("{process.exit,log.err}"));
    assert!(!grants.allows_topic("daemon.shutdown"));
    assert!(Grants::parse(["events:"]).unwrap().allows_topic("*"));
    assert!(
        !Grants::parse(["flock.read"])
            .unwrap()
            .allows_topic("process.exit")
    );
}

#[test]
fn grants_are_a_list_of_names_on_the_wire() {
    let grants = Grants::parse(["logs.read", "flock.read"]).unwrap();
    assert_eq!(
        serde_json::to_string(&grants).unwrap(),
        r#"["flock.read","logs.read"]"#
    );
    assert_eq!(
        serde_json::from_str::<Grants>(r#"["flock.read","logs.read"]"#).unwrap(),
        grants
    );
}

#[test]
fn a_dog_grant_covers_that_dogs_events_and_no_other_dogs() {
    let grants = Grants::parse(["events:dog.otel"]).unwrap();
    assert!(grants.allows_topic("dog.otel.exported"));
    assert!(grants.allows_topic("dog.otel.*"));
    assert!(!grants.allows_topic("dog.otel-2.exported"));
    assert!(!grants.allows_topic("dog.otel*"));
    assert!(!grants.allows_topic("dog.*"));
    assert!(!grants.allows_topic("dog.bark.sent"));
    assert!(!grants.allows_topic("config.dog.otel"));
}

#[test]
fn a_broader_events_grant_still_covers_every_dog() {
    assert!(
        Grants::parse(["events:dog."])
            .unwrap()
            .allows_topic("dog.*")
    );
    assert!(Grants::parse(["events:"]).unwrap().allows_topic("dog.*"));
}
