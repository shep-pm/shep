# Lookout pane 1h (secrets) implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `shep lookout`'s secrets pane against the shipped secret store, so an operator can see, set, reveal and delete the values a Flockfile refers to without leaving the dashboard.

**Architecture:** A fifth `Body` variant over the existing lookout reducer. Every read is a file read on `spawn_blocking` (`secrets.json`, `secrets-cache.json`, the muster roll, `shep.toml`), and every write goes straight to `secrets.json` under its own lock, exactly as `shep secret` and the settings screen already do. Nothing new crosses the wire, and `PROTOCOL_VERSION` does not move.

**Tech Stack:** Rust 2024, MSRV 1.88, ratatui, tokio. Crates touched: `shep` (published as the `shep` package, directory `crates/shep-cli`).

**Spec:** [docs/brainstorming/specs/2026-09-08-lookout-1h-secrets-design.md](../../brainstorming/specs/2026-09-08-lookout-1h-secrets-design.md)

## Global constraints

- **Read the spec before Task 1.** The plan argues from it and does not repeat its reasoning.
- **Code shown for files that already exist is a guess.** Every snippet quoting current code is reconstructed from a read at planning time. Grep the real file before editing, and if it disagrees with this plan, the file wins. Say so in the DONE report rather than making the file match the plan.
- **Invoke the `shep-idiomatic-rust` skill before writing any Rust.** IR-41 (redacted `Debug` plus an exact-string test) applies to three new types in this plan and is not optional.
- **Conventional commit subjects, every commit:** `type(scope): summary`, with `!` on anything that breaks. Accepted types are `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `ci`, `chore`, `style`. Not `revert`, not `build`: release-plz discards both silently. `.github/workflows/commits.yml` gates this.
- **One cargo shape for the whole plan:**
  ```bash
  cargo test -p shep --lib --bins --all-features -- --skip ::slow::
  ```
  The package is `shep`, not `shep-cli`. `-p shep-cli` runs zero tests and exits 0.
- **Comment density (IR-47).** Comment what the next reader could not infer. Do not narrate the change, do not record rejected alternatives, do not paraphrase the code.
- **Never write an absolute local path or a personal name** into any file, commit message or comment. Paths are repo-relative or `$SHEP_HOME`.
- **Mutation receipt per test.** After a test passes, break the thing it claims to pin, confirm it fails, restore, and quote the failure line in the DONE report. A test whose mutation stays green is reported, not kept quietly.

---

### Task 1: Extract the roll walk that finds who names a secret

`gather_secrets` in `crates/shep-cli/src/commands/query.rs` already walks the muster roll to find which app names which `{{secret:...}}` reference, and in which environment. The pane needs the same walk with a different output. This task lifts the walk and leaves `shep describe` behaving identically.

**Files:**
- Create: `crates/shep-cli/src/secret_readers.rs`
- Modify: `crates/shep-cli/src/lib.rs` (add `mod secret_readers;`)
- Modify: `crates/shep-cli/src/commands/query.rs:179-227` (`gather_secrets` calls the new walk)

**Interfaces:**
- Consumes: `shep_core::secrets::references`, `shep_core::paths::ShepPaths`, `shep_core::protocol::ProcessInfo`, `crate::commands::query::read_roll`.
- Produces: `crate::secret_readers::SecretNamer { name: String, environment: String, references: BTreeSet<String> }` and `pub(crate) fn namers(paths: &ShepPaths, procs: &[ProcessInfo]) -> Vec<SecretNamer>`.

- [ ] **Step 1: Write the failing test**

Create `crates/shep-cli/src/secret_readers.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_naming_no_reference_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::under(dir.path());
        let mut plain = AppConfig::minimal("plain", "./srv");
        plain.env.insert("PORT".into(), "8080".into());
        let mut secretive = AppConfig::minimal("secretive", "./srv");
        secretive
            .env
            .insert("PW".into(), "{{secret:DB_PASSWORD}}".into());
        write_roll(&paths, &[plain, secretive]);

        let found = namers(&paths, &[info("plain"), info("secretive")]);

        assert_eq!(found.len(), 1, "only the app with a reference: {found:?}");
        assert_eq!(found[0].name, "secretive");
        assert!(found[0].references.contains("DB_PASSWORD"));
    }

    #[test]
    fn an_apps_own_environment_beats_the_daemon_default() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::under(dir.path());
        let mut pinned = AppConfig::minimal("pinned", "./srv");
        pinned.environment = Some("staging".into());
        pinned.env.insert("PW".into(), "{{secret:K}}".into());
        let mut floating = AppConfig::minimal("floating", "./srv");
        floating.env.insert("PW".into(), "{{secret:K}}".into());
        write_roll(&paths, &[pinned, floating]);

        let found = namers(&paths, &[info("pinned"), info("floating")]);

        let pinned = found.iter().find(|n| n.name == "pinned").unwrap();
        let floating = found.iter().find(|n| n.name == "floating").unwrap();
        assert_eq!(pinned.environment, "staging");
        assert_eq!(
            floating.environment,
            daemon_config(&paths).daemon.environment,
            "no environment of its own falls back to the daemon's"
        );
    }

    #[test]
    fn one_entry_per_app_however_many_instances_are_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::under(dir.path());
        let mut web = AppConfig::minimal("web", "./srv");
        web.env.insert("PW".into(), "{{secret:K}}".into());
        write_roll(&paths, &[web]);

        let found = namers(&paths, &[info("web"), info("web"), info("web")]);

        assert_eq!(found.len(), 1, "three instances, one config: {found:?}");
    }
}
```

The three helpers (`info`, `write_roll`, and the `ShepPaths::under` call) must match what the repository already provides. Grep `crates/shep-cli/src/commands/query.rs`'s own test module for the fixtures it uses to build a roll and a `ProcessInfo`, and reuse those rather than inventing new ones.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p shep --lib --all-features secret_readers
```

Expected: FAIL, `cannot find function \`namers\` in this scope`.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/shep-cli/src/secret_readers.rs`:

```rust
//! Which sheep name which secret, read from the muster roll.
//!
//! The wire cannot answer this: `SheepConfigView::new` clears `env` before
//! the struct is built, and a `{{secret:...}}` reference almost always
//! lives in an env value. The roll keeps `env` verbatim while keeping the
//! reference rather than the value it resolves to.

use std::collections::BTreeSet;

use shep_core::paths::ShepPaths;
use shep_core::protocol::ProcessInfo;
use shep_core::secrets;

use crate::commands::query::read_roll;
use crate::commands::secret::daemon_config;

/// One app from the roll that names at least one secret.
///
/// `Debug` is derived: a reference is a key name, and no value reaches
/// this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretNamer {
    /// The app's name, as the roll spells it.
    pub name: String,
    /// The environment its references resolve against: its own when it has
    /// one, the daemon's default otherwise.
    pub environment: String,
    /// Every reference it names, as the operator wrote it (`KEY` or
    /// `namespace/KEY`, no braces).
    pub references: BTreeSet<String>,
}

/// Every app in `procs` that the roll has a config for and that names at
/// least one secret, deduplicated by name.
///
/// One entry per app rather than per instance: every instance of an app
/// shares one config, so three copies would say the same thing three
/// times.
///
/// Best-effort by construction. An app missing from the roll contributes
/// nothing, which is what a sheep registered since the last roll write
/// looks like.
pub(crate) fn namers(paths: &ShepPaths, procs: &[ProcessInfo]) -> Vec<SecretNamer> {
    let Some(roll) = read_roll(paths) else {
        return Vec::new();
    };
    let host_environment = daemon_config(paths).daemon.environment;
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut found = Vec::new();
    for proc in procs {
        if !seen.insert(proc.name.as_str()) {
            continue;
        }
        let Some(config) = roll
            .apps
            .iter()
            .find(|app| app.app.name == proc.name)
            .map(|app| &app.app)
        else {
            continue;
        };
        let references = secrets::references(config);
        if references.is_empty() {
            continue;
        }
        found.push(SecretNamer {
            name: proc.name.clone(),
            environment: config
                .environment
                .clone()
                .unwrap_or_else(|| host_environment.clone()),
            references,
        });
    }
    found
}
```

`read_roll` and `daemon_config` are currently private. Widen each to `pub(crate)` at its definition, and no further.

Add `mod secret_readers;` to `crates/shep-cli/src/lib.rs` beside the other private modules.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features secret_readers
```

Expected: PASS, three tests.

- [ ] **Step 5: Refactor `gather_secrets` onto the shared walk**

In `crates/shep-cli/src/commands/query.rs`, replace the body's roll walk with a call to `crate::secret_readers::namers`. The resolution loop stays where it is: `gather_secrets` still builds a `SecretView` per environment and still emits one `DescribedSecret` per (app, reference).

```rust
fn gather_secrets(
    paths: &ShepPaths,
    procs: &[ProcessInfo],
) -> (Vec<DescribedSecret>, Option<secrets::SecretError>) {
    let (store, unreadable) = match secrets::all(&paths.secrets) {
        Ok(store) => (store, None),
        Err(error) => (BTreeMap::new(), Some(error)),
    };
    let providers = secrets::provider_cache_on_disk(&paths.secrets_cache);

    let mut json = Vec::new();
    for namer in crate::secret_readers::namers(paths, procs) {
        let view = SecretView::new(namer.environment.clone(), store.clone(), providers.clone());
        for reference in &namer.references {
            let Some(parsed) = SecretRef::parse(reference) else {
                continue;
            };
            json.push(DescribedSecret {
                name: namer.name.clone(),
                reference: reference.clone(),
                environment: namer.environment.clone(),
                status: SecretStatus::from_resolution(&view.resolve(&parsed)),
            });
        }
    }
    (json, unreadable)
}
```

- [ ] **Step 6: Run `describe`'s own tests to verify nothing moved**

```bash
cargo test -p shep --lib --all-features query
```

Expected: PASS, with the pre-existing `describe` secret tests unchanged. If any of them changed shape, the refactor changed behaviour: stop and report rather than editing the test.

- [ ] **Step 7: Mutation receipt**

Break `seen.insert` into an unconditional `true` and confirm `one_entry_per_app_however_many_instances_are_running` fails. Restore. Quote the failure line.

- [ ] **Step 8: Commit**

```bash
git add crates/shep-cli/src/secret_readers.rs crates/shep-cli/src/lib.rs crates/shep-cli/src/commands/query.rs
git commit -m "refactor(cli): share the roll walk that finds who names a secret"
```

---

### Task 2: Invert the walk into readers per reference

The pane asks the opposite question from `shep describe`: given a key, which sheep name it, and is each one running.

**Files:**
- Modify: `crates/shep-cli/src/secret_readers.rs`

**Interfaces:**
- Consumes: `SecretNamer` and `namers` from Task 1.
- Produces: `pub(crate) struct Reader { name: String, environment: String, online: bool }` and `pub(crate) fn by_reference(paths: &ShepPaths, procs: &[ProcessInfo]) -> BTreeMap<String, Vec<Reader>>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn two_apps_naming_one_key_both_appear_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::under(dir.path());
    let mut catcher = AppConfig::minimal("catcher", "./srv");
    catcher.env.insert("A".into(), "{{secret:SENTRY_DSN}}".into());
    let mut web = AppConfig::minimal("web", "./srv");
    web.env.insert("B".into(), "{{secret:SENTRY_DSN}}".into());
    write_roll(&paths, &[catcher, web]);

    let map = by_reference(
        &paths,
        &[online("catcher"), stopped("web")],
    );

    let readers = map.get("SENTRY_DSN").expect("the key has readers");
    assert_eq!(readers.len(), 2);
    assert!(readers.iter().any(|r| r.name == "catcher" && r.online));
    assert!(readers.iter().any(|r| r.name == "web" && !r.online));
}

#[test]
fn readers_are_in_name_order_so_the_panel_does_not_reshuffle() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::under(dir.path());
    let mut zeta = AppConfig::minimal("zeta", "./srv");
    zeta.env.insert("A".into(), "{{secret:K}}".into());
    let mut alpha = AppConfig::minimal("alpha", "./srv");
    alpha.env.insert("A".into(), "{{secret:K}}".into());
    write_roll(&paths, &[zeta, alpha]);

    let map = by_reference(&paths, &[online("zeta"), online("alpha")]);

    let names: Vec<&str> = map["K"].iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["alpha", "zeta"]);
}
```

`online` and `stopped` build a `ProcessInfo` with `ProcStatus::Online` and `ProcStatus::Stopped`. Grep `crates/shep-cli/src/lookout/view/fixtures.rs` for `sheep_in_fold_with_status` and follow its construction rather than writing a new builder.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p shep --lib --all-features secret_readers
```

Expected: FAIL, `cannot find function \`by_reference\``.

- [ ] **Step 3: Write the implementation**

```rust
/// One sheep that names a secret, and whether it is running now.
///
/// `online` is deliberately not "holds the current value". Nothing records
/// when a value was set, so a running sheep was given *a* value at spawn
/// and may have been given an older one. The pane's caption says exactly
/// that and no more.
///
/// `Debug` is derived: a name and an environment, no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reader {
    /// The sheep's name.
    pub name: String,
    /// The environment its reference resolves against.
    pub environment: String,
    /// Whether the shepherd currently reports it `Online`.
    pub online: bool,
}

/// Every secret reference the roll names, mapped to the sheep that name
/// it, in name order.
///
/// Keys are references exactly as an operator wrote them, so a namespaced
/// one arrives as `namespace/KEY` and matches the pane's own row key for a
/// provider row.
pub(crate) fn by_reference(
    paths: &ShepPaths,
    procs: &[ProcessInfo],
) -> BTreeMap<String, Vec<Reader>> {
    let online: BTreeSet<&str> = procs
        .iter()
        .filter(|proc| proc.status == ProcStatus::Online)
        .map(|proc| proc.name.as_str())
        .collect();
    let mut map: BTreeMap<String, Vec<Reader>> = BTreeMap::new();
    for namer in namers(paths, procs) {
        for reference in &namer.references {
            map.entry(reference.clone()).or_default().push(Reader {
                name: namer.name.clone(),
                environment: namer.environment.clone(),
                online: online.contains(namer.name.as_str()),
            });
        }
    }
    for readers in map.values_mut() {
        readers.sort_by(|a, b| a.name.cmp(&b.name));
    }
    map
}
```

Add `use std::collections::BTreeMap;` and `use shep_core::status::ProcStatus;` to the module's imports.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features secret_readers
```

Expected: PASS, five tests.

- [ ] **Step 5: Mutation receipt**

Delete the `sort_by` and confirm `readers_are_in_name_order_so_the_panel_does_not_reshuffle` fails. Restore. Then ask what else could make each assertion pass, and record the answer: `namers` returning apps in roll order would satisfy the first test by accident, which is why the second test names `zeta` before `alpha` in the roll.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/secret_readers.rs
git commit -m "feat(cli): map each secret reference to the sheep that name it"
```

---

### Task 3: The pane's data model

Everything the pane draws, computed with no ratatui in sight, so the whole of it is testable without a buffer.

**Files:**
- Create: `crates/shep-cli/src/lookout/secrets.rs`
- Modify: `crates/shep-cli/src/lookout/mod.rs` (add `pub(crate) mod secrets;`)

**Interfaces:**
- Consumes: `shep_core::secrets::{all, provider_cache_on_disk, ALL_ENVIRONMENTS}`, `crate::secret_readers::{Reader, by_reference}`.
- Produces:
  - `pub(crate) enum Source { Operator, Namespace(String) }`
  - `pub(crate) struct SecretRow { key: String, source: Source, in_force: Option<String>, set_in: Vec<String>, byte_len: Option<usize>, readers: Vec<Reader> }`
  - `pub(crate) struct SecretsModel { environments: Vec<String>, rows: Vec<SecretRow>, unreadable: Option<String>, roll_age: Option<Duration> }`
  - `pub(crate) fn model(paths: &ShepPaths, procs: &[ProcessInfo], environment: &str) -> SecretsModel`
  - `SecretsModel::rows_for(&self, source: &Source) -> impl Iterator<Item = &SecretRow>`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_force_agrees_with_secret_view_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::under(dir.path());
        secrets::set(&paths.secrets, "EXACT", "production", "a").unwrap();
        secrets::set(&paths.secrets, "FALLBACK", ALL_ENVIRONMENTS, "b").unwrap();
        secrets::set(&paths.secrets, "ELSEWHERE", "ci", "c").unwrap();

        let built = model(&paths, &[], "production");
        let store = secrets::all(&paths.secrets).unwrap();
        let view = SecretView::new(
            "production".to_string(),
            store,
            secrets::provider_cache_on_disk(&paths.secrets_cache),
        );

        for row in &built.rows {
            let reference = SecretRef::parse(&row.key).unwrap();
            let resolved = matches!(view.resolve(&reference), Resolution::Found(_));
            assert_eq!(
                row.in_force.is_some(),
                resolved,
                "{} disagreed with resolve",
                row.key
            );
        }
        let by_key = |key: &str| {
            built
                .rows
                .iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("{key} missing"))
        };
        assert_eq!(by_key("EXACT").in_force.as_deref(), Some("production"));
        assert_eq!(by_key("FALLBACK").in_force.as_deref(), Some(ALL_ENVIRONMENTS));
        assert_eq!(by_key("ELSEWHERE").in_force, None);
    }

    #[test]
    fn a_named_environment_never_falls_back_to_a_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::under(dir.path());
        secrets::set(&paths.secrets, "K", "production", "live").unwrap();

        let built = model(&paths, &[], "staging");

        let row = built.rows.iter().find(|row| row.key == "K").unwrap();
        assert_eq!(row.in_force, None, "staging must not borrow production's");
        assert_eq!(row.set_in, ["production"]);
    }

    #[test]
    fn set_in_lists_every_environment_the_key_has_a_slot_for() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::under(dir.path());
        secrets::set(&paths.secrets, "K", ALL_ENVIRONMENTS, "x").unwrap();
        secrets::set(&paths.secrets, "K", "ci", "y").unwrap();

        let built = model(&paths, &[], "production");

        let row = built.rows.iter().find(|row| row.key == "K").unwrap();
        assert_eq!(row.set_in, [ALL_ENVIRONMENTS, "ci"]);
    }

    #[test]
    fn environments_are_the_union_of_the_store_plus_all() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::under(dir.path());
        secrets::set(&paths.secrets, "A", "production", "x").unwrap();
        secrets::set(&paths.secrets, "B", "ci", "y").unwrap();

        let built = model(&paths, &[], "production");

        assert_eq!(built.environments, [ALL_ENVIRONMENTS, "ci", "production"]);
    }

    #[test]
    fn an_unreadable_store_reports_rather_than_reading_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::under(dir.path());
        std::fs::write(&paths.secrets, "{\"version\":9999,\"entries\":{}}").unwrap();

        let built = model(&paths, &[], "production");

        assert!(built.rows.is_empty());
        assert!(
            built.unreadable.as_deref().is_some_and(|m| m.contains("9999")),
            "got {:?}",
            built.unreadable
        );
    }

    #[test]
    fn byte_len_is_the_values_length_and_never_the_value() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::under(dir.path());
        secrets::set(&paths.secrets, "K", "production", "hunter2").unwrap();

        let built = model(&paths, &[], "production");

        let row = built.rows.iter().find(|row| row.key == "K").unwrap();
        assert_eq!(row.byte_len, Some(7));
        assert!(
            !format!("{row:?}").contains("hunter2"),
            "the row must not carry the value: {row:?}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p shep --lib --all-features lookout::secrets
```

Expected: FAIL, `cannot find function \`model\``.

- [ ] **Step 3: Write the implementation**

```rust
//! What the secrets pane draws, computed off the files it reads.
//!
//! No value reaches this module. A row carries a length, and the value
//! itself is fetched only by an explicit reveal, which is Task 6's job.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use shep_core::paths::ShepPaths;
use shep_core::protocol::ProcessInfo;
use shep_core::secrets::{self, ALL_ENVIRONMENTS};

use crate::secret_readers::{self, Reader};

/// Which store a row came from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Source {
    /// `secrets.json`, the operator's own. Writable.
    Operator,
    /// One provider dog's pushed values, out of `secrets-cache.json`.
    /// Read-only: they are a cache of what the dog said.
    Namespace(String),
}

/// One key, as this pane's current environment tab sees it.
///
/// `Debug` is derived and safe: `byte_len` is a length, and no field holds
/// a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretRow {
    /// The key, `namespace/KEY` for a provider row.
    pub key: String,
    /// Which store it came from.
    pub source: Source,
    /// The environment slot supplying this tab's value: the tab's own
    /// name, [`ALL_ENVIRONMENTS`], or `None` when nothing resolves here.
    ///
    /// Mirrors `SecretView::resolve`'s order, which is exact environment,
    /// then `all`, then nothing, and never another named environment.
    pub in_force: Option<String>,
    /// Every environment with a slot for this key, in name order.
    pub set_in: Vec<String>,
    /// The in-force value's length in bytes, or `None` when nothing
    /// resolves here.
    pub byte_len: Option<usize>,
    /// The sheep that name this key, in name order.
    pub readers: Vec<Reader>,
}

/// Everything the pane needs for one environment tab.
#[derive(Debug, Clone, Default)]
pub(crate) struct SecretsModel {
    /// Every environment the store holds a slot for, plus
    /// [`ALL_ENVIRONMENTS`], in name order. The tab row.
    pub environments: Vec<String>,
    /// Operator rows first, then each namespace's, keys in order within
    /// each.
    pub rows: Vec<SecretRow>,
    /// Why the operator's store would not read, when it would not.
    pub unreadable: Option<String>,
    /// How old the muster roll is, or `None` when it is missing.
    pub roll_age: Option<Duration>,
}

impl SecretsModel {
    /// This model's rows from one store, in key order.
    pub fn rows_for<'a>(&'a self, source: &'a Source) -> impl Iterator<Item = &'a SecretRow> {
        self.rows.iter().filter(move |row| &row.source == source)
    }
}

/// Builds the model for `environment`.
///
/// Best-effort throughout. An unreadable operator store reports itself and
/// leaves the provider rows alone; a missing roll costs the readers and
/// nothing else.
pub(crate) fn model(
    paths: &ShepPaths,
    procs: &[ProcessInfo],
    environment: &str,
) -> SecretsModel {
    let (store, unreadable) = match secrets::all(&paths.secrets) {
        Ok(store) => (store, None),
        Err(error) => (BTreeMap::new(), Some(error.to_string())),
    };
    let providers = secrets::provider_cache_on_disk(&paths.secrets_cache);
    let readers = secret_readers::by_reference(paths, procs);

    let mut environments: BTreeSet<String> = BTreeSet::new();
    environments.insert(ALL_ENVIRONMENTS.to_string());
    for slots in store.values() {
        environments.extend(slots.keys().cloned());
    }

    let mut rows = Vec::new();
    for (key, slots) in &store {
        rows.push(row(key.clone(), Source::Operator, slots, environment, &readers));
    }
    for (namespace, keys) in &providers.values {
        for (key, slots) in keys {
            let qualified = format!("{namespace}/{key}");
            rows.push(row(
                qualified,
                Source::Namespace(namespace.clone()),
                slots,
                environment,
                &readers,
            ));
        }
    }

    SecretsModel {
        environments: environments.into_iter().collect(),
        rows,
        unreadable,
        roll_age: secret_readers::roll_age(paths),
    }
}

/// One row from one key's environment slots.
fn row(
    key: String,
    source: Source,
    slots: &BTreeMap<String, String>,
    environment: &str,
    readers: &BTreeMap<String, Vec<Reader>>,
) -> SecretRow {
    let in_force = if slots.contains_key(environment) {
        Some(environment.to_string())
    } else if slots.contains_key(ALL_ENVIRONMENTS) {
        Some(ALL_ENVIRONMENTS.to_string())
    } else {
        None
    };
    let byte_len = in_force
        .as_ref()
        .and_then(|slot| slots.get(slot))
        .map(String::len);
    SecretRow {
        byte_len,
        in_force,
        set_in: slots.keys().cloned().collect(),
        readers: readers.get(&key).cloned().unwrap_or_default(),
        key,
        source,
    }
}
```

Add to `crates/shep-cli/src/secret_readers.rs`:

```rust
/// How long ago the muster roll was written, or `None` when it is missing
/// or unreadable.
///
/// The pane states this because a failed roll write only warns, so a stale
/// roll is otherwise silent. A roll from the future reads as zero rather
/// than as an error: a clock that moved is not the operator's problem to
/// solve from this screen.
pub(crate) fn roll_age(paths: &ShepPaths) -> Option<Duration> {
    let roll = read_roll(paths)?;
    let now = shep_core::now_ms();
    Some(Duration::from_millis(now.saturating_sub(roll.saved_at_ms)))
}
```

Grep for the real name of the current-milliseconds helper before writing that line: `crates/shep-daemon/src/snapshot.rs` calls `crate::now_ms()`, and the equivalent reachable from `shep-cli` may differ.

Add `pub(crate) mod secrets;` to `crates/shep-cli/src/lookout/mod.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features lookout::secrets
```

Expected: PASS, six tests.

- [ ] **Step 5: Mutation receipt**

Three mutations, each with its own expected failure:

1. Swap `in_force`'s two branches so `all` is checked first. `in_force_agrees_with_secret_view_resolve` must fail on `EXACT`.
2. Add a third branch falling back to any remaining slot. `a_named_environment_never_falls_back_to_a_sibling` must fail.
3. Change `byte_len` to `Some(0)`. `byte_len_is_the_values_length_and_never_the_value` must fail.

Restore after each and quote all three failure lines.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/secrets.rs crates/shep-cli/src/lookout/mod.rs crates/shep-cli/src/secret_readers.rs
git commit -m "feat(lookout): model the secrets pane's rows off the store and the roll"
```

---

### Task 4: Open and close the pane

`S` opens the pane and `S` closes it, mirroring the settings screen. Nothing is drawn yet: this task is the reducer path and the load.

**Files:**
- Modify: `crates/shep-cli/src/lookout/input.rs` (bind `S`, `left`, `right`, and route `z`)
- Modify: `crates/shep-cli/src/lookout/app.rs` (`KeyPress::Secrets`, `Body::Secrets`, `SecretsPane`, `Effect::LoadSecrets`, `Msg::Secrets`)
- Modify: `crates/shep-cli/src/lookout/mod.rs` (run the effect on `spawn_blocking`)

**Interfaces:**
- Consumes: `crate::lookout::secrets::{SecretsModel, model}` from Task 3.
- Produces: `Body::Secrets(SecretsPane)`, where `SecretsPane { model: SecretsModel, tab: usize, selected: usize, collapsed: HashSet<String>, reveal: Option<Reveal>, armed: Option<String>, typing: Option<Typing> }`, plus `KeyPress::TabPrev` and `KeyPress::TabNext`. Tasks 6, 7 and 8 fill `reveal`, `armed` and `typing`; this task creates them as `None` and nothing reads them yet.

The tab keys land here rather than with the panels, because this task already owns `tab` and Task 6's reveal has to clear on a tab move. `z` reuses the existing `KeyPress::Collapse` and needs no new variant.

- [ ] **Step 1: Write the failing test**

In `crates/shep-cli/src/lookout/input.rs`'s test module:

```rust
#[test]
fn capital_s_opens_the_secrets_pane_and_lower_s_still_opens_settings() {
    assert_eq!(
        map_key(&key(KeyCode::Char('S')), InputMode::Normal),
        Some(KeyPress::Secrets)
    );
    assert_eq!(
        map_key(&key(KeyCode::Char('s')), InputMode::Normal),
        Some(KeyPress::Settings),
        "the settings screen keeps its own key"
    );
    assert_eq!(
        map_key(&key(KeyCode::Char('g')), InputMode::Normal),
        Some(KeyPress::SelectFirst),
        "the frame wanted `g` for secrets; `g` is still SelectFirst"
    );
}

#[test]
fn the_arrow_keys_move_the_environment_tab() {
    assert_eq!(
        map_key(&key(KeyCode::Left), InputMode::Normal),
        Some(KeyPress::TabPrev)
    );
    assert_eq!(
        map_key(&key(KeyCode::Right), InputMode::Normal),
        Some(KeyPress::TabNext)
    );
    assert_eq!(
        map_key(&key(KeyCode::Up), InputMode::Normal),
        Some(KeyPress::SelectUp),
        "the vertical arrows keep the meaning they already have"
    );
}
```

In `crates/shep-cli/src/lookout/app.rs`'s test module:

```rust
#[test]
fn capital_s_opens_the_pane_and_pressing_it_again_closes_it() {
    let mut app = fixtures::full_app();

    let effect = app.on_key(KeyPress::Secrets);

    assert!(matches!(effect, Effect::LoadSecrets));
    assert!(matches!(app.body(), Body::Secrets(_)), "pane is open");

    let effect = app.on_key(KeyPress::Secrets);

    assert!(matches!(effect, Effect::None));
    assert!(matches!(app.body(), Body::FlockTable), "pane is closed");
}

#[test]
fn escape_closes_the_pane_too() {
    let mut app = fixtures::full_app();
    app.on_key(KeyPress::Secrets);

    app.on_key(KeyPress::Escape);

    assert!(matches!(app.body(), Body::FlockTable));
}

#[test]
fn the_tab_moves_and_stops_at_both_ends() {
    let mut app = fixtures::full_app();
    app.on_key(KeyPress::Secrets);
    app.on_msg(Msg::Secrets { model: Box::new(fixtures::three_environment_model()) });

    app.on_key(KeyPress::TabPrev);
    assert_eq!(fixtures::tab_of(&app), 0, "the first tab does not wrap");

    for _ in 0..6 {
        app.on_key(KeyPress::TabNext);
    }
    assert_eq!(fixtures::tab_of(&app), 2, "the last of three does not wrap");
}

#[test]
fn a_tab_move_reloads_because_in_force_is_per_environment() {
    let mut app = fixtures::full_app();
    app.on_key(KeyPress::Secrets);
    app.on_msg(Msg::Secrets { model: Box::new(fixtures::three_environment_model()) });

    let effect = app.on_key(KeyPress::TabNext);

    assert!(
        matches!(effect, Effect::LoadSecrets),
        "every row's IN FORCE, VALUE and byte length belong to one \
         environment, so the tab cannot move without rebuilding them"
    );
}

#[test]
fn z_collapses_a_namespace_group_and_leaves_its_header() {
    let mut app = fixtures::app_with_a_pushed_secret();
    app.on_key(KeyPress::Secrets);
    fixtures::select_row(&mut app, "vercel/API_TOKEN");

    app.on_key(KeyPress::Collapse);

    assert!(
        fixtures::collapsed_of(&app).contains("vercel"),
        "the group is collapsed"
    );

    app.on_key(KeyPress::Collapse);

    assert!(fixtures::collapsed_of(&app).is_empty(), "and pressing it again undoes that");
}

#[test]
fn a_late_model_for_a_closed_pane_does_not_reopen_it() {
    let mut app = fixtures::full_app();
    app.on_key(KeyPress::Secrets);
    app.on_key(KeyPress::Escape);

    app.on_msg(Msg::Secrets {
        model: Box::new(SecretsModel::default()),
    });

    assert!(
        matches!(app.body(), Body::FlockTable),
        "a reply that outlived its pane must be dropped"
    );
}
```

`app.body()` may not exist. Grep for how the existing settings tests inspect `App::body` and follow that, adding a `#[cfg(test)]` accessor only if there is no other way.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: secrets
```

Expected: FAIL, `no variant named \`Secrets\``.

- [ ] **Step 3: Write the implementation**

`crates/shep-cli/src/lookout/input.rs`, in the normal-mode match, beside the `'s'` arm:

```rust
        KeyCode::Char('S') => Some(KeyPress::Secrets),
        KeyCode::Left => Some(KeyPress::TabPrev),
        KeyCode::Right => Some(KeyPress::TabNext),
```

`Up` and `Down` are already bound to the selection and stay that way; only the
horizontal pair is new.

`crates/shep-cli/src/lookout/app.rs`, a new `KeyPress` variant:

```rust
    /// `S`: opens the secrets pane, or closes it from inside.
    ///
    /// Picked rather than found. The design named `g`, which is
    /// [`Self::SelectFirst`]; `s` is the settings screen and `S` is its
    /// neighbour, both change-screens over the shepherd's own
    /// configuration.
    Secrets,
    /// `Left`: the previous environment tab, in the secrets pane. Stops at
    /// the first rather than wrapping. Ignored elsewhere.
    TabPrev,
    /// `Right`: the next one, stopping at the last.
    TabNext,
```

A tab move rebuilds the model, because `in_force`, the value and its byte length
are all per environment. So both arms clamp the index and return
[`Effect::LoadSecrets`], and `z` toggles the selected row's namespace in
`SecretsPane::collapsed` through the existing [`KeyPress::Collapse`].

A new `Body` variant:

```rust
    /// The secrets pane, opened by [`KeyPress::Secrets`].
    Secrets(SecretsPane),
```

The pane's own state:

```rust
/// The secrets pane's state.
///
/// `Debug` is manual (IR-41): [`Self::reveal`] and [`Self::typing`] carry
/// an operator's plaintext.
pub(crate) struct SecretsPane {
    /// Everything drawn, rebuilt by every load.
    pub model: Box<SecretsModel>,
    /// Which environment tab is showing, an index into
    /// [`SecretsModel::environments`].
    pub tab: usize,
    /// Which row the panels describe, an index into
    /// [`SecretsModel::rows`].
    pub selected: usize,
    /// The value on screen and when it leaves, or `None`.
    pub reveal: Option<Reveal>,
    /// The key whose deletion is armed, or `None`. While this is set,
    /// `Enter` confirms the delete rather than opening the value input.
    pub armed: Option<String>,
    /// The open text input, or `None`.
    pub typing: Option<Typing>,
}

/// Redacted (IR-41): `reveal` and `typing` hold a plaintext value.
impl fmt::Debug for SecretsPane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretsPane")
            .field("rows", &self.model.rows.len())
            .field("tab", &self.tab)
            .field("selected", &self.selected)
            .field("revealing", &self.reveal.is_some())
            .field("armed", &self.armed)
            .field("typing", &self.typing.is_some())
            .finish()
    }
}
```

Declare `Reveal` and `Typing` now as empty-enough placeholders so this task compiles, and let Tasks 6 and 7 fill them:

```rust
/// A value on screen, and the instant it leaves.
pub(crate) struct Reveal {
    /// The key it belongs to.
    pub key: String,
    /// The plaintext.
    pub value: String,
    /// When it clears.
    pub until: Instant,
}

/// Redacted (IR-41): `value` is the secret.
impl fmt::Debug for Reveal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reveal")
            .field("key", &self.key)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .finish_non_exhaustive()
    }
}

/// An open text input in the secrets pane.
pub(crate) struct Typing {
    /// What is being typed: a new key's name, or a value for a key.
    pub what: TypingWhat,
    /// The buffer.
    pub buffer: String,
}

/// Redacted (IR-41): a value buffer is the secret being typed.
impl fmt::Debug for Typing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Typing")
            .field("what", &self.what)
            .field("buffer", &format_args!("<{} bytes>", self.buffer.len()))
            .finish()
    }
}

/// Which of the pane's two inputs is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypingWhat {
    /// The `+ new key` row's name input.
    NewKey,
    /// A value for the named key, in the current tab's environment.
    ValueFor(String),
}
```

A new `Effect` and a new `Msg`:

```rust
    /// Read the secret store, the provider cache and the roll; the result
    /// lands as [`Msg::Secrets`].
    ///
    /// Runs on `spawn_blocking` for [`Self::WriteSetting`]'s reason: the
    /// store's own lock acquires with no deadline.
    LoadSecrets,
```

```rust
    /// A secrets model came back. Dropped when no pane is open, the way a
    /// late config reply is.
    Secrets {
        /// The model, boxed: it is much larger than every other variant.
        model: Box<SecretsModel>,
    },
```

The reducer arms, beside the settings ones:

```rust
            KeyPress::Secrets => match self.body {
                Body::Secrets(_) => {
                    self.body = Body::FlockTable;
                    Effect::None
                }
                _ => {
                    self.body = Body::Secrets(SecretsPane {
                        model: Box::default(),
                        tab: 0,
                        selected: 0,
                        reveal: None,
                        armed: None,
                        typing: None,
                    });
                    Effect::LoadSecrets
                }
            },
```

```rust
            Msg::Secrets { model } => {
                if let Body::Secrets(pane) = &mut self.body {
                    pane.model = model;
                    pane.selected = pane.selected.min(pane.model.rows.len().saturating_sub(1));
                }
            }
```

`Escape` already closes a body; extend its existing match arm to cover `Body::Secrets` rather than adding a second path.

`crates/shep-cli/src/lookout/mod.rs`, beside the `Effect::LoadSettings` arm:

```rust
            Effect::LoadSecrets => {
                let paths = paths.clone();
                let procs = app.flock_infos();
                let environment = app.secrets_tab_environment();
                let handle = tokio::task::spawn_blocking(move || {
                    crate::lookout::secrets::model(&paths, &procs, &environment)
                });
                // The join handle becomes a `Msg` exactly as
                // `Effect::LoadSettings`'s does.
            }
```

The line after `spawn_blocking` is deliberately not written out here, because the surrounding code is the authority on it. Open the `Effect::LoadSettings` arm at `crates/shep-cli/src/lookout/mod.rs:426`, read how its handle reaches `Msg::Settings`, and repeat that shape with `Msg::Secrets`. If `LoadSettings` turns out to do something other than what this plan assumes, follow the file and say so in the DONE report.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: secrets
```

Expected: PASS.

- [ ] **Step 5: Prove the redaction**

```rust
#[test]
fn the_pane_debug_never_prints_a_revealed_value() {
    let pane = SecretsPane {
        model: Box::default(),
        tab: 0,
        selected: 0,
        reveal: Some(Reveal {
            key: "K".into(),
            value: "hunter2".into(),
            until: Instant::now(),
        }),
        armed: None,
        typing: Some(Typing {
            what: TypingWhat::ValueFor("K".into()),
            buffer: "hunter2".into(),
        }),
    };

    let printed = format!("{pane:?}");

    assert_eq!(
        printed,
        "SecretsPane { rows: 0, tab: 0, selected: 0, revealing: true, armed: None, typing: true }"
    );
    assert!(!format!("{:?}", pane.reveal).contains("hunter2"));
    assert!(!format!("{:?}", pane.typing).contains("hunter2"));
}
```

Exact-string, so restoring a derived `Debug` fails this test rather than silently reopening the leak (IR-41).

- [ ] **Step 6: Mutation receipt**

Replace the manual `Debug for Reveal` with `#[derive(Debug)]` and confirm the test fails. Restore.

- [ ] **Step 7: Commit**

```bash
git add crates/shep-cli/src/lookout/input.rs crates/shep-cli/src/lookout/app.rs crates/shep-cli/src/lookout/mod.rs
git commit -m "feat(lookout): open the secrets pane on S"
```

---

### Task 5: Draw the pane

**Files:**
- Create: `crates/shep-cli/src/lookout/view/secrets.rs`
- Modify: `crates/shep-cli/src/lookout/view/mod.rs:228-261` (dispatch `Body::Secrets`)

**Interfaces:**
- Consumes: `SecretsModel`, `SecretRow`, `Source`, `SecretsPane`.
- Produces: `pub(super) fn draw(buffer: &mut Buffer, area: Rect, pane: &SecretsPane, palette: &Palette)`, `pub(super) const SECRET_TIERS`, `pub(super) fn columns_for(width: u16) -> &'static [Column]`, and `#[cfg(test)] pub(super) fn cell(buffer: &Buffer, row: u16, column: Column) -> String`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_tier_fits_the_width_it_claims() {
    for (threshold, columns) in SECRET_TIERS {
        let spent: u16 = columns.iter().map(|column| column.width()).sum();
        assert!(
            spent + GUTTER <= *threshold,
            "tier {threshold} spends {spent} plus {GUTTER} of gutter"
        );
    }
}

#[test]
fn the_columns_drop_in_the_specified_order() {
    let widest = columns_for(160);
    assert!(widest.contains(&Column::Lands));
    assert!(!columns_for(118).contains(&Column::Lands), "LANDS goes first");
    assert!(columns_for(118).contains(&Column::ReadBy));
    assert!(!columns_for(96).contains(&Column::ReadBy), "READ BY goes second");
    assert!(!columns_for(74).contains(&Column::SetIn), "SET IN goes third");
    for width in [160, 118, 96, 74, 40] {
        let columns = columns_for(width);
        assert!(columns.contains(&Column::Key), "KEY is the pane at {width}");
        assert!(columns.contains(&Column::Value), "VALUE is the pane at {width}");
        assert!(
            columns.contains(&Column::InForce),
            "IN FORCE is the pane at {width}"
        );
    }
}

#[test]
fn in_force_is_read_out_of_its_own_column_not_found_anywhere_in_the_row() {
    let mut app = fixtures::app_with_secrets();
    app.on_key(KeyPress::Secrets);
    let buffer = fixtures::render(&app, 160, 48);

    // `production` is also the tab label and appears in SET IN. Reading the
    // cell rather than the row is what makes this assertion mean anything.
    assert_eq!(cell(&buffer, first_row(), Column::InForce).trim(), "production");
}

#[test]
fn a_key_with_no_slot_here_says_so_rather_than_showing_a_block_run() {
    let mut app = fixtures::app_with_secrets();
    app.on_key(KeyPress::Secrets);
    let buffer = fixtures::render(&app, 160, 48);
    let row = row_of(&buffer, "ELSEWHERE_ONLY");

    assert_eq!(cell(&buffer, row, Column::Value).trim(), "not set here");
    assert_eq!(cell(&buffer, row, Column::InForce).trim(), "-");
}

#[test]
fn a_four_kilobyte_value_never_overflows_its_column() {
    let mut app = fixtures::app_with_a_maximum_length_secret();
    app.on_key(KeyPress::Secrets);
    let buffer = fixtures::render(&app, 160, 48);
    let value = cell(&buffer, first_row(), Column::Value);

    assert!(
        value.chars().count() <= Column::Value.width() as usize,
        "spilled: {value:?}"
    );
    assert!(value.contains("4096 bytes"), "the exact length is stated: {value:?}");
}

#[test]
fn a_provider_group_says_it_is_read_only() {
    let mut app = fixtures::app_with_a_pushed_secret();
    app.on_key(KeyPress::Secrets);
    let buffer = fixtures::render(&app, 160, 48);
    let text = fixtures::rows_of(&buffer);

    assert!(
        text.iter().any(|line| line.contains("read-only here")),
        "the group header states it: {text:?}"
    );
}
```

The three fixtures (`app_with_secrets`, `app_with_a_maximum_length_secret`, `app_with_a_pushed_secret`) go in `crates/shep-cli/src/lookout/view/fixtures.rs` beside the existing ones and follow their construction. `app_with_secrets` holds `DB_PASSWORD` set for `production`, `ELSEWHERE_ONLY` set only for `ci`, and the tab on `production`.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p shep --lib --all-features view::secrets
```

Expected: FAIL, the module does not exist.

- [ ] **Step 3: Write the implementation**

```rust
//! The secrets pane, drawn straight into the buffer: this screen owns the
//! whole body between the title band and the status bar.
//!
//! Every cell goes through [`fit`], so a long key ends in `…` rather than
//! spilling into the next column.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::super::app::SecretsPane;
use super::super::secrets::{SecretRow, Source};
use super::super::theme::Palette;
use super::flock::{GUTTER, fit, mark};

/// One column of the secrets table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Column {
    /// The key, `namespace/KEY` for a provider row.
    Key,
    /// A block run and the in-force value's exact byte count, or the
    /// revealed plaintext.
    Value,
    /// Which environment slot supplies this tab's value.
    InForce,
    /// Every environment with a slot, and how many that is.
    SetIn,
    /// How many sheep name this key, and how many are running.
    ReadBy,
    /// When a change reaches a process, or a reveal's countdown.
    Lands,
}

impl Column {
    /// This column's width in cells at the design tier.
    pub(super) const fn width(self) -> u16 {
        match self {
            Self::Key => 28,
            Self::Value => 30,
            Self::InForce => 14,
            Self::SetIn => 22,
            Self::ReadBy => 22,
            Self::Lands => 42,
        }
    }

    /// The heading printed over it.
    pub(super) const fn heading(self) -> &'static str {
        match self {
            Self::Key => "KEY",
            Self::Value => "VALUE",
            Self::InForce => "IN FORCE",
            Self::SetIn => "SET IN",
            Self::ReadBy => "READ BY",
            Self::Lands => "LANDS",
        }
    }
}

/// The columns each width still fits, widest first.
///
/// Each threshold is the narrowest terminal that still fits its row, which
/// `every_tier_fits_the_width_it_claims` holds to. `LANDS` drops first,
/// then `READ BY`, then `SET IN`. The remaining three are the pane: a key
/// with no value and no scope answers nothing.
pub(super) const SECRET_TIERS: &[(u16, &[Column])] = &[
    (
        160,
        &[
            Column::Key,
            Column::Value,
            Column::InForce,
            Column::SetIn,
            Column::ReadBy,
            Column::Lands,
        ],
    ),
    (
        118,
        &[
            Column::Key,
            Column::Value,
            Column::InForce,
            Column::SetIn,
            Column::ReadBy,
        ],
    ),
    (
        96,
        &[Column::Key, Column::Value, Column::InForce, Column::SetIn],
    ),
    (74, &[Column::Key, Column::Value, Column::InForce]),
];

/// The widest tier `width` still fits, or the narrowest tier below every
/// threshold.
pub(super) fn columns_for(width: u16) -> &'static [Column] {
    SECRET_TIERS
        .iter()
        .find(|(threshold, _)| width >= *threshold)
        .map_or(SECRET_TIERS[SECRET_TIERS.len() - 1].1, |(_, columns)| *columns)
}

/// What one row shows in `VALUE`.
///
/// A run proportional to the value's length rather than equal to it: the
/// column is 30 cells and `MAX_VALUE_BYTES` is 4096, so an equal run
/// cannot be drawn. The byte count carries the exact figure, which is what
/// the design's second rule asks for.
fn value_cell(row: &SecretRow, revealed: Option<&str>, width: u16) -> String {
    if let Some(plain) = revealed {
        return fit(plain, width);
    }
    let Some(len) = row.byte_len else {
        return "not set here".to_string();
    };
    let suffix = format!(" {len} bytes");
    let run = usize::from(width).saturating_sub(suffix.len()).min(len);
    format!("{}{suffix}", "█".repeat(run.max(1)))
}
```

The drawing itself follows `crates/shep-cli/src/lookout/view/settings.rs` row for row: a band, the terms rows, the tab row, the heading row, a hairline, then group headers and rows. Read that file and mirror its structure rather than inventing a second layout idiom.

Two glyph rules, both already settled in this repository:

- The selection uses `flock::mark` and `flock::edge`, never `▌`. `crates/shep-cli/src/lookout/view/flock.rs:75` records why: every block glyph here is East-Asian Ambiguous, and a doubled cell in the gutter shifts the whole row.
- The active tab is bracketed, `[production]`, in addition to whatever the palette paints. A tab marked by colour alone says nothing under `NO_COLOR`.

The test helper, which is the point of the task:

```rust
/// One cell's text, read at its own column offset.
///
/// Assertions go through this rather than searching the whole row.
/// `production` is the tab label, a `SET IN` entry and an `IN FORCE`
/// value at the same time, so a row-wide `contains` would pass on any of
/// the three.
#[cfg(test)]
pub(super) fn cell(buffer: &Buffer, row: u16, column: Column) -> String {
    let columns = columns_for(buffer.area.width);
    let mut x = GUTTER;
    for candidate in columns {
        if *candidate == column {
            return (x..x + column.width())
                .map(|x| buffer[(x, row)].symbol())
                .collect();
        }
        x += candidate.width();
    }
    panic!("{column:?} is not drawn at width {}", buffer.area.width);
}
```

`crates/shep-cli/src/lookout/view/mod.rs`, beside the other `Body` arms:

```rust
        Body::Secrets(pane) => {
            secrets::draw(buffer, body, pane, palette);
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features view::secrets
```

Expected: PASS, six tests.

- [ ] **Step 5: Mutation receipt**

1. Change `Column::InForce`'s width to 13 and confirm `every_tier_fits_the_width_it_claims` still passes but `in_force_is_read_out_of_its_own_column_not_found_anywhere_in_the_row` fails, which is the whole reason the positional helper exists.
2. Drop `SetIn` before `ReadBy` in the tiers and confirm `the_columns_drop_in_the_specified_order` fails.
3. Remove the `.min(len)` from `value_cell` and confirm `a_four_kilobyte_value_never_overflows_its_column` fails.

Restore after each, quote all three failure lines.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/view/secrets.rs crates/shep-cli/src/lookout/view/mod.rs crates/shep-cli/src/lookout/view/fixtures.rs
git commit -m "feat(lookout): draw the secrets table, its groups and its tiers"
```

---

### Task 6: Reveal a value for ten seconds

**Files:**
- Modify: `crates/shep-cli/src/lookout/input.rs` (bind `v`)
- Modify: `crates/shep-cli/src/lookout/app.rs` (`KeyPress::Reveal`, the reveal arms, the tick)
- Modify: `crates/shep-cli/src/lookout/view/secrets.rs` (countdown in `LANDS`)

**Interfaces:**
- Consumes: `Reveal` from Task 4, `SettingsSnapshot`'s `[secrets] allow_read`.
- Produces: `KeyPress::Reveal`, `App::reveal_gate_open() -> bool`.

- [ ] **Step 1: Write the failing test**

```rust
const REVEAL_HOLDS: Duration = Duration::from_secs(10);

#[test]
fn v_reveals_only_when_allow_read_is_on() {
    let mut shut = fixtures::app_with_secrets_and_reads(false);
    shut.on_key(KeyPress::Secrets);

    shut.on_key(KeyPress::Reveal);

    assert!(fixtures::reveal_of(&shut).is_none(), "the gate is shut");
    assert!(
        fixtures::notice_of(&shut).is_some_and(|n| n.contains("allow_read")),
        "and it says which gate and where"
    );

    let mut open = fixtures::app_with_secrets_and_reads(true);
    open.on_key(KeyPress::Secrets);

    open.on_key(KeyPress::Reveal);

    assert_eq!(fixtures::reveal_of(&open).map(|r| r.key.as_str()), Some("DB_PASSWORD"));
}

#[test]
fn a_reveal_clears_on_every_one_of_its_six_triggers_that_exist_yet() {
    for (name, press) in [
        ("selection", Some(KeyPress::SelectDown)),
        ("tab", Some(KeyPress::TabNext)),
        ("escape", Some(KeyPress::Escape)),
        ("close", Some(KeyPress::Secrets)),
        ("refresh", Some(KeyPress::Refresh)),
    ] {
        let mut app = fixtures::app_revealing();
        app.on_key(press.unwrap());
        assert!(
            fixtures::reveal_of(&app).is_none(),
            "{name} left the value on screen"
        );
    }

    let mut timed = fixtures::app_revealing();
    let start = timed.now();
    timed.on_msg(Msg::Tick { at: start + REVEAL_HOLDS });
    assert!(fixtures::reveal_of(&timed).is_none(), "timeout left it on screen");
}

#[test]
fn a_reveal_survives_the_tick_before_it_expires() {
    let mut app = fixtures::app_revealing();
    let start = app.now();

    app.on_msg(Msg::Tick { at: start + REVEAL_HOLDS - Duration::from_millis(1) });

    assert!(
        fixtures::reveal_of(&app).is_some(),
        "clearing early makes the countdown a lie"
    );
}
```

The last test is the one that stops a "clear on every tick" implementation passing the previous test for the wrong reason.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: reveal
```

Expected: FAIL, `no variant named \`Reveal\``.

- [ ] **Step 3: Write the implementation**

Bind `v` in `input.rs`:

```rust
        KeyCode::Char('v') => Some(KeyPress::Reveal),
```

```rust
    /// `v`: shows the selected secret's value for ten seconds, in the
    /// secrets pane. Refuses when `[secrets] allow_read` is off, naming
    /// the gate. Ignored on every other screen.
    Reveal,
```

The reveal reads the one value it needs rather than holding every value in the model:

```rust
/// How long a revealed value stays on screen.
///
/// The pane prints this number, so the two cannot drift.
pub(crate) const REVEAL_HOLDS: Duration = Duration::from_secs(10);
```

The clear sites are the point of this task. Rather than seven scattered `pane.reveal = None` lines, one method that every trigger calls:

```rust
impl SecretsPane {
    /// Takes the value off the screen.
    ///
    /// One method rather than an assignment at each trigger: a new trigger
    /// added later has one thing to call, and the seven that exist cannot
    /// drift apart.
    pub(crate) fn hide(&mut self) {
        self.reveal = None;
    }
}
```

Call it from: `SelectUp`, `SelectDown`, `SelectFirst`, `SelectLast`, `TabPrev`, `TabNext`, `Escape`, the pane-closing arm of `Secrets`, `Refresh`, and the `Msg::Tick` arm once `now >= until`. The seventh trigger is a successful write, whose message does not exist until Task 7; that task adds the call and extends this test.

The gate is read off the settings snapshot the app already loads. Grep for how `App` reaches `SettingsSnapshot` before writing `reveal_gate_open`; the pane must not read `shep.toml` a second time on its own.

The countdown in `LANDS` states seconds and draws a ten-cell gauge, both from the same remaining duration, so the words and the blocks cannot disagree.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: reveal
```

Expected: PASS.

- [ ] **Step 5: Mutation receipt**

Remove the `hide()` call from the tab arm and confirm the seven-trigger test fails naming `tab`. Then change the tick comparison from `>=` to `>` and confirm nothing fails, which shows the boundary is untested: add the equality case to `a_reveal_survives_the_tick_before_it_expires`'s sibling assertion before restoring.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/input.rs crates/shep-cli/src/lookout/app.rs crates/shep-cli/src/lookout/view/secrets.rs
git commit -m "feat(lookout): reveal a secret for ten seconds behind allow_read"
```

---

### Task 7: Set a value

**Files:**
- Modify: `crates/shep-cli/src/lookout/app.rs`
- Modify: `crates/shep-cli/src/lookout/mod.rs` (run `Effect::WriteSecret`)
- Modify: `crates/shep-cli/src/lookout/view/secrets.rs` (the input chip and the `+ new key` row)

**Interfaces:**
- Consumes: `Typing`, `TypingWhat`, `WriteAuthority`.
- Produces: `Effect::WriteSecret(SecretEdit, WriteAuthority)`, `Msg::SecretWritten { result }`, `pub(crate) struct SecretEdit { key: String, environment: String, value: Option<String> }` where `None` means delete (Task 8 uses it).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn enter_opens_the_value_input_seeded_empty() {
    let mut app = fixtures::app_with_secrets_and_control();
    app.on_key(KeyPress::Secrets);

    app.on_key(KeyPress::Confirm);

    let typing = fixtures::typing_of(&app).expect("the input is open");
    assert_eq!(typing.what, TypingWhat::ValueFor("DB_PASSWORD".into()));
    assert_eq!(
        typing.buffer, "",
        "seeding it with the stored value would put a secret on screen \
         that `v` and its gate exist to control"
    );
}

#[test]
fn a_write_refuses_without_the_control_gate() {
    let mut app = fixtures::app_with_secrets_read_only();
    app.on_key(KeyPress::Secrets);

    app.on_key(KeyPress::Confirm);

    assert!(fixtures::typing_of(&app).is_none());
    assert!(
        fixtures::notice_of(&app).is_some_and(|n| n.contains("read-only")),
        "the existing refusal, not a second one"
    );
}

#[test]
fn a_value_over_the_cap_is_refused_at_the_input_not_at_the_file() {
    let mut app = fixtures::app_typing_a_value();
    for _ in 0..=MAX_VALUE_BYTES {
        app.on_key(KeyPress::TextChar('x'));
    }

    let effect = app.on_key(KeyPress::TextApply);

    assert!(matches!(effect, Effect::None), "nothing reached the file");
    assert!(fixtures::notice_of(&app).is_some_and(|n| n.contains("4096")));
}

#[test]
fn a_key_outside_the_grammar_is_refused_with_the_grammar() {
    let mut app = fixtures::app_typing_a_new_key();
    for c in ".bad".chars() {
        app.on_key(KeyPress::TextChar(c));
    }

    let effect = app.on_key(KeyPress::TextApply);

    assert!(matches!(effect, Effect::None));
    assert!(
        fixtures::notice_of(&app).is_some_and(|n| n.contains("not starting with a dot")),
        "the refusal states the rule, not just that it failed"
    );
}

#[test]
fn a_value_lands_in_the_tabs_own_environment() {
    let mut app = fixtures::app_with_secrets_and_control();
    app.on_key(KeyPress::Secrets);
    app.on_key(KeyPress::TabNext);
    app.on_key(KeyPress::Confirm);
    for c in "s3cret".chars() {
        app.on_key(KeyPress::TextChar(c));
    }

    let effect = app.on_key(KeyPress::TextApply);

    let Effect::WriteSecret(edit, _) = effect else {
        panic!("expected a write, got {effect:?}");
    };
    assert_eq!(edit.environment, fixtures::second_tab_of(&app));
    assert_eq!(edit.value.as_deref(), Some("s3cret"));
}

#[test]
fn a_successful_write_takes_a_revealed_value_off_the_screen() {
    let mut app = fixtures::app_revealing_with_control();

    app.on_msg(Msg::SecretWritten { result: Ok(()) });

    assert!(
        fixtures::reveal_of(&app).is_none(),
        "the value on screen belonged to what the store held before the write"
    );
}

#[test]
fn a_failed_write_says_why_and_leaves_the_table_alone() {
    let mut app = fixtures::app_with_secrets_and_control();
    app.on_key(KeyPress::Secrets);
    let before = fixtures::row_count(&app);

    app.on_msg(Msg::SecretWritten {
        result: Err("permission denied (os error 13)".to_string()),
    });

    assert!(
        fixtures::notice_of(&app).is_some_and(|n| n.contains("permission denied")),
        "the operator gets the reason, not a silent no-op"
    );
    assert_eq!(fixtures::row_count(&app), before, "and nothing is redrawn as changed");
}

#[test]
fn the_new_key_row_opens_a_name_input_and_then_a_value_input() {
    let mut app = fixtures::app_with_secrets_and_control();
    app.on_key(KeyPress::Secrets);
    fixtures::select_new_key_row(&mut app);

    app.on_key(KeyPress::Confirm);
    assert_eq!(
        fixtures::typing_of(&app).map(|t| t.what.clone()),
        Some(TypingWhat::NewKey)
    );

    for c in "NEW_KEY".chars() {
        app.on_key(KeyPress::TextChar(c));
    }
    let effect = app.on_key(KeyPress::TextApply);

    assert!(
        matches!(effect, Effect::None),
        "naming a key writes nothing on its own"
    );
    assert_eq!(
        fixtures::typing_of(&app).map(|t| t.what.clone()),
        Some(TypingWhat::ValueFor("NEW_KEY".into())),
        "the name input hands straight over to the value input"
    );
}

#[test]
fn a_provider_row_refuses_a_write() {
    let mut app = fixtures::app_with_a_pushed_secret_selected_and_control();
    app.on_key(KeyPress::Secrets);

    app.on_key(KeyPress::Confirm);

    assert!(fixtures::typing_of(&app).is_none());
    assert!(
        fixtures::notice_of(&app).is_some_and(|n| n.contains("pushed by a dog")),
        "and it says why rather than doing nothing"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: secret
```

Expected: FAIL, `no variant named \`WriteSecret\``.

- [ ] **Step 3: Write the implementation**

```rust
/// One change to the operator's store.
///
/// `Debug` is manual (IR-41): `value` is the operator's plaintext.
pub(crate) struct SecretEdit {
    /// The key.
    pub key: String,
    /// Which environment's slot moves.
    pub environment: String,
    /// The new value, or `None` to remove the slot.
    pub value: Option<String>,
}

/// Redacted (IR-41), matching `SecretCommand::Set`: a length, never a
/// value.
impl fmt::Debug for SecretEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self
            .value
            .as_ref()
            .map_or_else(|| "None".to_string(), |v| format!("Some(<{} bytes>)", v.len()));
        f.debug_struct("SecretEdit")
            .field("key", &self.key)
            .field("environment", &self.environment)
            .field("value", &format_args!("{value}"))
            .finish()
    }
}
```

The `Effect` and `Msg`:

```rust
    /// Apply one change to `secrets.json`; the result lands as
    /// [`Msg::SecretWritten`].
    ///
    /// Runs on `spawn_blocking` for [`Self::WriteSetting`]'s reason: the
    /// store's lock acquires with no deadline, and the UI task's redraw,
    /// tick and bus drain would block with the write.
    ///
    /// The [`WriteAuthority`] is not decoration: this variant cannot be
    /// named without having passed the gate.
    WriteSecret(SecretEdit, WriteAuthority),
```

```rust
    /// A store write finished.
    SecretWritten {
        /// What the write returned, its error already rendered: a
        /// `SecretError` names a key, never a value, but the pane has no
        /// use for the type.
        result: Result<(), String>,
    },
```

Validation happens on `TextApply`, before any `Effect` is produced, using the store's own rules rather than a second copy of them: `shep_core::secrets::check_key` for a new key's name and `MAX_VALUE_BYTES` for a value's length. Do not re-implement the grammar in the pane.

`Effect::WriteSecret` runs `secrets::set` when `value` is `Some` and `secrets::unset` when it is `None`, on `spawn_blocking`, following `Effect::WriteSetting`'s existing plumbing. A successful write raises `Effect::LoadSecrets` so the table shows what landed rather than what was typed, and calls `SecretsPane::hide`: the value on screen belonged to the store as it was before the write. That is the seventh reveal trigger Task 6 left open, so extend Task 6's clear test with it here rather than writing a second test that asserts the same thing.

A failed write raises no reload. The table keeps saying what the store last actually held, and the error goes to the notice line.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: secret
```

Expected: PASS.

- [ ] **Step 5: Prove the redaction**

```rust
#[test]
fn a_secret_edit_debug_prints_a_length_and_never_the_value() {
    let edit = SecretEdit {
        key: "DB_PASSWORD".into(),
        environment: "production".into(),
        value: Some("hunter2".into()),
    };

    assert_eq!(
        format!("{edit:?}"),
        "SecretEdit { key: \"DB_PASSWORD\", environment: \"production\", \
         value: Some(<7 bytes>) }"
    );
}
```

- [ ] **Step 6: Mutation receipt**

Seed the value input with the stored value and confirm `enter_opens_the_value_input_seeded_empty` fails. Then ask what else could make `a_value_lands_in_the_tabs_own_environment` pass: an implementation always writing to the daemon's default environment would pass it whenever that default happens to be the second tab, so the fixture must place the second tab somewhere the default is not. Fix the fixture if it does not already.

- [ ] **Step 7: Commit**

```bash
git add crates/shep-cli/src/lookout/app.rs crates/shep-cli/src/lookout/mod.rs crates/shep-cli/src/lookout/view/secrets.rs
git commit -m "feat(lookout): set a secret from the pane, gated and validated"
```

---

### Task 8: Delete a key, and the Enter collision

`D` arms and `Enter` confirms, like `x`, `R` and `L`. `Enter` also opens the value input, so the two meanings have to be told apart.

**Files:**
- Modify: `crates/shep-cli/src/lookout/input.rs` (bind `D`)
- Modify: `crates/shep-cli/src/lookout/app.rs`

**Interfaces:**
- Consumes: `SecretEdit` with `value: None`, `SecretsPane::armed`.
- Produces: `KeyPress::SecretDelete`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn enter_sets_when_nothing_is_armed_and_confirms_when_something_is() {
    let mut idle = fixtures::app_with_secrets_and_control();
    idle.on_key(KeyPress::Secrets);

    idle.on_key(KeyPress::Confirm);

    assert!(
        fixtures::typing_of(&idle).is_some(),
        "unarmed Enter opens the value input"
    );

    let mut armed = fixtures::app_with_secrets_and_control();
    armed.on_key(KeyPress::Secrets);
    armed.on_key(KeyPress::SecretDelete);

    let effect = armed.on_key(KeyPress::Confirm);

    assert!(
        fixtures::typing_of(&armed).is_none(),
        "armed Enter must not also open the input"
    );
    let Effect::WriteSecret(edit, _) = effect else {
        panic!("expected the delete, got {effect:?}");
    };
    assert_eq!(edit.key, "DB_PASSWORD");
    assert_eq!(edit.value, None, "None is what removes the slot");
}

#[test]
fn escape_disarms_before_it_closes_the_pane() {
    let mut app = fixtures::app_with_secrets_and_control();
    app.on_key(KeyPress::Secrets);
    app.on_key(KeyPress::SecretDelete);

    app.on_key(KeyPress::Escape);

    assert!(matches!(app.body(), Body::Secrets(_)), "the pane stays open");
    assert!(fixtures::armed_of(&app).is_none(), "and the arm is gone");

    app.on_key(KeyPress::Escape);

    assert!(matches!(app.body(), Body::FlockTable), "a second Escape closes it");
}

#[test]
fn moving_the_selection_disarms() {
    let mut app = fixtures::app_with_secrets_and_control();
    app.on_key(KeyPress::Secrets);
    app.on_key(KeyPress::SecretDelete);

    app.on_key(KeyPress::SelectDown);

    assert!(
        fixtures::armed_of(&app).is_none(),
        "an arm must not follow the cursor onto another key"
    );
}

#[test]
fn a_delete_refuses_without_the_control_gate() {
    let mut app = fixtures::app_with_secrets_read_only();
    app.on_key(KeyPress::Secrets);

    app.on_key(KeyPress::SecretDelete);

    assert!(fixtures::armed_of(&app).is_none());
    assert!(fixtures::notice_of(&app).is_some_and(|n| n.contains("read-only")));
}

#[test]
fn a_provider_row_refuses_a_delete() {
    let mut app = fixtures::app_with_a_pushed_secret_selected_and_control();
    app.on_key(KeyPress::Secrets);

    app.on_key(KeyPress::SecretDelete);

    assert!(fixtures::armed_of(&app).is_none());
    assert!(fixtures::notice_of(&app).is_some_and(|n| n.contains("pushed by a dog")));
}
```

`moving_the_selection_disarms` is the one that matters most: an arm that survives a cursor move deletes the wrong key.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: delete
```

Expected: FAIL, `no variant named \`SecretDelete\``.

- [ ] **Step 3: Write the implementation**

```rust
        KeyCode::Char('D') => Some(KeyPress::SecretDelete),
```

```rust
    /// `D`: arms the removal of the selected key's value in the current
    /// tab's environment, in the secrets pane. `Enter` confirms.
    ///
    /// Capital, and `d` is left alone: `d` is already
    /// [`Self::ListRemove`], and `map_key` dispatches on mode rather than
    /// pane, so the two cannot share a key.
    SecretDelete,
```

The `Confirm` arm inside the pane branches on `armed` first:

```rust
            KeyPress::Confirm => match self.secrets_pane_mut() {
                // Armed first. `Enter` means two things here, and the
                // status bar says which; a delete waiting on a confirm
                // outranks opening an input the operator did not ask for.
                Some(pane) if pane.armed.is_some() => self.confirm_secret_delete(),
                Some(_) => self.open_secret_value_input(),
                None => self.confirm_armed_action(),
            },
```

`confirm_armed_action` stands for whatever the existing `KeyPress::Confirm` arm
already does off this pane. Do not write a new function: move the current body
into the `None` branch unchanged, so every other screen's `Enter` keeps behaving
exactly as it does today.

Every selection move and every tab move clears `armed` alongside `hide()`.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: delete
```

Expected: PASS.

- [ ] **Step 5: Mutation receipt**

Reverse the two `Confirm` arms so the input wins over the arm, and confirm `enter_sets_when_nothing_is_armed_and_confirms_when_something_is` fails on its second half. Then remove the disarm from the selection move and confirm `moving_the_selection_disarms` fails. Restore both.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/input.rs crates/shep-cli/src/lookout/app.rs
git commit -m "feat(lookout): delete a secret behind an arm and a confirm"
```

---

### Task 9: Copy to the clipboard over OSC 52

**Files:**
- Modify: `crates/shep-cli/src/serve/auth.rs` (promote the base64 encoder out of the test module)
- Modify: `crates/shep-cli/src/lookout/input.rs` (bind `y`)
- Modify: `crates/shep-cli/src/lookout/app.rs`, `crates/shep-cli/src/lookout/term.rs`

**Interfaces:**
- Consumes: the promoted `base64_encode`.
- Produces: `KeyPress::Copy`, `Effect::CopyToClipboard(String)`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn base64_encode_round_trips_through_the_decoder_beside_it() {
    for input in ["", "a", "ab", "abc", "alice:s3cret", "hunter2"] {
        let encoded = base64_encode(input.as_bytes());
        assert_eq!(
            base64_decode(&encoded).as_deref(),
            Some(input.as_bytes()),
            "{input:?} did not survive the round trip as {encoded:?}"
        );
    }
}

#[test]
fn the_osc_52_sequence_carries_the_encoded_value_and_nothing_else() {
    let sequence = super::osc52("hunter2");

    assert_eq!(sequence, "\x1b]52;c;aHVudGVyMg==\x07");
    assert!(!sequence.contains("hunter2"), "the plaintext must not ride along");
}

#[test]
fn copying_says_it_was_sent_rather_than_that_it_arrived() {
    let mut app = fixtures::app_revealing();

    app.on_key(KeyPress::Copy);

    let notice = fixtures::notice_of(&app).expect("a notice");
    assert!(notice.contains("sent to the terminal"), "got {notice:?}");
    assert!(
        !notice.contains("copied"),
        "OSC 52 is write-only and many terminals refuse it, so claiming \
         success is a claim nothing can check: {notice:?}"
    );
}

#[test]
fn copy_needs_a_revealed_value_rather_than_reading_the_store_behind_the_gate() {
    let mut app = fixtures::app_with_secrets_and_reads(false);
    app.on_key(KeyPress::Secrets);

    app.on_key(KeyPress::Copy);

    assert!(
        fixtures::notice_of(&app).is_some_and(|n| n.contains("allow_read")),
        "copy is a reveal by another route and takes the same gate"
    );
}
```

The last test is the security-relevant one: `y` must not become a way around `allow_read`.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: base64
```

Expected: FAIL, `cannot find function \`base64_encode\``.

- [ ] **Step 3: Write the implementation**

Move the encoder in `crates/shep-cli/src/serve/auth.rs` out of `#[cfg(test)]` and beside `base64_decode`, taking `&[u8]` rather than `&str`. Correct the comment at line 200 that calls the Basic header the only base64 in the crate.

```rust
/// The OSC 52 sequence that offers `value` to the terminal's clipboard.
///
/// Write-only: the terminal sends nothing back, and many refuse the
/// sequence by default, so no caller can report success. The pane's own
/// wording says the copy was sent rather than that it arrived.
fn osc52(value: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(value.as_bytes()))
}
```

`Effect::CopyToClipboard(String)` writes the sequence to the same terminal handle the rest of `term.rs` uses, and never through `tracing`: the whole point is that the value does not reach a log.

The `Copy` arm takes the value from `pane.reveal` and refuses with the `allow_read` sentence when there is none.

The FOCUSED panel gains one line, once: the system clipboard is readable by every process on the desktop.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: base64 clipboard
```

Expected: PASS.

- [ ] **Step 5: Mutation receipt**

Make `Copy` read the store directly instead of `pane.reveal` and confirm `copy_needs_a_revealed_value_rather_than_reading_the_store_behind_the_gate` fails. Restore.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/serve/auth.rs crates/shep-cli/src/lookout/input.rs crates/shep-cli/src/lookout/app.rs crates/shep-cli/src/lookout/term.rs
git commit -m "feat(lookout): copy a revealed secret over OSC 52"
```

---

### Task 10: The two panels

**Files:**
- Modify: `crates/shep-cli/src/lookout/view/secrets.rs`

**Interfaces:**
- Consumes: `SecretRow::readers`, `SecretsModel::roll_age`, and the tab keys Task 4 bound.
- Produces: nothing new. This task draws.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_reader_is_never_said_to_hold_the_current_value() {
    let buffer = fixtures::render_secrets_with_readers();
    let text = fixtures::rows_of(&buffer);

    assert!(
        text.iter().any(|l| l.contains("was given a value at spawn")),
        "an online reader: {text:?}"
    );
    assert!(
        text.iter().any(|l| l.contains("reads it at next start")),
        "an offline one: {text:?}"
    );
    assert!(
        !text.iter().any(|l| l.contains("holds the value")),
        "nothing can tell a sheep spawned before a set from one spawned \
         after, so the pane must not claim it: {text:?}"
    );
}

#[test]
fn the_focused_panel_states_the_gate_and_not_an_audit() {
    let buffer = fixtures::render_secrets_gate_shut();
    let text = fixtures::rows_of(&buffer);

    assert!(text.iter().any(|l| l.contains("allow_read")));
    assert!(
        !text.iter().any(|l| l.contains("audit")),
        "there is no audit log, so promising one is a promise nothing keeps"
    );
}

#[test]
fn a_stale_roll_states_its_age() {
    let buffer = fixtures::render_secrets_with_roll_age(Duration::from_secs(3600));
    let text = fixtures::rows_of(&buffer);

    assert!(
        text.iter().any(|l| l.contains("roll") && l.contains("1h")),
        "a failed roll write only warns, so the age is the only signal: {text:?}"
    );
}

#[test]
fn a_missing_roll_says_so_rather_than_showing_an_empty_reader_list() {
    let buffer = fixtures::render_secrets_with_no_roll();
    let text = fixtures::rows_of(&buffer);

    assert!(
        text.iter().any(|l| l.contains("no muster roll")),
        "an absent roll and a key nothing reads look identical otherwise: {text:?}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: panel
```

Expected: FAIL.

- [ ] **Step 3: Write the implementation**

The panels are drawn below the hairline, the left one 88 cells wide and the right one taking the rest. The right panel lists the selected row's `readers`, `█` plus `online, was given a value at spawn` or `░` plus `not running, reads it at next start`. Both glyph and words, per the design's third rule.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p shep --lib --all-features -- --skip ::slow:: panel
```

Expected: PASS.

- [ ] **Step 5: Mutation receipt**

Change the online caption to "holds the value" and confirm `a_reader_is_never_said_to_hold_the_current_value` fails. Then make `roll_age` return `Some(Duration::ZERO)` for a missing roll and confirm `a_missing_roll_says_so_rather_than_showing_an_empty_reader_list` fails. Restore both.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/view/secrets.rs crates/shep-cli/src/lookout/input.rs crates/shep-cli/src/lookout/app.rs
git commit -m "feat(lookout): draw the focused and who-reads-it panels"
```

---

### Task 11: Snapshot scene and the docs site

**Files:**
- Modify: `crates/shep-cli/src/lookout/frames.rs` (a `Scene` for the pane)
- Modify: `web/src/pages/docs/*.astro` (whichever pages name lookout's keys)
- Regenerate: the CLI reference

- [ ] **Step 1: Add the snapshot scene**

Add a `Scene::Secrets` beside the existing variants, rendering `fixtures::app_with_secrets` with the pane open, one revealed row, one provider group and one `not set here` row. Follow how the existing scenes are registered and asserted.

- [ ] **Step 2: Run the whole lib suite**

```bash
cargo test -p shep --lib --bins --all-features -- --skip ::slow::
```

Expected: PASS.

- [ ] **Step 3: Regenerate the CLI reference**

```bash
cargo build --release
```
```bash
./web/scripts/generate-cli-reference.sh
```

`git diff` afterwards is the check.

- [ ] **Step 4: Update the prose pages**

Grep `web/src/pages/docs/` for the lookout keymap and for `allow_read`. Every page listing lookout's keys gains `S`, and the secrets page states both gates: `lookout.allow_control` for changing anything, `[secrets] allow_read` for revealing a value.

- [ ] **Step 5: Build and check the site**

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

Both. `check` is the one that catches a wrong prop; `build` stays green through a prop the component does not have.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/lookout/frames.rs web/
git commit -m "docs(lookout): document the secrets pane's keys and gates"
```

---

## The task gate, once, when every task is done

One cargo command at a time, each from its own command, never through a pipe.

```bash
cargo fmt --all --check
```
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
```bash
cargo test --workspace --all-features
```
```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

The local gate does not cover Linux or Windows. Read CI before calling the branch green.

## Coverage

Every spec requirement, the task that implements it, and the test that pins it. Fill the third column with a real `file:line` while implementing; a row that cannot cite one is not covered.

| Spec requirement | Task | Test |
|---|---|---|
| tab row is environments, namespaces are row groups | 3, 5 | `environments_are_the_union_of_the_store_plus_all`, `a_provider_group_says_it_is_read_only` |
| `IN FORCE` mirrors `SecretView::resolve` | 3 | `in_force_agrees_with_secret_view_resolve`, `a_named_environment_never_falls_back_to_a_sibling` |
| `SET IN` replaces `LAST SET` | 3 | `set_in_lists_every_environment_the_key_has_a_slot_for` |
| `READ BY` from the roll, not a new request | 1, 2 | `two_apps_naming_one_key_both_appear_under_it` |
| reader caption never claims the current value | 2, 10 | `a_reader_is_never_said_to_hold_the_current_value` |
| roll age is stated | 3, 10 | `a_stale_roll_states_its_age` |
| `S` opens and closes; `g` unchanged | 4 | `capital_s_opens_the_secrets_pane_and_lower_s_still_opens_settings` |
| block run proportional, byte count exact | 5 | `a_four_kilobyte_value_never_overflows_its_column` |
| tiers and drop order | 5 | `every_tier_fits_the_width_it_claims`, `the_columns_drop_in_the_specified_order` |
| reveal gated on `allow_read` | 6 | `v_reveals_only_when_allow_read_is_on` |
| reveal clears on every trigger | 6 | `a_reveal_clears_on_every_one_of_its_triggers_that_exists_yet`, `a_reveal_survives_the_tick_before_it_expires` |
| write gated on `allow_control` | 7, 8 | `a_write_refuses_without_the_control_gate`, `a_delete_refuses_without_the_control_gate` |
| value input seeds empty | 7 | `enter_opens_the_value_input_seeded_empty` |
| validation at the input | 7 | `a_value_over_the_cap_is_refused_at_the_input_not_at_the_file`, `a_key_outside_the_grammar_is_refused_with_the_grammar` |
| write lands in the tab's environment | 7 | `a_value_lands_in_the_tabs_own_environment` |
| provider rows refuse writes | 7, 8 | `a_provider_row_refuses_a_write`, `a_provider_row_refuses_a_delete` |
| `Enter` collision resolved | 8 | `enter_sets_when_nothing_is_armed_and_confirms_when_something_is` |
| an arm does not follow the cursor | 8 | `moving_the_selection_disarms` |
| copy is honest and gated | 9 | `copying_says_it_was_sent_rather_than_that_it_arrived`, `copy_needs_a_revealed_value_rather_than_reading_the_store_behind_the_gate` |
| no audit promise on screen | 10 | `the_chrome_states_the_gate_and_no_panel_promises_an_audit` |
| three plaintext types redact (IR-41) | 4, 7 | `the_pane_debug_never_prints_a_revealed_value`, `a_secret_edit_debug_prints_a_length_and_never_the_value` |
| unreadable store reports itself | 3 | `an_unreadable_store_reports_rather_than_reading_as_empty` |
| selection glyph is not `▌` | 5 | covered by `flock::mark`'s existing test; add no second one |
| tab moves, stops at both ends, and reloads | 4 | `the_tab_moves_and_stops_at_both_ends`, `a_tab_move_reloads_because_in_force_is_per_environment` |
| `z` collapses a namespace group | 4 | `z_collapses_a_namespace_group_and_leaves_its_header` |
| `+ new key` is a name input then a value input | 7 | `the_new_key_row_opens_a_name_input_and_then_a_value_input` |
| a failed write says why | 7 | `a_failed_write_says_why_and_leaves_the_table_alone` |
| a missing roll is distinguishable from no readers | 10 | `a_missing_roll_says_so_rather_than_showing_an_empty_reader_list` |
| `web/` states the new key and both gates | 11 | `astro check`, and `git diff` on the regenerated reference |
