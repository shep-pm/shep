# Secret store implementation plan (shep side)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give shep a secret store with a per-environment axis, a `{{secret:NAME}}` token that resolves at spawn, and a push API so a dog can supply values from an external provider.

**Architecture:** A versioned JSON file at `$SHEP_HOME/secrets.json`, written by the CLI directly and read by the daemon in the caller of `assemble`. `template::render` becomes fallible and resolves `{{secret:...}}` against a `SecretView` the caller builds. Provider dogs push into namespaces over a new request variant; those values are held in memory and mirrored to a separate cache file.

**Tech Stack:** Rust, edition 2024, MSRV 1.88. `serde`, `serde_json`, `tempfile`, `nix` (unix locking). No new dependencies.

**Spec:** [docs/brainstorming/specs/2026-09-06-secrets-store-design.md](../../brainstorming/specs/2026-09-06-secrets-store-design.md)

**This is plan 1 of 2.** Plan 2 covers `shep-vercel`, a Node dog in its own repository, and is written after this merges so the dog has a published protocol to speak.

## Global Constraints

- **Clean room.** Never open, read, or port source from any pm2 checkout.
- **Inner loop:** `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`. For shep-core work: `cargo test -p shep-core --lib --all-features`. For CLI work: `cargo test -p shep --lib --bins --all-features -- --skip ::slow::`.
- **One cargo command shape per task.** Do not alternate `--workspace` with `-p`. Each task below names its shape; use only that one.
- **Task gate**, run once when a task is otherwise done, each from its own command with `$?` read directly and never through a pipe:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace --all-features`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`
- **Conventional commit subjects**, `type(scope): summary`. A `!` only on Task 2, which is the one that breaks a signature. release-plz walks individual commits and silently drops anything that does not parse.
- **Code style:** invoke the `shep-idiomatic-rust` skill before writing Rust. `core::error::Error` never `std::error::Error` (IR-19). Every `Result`-returning public function gets an `# Errors` section (IR-28). `# Panics` and `#[track_caller]` travel together (IR-21). No panicking constructors outside the `shep` crate (IR-21).
- **IR-41 is load-bearing in this plan.** Every new type that can hold a secret value gets a hand-written `Debug` and an exact-string test that fails if somebody adds `#[derive(Debug)]` later.
- **Errors name the key, the namespace and the environment. Never the value.**
- **No em dashes** anywhere, including code comments and commit messages.
- **No absolute home paths and no personal names** in any committed file or commit message. Repo-relative paths only.
- **Never touch `~/.shep`.** Any live-daemon run uses `SHEP_HOME` pointed at a short `mktemp -d` path, because a long path exceeds `SUN_LEN` for the control socket.

---

## File structure

| File | Responsibility | Task |
|---|---|---|
| `crates/shep-core/src/secrets.rs` | the store file, its grammar, `SecretRef`, `SecretView`, resolution | 1 |
| `crates/shep-core/src/paths.rs` | two new fields on `ShepPaths` | 1 |
| `crates/shep-core/src/config/template.rs` | the `secret:` token, `render_positional`, fallible `render` | 2 |
| `crates/shep-core/src/config/normalize.rs` | switch its collision check to `render_positional` | 2 |
| `crates/shep-core/src/config/app.rs` | `AppConfig::environment` | 3 |
| `crates/shep-core/src/config/apply.rs` | classify `environment` | 3 |
| `crates/shep-core/src/config/daemon.rs` | `[daemon] environment`, `[secrets] allow_read` | 3, 6 |
| `crates/shep-daemon/src/assemble.rs` | fallible assemble, `SHEP_ENVIRONMENT` | 4 |
| `crates/shep-daemon/src/supervisor.rs` | build a `SecretView`, handle the two refusal shapes | 4 |
| `crates/shep-daemon/src/dogs.rs` | one `assemble` call site | 4 |
| `crates/shep-core/src/protocol/request.rs` | `PutSecrets` / `SecretsPut` | 5 |
| `crates/shep-daemon/src/rpc.rs` | the `PutSecrets` arm | 5 |
| `crates/shep-daemon/src/secrets.rs` | the in-memory namespace map and its cache file | 5 |
| `crates/shep-cli/src/commands/secret.rs` | `shep secret set/get/unset/list` | 6 |
| `crates/shep-cli/src/cli.rs` | the verb and its args | 6 |
| `crates/shep-cli/src/commands/query.rs` | `shep describe` secret references | 7 |
| `web/src/pages/docs/secrets.astro` | the operator page | 8 |

---

## Task 1: The store module

**Cargo shape for this task:** `cargo test -p shep-core --lib --all-features`

**Files:**
- Create: `crates/shep-core/src/secrets.rs`
- Modify: `crates/shep-core/src/lib.rs` (add `pub mod secrets;`)
- Modify: `crates/shep-core/src/paths.rs` (two fields on `ShepPaths`, set where `kv` and `overrides` are set)

**Interfaces:**
- Consumes: `crate::atomic_file::{create_staging_file, sync_dir, OWNER_ONLY_FILE_MODE}`.
- Produces, relied on by every later task:
  ```rust
  pub const SECRETS_VERSION: u32 = 1;
  pub const MAX_KEY_BYTES: usize = 128;
  pub const MAX_VALUE_BYTES: usize = 4096;
  pub const ALL_ENVIRONMENTS: &str = "all";

  pub enum SecretError { Io(std::io::Error), Decode(serde_json::Error),
      InvalidKey(String), InvalidEnvironment(String),
      ValueTooLong { key: String, len: usize }, FutureVersion(u32) }

  pub fn all(path: &Path) -> Result<BTreeMap<String, BTreeMap<String, String>>, SecretError>;
  pub fn get(path: &Path, key: &str, environment: &str) -> Result<Option<String>, SecretError>;
  pub fn set(path: &Path, key: &str, environment: &str, value: &str) -> Result<(), SecretError>;
  pub fn unset(path: &Path, key: &str, environment: &str) -> Result<bool, SecretError>;

  pub struct SecretRef<'a> { pub namespace: Option<&'a str>, pub key: &'a str }
  impl<'a> SecretRef<'a> { pub fn parse(token_body: &'a str) -> Option<Self>; }
  impl fmt::Display for SecretRef<'_>;

  pub struct SecretView { /* private */ }
  impl SecretView {
      pub fn new(environment: String,
                 store: BTreeMap<String, BTreeMap<String, String>>,
                 namespaces: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>) -> Self;
      pub fn empty(environment: String) -> Self;
      pub fn environment(&self) -> &str;
      pub fn resolve(&self, reference: &SecretRef<'_>) -> Resolution<'_>;
  }

  pub enum Resolution<'a> { Found(&'a str), MissingKey, MissingNamespace }
  ```
  `ShepPaths` gains `pub secrets: PathBuf` (`secrets.json`) and `pub secrets_cache: PathBuf` (`secrets-cache.json`).

- [ ] **Step 1: Write the failing tests**

Create `crates/shep-core/src/secrets.rs` with only the test module at first, so the compile failure is about missing items rather than syntax:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_round_trips_through_one_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "DB_PASSWORD", "production", "hunter2").unwrap();
        assert_eq!(
            get(&path, "DB_PASSWORD", "production").unwrap().as_deref(),
            Some("hunter2")
        );
        assert_eq!(get(&path, "DB_PASSWORD", "staging").unwrap(), None);
    }

    #[test]
    fn a_missing_store_reads_as_empty_rather_than_enoent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        assert!(all(&path).unwrap().is_empty());
        assert_eq!(get(&path, "ANY", "production").unwrap(), None);
    }

    #[test]
    fn unset_removes_one_environment_and_leaves_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "K", "production", "p").unwrap();
        set(&path, "K", "staging", "s").unwrap();
        assert!(unset(&path, "K", "staging").unwrap());
        assert_eq!(get(&path, "K", "production").unwrap().as_deref(), Some("p"));
        assert_eq!(get(&path, "K", "staging").unwrap(), None);
        assert!(!unset(&path, "K", "staging").unwrap(), "already gone");
    }

    #[test]
    fn a_key_that_empties_is_removed_rather_than_left_as_an_empty_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "K", "production", "p").unwrap();
        assert!(unset(&path, "K", "production").unwrap());
        assert!(all(&path).unwrap().is_empty(), "no empty husk left behind");
    }

    #[test]
    fn a_bad_key_is_refused_by_name_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        for key in ["", ".hidden", "has space", "has/slash", "has:colon"] {
            let err = set(&path, key, "production", "v").unwrap_err();
            assert!(
                matches!(&err, SecretError::InvalidKey(k) if k == key),
                "{key:?}: {err:?}"
            );
        }
        assert!(!path.exists(), "a refused set must not create the store");
    }

    #[test]
    fn all_is_a_reserved_environment_name_for_writing() {
        // Writing the `all` slot is how a value covers every environment,
        // so `set` accepts it. Nothing else about the name is special.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "K", ALL_ENVIRONMENTS, "everywhere").unwrap();
        assert_eq!(get(&path, "K", "all").unwrap().as_deref(), Some("everywhere"));
    }

    #[test]
    fn an_environment_outside_the_grammar_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        for env in ["", "has space", "has/slash"] {
            let err = set(&path, "K", env, "v").unwrap_err();
            assert!(
                matches!(&err, SecretError::InvalidEnvironment(e) if e == env),
                "{env:?}: {err:?}"
            );
        }
    }

    #[test]
    fn an_oversized_value_is_refused_by_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let big = "x".repeat(MAX_VALUE_BYTES + 1);
        let err = set(&path, "K", "production", &big).unwrap_err();
        assert!(matches!(err, SecretError::ValueTooLong { len, .. } if len == big.len()));
    }

    #[test]
    fn a_future_version_is_refused_rather_than_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, r#"{"version":999,"entries":{}}"#).unwrap();
        assert!(matches!(all(&path), Err(SecretError::FutureVersion(999))));
        assert!(matches!(
            set(&path, "K", "production", "v"),
            Err(SecretError::FutureVersion(999))
        ));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("999"), "the refused store is untouched");
    }

    #[test]
    #[cfg(unix)]
    fn the_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        set(&path, "K", "production", "v").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_reference_parses_with_and_without_a_namespace() {
        let bare = SecretRef::parse("DB_PASSWORD").unwrap();
        assert_eq!(bare.namespace, None);
        assert_eq!(bare.key, "DB_PASSWORD");

        let scoped = SecretRef::parse("vercel/DB_PASSWORD").unwrap();
        assert_eq!(scoped.namespace, Some("vercel"));
        assert_eq!(scoped.key, "DB_PASSWORD");

        for bad in ["", "/KEY", "ns/", "a/b/c", "ns/bad key", "bad ns/KEY"] {
            assert!(SecretRef::parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn resolution_prefers_the_exact_environment_then_all_then_gives_up() {
        let mut store = BTreeMap::new();
        store.insert(
            "K".to_string(),
            BTreeMap::from([
                ("production".to_string(), "prod".to_string()),
                ("all".to_string(), "fallback".to_string()),
            ]),
        );
        store.insert(
            "ONLY_ALL".to_string(),
            BTreeMap::from([("all".to_string(), "everywhere".to_string())]),
        );
        store.insert(
            "ONLY_PROD".to_string(),
            BTreeMap::from([("production".to_string(), "prod".to_string())]),
        );

        let view = SecretView::new("staging".to_string(), store, BTreeMap::new());
        assert!(matches!(
            view.resolve(&SecretRef { namespace: None, key: "K" }),
            Resolution::Found("fallback")
        ));
        assert!(matches!(
            view.resolve(&SecretRef { namespace: None, key: "ONLY_ALL" }),
            Resolution::Found("everywhere")
        ));
        // The whole point: staging never falls back to production's value.
        assert!(matches!(
            view.resolve(&SecretRef { namespace: None, key: "ONLY_PROD" }),
            Resolution::MissingKey
        ));
        assert!(matches!(
            view.resolve(&SecretRef { namespace: None, key: "ABSENT" }),
            Resolution::MissingKey
        ));
    }

    #[test]
    fn an_unpopulated_namespace_is_told_apart_from_a_missing_key() {
        let namespaces = BTreeMap::from([(
            "vercel".to_string(),
            BTreeMap::from([(
                "PRESENT".to_string(),
                BTreeMap::from([("production".to_string(), "v".to_string())]),
            )]),
        )]);
        let view = SecretView::new("production".to_string(), BTreeMap::new(), namespaces);

        assert!(matches!(
            view.resolve(&SecretRef { namespace: Some("vercel"), key: "PRESENT" }),
            Resolution::Found("v")
        ));
        // The dog is up and simply does not have this key: a person's problem.
        assert!(matches!(
            view.resolve(&SecretRef { namespace: Some("vercel"), key: "ABSENT" }),
            Resolution::MissingKey
        ));
        // No dog has ever pushed under this name: transient, retry.
        assert!(matches!(
            view.resolve(&SecretRef { namespace: Some("vault"), key: "ANY" }),
            Resolution::MissingNamespace
        ));
    }

    #[test]
    fn a_reference_displays_the_way_an_operator_wrote_it() {
        assert_eq!(
            SecretRef { namespace: None, key: "K" }.to_string(),
            "{{secret:K}}"
        );
        assert_eq!(
            SecretRef { namespace: Some("vercel"), key: "K" }.to_string(),
            "{{secret:vercel/K}}"
        );
    }

    /// IR-41. Fails the moment somebody replaces the hand-written impl with
    /// a derive, which is the only way this leak comes back.
    #[test]
    fn debug_never_prints_a_value() {
        let store = BTreeMap::from([(
            "K".to_string(),
            BTreeMap::from([("production".to_string(), "hunter2".to_string())]),
        )]);
        let view = SecretView::new("production".to_string(), store, BTreeMap::new());
        let rendered = format!("{view:?}");
        assert_eq!(
            rendered,
            "SecretView { environment: \"production\", keys: 1, namespaces: 0 }"
        );
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn error_messages_name_the_key_and_never_a_value() {
        let err = SecretError::ValueTooLong { key: "K".to_string(), len: 9999 };
        let rendered = err.to_string();
        assert!(rendered.contains('K'), "{rendered}");
        assert!(rendered.contains("9999"), "{rendered}");
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p shep-core --lib --all-features secrets`
Expected: compile failure, `cannot find function set in this scope` and similar.

- [ ] **Step 3: Write the module**

Write the rest of `crates/shep-core/src/secrets.rs` above the test module. Copy the file mechanics from `crates/shep-core/src/kv.rs`: `SecretLock` mirroring `KvLock` on both platform arms, `lock_path`, `read_file`, `write_file` staged through `create_staging_file(parent, "secrets", ".tmp")`. Do not reimplement the staging or the directory sync; call `atomic_file`.

The parts that are not a copy of `kv.rs`:

```rust
/// The file's shape: a version and a key to environment to value map.
///
/// `BTreeMap` throughout so two writes of the same content produce
/// byte-identical files.
#[derive(Default, Serialize, Deserialize)]
struct SecretFile {
    version: u32,
    entries: BTreeMap<String, BTreeMap<String, String>>,
}

/// Redacted (IR-41): `entries` is the whole point of this type.
impl fmt::Debug for SecretFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretFile")
            .field("version", &self.version)
            .field("keys", &self.entries.len())
            .finish()
    }
}

/// The environment name that covers every environment.
///
/// A value here is used when the sheep's own environment has no slot of its
/// own. Cannot be a sheep's `environment`, which `AppConfig` refuses in
/// task 3.
pub const ALL_ENVIRONMENTS: &str = "all";

/// Checks one environment name against the grammar.
///
/// Same shape as a key, so a name can never contain the `/` that separates
/// a namespace from a key.
///
/// # Errors
/// [`SecretError::InvalidEnvironment`]: empty, over [`MAX_KEY_BYTES`],
/// starting with `.`, or containing anything outside `[A-Za-z0-9._-]`.
fn check_environment(environment: &str) -> Result<(), SecretError> {
    if is_name(environment) {
        Ok(())
    } else {
        Err(SecretError::InvalidEnvironment(environment.to_string()))
    }
}

/// The grammar shared by keys, namespaces and environment names.
fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEY_BYTES
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
```

`SecretRef::parse` splits on the first `/` and requires both halves to satisfy `is_name`, which rejects `a/b/c` because `b/c` is not a name:

```rust
impl<'a> SecretRef<'a> {
    /// Parses the body of a `{{secret:...}}` token, braces and prefix
    /// already stripped.
    ///
    /// Returns `None` for anything outside the grammar, which is how
    /// `template::validate` refuses a bad reference at config time.
    #[must_use]
    pub fn parse(body: &'a str) -> Option<Self> {
        match body.split_once('/') {
            None if is_name(body) => Some(Self { namespace: None, key: body }),
            Some((namespace, key)) if is_name(namespace) && is_name(key) => {
                Some(Self { namespace: Some(namespace), key })
            }
            _ => None,
        }
    }
}
```

`SecretView::resolve` is the whole resolution rule and is the one function in this module worth reading twice:

```rust
impl SecretView {
    /// The value `reference` resolves to in this view's environment.
    ///
    /// Exact environment, then [`ALL_ENVIRONMENTS`], then nothing. There is
    /// deliberately no fallback to another named environment: filling an
    /// empty `staging` slot from `production` would hand a live credential
    /// to staging the first time somebody forgot to set one.
    #[must_use]
    pub fn resolve(&self, reference: &SecretRef<'_>) -> Resolution<'_> {
        let table = match reference.namespace {
            None => &self.store,
            Some(namespace) => match self.namespaces.get(namespace) {
                Some(table) => table,
                None => return Resolution::MissingNamespace,
            },
        };
        let Some(by_environment) = table.get(reference.key) else {
            return Resolution::MissingKey;
        };
        by_environment
            .get(&self.environment)
            .or_else(|| by_environment.get(ALL_ENVIRONMENTS))
            .map_or(Resolution::MissingKey, |value| Resolution::Found(value))
    }
}
```

`unset` removes the environment's slot and then removes the key entirely if that emptied it, which is what `a_key_that_empties_is_removed_rather_than_left_as_an_empty_map` pins.

- [ ] **Step 4: Add the module and the paths**

In `crates/shep-core/src/lib.rs`, add `pub mod secrets;` in the existing alphabetical position.

In `crates/shep-core/src/paths.rs`, add two fields to `ShepPaths` beside `overrides`, and set them in the same constructor that sets `kv` and `overrides`:

```rust
    /// Secret store: `secrets.json`
    pub secrets: PathBuf,
    /// Cached provider values: `secrets-cache.json`
    ///
    /// Derived and safe to delete, unlike [`Self::secrets`]: a provider dog
    /// rewrites it on its next push.
    pub secrets_cache: PathBuf,
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p shep-core --lib --all-features secrets`
Expected: PASS, every test in the module.

- [ ] **Step 6: Run the task gate**

Run each of the four gate commands from the Global Constraints section, one at a time.
Expected: `EXIT=0` from each.

- [ ] **Step 7: Commit**

```bash
git add crates/shep-core/src/secrets.rs crates/shep-core/src/lib.rs crates/shep-core/src/paths.rs
git commit -m "feat(core): add the secret store, its grammar and its resolution rule"
```

---

## Task 2: The `{{secret:}}` token, and `render` becomes fallible

**Cargo shape for this task:** `cargo test -p shep-core --lib --all-features`

This is the one task that breaks a published signature. Its commit takes a `!`.

**Files:**
- Modify: `crates/shep-core/src/config/template.rs`
- Modify: `crates/shep-core/src/config/normalize.rs:275-276` (switch to `render_positional`)
- Modify: `crates/shep-core/CHANGELOG.md`

**Interfaces:**
- Consumes: `crate::secrets::{SecretRef, SecretView, Resolution}` from Task 1.
- Produces:
  ```rust
  pub fn render_positional(value: &str, name: &str, instance: u32) -> String;
  pub fn render(value: &str, name: &str, instance: u32, secrets: &SecretView)
      -> Result<String, RenderError>;

  pub enum RenderError {
      Unresolved { reference: String, environment: String },
      NamespaceUnready { namespace: String, reference: String },
  }
  impl RenderError { pub fn is_retriable(&self) -> bool; }
  ```
  `RenderError::is_retriable` is `true` only for `NamespaceUnready`. Task 4 branches on it.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `template.rs`:

```rust
    use crate::secrets::{Resolution, SecretRef, SecretView};

    fn view(environment: &str) -> SecretView {
        use std::collections::BTreeMap;
        let store = BTreeMap::from([(
            "DB_PASSWORD".to_string(),
            BTreeMap::from([("production".to_string(), "hunter2".to_string())]),
        )]);
        let namespaces = BTreeMap::from([(
            "vercel".to_string(),
            BTreeMap::from([(
                "API_KEY".to_string(),
                BTreeMap::from([("production".to_string(), "sk_live".to_string())]),
            )]),
        )]);
        SecretView::new(environment.to_string(), store, namespaces)
    }

    #[test]
    fn a_secret_token_validates_with_and_without_a_namespace() {
        assert!(validate("{{secret:DB_PASSWORD}}").is_ok());
        assert!(validate("{{secret:vercel/API_KEY}}").is_ok());
        assert!(validate("postgres://u:{{secret:DB_PASSWORD}}@db/app").is_ok());
    }

    #[test]
    fn a_malformed_reference_is_refused_at_config_time() {
        for bad in [
            "{{secret:}}",
            "{{secret:/KEY}}",
            "{{secret:ns/}}",
            "{{secret:a/b/c}}",
            "{{secret:has space}}",
        ] {
            let err = validate(bad).unwrap_err();
            let rendered = err.to_string();
            assert!(rendered.contains("secret"), "{bad}: {rendered}");
        }
    }

    #[test]
    fn an_unknown_prefix_is_still_refused_by_name() {
        // The closed token set is the whole reason the prefix exists.
        let err = validate("{{sekret:K}}").unwrap_err();
        assert!(matches!(&err, TemplateError::UnknownToken { token } if token == "sekret:K"));
    }

    #[test]
    fn render_substitutes_a_resolved_secret() {
        assert_eq!(
            render("pw={{secret:DB_PASSWORD}}", "web", 0, &view("production")).unwrap(),
            "pw=hunter2"
        );
        assert_eq!(
            render("{{secret:vercel/API_KEY}}", "web", 0, &view("production")).unwrap(),
            "sk_live"
        );
    }

    #[test]
    fn positional_tokens_still_render_beside_a_secret() {
        assert_eq!(
            render("{{name}}-{{instance}}-{{secret:DB_PASSWORD}}", "web", 3, &view("production"))
                .unwrap(),
            "web-3-hunter2"
        );
    }

    #[test]
    fn an_unresolvable_key_errors_naming_the_reference_and_the_environment() {
        let err = render("{{secret:ABSENT}}", "web", 0, &view("production")).unwrap_err();
        assert!(!err.is_retriable(), "a missing key is nobody's to retry");
        let rendered = err.to_string();
        assert!(rendered.contains("{{secret:ABSENT}}"), "{rendered}");
        assert!(rendered.contains("production"), "{rendered}");
    }

    #[test]
    fn a_secret_missing_only_in_this_environment_errors_rather_than_borrowing_another() {
        let err = render("{{secret:DB_PASSWORD}}", "web", 0, &view("staging")).unwrap_err();
        assert!(err.to_string().contains("staging"));
    }

    #[test]
    fn an_unready_namespace_is_retriable_and_says_which_one() {
        let err = render("{{secret:vault/ANY}}", "web", 0, &view("production")).unwrap_err();
        assert!(err.is_retriable(), "no dog has pushed under this name yet");
        let rendered = err.to_string();
        assert!(rendered.contains("vault"), "{rendered}");
    }

    #[test]
    fn a_namespace_that_is_up_and_lacks_the_key_is_not_retriable() {
        let err = render("{{secret:vercel/ABSENT}}", "web", 0, &view("production")).unwrap_err();
        assert!(!err.is_retriable());
    }

    #[test]
    fn no_render_error_ever_prints_a_value() {
        // Every variant, checked against the one value the fixture holds.
        for value in ["{{secret:ABSENT}}", "{{secret:vault/ANY}}"] {
            let err = render(value, "web", 0, &view("production")).unwrap_err();
            let rendered = err.to_string();
            assert!(!rendered.contains("hunter2"), "{rendered}");
            assert!(!rendered.contains("sk_live"), "{rendered}");
            assert!(
                !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
                "no em or en dash in copy a user reads: {rendered}"
            );
        }
    }

    #[test]
    fn render_positional_leaves_a_secret_token_alone() {
        // normalize's log-path collision check runs at config time with no
        // store, and two instances share a secret's value anyway.
        assert_eq!(
            render_positional("{{secret:DB_PASSWORD}}-{{instance}}", "web", 2),
            "{{secret:DB_PASSWORD}}-2"
        );
    }

    #[test]
    fn doubling_still_escapes_a_secret_token() {
        assert_eq!(
            render("{{{{secret:DB_PASSWORD}}}}", "web", 0, &view("production")).unwrap(),
            "{{secret:DB_PASSWORD}}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p shep-core --lib --all-features template`
Expected: compile failure, `cannot find function render_positional`, `this function takes 3 arguments but 4 were supplied`.

- [ ] **Step 3: Teach `validate` the prefix**

`TOKENS` stays a list of exact tokens for the positional ones. The `secret:` prefix is checked separately, so a token is valid if it is in `TOKENS` or it starts with `SECRET_PREFIX` and its body parses:

```rust
/// The prefix marking a store lookup, as it appears inside the braces.
const SECRET_PREFIX: &str = "secret:";

/// Whether `token` is a well-formed `{{secret:...}}` body.
fn is_secret_token(token: &str) -> bool {
    token
        .strip_prefix(SECRET_PREFIX)
        .is_some_and(|body| crate::secrets::SecretRef::parse(body).is_some())
}
```

`validate`'s closure becomes:

```rust
    walk(value, |segment| match segment {
        Segment::Literal(_) => Ok(()),
        Segment::Token(token) if TOKENS.contains(&token) || is_secret_token(token) => Ok(()),
        Segment::Token(token) => Err(TemplateError::UnknownToken {
            token: token.to_string(),
        }),
    })
```

Extend `TemplateError::UnknownToken`'s `Display` so a body that starts with `secret:` but does not parse says so rather than listing the positional tokens. `a_malformed_reference_is_refused_at_config_time` is what pins that.

- [ ] **Step 4: Split `render`**

Rename the current body to `render_positional`, keeping it infallible and leaving any `secret:` token untouched:

```rust
/// Substitutes `{{instance}}` and `{{name}}` only, leaving a
/// `{{secret:...}}` exactly as written.
///
/// For callers that have no store to consult. `normalize` uses it to compare
/// two instances' log paths, where a secret resolves to the same value for
/// both instances and so cannot disambiguate them anyway.
#[must_use]
pub fn render_positional(value: &str, name: &str, instance: u32) -> String {
```

Then add the full one:

```rust
/// Substitutes every token in `value`, resolving `{{secret:...}}` against
/// `secrets`.
///
/// Call `validate` first: this assumes the grammar already passed.
///
/// # Errors
/// - [`RenderError::Unresolved`]: a reference the store has no value for in
///   this view's environment. Nothing will supply it but a person.
/// - [`RenderError::NamespaceUnready`]: a namespace no dog has pushed under
///   yet. [`RenderError::is_retriable`] is `true` for this one alone.
pub fn render(
    value: &str,
    name: &str,
    instance: u32,
    secrets: &SecretView,
) -> Result<String, RenderError> {
```

`render`'s walk closure handles a `secret:` token by parsing it, calling `secrets.resolve`, and mapping `Resolution` onto the two error variants. `RenderError`'s `Display` prints the reference through `SecretRef`'s own `Display`, so the message quotes the token the way the operator wrote it.

- [ ] **Step 5: Update `normalize.rs`**

`crates/shep-core/src/config/normalize.rs:275-276` compares the two rendered paths. Change both calls from `template::render` to `template::render_positional`. No other change: the function stays infallible.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p shep-core --lib --all-features`
Expected: PASS. shep-daemon does not compile yet; that is Task 4 and is expected here.

- [ ] **Step 7: Record the break**

Add to `crates/shep-core/CHANGELOG.md` under Unreleased, in the file's existing style:

```markdown
### Changed

- `config::template::render` now takes a `secrets::SecretView` and returns
  `Result<String, RenderError>`, so a spawn can refuse on a secret it cannot
  resolve. Callers that have no store use the new `render_positional`, which
  substitutes `{{instance}}` and `{{name}}` and leaves `{{secret:...}}` alone.
```

- [ ] **Step 8: Commit**

The whole workspace does not build at this point, so the gate cannot pass yet. Commit anyway, because splitting a signature change from its call sites is what makes each half reviewable, and Task 4 closes it.

```bash
git add crates/shep-core/src/config/template.rs crates/shep-core/src/config/normalize.rs crates/shep-core/CHANGELOG.md
git commit -m "refactor(core)!: make template::render fallible and teach it {{secret:}}"
```

---

## Task 3: `environment` on `AppConfig`, and the host default

**Cargo shape for this task:** `cargo test -p shep-core --lib --all-features`

**Files:**
- Modify: `crates/shep-core/src/config/app.rs` (field, default, doc)
- Modify: `crates/shep-core/src/config/apply.rs` (one `FIELDS` entry)
- Modify: `crates/shep-core/src/config/normalize.rs` (refuse `all` as a sheep's environment)
- Modify: `crates/shep-core/src/config/daemon.rs` (`DaemonSection::environment`)
- Modify: `crates/shep-core/assets/flockfile.schema.json` (regenerated, never hand-edited)

**Interfaces:**
- Produces: `AppConfig::environment: Option<String>`, and `DaemonSection::environment: String` defaulting to `"production"`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-core/src/config/app.rs`'s test module:

```rust
    #[test]
    fn environment_defaults_to_absent_and_parses_from_a_flockfile() {
        assert_eq!(AppConfig::default().environment, None);
        let app: AppConfig =
            toml::from_str("name = \"web\"\nscript = \"./srv\"\nenvironment = \"staging\"")
                .unwrap();
        assert_eq!(app.environment.as_deref(), Some("staging"));
    }
```

In `crates/shep-core/src/config/normalize.rs`'s test module:

```rust
    #[test]
    fn a_sheep_cannot_claim_the_all_environment() {
        // `all` is the store's every-environment slot. A sheep resolving in
        // it would read that slot twice and never its own.
        let mut app = AppConfig::minimal("web", "./srv");
        app.environment = Some("all".to_string());
        let err = normalize(app).unwrap_err();
        assert!(err.to_string().contains("all"), "{err}");
    }

    #[test]
    fn an_environment_outside_the_grammar_is_refused() {
        for bad in ["", "has space", "has/slash"] {
            let mut app = AppConfig::minimal("web", "./srv");
            app.environment = Some(bad.to_string());
            assert!(normalize(app).is_err(), "{bad:?} must be refused");
        }
    }
```

In `crates/shep-core/src/config/daemon.rs`'s test module:

```rust
    #[test]
    fn the_host_environment_defaults_to_production() {
        let cfg = DaemonConfig::load(None, &|_| None).unwrap();
        assert_eq!(cfg.daemon.environment, "production");
    }

    #[test]
    fn the_host_environment_reads_from_the_file() {
        let cfg = DaemonConfig::load(
            Some("[daemon]\nenvironment = \"staging\"\n"),
            &|_| None,
        )
        .unwrap();
        assert_eq!(cfg.daemon.environment, "staging");
    }
```

`crates/shep-core/src/config/apply.rs`'s existing `every_appconfig_field_has_a_group` fails on its own once the field exists, which is the coverage test doing its job. No new test needed there.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p shep-core --lib --all-features`
Expected: FAIL, including `every_appconfig_field_has_a_group` naming `environment`.

- [ ] **Step 3: Add the field**

In `app.rs`'s `AppConfig`, beside `env`:

```rust
    /// Which environment this sheep resolves `{{secret:...}}` in.
    ///
    /// Absent falls back to `[daemon] environment` in `shep.toml`, which
    /// itself defaults to `production`. Never `all`: that is the store's
    /// every-environment slot, and a sheep claiming it would read that slot
    /// twice and never one of its own.
    pub environment: Option<String>,
```

`Default` gets `environment: None`. The manual `Debug` at `app.rs:439` is `finish_non_exhaustive`, so it needs no change: an environment name is not a secret, and adding it would be noise.

- [ ] **Step 4: Classify it**

In `apply.rs`'s `FIELDS`, in the `NeedsRespawn` block beside `env`:

```rust
    // Decides what every `{{secret:...}}` in this child's env resolved to,
    // and those are baked in at exec like the rest of the environment.
    ("environment", ApplyGroup::NeedsRespawn),
```

- [ ] **Step 5: Refuse the reserved name**

In `normalize.rs`, beside the other per-field checks, refuse an `environment` that is `all` or outside the name grammar, with a `NormalizeError` variant that names the field and the value. Reuse `crate::secrets::ALL_ENVIRONMENTS` rather than spelling `"all"` again.

- [ ] **Step 6: Add the host default**

In `daemon.rs`'s `DaemonSection`:

```rust
    /// The environment every sheep resolves in unless it sets its own.
    ///
    /// A shepherd supervising real processes on a host is production unless
    /// somebody says otherwise.
    pub environment: String,
```

Default `"production"`. Follow the section's existing default mechanism rather than inventing one.

- [ ] **Step 7: Regenerate the schema**

Run: `cargo run -p shep --bin shep -- schema > crates/shep-core/assets/flockfile.schema.json`
Then `git diff` it and confirm the only change is the new `environment` property.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p shep-core --lib --all-features`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/shep-core/src/config/ crates/shep-core/assets/flockfile.schema.json
git commit -m "feat(core): give a sheep an environment, with a host-level default"
```

---

## Task 4: `assemble` resolves, refuses, and injects `SHEP_ENVIRONMENT`

**Cargo shape for this task:** `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`

**Files:**
- Modify: `crates/shep-daemon/src/assemble.rs`
- Modify: `crates/shep-daemon/src/supervisor.rs` (nine `assemble` call sites)
- Modify: `crates/shep-daemon/src/dogs.rs:1398` (the tenth)

**Interfaces:**
- Consumes: `SecretView`, `Resolution` (Task 1); `render`, `RenderError` (Task 2); `AppConfig::environment`, `DaemonSection::environment` (Task 3).
- Produces:
  ```rust
  pub enum AssembleError { Template { field: String, source: RenderError } }
  impl AssembleError { pub fn is_retriable(&self) -> bool; }

  pub fn assemble(
      app: &ResolvedApp,
      instance: u32,
      paths: &ShepPaths,
      credentials: Option<Credentials>,
      secrets: &SecretView,
  ) -> Result<SpawnSpec, AssembleError>;
  ```

- [ ] **Step 1: Write the failing tests**

In `assemble.rs`'s test module:

```rust
    fn view_with(environment: &str, key: &str, value: &str) -> SecretView {
        use std::collections::BTreeMap;
        SecretView::new(
            environment.to_string(),
            BTreeMap::from([(
                key.to_string(),
                BTreeMap::from([(environment.to_string(), value.to_string())]),
            )]),
            BTreeMap::new(),
        )
    }

    #[test]
    fn a_resolved_secret_reaches_the_child_env() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("PW".into(), "{{secret:DB_PASSWORD}}".into());
        let app = normalize(config).unwrap();
        let spec = assemble(
            &app,
            0,
            &paths(),
            None,
            &view_with("production", "DB_PASSWORD", "hunter2"),
        )
        .unwrap();
        assert_eq!(spec.env.get("PW").map(String::as_str), Some("hunter2"));
    }

    #[test]
    fn shep_environment_is_injected_and_matches_the_view() {
        let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
        let spec = assemble(&app, 0, &paths(), None, &SecretView::empty("staging".into()))
            .unwrap();
        assert_eq!(spec.env.get("SHEP_ENVIRONMENT").map(String::as_str), Some("staging"));
    }

    #[test]
    fn a_missing_key_refuses_the_spawn_and_names_the_field() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("PW".into(), "{{secret:ABSENT}}".into());
        let app = normalize(config).unwrap();
        let err = assemble(&app, 0, &paths(), None, &SecretView::empty("production".into()))
            .unwrap_err();
        assert!(!err.is_retriable());
        let rendered = err.to_string();
        assert!(rendered.contains("PW"), "names the env key: {rendered}");
        assert!(rendered.contains("ABSENT"), "{rendered}");
    }

    #[test]
    fn an_unready_namespace_refuses_retriably() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("PW".into(), "{{secret:vault/K}}".into());
        let app = normalize(config).unwrap();
        let err = assemble(&app, 0, &paths(), None, &SecretView::empty("production".into()))
            .unwrap_err();
        assert!(err.is_retriable());
    }

    #[test]
    fn a_secret_resolves_in_args_and_in_an_explicit_log_path() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.args = vec!["--token={{secret:K}}".into()];
        config.out_file = Some("/tmp/{{secret:K}}.log".into());
        config.merge_logs = true;
        let app = normalize(config).unwrap();
        let spec = assemble(&app, 0, &paths(), None, &view_with("production", "K", "v")).unwrap();
        assert_eq!(spec.args, vec!["--token=v".to_string()]);
        assert!(spec.out_file.to_string_lossy().contains("/tmp/v.log"));
    }

    #[test]
    fn an_app_cannot_set_shep_environment_by_hand() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("SHEP_ENVIRONMENT".into(), "sneaky".into());
        // normalize refuses it, the same way it refuses SHEP_NAME.
        assert!(normalize(config).is_err());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p shep-daemon --lib --all-features -- --skip ::slow:: assemble`
Expected: compile failure across the crate, since `template::render` changed arity in Task 2.

- [ ] **Step 3: Make `assemble` fallible**

Add the fifth parameter and the `Result`. Every `template::render` call inside becomes `?`, wrapped so the error carries which field it came from:

```rust
/// Which `AppConfig` field a template failed in, and why.
///
/// `field` is the env key for an `env` failure, and otherwise the field's
/// own name (`args`, `out_file`, `err_file`). Carries no value: `source`'s
/// own `Display` quotes only the reference and the environment.
#[non_exhaustive]
#[derive(Debug)]
pub enum AssembleError {
    /// A template in `field` could not be rendered.
    Template {
        /// The env key or field name it was in.
        field: String,
        /// Why.
        source: RenderError,
    },
}
```

Injection sits beside the two existing fixed names:

```rust
    env.insert("SHEP_INSTANCE".to_string(), instance.to_string());
    env.insert("SHEP_NAME".to_string(), name.clone());
    env.insert("SHEP_ENVIRONMENT".to_string(), secrets.environment().to_string());
```

Add `SHEP_ENVIRONMENT` to whatever list in `normalize.rs` already refuses a hand-set `SHEP_INSTANCE` and `SHEP_NAME`. Grep for `SHEP_INSTANCE` in `normalize.rs` to find it.

- [ ] **Step 4: Build the view once per spawn, in the caller**

In `supervisor.rs`, add a private helper and call it at each of the nine sites. It reads the store fresh, which is the point: a `shep secret set` between two spawns is visible to the second without any daemon restart.

```rust
    /// The secret view one spawn of `app` resolves against.
    ///
    /// Reads `secrets.json` here rather than inside `assemble`, the same way
    /// `credentials` is resolved by the caller: real I/O belongs to the
    /// caller so `assemble` stays a pure function of its arguments.
    ///
    /// A store that cannot be read yields an empty view rather than failing
    /// here. A sheep that needs nothing from it still spawns, and one that
    /// does gets the ordinary refusal naming its own reference.
    fn secret_view(&self, app: &ResolvedApp) -> SecretView {
        let environment = app
            .config()
            .environment
            .clone()
            .unwrap_or_else(|| self.host_environment.clone());
        let store = shep_core::secrets::all(&self.paths.secrets).unwrap_or_default();
        SecretView::new(environment, store, self.provider_secrets.snapshot())
    }
```

`self.host_environment` is a `String` the supervisor is given at construction from `DaemonConfig::daemon::environment`. `self.provider_secrets` is Task 5's registry; until that task lands, use `BTreeMap::new()` here and replace it in Task 5.

- [ ] **Step 5: Handle the two refusal shapes**

At each call site, an `Err` becomes a spawn failure. The retriable case must reach the ordinary restart machinery so it shows as `waiting-restart`, and the non-retriable case must go straight to `Errored`.

Find the existing spawn-failure path (grep `SpawnFailed` in `supervisor.rs`) and branch on `AssembleError::is_retriable`. Write the refusal into the sheep's own log through the same narration the daemon already uses for a spawn failure, so `shep bleats <name>` shows it.

For the `preflight` site at `supervisor.rs:2796` and the prober site at `supervisor.rs:5022`, an `Err` means the sheep cannot be described; return the same refusal rather than unwrapping.

- [ ] **Step 6: Fix the dogs call site**

`dogs.rs:1398` assembles a dog. A dog's own config is not operator-templated, so pass `SecretView::empty(host_environment)` and treat an `Err` as an internal error: it cannot happen unless somebody puts a token in a built-in dog's config.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`
Expected: PASS.

Then run the unfiltered lib suite once, because this task touches the spawn path that the `slow` tier exercises:

Run: `cargo test -p shep-daemon --lib --all-features`
Expected: PASS.

- [ ] **Step 8: Run the task gate**

Four commands, one at a time. Expected `EXIT=0` from each.

- [ ] **Step 9: Commit**

```bash
git add crates/shep-daemon/src/assemble.rs crates/shep-daemon/src/supervisor.rs crates/shep-daemon/src/dogs.rs crates/shep-core/src/config/normalize.rs
git commit -m "feat(daemon): resolve {{secret:}} at spawn and inject SHEP_ENVIRONMENT"
```

---

## Task 5: The push API and the provider cache

**Cargo shape for this task:** `cargo test --workspace --all-features -- --skip ::slow::`

This one crosses shep-core and shep-daemon, so it uses the workspace shape rather than `-p`.

**Files:**
- Modify: `crates/shep-core/src/protocol/request.rs` (two variants)
- Modify: `crates/shep-core/src/protocol/mod.rs` (`PROTOCOL_VERSION` to 5, doc)
- Modify: `crates/shep-core/src/protocol/snapshots/` (rename `*_wire_v4` to `*_wire_v5`)
- Create: `crates/shep-daemon/src/secrets.rs` (the in-memory registry and its cache file)
- Modify: `crates/shep-daemon/src/rpc.rs` (the `PutSecrets` arm)
- Modify: `crates/shep-daemon/src/supervisor.rs` (wire the registry into `secret_view`)
- Modify: `crates/shep-core/CHANGELOG.md`, `crates/shep-daemon/CHANGELOG.md`

**Interfaces:**
- Consumes: `EnvValue` (already in `request.rs`, already redacted with its own exact-string test).
- Produces:
  ```rust
  Request::PutSecrets {
      namespace: String,
      environment: String,
      entries: BTreeMap<String, EnvValue>,
  }
  Response::SecretsPut { accepted: u32 }

  // crates/shep-daemon/src/secrets.rs
  pub struct ProviderSecrets { /* private */ }
  impl ProviderSecrets {
      pub fn load(cache: &Path) -> Self;
      pub fn put(&self, namespace: &str, environment: &str,
                 entries: BTreeMap<String, String>, persist: bool) -> std::io::Result<u32>;
      pub fn snapshot(&self) -> BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;
      pub fn namespaces(&self) -> BTreeSet<String>;
  }
  ```

- [ ] **Step 1: Write the failing tests**

In `crates/shep-daemon/src/secrets.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_replaces_that_namespace_and_environment_rather_than_merging() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");
        let store = ProviderSecrets::load(&cache);

        store
            .put("vercel", "production",
                 BTreeMap::from([("A".into(), "1".into()), ("B".into(), "2".into())]), false)
            .unwrap();
        // B is gone at the provider, so it must be gone here too.
        store
            .put("vercel", "production", BTreeMap::from([("A".into(), "9".into())]), false)
            .unwrap();

        let snap = store.snapshot();
        let vercel = &snap["vercel"];
        assert_eq!(vercel["A"]["production"], "9");
        assert!(!vercel.contains_key("B"), "a replaced push drops what it omits");
    }

    #[test]
    fn one_environment_does_not_disturb_another() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderSecrets::load(&dir.path().join("secrets-cache.json"));
        store.put("v", "production", BTreeMap::from([("A".into(), "p".into())]), false).unwrap();
        store.put("v", "staging", BTreeMap::from([("A".into(), "s".into())]), false).unwrap();
        let snap = store.snapshot();
        assert_eq!(snap["v"]["A"]["production"], "p");
        assert_eq!(snap["v"]["A"]["staging"], "s");
    }

    #[test]
    fn a_persisted_push_survives_a_reload_and_an_unpersisted_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");

        let first = ProviderSecrets::load(&cache);
        first.put("kept", "production", BTreeMap::from([("A".into(), "1".into())]), true).unwrap();
        first.put("gone", "production", BTreeMap::from([("B".into(), "2".into())]), false).unwrap();

        let second = ProviderSecrets::load(&cache);
        assert!(second.namespaces().contains("kept"));
        assert!(!second.namespaces().contains("gone"));
    }

    #[test]
    fn a_namespace_is_known_as_soon_as_it_is_pushed_even_when_empty() {
        // MissingNamespace means "no dog has pushed here"; an empty push is
        // a dog saying it has nothing, which is a different answer.
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderSecrets::load(&dir.path().join("secrets-cache.json"));
        store.put("v", "production", BTreeMap::new(), false).unwrap();
        assert!(store.namespaces().contains("v"));
    }

    #[test]
    #[cfg(unix)]
    fn the_cache_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("secrets-cache.json");
        ProviderSecrets::load(&cache)
            .put("v", "production", BTreeMap::from([("A".into(), "1".into())]), true)
            .unwrap();
        let mode = std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// IR-41.
    #[test]
    fn debug_never_prints_a_value() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderSecrets::load(&dir.path().join("secrets-cache.json"));
        store.put("v", "production", BTreeMap::from([("A".into(), "hunter2".into())]), false)
            .unwrap();
        let rendered = format!("{store:?}");
        assert_eq!(rendered, "ProviderSecrets { namespaces: 1 }");
    }
}
```

In `crates/shep-core/src/protocol/request.rs`'s test module:

```rust
    #[test]
    fn put_secrets_round_trips_and_hides_its_values() {
        let request = Request::PutSecrets {
            namespace: "vercel".into(),
            environment: "production".into(),
            entries: BTreeMap::from([("API_KEY".to_string(), EnvValue::from("sk_live".to_string()))]),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), request);
        assert!(!format!("{request:?}").contains("sk_live"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --all-features -- --skip ::slow:: secrets`
Expected: FAIL, `no variant named PutSecrets`, `cannot find type ProviderSecrets`.

- [ ] **Step 3: Add the wire variants**

In `request.rs`, following the shape of the neighbouring variants and documenting each field:

```rust
    /// A provider dog's values for one namespace and one environment.
    ///
    /// Replaces that pair rather than merging into it, so a key deleted at
    /// the provider disappears here on the next push instead of lingering.
    ///
    /// `namespace` is the dog's own registered name. It is bookkeeping, not
    /// authorization: `Hello::dog_name` is self-declared and nothing checks
    /// it against the spawn. The boundary is the socket itself, which lives
    /// under `$SHEP_HOME` at `0700`.
    ///
    /// Answers [`Response::SecretsPut`].
    PutSecrets {
        /// The dog's registered name.
        namespace: String,
        /// Which environment these values are for.
        environment: String,
        /// The values, keyed by secret name. [`EnvValue`] so a `{:?}` of
        /// this request cannot print them.
        entries: BTreeMap<String, EnvValue>,
    },
```

and

```rust
    /// Answer to [`Request::PutSecrets`]: how many entries were stored.
    SecretsPut {
        /// Entry count, after the namespace and environment were replaced.
        accepted: u32,
    },
```

- [ ] **Step 4: Move `PROTOCOL_VERSION`**

Set it to 5 in `crates/shep-core/src/protocol/mod.rs` and update the module doc's version references. Rename the `*_wire_v4` snapshot tests and their fixture files to `v5`, per the convention that module's own doc states. Leave every `v1_*_fixture_still_deserializes` name alone: those record where bytes came from and never move.

Add to `crates/shep-core/CHANGELOG.md`:

```markdown
- `PROTOCOL_VERSION` is 5. `Request::PutSecrets` and `Response::SecretsPut`
  are additive, and the rule keeps the version for an addition, but the move
  to 4 already settled that an unbumped addition fails the operator: a newer
  client passes the handshake and the daemon then drops the connection on an
  envelope it cannot decode. A named `protocol_mismatch` refusal is better.
  Run `shep daemon reload` after upgrading.
```

- [ ] **Step 5: Write the registry**

`crates/shep-daemon/src/secrets.rs` holds a `Mutex<BTreeMap<...>>` plus the cache path. `put` replaces one `(namespace, environment)` pair, records the namespace as known even when `entries` is empty, and when `persist` is true rewrites the cache file through `create_staging_file(parent, "secrets-cache", ".tmp")` and `sync_dir`, the same mechanics Task 1 used. `load` reads the cache if present and treats a missing or unparseable file as empty, because a derived file is not worth refusing a boot over. Hand-written redacted `Debug`.

- [ ] **Step 6: Dispatch the request**

Add the `PutSecrets` arm to `rpc.rs` beside `SetDogConfig`. Read `persist` from the dog's own `[<namespace>]` section in `dogs.toml`, defaulting to `true`. Validate `namespace` and `environment` against the same grammar Task 1 exposes and refuse with `RpcErrorCode::InvalidConfig` rather than storing under a name a token could never reference.

- [ ] **Step 7: Wire the registry into the supervisor**

Replace the `BTreeMap::new()` placeholder Task 4 left in `secret_view` with `self.provider_secrets.snapshot()`, and construct `ProviderSecrets::load(&paths.secrets_cache)` where the supervisor is built.

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 9: Run the task gate**

Four commands, one at a time. Expected `EXIT=0` from each.

- [ ] **Step 10: Commit**

```bash
git add crates/shep-core/src/protocol/ crates/shep-daemon/src/secrets.rs crates/shep-daemon/src/rpc.rs crates/shep-daemon/src/lib.rs crates/shep-daemon/src/supervisor.rs crates/shep-core/CHANGELOG.md crates/shep-daemon/CHANGELOG.md
git commit -m "feat(daemon): let a provider dog push secrets into a namespace"
```

---

## Task 6: `shep secret`

**Cargo shape for this task:** `cargo test -p shep --lib --bins --all-features -- --skip ::slow::`

**Files:**
- Create: `crates/shep-cli/src/commands/secret.rs`
- Modify: `crates/shep-cli/src/commands/mod.rs`, `crates/shep-cli/src/cli.rs`, `crates/shep-cli/src/lib.rs`
- Modify: `crates/shep-core/src/config/daemon.rs` (`SecretsSection`)
- Modify: `crates/shep-cli/src/output.rs` (row types)

**Interfaces:**
- Consumes: `shep_core::secrets::{all, get, set, unset, SecretError, ALL_ENVIRONMENTS}` (Task 1); `DaemonConfig` (Task 3).
- Produces: `Commands::Secret(SecretArgs)` with a `SecretCommand` subcommand enum.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-cli/src/commands/secret.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_list_names_the_key_and_its_environments_but_no_value() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "DB_PASSWORD", None, "hunter2").unwrap();
        run_set(&paths, "DB_PASSWORD", Some("staging"), "staging-pw").unwrap();

        let listed = render_list(&paths).unwrap();
        assert!(listed.contains("DB_PASSWORD"));
        assert!(listed.contains("all"));
        assert!(listed.contains("staging"));
        assert!(!listed.contains("hunter2"), "a list never prints a value");
        assert!(!listed.contains("staging-pw"));
    }

    #[test]
    fn set_with_no_env_writes_the_all_slot() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "v").unwrap();
        assert_eq!(
            shep_core::secrets::get(&paths.secrets, "K", ALL_ENVIRONMENTS).unwrap().as_deref(),
            Some("v")
        );
    }

    #[test]
    fn get_is_refused_unless_shep_toml_turns_it_on() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "v").unwrap();

        let (code, err) = run_get_capturing(&paths, "K", None, /* allow_read */ false);
        assert_eq!(code, ExitCode::InvalidConfig);
        assert!(err.contains("allow_read"), "{err}");
        assert!(err.contains("[secrets]"), "{err}");
        assert!(!err.contains('v') || !err.contains("value is"), "no value in the refusal");
    }

    #[test]
    fn get_prints_the_value_once_it_is_turned_on() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "v").unwrap();
        let (code, out) = run_get_capturing_out(&paths, "K", None, /* allow_read */ true);
        assert_eq!(code, ExitCode::Success);
        assert_eq!(out.trim(), "v");
    }

    #[test]
    fn get_on_a_missing_key_exits_not_found_and_prints_nothing() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        let (code, out) = run_get_capturing_out(&paths, "ABSENT", None, true);
        assert_eq!(code, ExitCode::NotFound, "so `shep secret get k || default` works");
        assert!(out.is_empty());
    }

    #[test]
    fn a_bad_key_exits_usage_and_a_future_store_exits_invalid_config() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        assert_eq!(exit_code_for(&SecretError::InvalidKey("x y".into())), ExitCode::Usage);
        assert_eq!(exit_code_for(&SecretError::FutureVersion(9)), ExitCode::InvalidConfig);
        let _ = paths;
    }

    #[test]
    fn the_store_works_with_no_shepherd_running() {
        // The whole reason this verb touches the file directly. If this test
        // ever needs a daemon, the design has been broken.
        let home = tempfile::tempdir().unwrap();
        let paths = paths_in(home.path());
        run_set(&paths, "K", None, "v").unwrap();
        assert!(paths.secrets.exists());
    }
}
```

Follow the existing test helpers in `crates/shep-cli/src/commands/kv.rs`'s test module for `paths_in` and for capturing `Streams`. Do not invent new harness shapes.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p shep --lib --bins --all-features -- --skip ::slow:: secret`
Expected: FAIL, module does not exist.

- [ ] **Step 3: Add the config section**

In `daemon.rs`, beside `WhistleSection`:

```rust
/// The `[secrets]` section: whether the CLI will print a stored value back.
///
/// `Debug` is derived rather than redacted: one boolean, no secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SecretsSection {
    /// Whether `shep secret get` prints a value. Default `false`.
    pub allow_read: bool,
}
```

Add `secrets: SecretsSection` to `RawDaemonConfig` and to `DaemonConfig`, following how `whistle` is threaded through `load_layered`.

- [ ] **Step 4: Add the verb**

In `cli.rs`:

```rust
/// Arguments to `shep secret`.
#[derive(Debug, clap::Args)]
pub struct SecretArgs {
    #[command(subcommand)]
    pub command: SecretCommand,
}

/// `shep secret`'s subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum SecretCommand {
    /// Store a value
    Set {
        /// The key
        key: String,
        /// The value
        value: String,
        /// Which environment; omit for every environment
        #[arg(long)]
        env: Option<String>,
    },
    /// Print a value back, if `[secrets] allow_read` is on
    Get {
        /// The key
        key: String,
        /// Which environment; omit to resolve the way a spawn would
        #[arg(long)]
        env: Option<String>,
    },
    /// Remove a value
    Unset {
        /// The key
        key: String,
        /// Which environment; omit for the every-environment slot
        #[arg(long)]
        env: Option<String>,
    },
    /// List keys and the environments each has a value for
    List,
}
```

Add `Secret(SecretArgs)` to `Commands`, file `"secret"` under the `"The shepherd"` group in `HELP_GROUPS` beside `set`/`get`/`unset`, and dispatch it in `lib.rs` beside `Commands::Set`. Two existing tests will fail until the help group and the docs generator both know the verb; that is `every_visible_verb_appears_in_exactly_one_help_group` and `every_visible_verb_reaches_the_docs_site_generator` doing their job. Add `secret` to `VERBS` in `web/scripts/generate-cli-reference.sh` to satisfy the second.

- [ ] **Step 5: Write the command module**

Model it on `crates/shep-cli/src/commands/kv.rs`, including its `exit_code_for` shape. No `Client` anywhere in this module, for the same reason `kv.rs:1` gives.

`get` with no `--env` resolves the way a spawn would, through a `SecretView` built for the host environment, so an operator checking a value sees what the sheep would see. `get` with `--env X` reads that slot exactly.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p shep --lib --bins --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 7: Run the task gate**

Four commands, one at a time. Expected `EXIT=0` from each.

- [ ] **Step 8: Commit**

```bash
git add crates/shep-cli/src crates/shep-core/src/config/daemon.rs web/scripts/generate-cli-reference.sh
git commit -m "feat(shep): add shep secret set, get, unset and list"
```

---

## Task 7: `shep describe` names a sheep's secret references

**Cargo shape for this task:** `cargo test -p shep --lib --bins --all-features -- --skip ::slow::`

**Files:**
- Modify: `crates/shep-cli/src/commands/query.rs`
- Modify: `crates/shep-cli/src/output.rs`

**Interfaces:**
- Consumes: `SecretRef`, `SecretView`, `Resolution` (Task 1); `template`'s walk over a value (Task 2).
- Produces: a `references(config: &AppConfig) -> BTreeSet<String>` helper in `shep_core::secrets`, returning each reference as it was written, so both `describe` and any later caller ask one place. This is also the seam the spec promises to whatever boot-dependency work lands: `references` plus `SecretRef::parse` answers "which namespaces does this sheep need" with no I/O.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-core/src/secrets.rs`:

```rust
    #[test]
    fn references_finds_every_secret_in_a_config_and_nothing_else() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("A".into(), "{{secret:ONE}}".into());
        config.env.insert("B".into(), "plain".into());
        config.env.insert("C".into(), "{{name}}-{{secret:vercel/TWO}}".into());
        config.args = vec!["--x={{secret:ONE}}".into()];

        let found = references(&config);
        assert_eq!(
            found,
            BTreeSet::from(["ONE".to_string(), "vercel/TWO".to_string()]),
            "deduplicated, and no positional tokens"
        );
    }

    #[test]
    fn namespaces_of_a_config_is_the_seam_boot_ordering_will_want() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("A".into(), "{{secret:ONE}}".into());
        config.env.insert("B".into(), "{{secret:vercel/TWO}}".into());
        assert_eq!(namespaces_of(&config), BTreeSet::from(["vercel".to_string()]));
    }
```

In `crates/shep-cli/src/commands/query.rs`:

```rust
    #[test]
    fn describe_lists_secret_references_with_a_verdict_and_no_values() {
        let rendered = render_describe_secrets(&[
            ("DB_PASSWORD", "production", Resolution::Found("hunter2")),
            ("vercel/API_KEY", "production", Resolution::MissingNamespace),
            ("ABSENT", "production", Resolution::MissingKey),
        ]);
        assert!(rendered.contains("DB_PASSWORD"));
        assert!(rendered.contains("vercel/API_KEY"));
        assert!(!rendered.contains("hunter2"), "never a value");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --workspace --all-features -- --skip ::slow:: references`
Expected: FAIL, `cannot find function references`.

- [ ] **Step 3: Add the helpers**

In `secrets.rs`, `references` walks `config.env`'s values, `config.args`, `config.out_file` and `config.err_file` through the same token walker `template` uses, collecting `secret:` bodies. Expose the walk from `template` as `pub(crate)` rather than writing a second parser, so the two can never disagree about what a token is.

`namespaces_of` is `references` mapped through `SecretRef::parse` and filtered to `Some(namespace)`.

- [ ] **Step 4: Render it in `describe`**

Add a section to `shep describe <sheep>` listing each reference, the environment it resolved in, and one of three verdicts: resolved, missing, or provider not ready. Keys and namespaces only. Follow the JSON output envelope's existing shape for the structured form, and add the field additively so `SCHEMA_VERSION` does not move.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 6: Run the task gate**

Four commands, one at a time. Expected `EXIT=0` from each.

- [ ] **Step 7: Commit**

```bash
git add crates/shep-core/src/secrets.rs crates/shep-cli/src/commands/query.rs crates/shep-cli/src/output.rs
git commit -m "feat(shep): show a sheep's secret references in describe"
```

---

## Task 8: Documentation

**Cargo shape for this task:** `cargo test --workspace --all-features`

The docs site is part of the public surface, and this change adds a verb, a flag, a `shep.toml` section, a Flockfile key and a token. All five are the hard trigger.

**Files:**
- Create: `web/src/pages/docs/secrets.astro`
- Modify: `web/src/data/cli-reference.generated.txt` (regenerated by `web/scripts/generate-cli-reference.sh`, not hand-edited)
- Modify: `web/src/pages/docs/overrides.astro`, `first-flockfile.astro`, `getting-started.astro`, `dogs.astro`
- Modify: `docs/terminology.md`, `docs/dogs.md`
- Modify: `CLAUDE.md` (the verb count and the protocol version paragraph)

- [ ] **Step 1: Write the operator page**

`web/src/pages/docs/secrets.astro`, modelled on `overrides.astro`, which is the closest existing page. Cover, in this order: what the store is and where it lives, the token and both its shapes, environments and the `all` slot, why there is no cross-environment fallback, the four `shep secret` subcommands, `[secrets] allow_read` and why it is off, what a refused spawn looks like and how to tell the two refusals apart, and what a provider dog does.

Two things the page must say plainly rather than imply:

- A namespace is bookkeeping, not authorization. Anything that can open the control socket can already start an arbitrary process as the shepherd's user.
- shep does not encrypt the store, and the reason is that a key beside it at the same mode is a second file to lose rather than a second factor.

Match the prose to `overrides.astro` rather than writing fresh copy: same
length, same directness, no em dashes anywhere. Cut before rephrasing if a
paragraph runs long.

- [ ] **Step 2: Update the neighbouring pages**

- `overrides.astro`: a `{{secret:}}` reference is the recommended value for a credential the operator would otherwise put in an override.
- `first-flockfile.astro`: `environment`, and the `env = { DB_PASSWORD = "{{secret:DB_PASSWORD}}" }` pattern.
- `getting-started.astro`: the existing paragraph about restarting after an upgrade needs the protocol 5 bump added to it.
- `dogs.astro` and `docs/dogs.md`: `Request::PutSecrets`, the `persist` key, and `SHEP_ENVIRONMENT` in the list of variables a dog gets.
- `docs/terminology.md`: the store and the namespace.

- [ ] **Step 3: Regenerate the CLI reference**

```bash
cargo build --release
```
```bash
./web/scripts/generate-cli-reference.sh
```
Then `git diff` and confirm `shep secret` appears with its four subcommands.

- [ ] **Step 4: Build and check the site**

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```
Both. `check` is the one that catches a wrong prop, which `build` does not.
Expected: `EXIT=0` from each.

- [ ] **Step 5: Update CLAUDE.md**

The verb count paragraph moves from 41 generated and 42 listed to 42 and 43. Add a paragraph on `PROTOCOL_VERSION` 5, following the shape of the existing ones for 3 and 4.

- [ ] **Step 6: Run the task gate**

Four commands, one at a time. Expected `EXIT=0` from each.

- [ ] **Step 7: Commit**

```bash
git add web docs CLAUDE.md
git commit -m "docs: document the secret store, its token and its environments"
```

---

## Phase gate, before opening the pull request

Beyond the per-task gate:

```bash
cargo test --workspace --all-features -- --test-threads=1
```
```bash
cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu
```
```bash
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

Then one live run against a real daemon, because none of the above starts one and the whole point of this change is what a child process ends up holding:

```bash
export SHEP_HOME=$(mktemp -d /tmp/shep.XXXX)
```
```bash
./target/release/shep secret set DB_PASSWORD hunter2
```
```bash
./target/release/shep secret set DB_PASSWORD --env staging staging-pw
```

Register one app whose `env` is `{ PW = "{{secret:DB_PASSWORD}}" }` and whose script prints its own environment, start it, and read `shep bleats` to confirm the child got `hunter2` and `SHEP_ENVIRONMENT=production`. Set `environment = "staging"` on the sheep, restart it, and confirm it got `staging-pw`. Then `shep secret unset DB_PASSWORD --env staging`, restart, and confirm the spawn is refused by name rather than falling back to production's value. That last check is the one this design exists for.

`SHEP_HOME` must be a short path: a long one exceeds `SUN_LEN` for the control socket. Never point it at a real flock.

---

## Self-review

**Spec coverage.** Decisions 1 through 15 map to tasks as follows: 1 to Task 1 and Task 6 (the CLI half of "the CLI writes the file"), 2 to Task 8 (a docs claim, no code), 3 to Task 1, 4 to Task 3, 5 to Task 2, 6 to Task 5, 7 to Task 5, 8 to Task 2 and Task 4, 9 to Task 4, 10 to Task 6, 11 to Task 7, 12 to nothing by design (whistle gets no tools, so there is nothing to build; Task 8 records it), 13 spread across every task's redaction tests, 14 to Task 5, 15 to Task 7's `namespaces_of`.

**Type consistency.** `SecretView::new` takes `(String, BTreeMap<String, BTreeMap<String, String>>, BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>)` in Task 1 and is called with exactly that in Tasks 2, 4 and 5. `ProviderSecrets::snapshot` returns the third of those types, which is what Task 4's `secret_view` passes as the third argument. `RenderError::is_retriable` in Task 2 is what `AssembleError::is_retriable` forwards in Task 4 and what Task 4's step 5 branches on.

**Known placeholder, deliberate and closed.** Task 4 step 4 passes `BTreeMap::new()` for the namespace map because `ProviderSecrets` does not exist until Task 5. Task 5 step 7 replaces it. Both steps say so.
