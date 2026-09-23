use shep_core::protocol::ProcessInfo;

use super::*;

fn one_sink(name: &str) -> BTreeMap<String, Sink> {
    let mut sinks = BTreeMap::new();
    sinks.insert(
        name.to_owned(),
        Sink::Json {
            url: "http://localhost/hook".to_owned(),
            body: None,
        },
    );
    sinks
}

/// The seam that matters: an operator hears about this when
/// `dogs.toml` is read, not when a rule first fires days later.
#[test]
fn a_sink_url_carrying_credentials_is_refused_when_the_rules_are_built() {
    let mut sinks = BTreeMap::new();
    sinks.insert(
        "ops".to_owned(),
        Sink::Json {
            url: "http://user:hunter2@localhost/hook".to_owned(),
            body: None,
        },
    );
    let err = Rules::new(Vec::new(), &sinks).unwrap_err();
    assert!(
        matches!(
            err,
            RulesError::UnusableSink(SinkConfigError::UrlCredentials { .. })
        ),
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

fn base_info(name: &str, status: ProcStatus) -> ProcessInfo {
    ProcessInfo::builder(1, name, status)
        .pid(Some(4242))
        .uptime_ms(1_000)
        .build()
}

fn errored_info(name: &str) -> ProcessInfo {
    base_info(name, ProcStatus::Errored)
}

fn online_info(name: &str) -> ProcessInfo {
    base_info(name, ProcStatus::Online)
}

fn process_event(name: &str, kind: ProcessEventKind) -> BusEvent {
    BusEvent::Process {
        event: kind,
        info: base_info(name, ProcStatus::Online),
        manually: false,
        at_ms: 0,
    }
}

fn errored_event(name: &str) -> BusEvent {
    process_event(name, ProcessEventKind::Errored)
}

fn restart_event(name: &str) -> BusEvent {
    process_event(name, ProcessEventKind::Restart)
}

fn gave_up_rules() -> Rules {
    let sinks = one_sink("ops");
    Rules::new(
        vec![Rule {
            when: Trigger::GaveUp {},
            sinks: vec!["ops".to_owned()],
            debounce: default_debounce(),
        }],
        &sinks,
    )
    .unwrap()
}

fn restart_rate_rules(restarts: u32, within: UpDuration) -> Rules {
    let sinks = one_sink("ops");
    Rules::new(
        vec![Rule {
            when: Trigger::RestartRate { restarts, within },
            sinks: vec!["ops".to_owned()],
            debounce: default_debounce(),
        }],
        &sinks,
    )
    .unwrap()
}

fn rule_to(sink: &str) -> Rule {
    Rule {
        when: Trigger::GaveUp {},
        sinks: vec![sink.to_owned()],
        debounce: default_debounce(),
    }
}

#[test]
fn an_errored_seen_by_both_routes_fires_once() {
    let mut rules = gave_up_rules();
    let first = rules.on_event(&errored_event("web"), 1_000);
    assert_eq!(first.len(), 1);
    let second = rules.on_poll(&[errored_info("web")], 2_000);
    assert!(second.is_empty(), "the debounce covers the other route");
}

#[test]
fn the_poll_fires_what_the_bus_never_carried() {
    let mut rules = gave_up_rules();
    let fired = rules.on_poll(&[errored_info("web")], 1_000);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].bark.subject, "web");
}

#[test]
fn one_flapping_sheep_does_not_mute_another_going_down() {
    let mut rules = gave_up_rules();
    assert_eq!(rules.on_event(&errored_event("web"), 1_000).len(), 1);
    assert_eq!(rules.on_event(&errored_event("api"), 1_100).len(), 1);
    assert!(rules.on_event(&errored_event("web"), 1_200).is_empty());
}

/// `info.restarts` says 9; only 3 restart events are fed through
/// `on_event`, so passing requires reading `info.restarts` rather than
/// a tally kept from the events.
#[test]
fn the_early_warning_counts_the_shepherds_restarts_and_not_its_own() {
    let mut rules = restart_rate_rules(5, UpDuration::from_millis(60_000));
    for at in [1_000, 2_000, 3_000] {
        let _ = rules.on_event(&restart_event("web"), at);
    }
    let mut info = online_info("web");
    info.restarts = 9;
    let fired = rules.on_poll(&[info], 4_000);
    assert_eq!(
        fired.len(),
        1,
        "9 restarts crosses a threshold of 5; 3 does not"
    );
}

#[test]
fn a_rule_routed_at_a_sink_that_does_not_exist_is_refused_at_startup() {
    let err = Rules::new(vec![rule_to("pager")], &BTreeMap::new()).unwrap_err();
    assert!(matches!(err, RulesError::UnknownSink { .. }));
    // Exact, not a `contains`: this string reaches an operator through
    // `Display`.
    assert_eq!(
        err.to_string(),
        "rule 0 routes to sink \"pager\", which [bark.sinks] in dogs.toml does not define"
    );
}

#[test]
fn a_bark_with_sinks_and_no_rules_still_alerts_when_the_shepherd_gives_up() {
    let sinks = one_sink("ops");
    let rules = Rules::default_rules(&sinks);
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].when, Trigger::GaveUp {});
    assert_eq!(rules[0].sinks, vec!["ops"]);
}

#[test]
fn a_rule_with_no_sinks_at_all_is_refused_at_startup() {
    let rule = Rule {
        when: Trigger::GaveUp {},
        sinks: Vec::new(),
        debounce: default_debounce(),
    };
    let err = Rules::new(vec![rule], &one_sink("ops")).unwrap_err();
    assert!(matches!(err, RulesError::NoSinks { .. }));
}

#[test]
fn an_event_rule_naming_an_unknown_kind_is_refused_at_startup() {
    let rule = Rule {
        when: Trigger::Event {
            kinds: vec!["exit".to_owned(), "not_a_real_kind".to_owned()],
        },
        sinks: vec!["ops".to_owned()],
        debounce: default_debounce(),
    };
    let err = Rules::new(vec![rule], &one_sink("ops")).unwrap_err();
    assert!(matches!(err, RulesError::UnknownKind { .. }));
    assert!(err.to_string().contains("not_a_real_kind"));
}

#[test]
fn an_event_rule_does_not_fire_on_a_kind_it_was_not_given() {
    let sinks = one_sink("ops");
    let mut rules = Rules::new(
        vec![Rule {
            when: Trigger::Event {
                kinds: vec!["exit".to_owned()],
            },
            sinks: vec!["ops".to_owned()],
            debounce: default_debounce(),
        }],
        &sinks,
    )
    .unwrap();
    let online = rules.on_event(&process_event("web", ProcessEventKind::Online), 1_000);
    assert!(online.is_empty(), "online was not in the configured kinds");
    let exit = rules.on_event(&process_event("web", ProcessEventKind::Exit), 1_100);
    assert_eq!(exit.len(), 1, "exit was, and should still fire");
}

#[test]
fn restart_rate_fires_at_the_threshold_and_not_one_below_it() {
    let mut rules = restart_rate_rules(5, UpDuration::from_millis(60_000));
    let mut below = online_info("web");
    below.restarts = 4;
    assert!(
        rules.on_poll(&[below], 1_000).is_empty(),
        "4 restarts is below a threshold of 5"
    );
    let mut at = online_info("web");
    at.restarts = 5;
    assert_eq!(
        rules.on_poll(&[at], 1_100).len(),
        1,
        "5 restarts meets a threshold of 5"
    );
}

/// Debounce is zeroed: only the window's own logic keeps this quiet.
#[test]
fn restart_rate_window_slides_once_it_elapses() {
    let sinks = one_sink("ops");
    let mut rules = Rules::new(
        vec![Rule {
            when: Trigger::RestartRate {
                restarts: 5,
                within: UpDuration::from_millis(1_000),
            },
            sinks: vec!["ops".to_owned()],
            debounce: UpDuration::from_millis(0),
        }],
        &sinks,
    )
    .unwrap();

    let mut info = online_info("web");
    info.restarts = 5;
    assert_eq!(
        rules.on_poll(&[info.clone()], 0).len(),
        1,
        "5 restarts opens the window past threshold"
    );

    // Window elapsed and the count did not move: it resets, with
    // nothing new to warn about.
    assert!(
        rules.on_poll(&[info.clone()], 2_000).is_empty(),
        "no new restarts since the window reset"
    );

    // Five more restarts inside the new window crosses it again.
    info.restarts = 10;
    assert_eq!(
        rules.on_poll(&[info], 2_100).len(),
        1,
        "5 more restarts inside the new window crosses it again"
    );
}

#[test]
fn memory_above_fires_at_the_ceiling_and_not_one_byte_below_it() {
    let sinks = one_sink("ops");
    let mut rules = Rules::new(
        vec![Rule {
            when: Trigger::MemoryAbove {
                bytes: MemSize::from_bytes(1_000),
            },
            sinks: vec!["ops".to_owned()],
            debounce: default_debounce(),
        }],
        &sinks,
    )
    .unwrap();

    let mut below = online_info("web");
    below.memory_bytes = Some(999);
    assert!(
        rules.on_poll(&[below], 1_000).is_empty(),
        "999 is below 1000"
    );

    let mut at = online_info("web");
    at.memory_bytes = Some(1_000);
    assert_eq!(rules.on_poll(&[at], 1_100).len(), 1, "1000 meets 1000");
}

/// Unknown memory (a stopped sheep, or one not yet sampled) must read
/// as "cannot alert", never as zero.
#[test]
fn memory_above_does_not_fire_when_usage_is_unknown() {
    let sinks = one_sink("ops");
    let mut rules = Rules::new(
        vec![Rule {
            when: Trigger::MemoryAbove {
                bytes: MemSize::from_bytes(1_000),
            },
            sinks: vec!["ops".to_owned()],
            debounce: default_debounce(),
        }],
        &sinks,
    )
    .unwrap();
    let info = online_info("web");
    assert!(info.memory_bytes.is_none());
    assert!(rules.on_poll(&[info], 1_000).is_empty());
}

#[test]
fn gave_up_does_not_fire_on_event_for_a_non_errored_kind() {
    let mut rules = gave_up_rules();
    let online = rules.on_event(&process_event("web", ProcessEventKind::Online), 1_000);
    assert!(online.is_empty(), "GaveUp fires on Errored only");
    let restart = rules.on_event(&restart_event("web"), 1_100);
    assert!(restart.is_empty(), "GaveUp fires on Errored only");
}

#[test]
fn gave_up_does_not_fire_on_poll_for_a_non_errored_status() {
    let mut rules = gave_up_rules();
    let fired = rules.on_poll(&[online_info("web")], 1_000);
    assert!(fired.is_empty(), "GaveUp fires when status is Errored only");
}

#[test]
fn debounce_boundary_is_inclusive_at_exactly_its_own_duration() {
    let mut rules = gave_up_rules();
    let debounce_ms = default_debounce().as_millis();
    assert_eq!(rules.on_event(&errored_event("web"), 0).len(), 1);
    assert!(
        rules
            .on_event(&errored_event("web"), debounce_ms - 1)
            .is_empty(),
        "one millisecond short of the debounce must still be quiet"
    );
    assert_eq!(
        rules.on_event(&errored_event("web"), debounce_ms).len(),
        1,
        "exactly at the debounce it may fire again"
    );
}

// The tests above build `Rule`/`Trigger` as Rust values, never running
// `Deserialize`. The tests below parse real TOML strings.

/// The exact shape `docs/dogs.md` and `web/src/pages/docs/dogs.astro`
/// publish as copy-pasteable.
#[test]
fn the_docs_gave_up_rule_parses_from_toml() {
    let rule: Rule = toml::from_str(
        r#"
on = "gave_up"
sinks = ["oncall", "audit"]
"#,
    )
    .unwrap();
    assert_eq!(rule.when, Trigger::GaveUp {});
    assert_eq!(rule.sinks, vec!["oncall", "audit"]);
    assert_eq!(rule.debounce, default_debounce(), "no override in the TOML");
}

/// `within`'s `"2m"` form uses [`UpDuration`]'s own duration grammar.
#[test]
fn the_docs_restart_rate_rule_parses_from_toml() {
    let rule: Rule = toml::from_str(
        r#"
on = "restart_rate"
restarts = 5
within = "2m"
sinks = ["oncall"]
"#,
    )
    .unwrap();
    assert_eq!(
        rule.when,
        Trigger::RestartRate {
            restarts: 5,
            within: UpDuration::from_millis(2 * 60_000),
        }
    );
}

/// Not in the published docs, but a real `Trigger` variant a rule can
/// name.
#[test]
fn an_event_rule_parses_from_toml() {
    let rule: Rule = toml::from_str(
        r#"
on = "event"
kinds = ["exit", "errored"]
sinks = ["oncall"]
"#,
    )
    .unwrap();
    assert_eq!(
        rule.when,
        Trigger::Event {
            kinds: vec!["exit".to_owned(), "errored".to_owned()],
        }
    );
}

/// `bytes`'s `"512M"` form uses [`MemSize`]'s own grammar.
#[test]
fn a_memory_above_rule_parses_from_toml() {
    let rule: Rule = toml::from_str(
        r#"
on = "memory_above"
bytes = "512M"
sinks = ["oncall"]
"#,
    )
    .unwrap();
    assert_eq!(
        rule.when,
        Trigger::MemoryAbove {
            // Binary units: MemSize's grammar is MiB, not MB.
            bytes: MemSize::from_bytes(512 * 1024 * 1024),
        }
    );
}

#[test]
fn a_rule_s_debounce_override_parses_from_toml() {
    let rule: Rule = toml::from_str(
        r#"
on = "gave_up"
sinks = ["oncall"]
debounce = "10m"
"#,
    )
    .unwrap();
    assert_eq!(rule.debounce, UpDuration::from_millis(10 * 60_000));
}

#[test]
fn a_misspelled_trigger_field_is_refused_with_the_bad_key_named() {
    let err = toml::from_str::<Rule>(
        r#"
on = "restart_rate"
retsarts = 5
within = "2m"
sinks = ["oncall"]
"#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("retsarts"),
        "the error must name the misspelled key, not just fail: {err}"
    );
}

/// A bare `GaveUp` unit variant would skip field checking for the rest
/// of the map, missing exactly this typo.
#[test]
fn a_misspelled_field_next_to_gave_up_is_still_refused() {
    let err = toml::from_str::<Rule>(
        r#"
on = "gave_up"
sinks = ["oncall"]
debuonce = "10m"
"#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("debuonce"),
        "the error must name the misspelled key, not just fail: {err}"
    );
}

/// `sinsk` matches no field on `Rule` or `Trigger`, so it is simply
/// absent, and the error names the missing `sinks` field instead of
/// the typo.
#[test]
fn a_misspelled_sinks_field_is_refused_as_a_missing_field() {
    let err = toml::from_str::<Rule>(
        r#"
on = "gave_up"
sinsk = ["oncall"]
"#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("sinks"),
        "the error must name the missing field: {err}"
    );
}

#[test]
fn an_unknown_on_variant_is_refused_with_the_bad_value_named() {
    let err = toml::from_str::<Rule>(
        r#"
on = "gav_up"
sinks = ["oncall"]
"#,
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("gav_up"),
        "the error must name the bad value: {message}"
    );
    assert!(
        message.contains("gave_up"),
        "the error must also name a real variant, so a typo suggests its own fix: {message}"
    );
}

/// [`wire_spelling`] hand-lists the spellings instead of reading
/// `ProcessEventKind`'s own `Serialize`, which is what makes it free of
/// an allocation on the bus route. This pins every arm back against what
/// serde actually emits, so a typo in an arm fails here rather than as a
/// rule that silently stops firing days later.
///
/// The enum is `#[non_exhaustive]`, so this list is the only thing that
/// would notice a variant being added: a new one needs a line here as well
/// as an arm in `wire_spelling`.
#[test]
fn every_wire_spelling_matches_what_serde_emits() {
    let kinds = [
        ProcessEventKind::Start,
        ProcessEventKind::Online,
        ProcessEventKind::Exit,
        ProcessEventKind::Restart,
        ProcessEventKind::Reload,
        ProcessEventKind::Reloaded,
        ProcessEventKind::ReloadAbandoned,
        ProcessEventKind::Stop,
        ProcessEventKind::Delete,
        ProcessEventKind::Errored,
        ProcessEventKind::Unrecognized,
    ];
    for kind in kinds {
        let from_serde = serde_json::to_value(kind)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        assert_eq!(
            wire_spelling(kind),
            from_serde,
            "{kind:?} must spell itself the way its own Serialize does"
        );
    }
}

/// A subject bark has not seen yet still gets its state, which is what
/// keeps the debounce honest for a first firing: `subject_state` inserts
/// on a miss rather than assuming the key is there.
#[test]
fn a_subject_seen_for_the_first_time_gets_its_debounce_state() {
    let sinks = one_sink("ops");
    let mut rules = Rules::new(
        vec![Rule {
            when: Trigger::Event {
                kinds: vec!["exit".to_owned()],
            },
            sinks: vec!["ops".to_owned()],
            debounce: default_debounce(),
        }],
        &sinks,
    )
    .unwrap();
    // Two different sheep, neither in the map yet: both must be recorded
    // independently, and neither may fire twice inside the debounce.
    for name in ["web", "worker"] {
        assert_eq!(
            rules
                .on_event(&process_event(name, ProcessEventKind::Exit), 1_000)
                .len(),
            1,
            "{name} had never been seen and should fire once"
        );
        assert!(
            rules
                .on_event(&process_event(name, ProcessEventKind::Exit), 1_100)
                .is_empty(),
            "{name} is inside its own debounce now"
        );
    }
    // Past the debounce each fires again, which is only true if the state
    // the insert created is the state being read back.
    for name in ["web", "worker"] {
        assert_eq!(
            rules
                .on_event(&process_event(name, ProcessEventKind::Exit), 900_000)
                .len(),
            1,
            "{name} is past its debounce and should fire again"
        );
    }
}
