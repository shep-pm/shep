//! Tests for the four reset depths.
//!
//! `file`, `policy`, `env` and `all` each put back a different amount and spend
//! a different set of overrides. The grid here pins every combination, because
//! the difference between them is easy to state and easy to get wrong.

use super::*;

/// The four-mode fixture: a template declaring `max_restarts` and `env`,
/// against a sheep whose operator has overridden `max_restarts`, added
/// `max_memory` (undeclared) and edited the one declared env key.
///
/// Returns the stored config, the template and the override record, in the
/// order [`merge_declared`] takes them. Every mode is asserted on the same
/// three values, since the four are not a two-by-two grid.
fn reset_grid() -> (AppConfig, DeclaredApp, shep_core::overrides::AppOverrides) {
    let mut stored = AppConfig::minimal("web", "./srv");
    stored.max_restarts = 3;
    stored.max_memory = Some(MemSize::from_bytes(2 << 30));
    stored.env = BTreeMap::from([("DB".to_string(), "operator".to_string())]);

    let mut template = AppConfig::minimal("web", "./srv");
    template.max_restarts = 10;
    template.env = BTreeMap::from([("DB".to_string(), "template".to_string())]);
    let incoming = declared_app(template, &["name", "script", "max_restarts", "env"]);

    let mut record = established(
        &["name", "script", "max_restarts", "env"],
        vec![
            ("max_restarts", serde_json::json!(3)),
            ("max_memory", serde_json::json!("2G")),
            ("env", serde_json::json!({ "DB": "operator" })),
        ],
    );
    // The template established `DB` on the load before this one, which is
    // how an operator's later edit to it became an override at all.
    record.declared_env = BTreeSet::from(["DB".to_string()]);
    (stored, incoming, record)
}

/// The merged config a mode produces over [`reset_grid`].
fn merged_over_grid(reset: ResetDepth) -> AppConfig {
    let (stored, incoming, record) = reset_grid();
    merge_declared(&stored, &incoming, &record, reset)
        .expect("the grid fixture travels through serde")
        .0
}

/// The override record a mode leaves over [`reset_grid`].
fn record_over_grid(reset: ResetDepth) -> shep_core::overrides::AppOverrides {
    let (stored, incoming, record) = reset_grid();
    merge_declared(&stored, &incoming, &record, reset)
        .expect("the grid fixture travels through serde")
        .1
}

/// Both axes in one test: the mode is the pair, and a test checking one
/// axis would pass for two different modes.
#[test]
fn file_puts_back_what_the_template_declares_and_leaves_the_rest() {
    let merged = merged_over_grid(ResetDepth::File);
    assert_eq!(merged.max_restarts, 10, "a declared key goes back");
    assert_eq!(
        merged.max_memory,
        Some(MemSize::from_bytes(2 << 30)),
        "a key the template never declares is not the template's to reset"
    );
    assert_eq!(
        merged.env.get("DB").map(String::as_str),
        Some("operator"),
        "`file` keeps env"
    );
}

#[test]
fn policy_puts_back_every_setting_declared_or_not_and_keeps_env() {
    let merged = merged_over_grid(ResetDepth::Policy);
    assert_eq!(merged.max_restarts, 10, "a declared key goes back");
    assert_eq!(
        merged.max_memory,
        AppConfig::default().max_memory,
        "`policy` resets a key the template is silent about, which for an \
         undeclared key means the value a fresh start off that template gives it"
    );
    assert_eq!(
        merged.env.get("DB").map(String::as_str),
        Some("operator"),
        "`policy` keeps env"
    );
}

/// An operator typing `--reset=env` does not expect their restart budget
/// put back because the template happens to mention it.
#[test]
fn env_resets_env_and_touches_no_setting_at_all() {
    let merged = merged_over_grid(ResetDepth::Env);
    assert_eq!(
        merged.max_restarts, 3,
        "a declared policy field is not `env`'s to reset"
    );
    assert_eq!(
        merged.max_memory,
        Some(MemSize::from_bytes(2 << 30)),
        "and neither is one the template is silent on"
    );
    assert_eq!(
        merged.env.get("DB").map(String::as_str),
        Some("template"),
        "`env` puts env back to the template"
    );
}

/// `all` is the widest mode: every setting back, declared or not, and env
/// with it.
#[test]
fn all_puts_back_every_setting_and_env_with_it() {
    let merged = merged_over_grid(ResetDepth::All);
    assert_eq!(merged.max_restarts, 10, "a declared key goes back");
    assert_eq!(
        merged.max_memory,
        AppConfig::default().max_memory,
        "`all` resets a key the template is silent about"
    );
    assert_eq!(
        merged.env.get("DB").map(String::as_str),
        Some("template"),
        "`all` puts env back to the template"
    );
}

/// An override is spent exactly where the merge overwrote it, so `file`
/// spends the declared setting and keeps both the undeclared one and env.
#[test]
fn file_spends_only_the_override_it_put_back() {
    let record = record_over_grid(ResetDepth::File);
    let mut held: Vec<&String> = record.fields.keys().collect();
    // Sorted: `serde_json::Map` is insertion-ordered, and this is a set.
    held.sort();
    assert_eq!(
        held,
        vec!["env", "max_memory"],
        "`file` keeps the undeclared override and env, and spends the rest"
    );
}

/// Every setting is in scope, so every setting override is spent; env is
/// untouched, so the env override stands.
#[test]
fn policy_spends_every_setting_override_and_keeps_env() {
    let record = record_over_grid(ResetDepth::Policy);
    let mut held: Vec<&String> = record.fields.keys().collect();
    // Sorted: `serde_json::Map` is insertion-ordered, and this is a set.
    held.sort();
    assert_eq!(
        held,
        vec!["env"],
        "`policy` spends both setting overrides and keeps env"
    );
}

/// An override is spent exactly where the merge overwrote it, and this mode
/// overwrites no setting, so both survive. Those record entries keep a
/// later plain load from appending the template's values over them.
#[test]
fn env_spends_only_the_env_override() {
    let record = record_over_grid(ResetDepth::Env);
    let mut held: Vec<&String> = record.fields.keys().collect();
    // Sorted: `serde_json::Map` is insertion-ordered, and this is a set.
    held.sort();
    assert_eq!(
        held,
        vec!["max_memory", "max_restarts"],
        "`env` spends env and keeps every setting override"
    );
}

/// Everything is in scope, so nothing is still overridden.
#[test]
fn all_spends_every_override() {
    let record = record_over_grid(ResetDepth::All);
    assert!(
        record.fields.is_empty(),
        "`all` holds nothing back: {:?}",
        record.fields.keys().collect::<Vec<_>>()
    );
}

/// Resetting a key's value and dropping its record entry are different
/// operations, and only `all` does both: under `env` the record still holds
/// `max_memory`, which is what stands between the operator's ceiling and
/// the next plain load.
#[tokio::test(start_paused = true)]
async fn an_env_reset_keeps_the_override_record() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(
        &dir,
        &[app_with("web", |app| {
            app.max_memory = Some(MemSize::from_bytes(2 << 30));
            app.env = BTreeMap::from([("DB".to_string(), "operator".to_string())]);
        })],
    );
    let mut record = established(
        &["name", "script", "env"],
        vec![
            ("max_memory", serde_json::json!("2G")),
            ("env", serde_json::json!({ "DB": "operator" })),
        ],
    );
    record.declared_env = BTreeSet::from(["DB".to_string()]);
    shep_core::overrides::put(&actor.paths.overrides, "web", &record).unwrap();

    let mut file = AppConfig::minimal("web", "./srv");
    file.env = BTreeMap::from([("DB".to_string(), "template".to_string())]);
    let file = declared_app(file, &["name", "script", "env"]);
    let reply = apply_config(&mut actor, vec![file], ResetDepth::Env).await;

    let written = shep_core::overrides::get(&actor.paths.overrides, "web")
        .unwrap()
        .unwrap_or_else(|| panic!("`env` keeps the record: {reply:?}"));
    assert!(
        written.fields.contains_key("max_memory"),
        "the undeclared override must survive an env reset: {reply:?}"
    );
    assert_eq!(
        actor.sheep[&0].entry.overridden,
        vec!["max_memory".to_string()],
        "and `shep flock`'s CFG column must still say so"
    );
}

/// The flag widens a load and never narrows one, so the additive default
/// underneath it still runs. The second assertion keeps that from reading
/// as an overwrite: `max_restarts` is established, and does not move.
#[test]
fn an_env_reset_still_appends_a_key_nobody_established() {
    let (stored, _, record) = reset_grid();
    let mut template = AppConfig::minimal("web", "./srv");
    template.max_restarts = 10;
    template.min_uptime = UpDuration::from_millis(9000);
    template.env = BTreeMap::from([("DB".to_string(), "template".to_string())]);
    let incoming = declared_app(
        template,
        &["name", "script", "max_restarts", "min_uptime", "env"],
    );

    let (merged, _) = merge_declared(&stored, &incoming, &record, ResetDepth::Env)
        .expect("the grid fixture travels through serde");
    assert_eq!(
        merged.min_uptime,
        UpDuration::from_millis(9000),
        "a declared key nobody established is appended under `env` too"
    );
    assert_eq!(
        merged.max_restarts, 3,
        "and an established one is still not overwritten"
    );
}

/// `instances` is held out of this depth as it is out of a plain load: the
/// store cannot tell a stocked count from a count nobody has touched, so
/// taking the file's would delete instances.
#[tokio::test(start_paused = true)]
async fn an_env_reset_never_reshapes_a_flock() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |app| app.instances = 4)]);
    let mut file = AppConfig::minimal("web", "./srv");
    file.instances = 2;
    let file = declared_app(file, &["name", "script", "instances"]);
    let reply = apply_config(&mut actor, vec![file], ResetDepth::Env).await;
    assert_eq!(
        actor.ids_of_name("web").len(),
        4,
        "`env` scaled a flock: {reply:?}"
    );
    // Exact, not a `contains("instances")`: the sentence an `env` operator
    // reads is the assertion, down to which mode it names and what that
    // mode costs.
    assert_eq!(
        reply[0].refused.as_deref(),
        Some(
            "instances: this load never reshapes a flock; no mode scales without also \
             putting back every setting the file declares, and `--reset=file` is the \
             narrowest that does, taking the file's count of 2"
        ),
        "an operator whose count did not move must be told why, and what the \
         mode they are pointed at would cost them: {reply:?}"
    );
}

/// An app stocked to four against a template carrying no `instances` line
/// keeps four. Under `policy` it drops to one, since the compiled default
/// wins an argument the file never entered. The second half keeps this from
/// passing on a `file` that refuses to scale at all.
#[tokio::test(start_paused = true)]
async fn a_file_reset_does_not_scale_an_app_the_template_says_nothing_about() {
    let stocked = || app_with("web", |app| app.instances = 4);

    let silent_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&silent_dir, &[stocked()]);
    let silent = declared_app(AppConfig::minimal("web", "./srv"), &["name", "script"]);
    let reply = apply_config(&mut actor, vec![silent], ResetDepth::File).await;
    assert_eq!(
        actor.ids_of_name("web").len(),
        4,
        "`file` scaled against a file with no `instances` line: {reply:?}"
    );
    assert!(
        !reply[0].applied.contains(&"instances".to_string()),
        "{reply:?}"
    );

    let declaring_dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&declaring_dir, &[stocked()]);
    let mut declaring = AppConfig::minimal("web", "./srv");
    declaring.instances = 2;
    let declaring = declared_app(declaring, &["name", "script", "instances"]);
    let reply = apply_config(&mut actor, vec![declaring], ResetDepth::File).await;
    assert_eq!(
        actor.ids_of_name("web").len(),
        2,
        "`file` must apply a count the template does declare: {reply:?}"
    );
    assert!(
        reply[0].applied.contains(&"instances".to_string()),
        "{reply:?}"
    );
}

/// The `Policy` depth touches env not at all, so a template that has grown
/// `NEW_KEY` reports nothing and merges nothing. Recording the key as
/// established anyway would leave no plain load able to append it, with
/// only `--reset=all` to recover, taking every other env value.
#[tokio::test(start_paused = true)]
async fn a_settings_reset_does_not_establish_an_env_key_it_never_merged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut actor, _enforcer) = actor_over(&dir, &[app_with("web", |_| {})]);
    shep_core::overrides::put(
        &actor.paths.overrides,
        "web",
        &established(&["name", "script"], Vec::new()),
    )
    .unwrap();

    let file = || {
        let mut file = AppConfig::minimal("web", "./srv");
        file.env = BTreeMap::from([("NEW_KEY".to_string(), "1".to_string())]);
        declared_app(file, &["name", "script", "env"])
    };
    let reset = apply_config(&mut actor, vec![file()], ResetDepth::Policy).await;

    assert!(
        actor.sheep[&0].entry.spec.config().env.is_empty(),
        "a `--reset=policy` merges no env at all: {reset:?}"
    );
    assert!(
        shep_core::overrides::get(&actor.paths.overrides, "web")
            .unwrap()
            .expect("the load records what it established")
            .declared_env
            .is_empty(),
        "a key that never merged is not established"
    );

    // The plain load after it can still append.
    let plain = apply_config(&mut actor, vec![file()], ResetDepth::None).await;
    assert_eq!(
        actor.sheep[&0]
            .entry
            .pending
            .as_ref()
            .expect("an env change parks for the next spawn")
            .config()
            .env
            .get("NEW_KEY")
            .map(String::as_str),
        Some("1"),
        "{plain:?}"
    );
}
