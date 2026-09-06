# Boot ordering with dependency trees, implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a Flockfile a `depends_on` field, derive boot stages from it,
and hold each stage until its members report ready.

**Architecture:** A pure topological sort in shep-core turns a set of names
and edges into stages. An async driver in shep-daemon runs the stages against
the supervisor, waiting on the bus rather than inside the actor, because the
actor's own message loop is what delivers readiness. Shutdown walks the same
stages in reverse.

**Tech Stack:** Rust 2024, MSRV 1.88, tokio, serde, schemars, proptest.

**Spec:** [docs/brainstorming/specs/2026-09-06-boot-ordering-design.md](../../brainstorming/specs/2026-09-06-boot-ordering-design.md)

**What shipped differs from this plan in one place, 2026-09-06.** The plan
below moves `PROTOCOL_VERSION` from 4 to 5 and stops. It ended at 6: a late
review found that `Response::Reloading` had to carry the apps a staged reload
refused, and turning that tuple variant into a struct variant retypes the wire
shape. The body is left as written, because a plan is a record of what was
planned rather than a second copy of the outcome. `docs/decisions.md` and the
spec carry the delivered contract.

## Global constraints

- **Clean room.** Never open, read, or port source from `~/GitHub/pm2`.
- **Conventional commit subjects**, `type(scope): summary`, with `!` on the
  commit that actually breaks something, in the crate that breaks. release-plz
  walks individual commits and drops whatever does not parse.
- **Inner loop:** `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`
- **One cargo command shape per task.** Never alternate `--workspace` with
  `-p <crate>` in the same task.
- **Task gate** (once, when the task is otherwise done, one command each):
  `cargo fmt --all --check`;
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
  `cargo test --workspace --all-features`;
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`.
- **No em dashes** anywhere, code comments and commit messages included.
- **Never write the maintainer's real name, personal email, or an absolute
  home directory path** into any committed file, commit message, or PR body.
  Repo-relative paths only.
- **Never touch `~/.shep`.** Any live-daemon run uses `SHEP_HOME` pointed at a
  short `mktemp -d` path, because a long path exceeds `SUN_LEN` for the
  control socket.
- **Every new public item needs docs and a deliberate `Debug` decision.**
  Invoke the `shep-idiomatic-rust` skill before writing Rust here.
- **`#![forbid(unsafe_code)]` is live** in shep-core, shep-client and shep.
- **No lookout pane work.** Out of scope, stated in the spec.

---

## File structure

**Created**

- `crates/shep-core/src/config/graph.rs` - the pure sort. Names and edges in,
  stages out, plus unresolved names and cycles. No I/O, no tokio.
- `crates/shep-daemon/src/boot_order.rs` - the async driver. Runs a stage,
  waits on the bus, advances. Also drives the reverse shutdown.
- `web/src/pages/docs/boot-order.astro` - the operator-facing page.

**Modified**

- `crates/shep-core/src/config/app.rs` - the `depends_on` field.
- `crates/shep-core/src/config/normalize.rs` - three refusals and the
  document-local cycle check.
- `crates/shep-core/src/config/apply.rs` - the `NextSpawn` classification.
- `crates/shep-core/src/config/daemon.rs` - `boot_first_dogs`.
- `crates/shep-core/src/config/mod.rs` - re-exports for the new module.
- `crates/shep-core/src/protocol/mod.rs` - `PROTOCOL_VERSION` 4 to 5.
- `crates/shep-core/src/protocol/request.rs` - `ProcessInfo::depends_on`.
- `crates/shep-core/assets/flockfile.schema.json` - regenerated.
- `crates/shep-daemon/src/supervisor.rs` - `Command::Start`'s gate set, and
  `spawn_fresh` honouring it.
- `crates/shep-daemon/src/boot.rs` - `BootOptions::boot_first_dogs`, the
  staged restore, the staged teardown.
- `crates/shep-daemon/src/snapshot.rs` - `muster` goes through the driver.
- `crates/shep-daemon/src/rpc.rs` - staged `Start`, `Restart`, `Reload`.
- `crates/shep-cli/src/commands/daemon.rs` - `boot_first_dogs` assembly.
- `crates/shep-cli/src/commands/lifecycle.rs` - the computed deadline.
- `crates/shep-cli/src/output/mod.rs` - `emit_described`'s new row.

---

### Task 1: The `depends_on` field and its grammar refusals

**Files:**
- Modify: `crates/shep-core/src/config/app.rs`
- Modify: `crates/shep-core/src/config/normalize.rs`
- Modify: `crates/shep-core/assets/flockfile.schema.json` (regenerated, not
  hand-edited)

**Interfaces:**
- Produces: `AppConfig::depends_on: Vec<String>`;
  `NormalizeError::SelfDependency(String)`;
  `NormalizeError::InstanceDependency { sheep: String, target: String }`

- [ ] **Step 1: Write the failing tests**

Add to `crates/shep-core/src/config/normalize.rs`'s `mod tests`:

```rust
#[test]
fn a_sheep_that_depends_on_itself_is_refused_by_name() {
    // fails if the self-edge check is missing, or if it reports a bare
    // cycle rather than naming the sheep
    let mut app = AppConfig::minimal("api", "./api");
    app.depends_on = vec!["api".to_string()];
    match normalize(app) {
        Err(NormalizeError::SelfDependency(name)) => assert_eq!(name, "api"),
        other => panic!("expected SelfDependency, got {other:?}"),
    }
}

#[test]
fn a_dependency_on_one_instance_is_refused_naming_the_app_form() {
    // fails if `name:slot` is accepted as a dependency target
    let mut app = AppConfig::minimal("api", "./api");
    app.depends_on = vec!["db:2".to_string()];
    match normalize(app) {
        Err(NormalizeError::InstanceDependency { sheep, target }) => {
            assert_eq!(sheep, "api");
            assert_eq!(target, "db:2");
        }
        other => panic!("expected InstanceDependency, got {other:?}"),
    }
    let rendered = NormalizeError::InstanceDependency {
        sheep: "api".to_string(),
        target: "db:2".to_string(),
    }
    .to_string();
    assert!(
        rendered.contains("`db`"),
        "the refusal must name the app-level form: {rendered}"
    );
}

#[test]
fn duplicate_dependencies_dedupe_rather_than_refusing() {
    // fails if a repeated name is an error, or if it survives into the
    // normalized config twice
    let mut app = AppConfig::minimal("api", "./api");
    app.depends_on = vec!["db".to_string(), "db".to_string()];
    let resolved = normalize(app).expect("a repeated name is not an error");
    assert_eq!(resolved.config().depends_on, vec!["db".to_string()]);
}

#[test]
fn an_ordinary_dependency_list_survives_normalize() {
    // fails if the field is dropped or reordered
    let mut app = AppConfig::minimal("api", "./api");
    app.depends_on = vec!["db".to_string(), "cache".to_string()];
    let resolved = normalize(app).expect("an ordinary list normalizes");
    assert_eq!(
        resolved.config().depends_on,
        vec!["db".to_string(), "cache".to_string()]
    );
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p shep-core --lib --all-features depends`
Expected: FAIL, `no field 'depends_on' on type 'AppConfig'`.

- [ ] **Step 3: Add the field**

In `crates/shep-core/src/config/app.rs`, inside `AppConfig`, immediately after
the `fold` field so it lands in the `process` group beside it:

```rust
    /// Sheep or dogs that must be up before this one starts
    ///
    /// Names, never `name:slot`: a dependency on one instance of a
    /// load-balanced app is not a claim about availability. A dependency on
    /// a multi-instance app waits for every instance.
    ///
    /// Read once when a batch is ordered, at a boot, a muster, or a staged
    /// start, so an edit reaches the next such operation rather than the
    /// running child.
    #[cfg_attr(feature = "schema", schemars(extend("init" = {
        "example": "[\"db\", \"cache\"]",
        "group": "process",
        "blurb": "Other sheep or dogs that must be up before this one starts"
    })))]
    pub depends_on: Vec<String>,
```

In the same file's `Default` impl, beside `fold: None,`:

```rust
            depends_on: Vec::new(),
```

- [ ] **Step 4: Add the two refusals**

In `crates/shep-core/src/config/normalize.rs`, add to the `NormalizeError`
enum:

```rust
    /// An app names itself in `depends_on`. Carries the sheep name. A
    /// one-node cycle, caught here rather than in the graph because it is
    /// visible in a single `AppConfig`.
    SelfDependency(String),
    /// A `depends_on` entry names one instance rather than an app. Carries
    /// the sheep and the offending target, so the refusal can name both.
    InstanceDependency {
        /// The sheep whose list holds the entry
        sheep: String,
        /// The entry as written
        target: String,
    },
```

Add the two `Display` arms beside the existing ones:

```rust
            Self::SelfDependency(n) => {
                write!(f, "`{n}` names itself in depends_on")
            }
            Self::InstanceDependency { sheep, target } => {
                let app = target.split(':').next().unwrap_or(target);
                write!(
                    f,
                    "`{sheep}` depends on `{target}`, which names one instance. \
                     Depend on `{app}` instead: a dependency waits for every \
                     instance of an app"
                )
            }
```

Add the validation inside the function that `normalize_with_home` runs per
app, before it builds the `ResolvedApp`, and dedupe in place:

```rust
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::with_capacity(app.depends_on.len());
    for target in &app.depends_on {
        if target == &app.name {
            return Err(NormalizeError::SelfDependency(app.name.clone()));
        }
        if target.contains(':') {
            return Err(NormalizeError::InstanceDependency {
                sheep: app.name.clone(),
                target: target.clone(),
            });
        }
        if seen.insert(target.clone()) {
            deduped.push(target.clone());
        }
    }
    app.depends_on = deduped;
```

Add the two lines to `normalize`'s `# Errors` list, in the same style as its
neighbours:

```rust
/// - [`NormalizeError::SelfDependency`]: an app names itself in `depends_on`.
/// - [`NormalizeError::InstanceDependency`]: a `depends_on` entry is written `name:slot`.
```

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cargo test -p shep-core --lib --all-features depends`
Expected: PASS, four tests.

- [ ] **Step 6: Regenerate the schema and run the whole crate**

The schema is generated from the parser's own document type. Print it and
write it back:

```bash
cargo run -q -p shep --all-features -- schema > crates/shep-core/assets/flockfile.schema.json
```

Run: `cargo test -p shep-core --lib --all-features`
Expected: PASS. `every_field_carries_a_group_and_a_blurb` in `scaffold.rs`
covers the new attribute, and a pinned-schema test covers the asset. If the
schema test compares against the asset, the regeneration above satisfies it.

- [ ] **Step 7: Commit**

```bash
git add crates/shep-core/src/config/app.rs crates/shep-core/src/config/normalize.rs crates/shep-core/assets/flockfile.schema.json
git commit -m "feat(core): add the depends_on Flockfile field"
```

---

### Task 2: Move `PROTOCOL_VERSION` to 5

**Files:**
- Modify: `crates/shep-core/src/protocol/mod.rs:44`
- Modify: every `*_wire_v4` test name across `crates/shep-core/src/protocol/`

**Interfaces:**
- Consumes: `AppConfig::depends_on` from Task 1
- Produces: `PROTOCOL_VERSION == 5`

`AppConfig` is `#[serde(deny_unknown_fields, default)]` at `app.rs:73`. The
protocol's additive rule assumes the receiver tolerates unknown fields, and
this one does not, so a daemon at protocol 4 fails to decode a `depends_on` a
newer client sends. This is the same class as the 2 to 3 move for the
`ResetDepth` rename: live functionality regresses for a daemon that has not
restarted, rather than a new feature merely being unreachable.

- [ ] **Step 1: Write the failing test**

Add to `crates/shep-core/src/protocol/mod.rs`'s `mod tests` (create the module
if there is none):

```rust
#[test]
fn depends_on_forced_the_protocol_version_up() {
    // fails if the field lands without the bump. AppConfig is
    // deny_unknown_fields, so an older daemon cannot decode the new key
    // and the handshake has to catch it.
    assert_eq!(PROTOCOL_VERSION, 5);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p shep-core --lib --all-features depends_on_forced`
Expected: FAIL, `assertion failed: left 4, right 5`.

- [ ] **Step 3: Bump and rename**

In `crates/shep-core/src/protocol/mod.rs`, set `PROTOCOL_VERSION` to 5 and
update the module doc's opening line to say version 5. Replace its
"Version 4 bumped on an addition" paragraph with:

```rust
//! Version 4 bumped on an addition. Version 5 bumped on a new `AppConfig`
//! field: that struct is `deny_unknown_fields`, so the additive rule below
//! does not cover it and an older daemon cannot decode `depends_on`.
```

Rename every `*_wire_v4` test to `*_wire_v5`. The `v1_*_fixture_still_deserializes`
names never move.

```bash
grep -rln '_wire_v4' crates/ | xargs sed -i '' 's/_wire_v4/_wire_v5/g'
```

- [ ] **Step 4: Run the crate's tests**

Run: `cargo test -p shep-core --lib --all-features`
Expected: PASS. A snapshot whose stored payload now differs needs its
expected value updated to the current shape, which is what the rename records.

- [ ] **Step 5: Commit**

The `!` goes here, in the crate that breaks.

```bash
git add crates/shep-core/
git commit -m "feat(core)!: move PROTOCOL_VERSION to 5 for depends_on"
```

---

### Task 3: The pure boot graph

**Files:**
- Create: `crates/shep-core/src/config/graph.rs`
- Modify: `crates/shep-core/src/config/mod.rs`
- Modify: `crates/shep-core/Cargo.toml` (proptest as a dev-dependency)

**Interfaces:**
- Produces: `shep_core::config::graph::{BootNode, NodeKind, BootPlan, Unresolved, plan, render_cycle}`

`plan` takes every node the flock has and answers with stages, the edges that
pointed at nothing, and any cycles. It knows the dog rule, so the rule is
testable without a daemon.

A dog's `depends_on` is always empty in practice, because `dog_app` builds a
dog's config from `AppConfig::minimal`. The type allows one anyway rather than
carrying a special case.

- [ ] **Step 1: Write the failing tests**

Create `crates/shep-core/src/config/graph.rs` with only the `mod tests` block
below plus `use super::*;`, so the file compiles as a test target that cannot
resolve its subject yet:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sheep(name: &str, deps: &[&str]) -> BootNode {
        BootNode {
            name: name.to_string(),
            depends_on: deps.iter().map(|d| (*d).to_string()).collect(),
            kind: NodeKind::Sheep,
        }
    }

    fn dog(name: &str, boot_first: bool) -> BootNode {
        BootNode {
            name: name.to_string(),
            depends_on: Vec::new(),
            kind: NodeKind::Dog { boot_first },
        }
    }

    #[test]
    fn a_chain_becomes_one_stage_per_link() {
        // fails if the sort collapses or reorders the chain
        let out = plan(&[sheep("web", &["api"]), sheep("api", &["db"]), sheep("db", &[])]);
        assert_eq!(out.stages, vec![vec!["db"], vec!["api"], vec!["web"]]);
    }

    #[test]
    fn independent_nodes_share_a_stage_sorted_by_name() {
        // fails if a stage's order depends on input order, which would make
        // the boot plan nondeterministic
        let out = plan(&[sheep("cache", &[]), sheep("db", &[]), sheep("api", &["db", "cache"])]);
        assert_eq!(out.stages, vec![vec!["cache", "db"], vec!["api"]]);
    }

    #[test]
    fn a_cycle_is_named_as_a_path_and_its_nodes_run_last() {
        // fails if the cycle is merely detected, or if a cycle sinks the
        // whole plan rather than being isolated into a final stage
        let out = plan(&[sheep("a", &["c"]), sheep("b", &["a"]), sheep("c", &["b"]), sheep("lone", &[])]);
        assert_eq!(out.cycles.len(), 1, "one cycle expected: {:?}", out.cycles);
        let rendered = render_cycle(&out.cycles[0]);
        assert!(rendered.starts_with("a -> ") || rendered.starts_with("b -> ") || rendered.starts_with("c -> "));
        assert_eq!(rendered.matches(" -> ").count(), 3, "the path must close: {rendered}");
        assert_eq!(out.stages.last().unwrap(), &vec!["a", "b", "c"]);
    }

    #[test]
    fn an_edge_to_a_name_nobody_has_is_recorded_and_dropped() {
        // fails if an unknown name refuses the plan or silently vanishes
        let out = plan(&[sheep("api", &["nope"])]);
        assert_eq!(out.stages, vec![vec!["api"]]);
        assert_eq!(
            out.unresolved,
            vec![Unresolved { dependent: "api".to_string(), missing: "nope".to_string() }]
        );
    }

    #[test]
    fn a_dog_nobody_depends_on_runs_last() {
        // fails if dogs join the ordinary sort, which would move every
        // existing install's boot order
        let out = plan(&[dog("metrics", false), sheep("db", &[]), sheep("api", &["db"])]);
        assert_eq!(out.stages, vec![vec!["db"], vec!["api"], vec!["metrics"]]);
    }

    #[test]
    fn a_boot_first_dog_runs_before_every_sheep() {
        // fails if boot_first is ignored, which is the log-rotate case
        let out = plan(&[dog("log-rotate", true), sheep("db", &[]), dog("metrics", false)]);
        assert_eq!(out.stages, vec![vec!["log-rotate"], vec!["db"], vec!["metrics"]]);
    }

    #[test]
    fn a_dog_something_depends_on_takes_its_graph_position() {
        // fails if the dogs-last default outranks an explicit edge
        let out = plan(&[
            dog("sidecar", false),
            sheep("db", &[]),
            sheep("api", &["db", "sidecar"]),
            dog("metrics", false),
        ]);
        assert_eq!(out.stages, vec![vec!["db", "sidecar"], vec!["api"], vec!["metrics"]]);
    }

    #[test]
    fn an_empty_flock_plans_no_stages() {
        // fails if the sort emits an empty stage, which the driver would
        // then wait on
        assert!(plan(&[]).stages.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p shep-core --lib --all-features graph::`
Expected: FAIL, `cannot find type 'BootNode' in this scope`.

- [ ] **Step 3: Write the module**

Above the `mod tests` block in the same file:

```rust
//! Boot ordering: names and edges in, stages out.
//!
//! Pure. No I/O and no runtime, so the dog positioning rule below is
//! testable without a daemon. The daemon's driver runs what this produces;
//! it decides nothing about order itself.

use std::collections::{BTreeMap, BTreeSet};

/// What a node is, which decides its default position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A managed user process. Sorted purely by its edges.
    Sheep,
    /// A plugin process the daemon supervises.
    Dog {
        /// Named in `[daemon] boot_first_dogs`, so it runs before every
        /// sheep.
        boot_first: bool,
    },
}

/// One node of the boot graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootNode {
    /// The sheep or dog name.
    pub name: String,
    /// Names this node waits for. Always empty for a dog, since `dog_app`
    /// builds a dog's config from `AppConfig::minimal`.
    pub depends_on: Vec<String>,
    /// Sheep or dog.
    pub kind: NodeKind,
}

/// An edge that pointed at a name the flock does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    /// The node whose list holds the edge.
    pub dependent: String,
    /// The name nothing answers to.
    pub missing: String,
}

/// The order to start a flock in, and what was wrong with the graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootPlan {
    /// Stages in start order. Every stage is non-empty and its names are
    /// sorted, so one flock always plans the same way.
    pub stages: Vec<Vec<String>>,
    /// Edges dropped because nothing answers to the name.
    pub unresolved: Vec<Unresolved>,
    /// Each cycle as the path a walk found it on, first name not repeated
    /// at the end. [`render_cycle`] closes it for display.
    pub cycles: Vec<Vec<String>>,
}

/// Renders one cycle as `a -> b -> c -> a`.
///
/// The closing repeat is what makes it readable as a cycle rather than as a
/// list of names that happen to be involved in one.
#[must_use]
pub fn render_cycle(cycle: &[String]) -> String {
    let mut path: Vec<&str> = cycle.iter().map(String::as_str).collect();
    if let Some(first) = cycle.first() {
        path.push(first.as_str());
    }
    path.join(" -> ")
}

/// Sorts `nodes` into stages.
///
/// Edges to a name outside `nodes` are dropped and recorded in
/// [`BootPlan::unresolved`], because a dependency on an app whose Flockfile
/// lives in another repository is legitimate.
///
/// Nodes in a cycle are lifted out of the sort into a final unordered stage
/// and recorded in [`BootPlan::cycles`]. Refusing here would strand an
/// unattended boot; the caller decides whether to refuse, and only the
/// operator-facing callers do.
///
/// Dogs default to a final stage, after every sheep, so an existing install
/// keeps the order `boot.rs` argues for. Two things move one: `boot_first`,
/// which puts it in the first stage, and anything depending on it, which
/// gives it an ordinary graph position. A dog runs at the earliest stage
/// anything asks for.
#[must_use]
pub fn plan(nodes: &[BootNode]) -> BootPlan {
    let names: BTreeSet<&str> = nodes.iter().map(|n| n.name.as_str()).collect();

    let mut unresolved = Vec::new();
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for node in nodes {
        let deps = edges.entry(node.name.as_str()).or_default();
        for target in &node.depends_on {
            if names.contains(target.as_str()) {
                deps.insert(target.as_str());
            } else {
                unresolved.push(Unresolved {
                    dependent: node.name.clone(),
                    missing: target.clone(),
                });
            }
        }
    }

    let cycles = find_cycles(&edges);
    let in_a_cycle: BTreeSet<&str> = cycles
        .iter()
        .flat_map(|cycle| cycle.iter().map(String::as_str))
        .collect();

    let depended_on: BTreeSet<&str> = edges.values().flatten().copied().collect();
    let mut first = Vec::new();
    let mut last = Vec::new();
    let mut ordered: BTreeSet<&str> = BTreeSet::new();
    for node in nodes {
        let name = node.name.as_str();
        if in_a_cycle.contains(name) {
            continue;
        }
        match node.kind {
            NodeKind::Dog { boot_first: true } => first.push(name),
            NodeKind::Dog { boot_first: false } if !depended_on.contains(name) => last.push(name),
            _ => {
                ordered.insert(name);
            }
        }
    }
    first.sort_unstable();
    last.sort_unstable();

    let mut stages: Vec<Vec<String>> = Vec::new();
    if !first.is_empty() {
        stages.push(first.iter().map(|n| (*n).to_string()).collect());
    }
    stages.extend(kahn(&ordered, &edges, &first));
    if !last.is_empty() {
        stages.push(last.iter().map(|n| (*n).to_string()).collect());
    }
    for cycle in &cycles {
        let mut names: Vec<String> = cycle.clone();
        names.sort();
        stages.push(names);
    }

    BootPlan {
        stages,
        unresolved,
        cycles,
    }
}

/// Kahn's algorithm over `ordered`, taking every node whose remaining edges
/// are satisfied as one stage. `already` names nodes placed in an earlier
/// stage, whose edges are therefore met.
fn kahn(
    ordered: &BTreeSet<&str>,
    edges: &BTreeMap<&str, BTreeSet<&str>>,
    already: &[&str],
) -> Vec<Vec<String>> {
    let mut placed: BTreeSet<&str> = already.iter().copied().collect();
    let mut left: BTreeSet<&str> = ordered.clone();
    let mut stages = Vec::new();
    while !left.is_empty() {
        let ready: Vec<&str> = left
            .iter()
            .copied()
            .filter(|name| {
                edges
                    .get(name)
                    .is_none_or(|deps| deps.iter().all(|d| placed.contains(d) || !left.contains(d)))
            })
            .collect();
        // Cycles were lifted out before this ran, so a stall is impossible.
        // Breaking rather than looping forever is the safe arm regardless.
        if ready.is_empty() {
            break;
        }
        for name in &ready {
            left.remove(name);
            placed.insert(name);
        }
        stages.push(ready.iter().map(|n| (*n).to_string()).collect());
    }
    stages
}

/// Every cycle in `edges`, each as the path the walk closed on.
///
/// A depth-first walk rather than Kahn's leftovers: "these nodes are in a
/// cycle" is not something an operator can act on, and the path is.
fn find_cycles(edges: &BTreeMap<&str, BTreeSet<&str>>) -> Vec<Vec<String>> {
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut found: Vec<Vec<String>> = Vec::new();
    for start in edges.keys() {
        let mut stack: Vec<&str> = Vec::new();
        walk(start, edges, &mut stack, &mut done, &mut found);
    }
    found
}

fn walk<'a>(
    name: &'a str,
    edges: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    stack: &mut Vec<&'a str>,
    done: &mut BTreeSet<&'a str>,
    found: &mut Vec<Vec<String>>,
) {
    if let Some(at) = stack.iter().position(|seen| *seen == name) {
        let cycle: Vec<String> = stack[at..].iter().map(|n| (*n).to_string()).collect();
        let mut sorted = cycle.clone();
        sorted.sort();
        let already = found.iter().any(|other| {
            let mut o = other.clone();
            o.sort();
            o == sorted
        });
        if !already {
            found.push(cycle);
        }
        return;
    }
    if done.contains(name) {
        return;
    }
    stack.push(name);
    if let Some(deps) = edges.get(name) {
        for dep in deps {
            walk(dep, edges, stack, done, found);
        }
    }
    stack.pop();
    done.insert(name);
}
```

- [ ] **Step 4: Register the module**

In `crates/shep-core/src/config/mod.rs`, add `pub mod graph;` in alphabetical
order (after `flockfile`), and the re-export beside its neighbours:

```rust
pub use graph::{BootNode, BootPlan, NodeKind, Unresolved, plan, render_cycle};
```

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cargo test -p shep-core --lib --all-features graph::`
Expected: PASS, eight tests.

- [ ] **Step 6: Add the property test**

In `crates/shep-core/Cargo.toml`, under `[dev-dependencies]`:

```toml
proptest.workspace = true
```

Add to `graph.rs`'s `mod tests`:

```rust
    proptest::proptest! {
        #[test]
        fn every_edge_is_respected_in_the_planned_order(
            edges in proptest::collection::vec((0usize..8, 0usize..8), 0..24)
        ) {
            // A DAG by construction: an edge only ever points at a lower
            // index, so no input can be cyclic and every edge must show up
            // as a strictly earlier stage.
            let mut deps: Vec<Vec<String>> = vec![Vec::new(); 8];
            for (from, to) in edges {
                if to < from {
                    deps[from].push(format!("n{to}"));
                }
            }
            let nodes: Vec<BootNode> = (0..8)
                .map(|i| BootNode {
                    name: format!("n{i}"),
                    depends_on: deps[i].clone(),
                    kind: NodeKind::Sheep,
                })
                .collect();
            let out = plan(&nodes);
            proptest::prop_assert!(out.cycles.is_empty());
            let mut stage_of = std::collections::BTreeMap::new();
            for (index, stage) in out.stages.iter().enumerate() {
                for name in stage {
                    stage_of.insert(name.clone(), index);
                }
            }
            for node in &nodes {
                for dep in &node.depends_on {
                    proptest::prop_assert!(stage_of[dep] < stage_of[&node.name]);
                }
            }
        }
    }
```

- [ ] **Step 7: Run the crate's tests**

Run: `cargo test -p shep-core --lib --all-features`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/shep-core/src/config/graph.rs crates/shep-core/src/config/mod.rs crates/shep-core/Cargo.toml
git commit -m "feat(core): add the boot graph and its topological sort"
```

---

### Task 4: The document-local cycle check, and the apply classification

**Files:**
- Modify: `crates/shep-core/src/config/normalize.rs`
- Modify: `crates/shep-core/src/config/apply.rs`

**Interfaces:**
- Consumes: `graph::{BootNode, NodeKind, plan, render_cycle}` from Task 3
- Produces: `NormalizeError::DependencyCycle(Vec<String>)`

`normalize_all` already validates a whole document and already owns
`DuplicateName`, so the cycle check belongs beside it. This catches a cycle
inside one Flockfile. A cycle spanning the file and the registered flock is
the daemon's to find, in Task 8.

- [ ] **Step 1: Write the failing tests**

Add to `crates/shep-core/src/config/normalize.rs`'s `mod tests`:

```rust
#[test]
fn a_cycle_inside_one_document_is_refused_and_named() {
    // fails if normalize_all accepts a cycle, or reports it without a path
    let mut a = AppConfig::minimal("a", "./a");
    a.depends_on = vec!["b".to_string()];
    let mut b = AppConfig::minimal("b", "./b");
    b.depends_on = vec!["a".to_string()];
    match normalize_all(vec![a, b]) {
        Err(NormalizeError::DependencyCycle(cycle)) => {
            let rendered = NormalizeError::DependencyCycle(cycle).to_string();
            assert!(rendered.contains("a"), "{rendered}");
            assert!(rendered.contains("b"), "{rendered}");
            assert!(rendered.contains(" -> "), "the path must be shown: {rendered}");
        }
        other => panic!("expected DependencyCycle, got {other:?}"),
    }
}

#[test]
fn a_dependency_on_an_app_outside_the_document_is_accepted() {
    // fails if the document-local check refuses a cross-repository
    // dependency, which would make depends_on unusable across Flockfiles
    let mut api = AppConfig::minimal("api", "./api");
    api.depends_on = vec!["db-in-another-repo".to_string()];
    normalize_all(vec![api]).expect("an unresolved name is not a document error");
}
```

Add to `crates/shep-core/src/config/apply.rs`'s `mod tests` (the existing
`every_appconfig_field_has_a_group` test already covers presence; this one
pins the verdict):

```rust
#[test]
fn depends_on_applies_at_the_next_spawn() {
    // fails if the field is classified Live, which would claim an edit
    // reaches a running flock's order, or Structural, which would route it
    // through handle_scale
    assert_eq!(apply_group("depends_on"), ApplyGroup::NextSpawn);
    assert!(is_classified("depends_on"));
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep-core --lib --all-features cycle_inside depends_on_applies`
Expected: FAIL, `no variant named DependencyCycle`.

- [ ] **Step 3: Classify the field**

In `crates/shep-core/src/config/apply.rs`, in the `FIELDS` table, immediately
after the `("autostart", ApplyGroup::NextSpawn)` entry and its comment:

```rust
    // Read once when a batch is ordered, by the boot-order driver: a boot, a
    // muster, or a staged start. Nothing re-reads it while a sheep runs, and
    // nothing about it is baked into the child.
    ("depends_on", ApplyGroup::NextSpawn),
```

- [ ] **Step 4: Add the cycle refusal**

In `crates/shep-core/src/config/normalize.rs`, add the variant:

```rust
    /// Two or more apps in one document depend on each other. Carries the
    /// cycle as a path, so the refusal names it rather than only reporting
    /// that one exists.
    DependencyCycle(Vec<String>),
```

Its `Display` arm:

```rust
            Self::DependencyCycle(cycle) => write!(
                f,
                "dependency cycle: {}",
                crate::config::graph::render_cycle(cycle)
            ),
```

And the check at the end of `normalize_all`, replacing its current
`.collect()` tail:

```rust
pub fn normalize_all(apps: Vec<AppConfig>) -> Result<Vec<ResolvedApp>, NormalizeError> {
    let mut seen = BTreeSet::new();
    let resolved: Vec<ResolvedApp> = apps
        .into_iter()
        .map(|app| {
            if !seen.insert(app.name.clone()) {
                return Err(NormalizeError::DuplicateName(app.name));
            }
            normalize(app)
        })
        .collect::<Result<_, _>>()?;
    // Document-local only: a name this document does not hold is left to the
    // daemon, which is the only place that knows the whole flock.
    let nodes: Vec<crate::config::graph::BootNode> = resolved
        .iter()
        .map(|app| crate::config::graph::BootNode {
            name: app.config().name.clone(),
            depends_on: app.config().depends_on.clone(),
            kind: crate::config::graph::NodeKind::Sheep,
        })
        .collect();
    if let Some(cycle) = crate::config::graph::plan(&nodes).cycles.into_iter().next() {
        return Err(NormalizeError::DependencyCycle(cycle));
    }
    Ok(resolved)
}
```

Add the line to `normalize_all`'s `# Errors` list:

```rust
/// [`NormalizeError::DependencyCycle`]: two apps in the document depend on each other.
```

- [ ] **Step 5: Run the crate's tests**

Run: `cargo test -p shep-core --lib --all-features`
Expected: PASS.

- [ ] **Step 6: Commit**

Two concerns, so two commits.

```bash
git add crates/shep-core/src/config/apply.rs
git commit -m "feat(core): classify depends_on as NextSpawn"
git add crates/shep-core/src/config/normalize.rs
git commit -m "feat(core): refuse a dependency cycle inside one Flockfile"
```

---

### Task 5: Gate a dependency on `ReadinessSource::Heuristic`

**Files:**
- Modify: `crates/shep-daemon/src/supervisor.rs` (the `Command::Start` variant
  near line 177, `SupervisorHandle::start_with` near line 743, `handle_command`
  near line 2520, `do_start` near line 2745, `spawn_fresh` near line 3033)

**Interfaces:**
- Produces: `Command::Start { apps, policy, gate: BTreeSet<String>, reply }`;
  `SupervisorHandle::start_staged(apps, gate, policy)`

Today `spawn_fresh` computes `gated = !matches!(source, ReadinessSource::Heuristic)`,
so an app with no readiness signal is `Online` at spawn. An app something later
depends on has to hold at `Starting` for its own `listen_timeout` instead, or a
stage gate on it means nothing. The gate is per app rather than per stage, so
`shep start db` on its own is untouched.

- [ ] **Step 1: Write the failing test**

Add to `crates/shep-daemon/src/supervisor.rs`'s `mod tests`, beside the
existing `Command::Start` tests near line 10884:

```rust
#[tokio::test]
async fn an_app_in_the_gate_set_holds_at_starting_without_a_probe() {
    // fails if the gate is ignored: with no wait_ready and no
    // readiness_probe, spawn_fresh's ungated arm reports Online at once and
    // a stage gate on this app would wait for nothing
    let mut actor = test_actor();
    let (reply, rx) = oneshot::channel();
    let mut app = AppConfig::minimal("db", "./db");
    app.listen_timeout = UpDuration::from_millis(50);
    actor.handle_command(Command::Start {
        apps: vec![normalize(app).unwrap()],
        policy: BatchPolicy::AllOrNothing,
        gate: BTreeSet::from(["db".to_string()]),
        reply,
    });
    let started = rx.await.unwrap().unwrap();
    assert_eq!(started[0].status, ProcStatus::Starting);
}

#[tokio::test]
async fn an_app_outside_the_gate_set_is_online_at_spawn() {
    // fails if gating leaks to every app, which would hold a plain
    // `shep start db` at starting for its whole listen_timeout
    let mut actor = test_actor();
    let (reply, rx) = oneshot::channel();
    actor.handle_command(Command::Start {
        apps: vec![normalize(AppConfig::minimal("db", "./db")).unwrap()],
        policy: BatchPolicy::AllOrNothing,
        gate: BTreeSet::new(),
        reply,
    });
    let started = rx.await.unwrap().unwrap();
    assert_eq!(started[0].status, ProcStatus::Online);
}
```

`test_actor()` is whatever the surrounding tests already use to build an actor
over the fake runner. Read the neighbouring tests and reuse their helper
rather than adding one.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep-daemon --lib --all-features gate_set -- --skip ::slow::`
Expected: FAIL, `struct variant Command::Start has no field named gate`.

- [ ] **Step 3: Thread the gate through**

In the `Command::Start` variant:

```rust
        /// Names in `apps` that a later boot stage depends on.
        ///
        /// Each one is armed with [`ReadinessSource::Heuristic`] rather than
        /// reported `Online` at spawn, so a stage waiting on it waits for
        /// its `listen_timeout` rather than for `fork` returning. Empty for
        /// every caller but the boot-order driver.
        gate: BTreeSet<String>,
```

In `SupervisorHandle`, rename `start_with` to take the gate and add the
staged entry point:

```rust
    /// [`Self::start`], holding every app in `gate` at `Starting` until its
    /// readiness deadline, so a later stage can wait on it.
    ///
    /// # Errors
    ///
    /// The same set [`Self::start`] documents.
    pub(crate) async fn start_staged(
        &self,
        apps: Vec<ResolvedApp>,
        gate: BTreeSet<String>,
        policy: BatchPolicy,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command(Command::Start {
                apps,
                policy,
                gate,
                reply,
            }))
            .await
            .map_err(|_| SupervisorError::EngineStopped)?;
        rx.await.map_err(|_| SupervisorError::EngineStopped)?
    }
```

Have `start` and `start_restored` call it with `BTreeSet::new()` and their
existing policies, and delete `start_with`.

In `handle_command`, destructure the new field and pass it on:

```rust
            Command::Start {
                apps,
                policy,
                gate,
                reply,
            } => {
                let result = if self.shutting_down {
                    Err(SupervisorError::EngineStopped)
                } else {
                    self.do_start(apps, None, policy, &gate)
                };
                let _ = reply.send(result);
                false
            }
```

Give `do_start` the parameter (`gate: &BTreeSet<String>`), pass it to
`spawn_fresh` at every call site inside it, and have `do_start_dog` pass
`&BTreeSet::new()`.

In `spawn_fresh`, take `gate: &BTreeSet<String>` and replace the `gated` line:

```rust
        let source = ReadinessSource::of(app.config())
            .expect("ResolvedApp already passed ProbeTarget::parse in normalize");
        // An app a later stage waits on is gated even with no signal of its
        // own: the wait then costs its `listen_timeout`, which is the field's
        // documented fallback, rather than costing nothing.
        let gated =
            !matches!(source, ReadinessSource::Heuristic) || gate.contains(&app.config().name);
```

- [ ] **Step 4: Run the inner loop**

Run: `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/shep-daemon/src/supervisor.rs
git commit -m "feat(daemon): gate a depended-on app on its readiness deadline"
```

---

### Task 6: The stage driver

**Files:**
- Create: `crates/shep-daemon/src/boot_order.rs`
- Modify: `crates/shep-daemon/src/lib.rs` (register the module)

**Interfaces:**
- Consumes: `shep_core::config::graph::{BootNode, BootPlan, NodeKind, plan}`;
  `SupervisorHandle::start_staged`; `SupervisorHandle::stop`
- Produces: `boot_order::{nodes_for, start_in_stages, stop_in_reverse}`

The driver lives outside the actor. `do_start` is a synchronous `fn` reached
from the same message loop that delivers `Msg::ReadyResult`, so a wait inside
it could never end.

- [ ] **Step 1: Write the failing tests**

Create `crates/shep-daemon/src/boot_order.rs` holding only `use super::*;` and
this `mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_later_stage_does_not_start_until_the_earlier_one_is_online() {
        // fails if the driver fires every stage at once, which is what the
        // supervisor already does and what this exists to change
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        let mut db = AppConfig::minimal("db", "./sleep");
        db.listen_timeout = UpDuration::from_millis(50);
        let mut api = AppConfig::minimal("api", "./sleep");
        api.depends_on = vec!["db".to_string()];
        let apps = normalize_all(vec![db, api]).unwrap();
        let plan = plan(&nodes_for(&apps, &[]));
        assert_eq!(plan.stages, vec![vec!["db"], vec!["api"]]);

        let mut rx = h.ctx.events.subscribe();
        start_in_stages(&plan, apps, &h.ctx.supervisor, &h.ctx.events, BatchPolicy::PerApp).await;

        let mut order = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let BusEvent::Process { kind: ProcessEventKind::Start, info, .. } = event.as_ref() {
                order.push(info.name.clone());
            }
        }
        assert_eq!(order, vec!["db", "api"], "db must start before api");
    }

    #[tokio::test]
    async fn a_member_that_exits_does_not_hold_its_stage() {
        // fails if the driver waits for Online only: a dependency whose
        // binary is missing would then hold every later stage for the full
        // deadline instead of resolving at once
        // The first script exits at once, standing in for a binary that is
        // not there; the second never exits.
        let h = harness(vec![ProcScript::const_exit(1), ProcScript::never_exits()]);
        let mut db = AppConfig::minimal("db", "./does-not-exist");
        db.listen_timeout = UpDuration::from_millis(30_000);
        let mut api = AppConfig::minimal("api", "./sleep");
        api.depends_on = vec!["db".to_string()];
        let apps = normalize_all(vec![db, api]).unwrap();
        let plan = plan(&nodes_for(&apps, &[]));

        let started = tokio::time::timeout(
            Duration::from_secs(5),
            start_in_stages(&plan, apps, &h.ctx.supervisor, &h.ctx.events, BatchPolicy::PerApp),
        )
        .await
        .expect("a dead dependency must not hold the stage for its deadline");
        assert!(started.iter().any(|info| info.name == "api"));
    }

    #[tokio::test]
    async fn a_dog_lands_in_the_final_stage_and_boot_first_moves_it() {
        // fails if the dogs-last default is lost, which would move every
        // existing install's boot order
        let apps = normalize_all(vec![AppConfig::minimal("web", "./sleep")]).unwrap();
        let dogs = ["metrics".to_string()];
        let plain = plan(&nodes_for_with_dogs(&apps, &dogs, &[]));
        assert_eq!(plain.stages, vec![vec!["web"], vec!["metrics"]]);
        let promoted = plan(&nodes_for_with_dogs(&apps, &dogs, &["metrics".to_string()]));
        assert_eq!(promoted.stages, vec![vec!["metrics"], vec!["web"]]);
    }

    #[tokio::test]
    async fn stopping_walks_the_stages_backwards() {
        // fails if shutdown stays parallel, which gives a worker and its
        // database the same SIGTERM millisecond
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        let mut db = AppConfig::minimal("db", "./sleep");
        db.listen_timeout = UpDuration::from_millis(50);
        let mut api = AppConfig::minimal("api", "./sleep");
        api.depends_on = vec!["db".to_string()];
        let apps = normalize_all(vec![db, api]).unwrap();
        let plan = plan(&nodes_for(&apps, &[]));
        start_in_stages(&plan, apps, &h.ctx.supervisor, &h.ctx.events, BatchPolicy::PerApp).await;

        let mut rx = h.ctx.events.subscribe();
        stop_in_reverse(&plan, &h.ctx.supervisor).await;

        let mut order = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let BusEvent::Process { kind: ProcessEventKind::Stop, info, .. } = event.as_ref() {
                order.push(info.name.clone());
            }
        }
        assert_eq!(order, vec!["api", "db"], "api must stop before db");
    }
}
```

`harness` is `crate::testing::harness` and `ProcScript` is
`crate::fake::ProcScript`. `harness(vec![ProcScript::never_exits(); n])`
builds a `ScriptedRunner` over `n` scripted processes plus an `RpcContext`
wired to it, and `h.ctx` carries `supervisor`, `events`, `registry` and
`snapshot_path`. Do not add a second harness beside it.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep-daemon --lib --all-features boot_order:: -- --skip ::slow::`
Expected: FAIL, `cannot find function 'nodes_for' in this scope`.

- [ ] **Step 3: Write the driver**

Above the `mod tests` block:

```rust
//! Running a flock in dependency order.
//!
//! The sort itself is `shep_core::config::graph`. This module runs what it
//! produces: start a stage, wait for it, advance.
//!
//! It lives outside the supervisor actor deliberately. `do_start` is a
//! synchronous `fn` reached from the actor's own message loop, and that loop
//! is what delivers `Msg::ReadyResult`, so a wait inside it could never end.

use core::time::Duration;

use std::collections::{BTreeMap, BTreeSet};

use shep_core::config::graph::{BootNode, BootPlan, NodeKind};
use shep_core::config::{AppConfig, ResolvedApp};
use shep_core::protocol::{BusEvent, ProcessEventKind, ProcessInfo};
use shep_core::selector::ProcessSelector;

use crate::bus::Bus;
use crate::supervisor::{BatchPolicy, SupervisorHandle};

/// How much longer than the stage's own longest `listen_timeout` the driver
/// waits before giving up on it.
///
/// Every member is already bounded by its own readiness task, which reports
/// at its own deadline, so this covers scheduling jitter only. The same
/// reasoning as `RELOAD_DEADLINE_SLACK`, and the same figure.
pub(crate) const STAGE_SLACK: Duration = Duration::from_secs(5);

/// Graph nodes for a flock of sheep, with no dogs.
#[must_use]
pub(crate) fn nodes_for(apps: &[ResolvedApp], dogs: &[String]) -> Vec<BootNode> {
    nodes_for_with_dogs(apps, dogs, &[])
}

/// The plan for a set of apps plus the dogs this shepherd holds.
///
/// The one place a plan is built, so every caller positions dogs the same
/// way.
#[must_use]
pub(crate) fn plan_for(apps: &[ResolvedApp], dogs: &[String], boot_first: &[String]) -> BootPlan {
    shep_core::config::graph::plan(&nodes_for_with_dogs(apps, dogs, boot_first))
}

/// Graph nodes for a flock plus its dogs.
///
/// `boot_first` names the dogs `[daemon] boot_first_dogs` promotes ahead of
/// every sheep. A dog carries no `depends_on` of its own: `dog_app` builds a
/// dog's config from `AppConfig::minimal`, so its list is always empty.
#[must_use]
pub(crate) fn nodes_for_with_dogs(
    apps: &[ResolvedApp],
    dogs: &[String],
    boot_first: &[String],
) -> Vec<BootNode> {
    let promoted: BTreeSet<&str> = boot_first.iter().map(String::as_str).collect();
    apps.iter()
        .map(|app| BootNode {
            name: app.config().name.clone(),
            depends_on: app.config().depends_on.clone(),
            kind: NodeKind::Sheep,
        })
        .chain(dogs.iter().map(|name| BootNode {
            name: name.clone(),
            depends_on: Vec::new(),
            kind: NodeKind::Dog {
                boot_first: promoted.contains(name.as_str()),
            },
        }))
        .collect()
}

/// Starts `apps` stage by stage, holding each stage until its members are
/// online, exited, or errored.
///
/// Dogs in `plan` are skipped: they are spawned by `dogs::spawn_enabled_dogs`,
/// which the caller runs at the stage boundary this plan puts them in.
/// Answers with every instance started, in stage order.
pub(crate) async fn start_in_stages(
    plan: &BootPlan,
    apps: Vec<ResolvedApp>,
    supervisor: &SupervisorHandle,
    events: &Bus,
    policy: BatchPolicy,
) -> Vec<ProcessInfo> {
    let by_name: BTreeMap<&str, &ResolvedApp> = apps
        .iter()
        .map(|app| (app.config().name.as_str(), app))
        .collect();
    // Every name a later stage waits on. Read once, so a stage's own members
    // are gated by what follows them rather than by what sits beside them.
    let mut depended_on: BTreeSet<String> = BTreeSet::new();
    for app in &apps {
        depended_on.extend(app.config().depends_on.iter().cloned());
    }

    let mut started = Vec::new();
    for (index, stage) in plan.stages.iter().enumerate() {
        let members: Vec<ResolvedApp> = stage
            .iter()
            .filter_map(|name| by_name.get(name.as_str()).map(|app| (*app).clone()))
            .collect();
        if members.is_empty() {
            continue;
        }
        let gate: BTreeSet<String> = members
            .iter()
            .map(|app| app.config().name.clone())
            .filter(|name| depended_on.contains(name))
            .collect();
        let waiting: BTreeSet<String> = gate.clone();
        let bound = members
            .iter()
            .map(|app| app.config().listen_timeout.as_duration())
            .max()
            .unwrap_or_default()
            + STAGE_SLACK;

        // Subscribed before the spawn, so a fast app's `Online` cannot land
        // between the start and the wait. Same reasoning `boot` gives for
        // subscribing `spawn_dog_watch` ahead of the supervisor.
        let rx = events.subscribe();
        tracing::info!(stage = index, members = ?stage, "boot stage starting");
        match supervisor.start_staged(members, gate, policy).await {
            Ok(infos) => started.extend(infos),
            // Never fails the boot for the reason `spawn_enabled_dogs` does
            // not: a stage that could not start is a gap, and refusing the
            // rest of the flock over it turns the gap into an outage.
            Err(err) => tracing::warn!(stage = index, %err, "a boot stage did not start"),
        }
        if !waiting.is_empty() {
            await_stage(rx, waiting, bound).await;
        }
    }
    started
}

/// Waits until every name in `waiting` has reached a terminal answer, or
/// until `bound` elapses.
///
/// Terminal is `Online`, `Exit` or `Errored`. A member that dies resolves at
/// once rather than holding its stage for the full deadline, which is what
/// keeps a missing binary from costing the boot its whole budget.
async fn await_stage(
    mut rx: tokio::sync::broadcast::Receiver<crate::bus::SharedEvent>,
    mut waiting: BTreeSet<String>,
    bound: Duration,
) {
    let settle = async {
        while !waiting.is_empty() {
            match rx.recv().await {
                Ok(event) => {
                    if let BusEvent::Process { kind, info, .. } = event.as_ref()
                        && matches!(
                            kind,
                            ProcessEventKind::Online
                                | ProcessEventKind::Exit
                                | ProcessEventKind::Errored
                        )
                    {
                        waiting.remove(&info.name);
                    }
                }
                // A lagged receiver has missed the event it was waiting for,
                // so the bound below is the only thing left to end this.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    if tokio::time::timeout(bound, settle).await.is_err() {
        tracing::warn!("a boot stage did not settle inside its bound; advancing anyway");
    }
}

/// Stops every sheep in `plan`, last stage first.
///
/// Dogs are not here. They stop in `SupervisorHandle::shutdown`, after every
/// sheep, because monitoring should outlive what it monitors and a strict
/// reverse would kill the bark dog before the flock it reports on.
pub(crate) async fn stop_in_reverse(plan: &BootPlan, supervisor: &SupervisorHandle) {
    for stage in plan.stages.iter().rev() {
        for name in stage {
            // One name at a time rather than concurrently: the members of one
            // stage do not depend on each other, so nothing is gained by
            // overlapping them, and a serial walk keeps the emitted order
            // readable in the log.
            if let Err(err) = supervisor.stop(ProcessSelector::Name(name.clone())).await {
                tracing::warn!(sheep = %name, %err, "a sheep did not stop in its stage");
            }
        }
    }
}
```

Add `mod boot_order;` to `crates/shep-daemon/src/lib.rs` in alphabetical
order.

- [ ] **Step 4: Run the inner loop**

Run: `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/shep-daemon/src/boot_order.rs crates/shep-daemon/src/lib.rs
git commit -m "feat(daemon): add the boot-order stage driver"
```

---

### Task 7: `boot_first_dogs`

**Files:**
- Modify: `crates/shep-core/src/config/daemon.rs` (the `DaemonSection` struct)
- Modify: `crates/shep-daemon/src/boot.rs` (`BootOptions`)
- Modify: `crates/shep-cli/src/commands/daemon.rs:233` (`boot_options`)

**Interfaces:**
- Produces: `DaemonSection::boot_first_dogs: Vec<String>`;
  `BootOptions::boot_first_dogs: Vec<String>`

This is the lever that answers "should all dogs boot first" with no. It lives
in `shep.toml` rather than in `dogs.toml`, because a dog's section there is
passed through to the dog itself and a shep key in it would reach a program
that does not know the key. `adopted_dogs`' own doc comment already argues
exactly this.

- [ ] **Step 1: Write the failing tests**

Add to `crates/shep-core/src/config/daemon.rs`'s `mod tests`:

```rust
#[test]
fn boot_first_dogs_parses_and_defaults_empty() {
    // fails if the key is unknown, which deny_unknown_fields turns into a
    // startup error, or if it is not defaulted
    let config: DaemonConfig = parse_daemon_config(
        r#"
[daemon]
enabled_dogs = ["metrics"]
boot_first_dogs = ["log-rotate"]
"#,
    )
    .expect("boot_first_dogs is a known key");
    assert_eq!(config.daemon.boot_first_dogs, vec!["log-rotate".to_string()]);

    let bare: DaemonConfig = parse_daemon_config("[daemon]\n").expect("an empty section parses");
    assert!(bare.daemon.boot_first_dogs.is_empty());
}
```

Use whatever parse entry point the neighbouring tests in that file already
use; `parse_daemon_config` above stands in for it.

Add to `crates/shep-cli/src/commands/daemon.rs`'s `mod tests`, beside the
existing `boot_options` tests near line 963:

```rust
#[test]
fn boot_options_carries_the_promoted_dogs() {
    // fails if the key parses but never reaches the daemon, which would
    // leave log-rotate starting after the flock it exists to serve
    let config = daemon_config_from(
        r#"
[daemon]
enabled_dogs = ["metrics", "log-rotate"]
boot_first_dogs = ["log-rotate"]
"#,
    );
    let opts = boot_options(&config, &DaemonArgs::default(), None);
    assert_eq!(opts.boot_first_dogs, vec!["log-rotate".to_string()]);
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep-core --lib --all-features boot_first_dogs`
Expected: FAIL, `unknown field 'boot_first_dogs'`.

- [ ] **Step 3: Add the key**

In `crates/shep-core/src/config/daemon.rs`, in `DaemonSection`, after
`adopted_dogs`:

```rust
    /// Dogs that run before every sheep, rather than after the flock.
    ///
    /// The default position for a dog is a final stage, for the reason
    /// `boot.rs` gives: a metrics dog must not answer for a flock that is not
    /// up yet. A log-rotation dog is the opposite case, since it has to be
    /// running before a sheep starts writing. shep cannot tell which is
    /// which, because an adopted dog is a third-party binary, so the
    /// operator says.
    ///
    /// Here rather than in `dogs.toml` for the reason [`Self::adopted_dogs`]
    /// gives: that file's `[<name>]` table is the dog's own opaque
    /// configuration and a shep-owned key inside it would collide with a
    /// third-party dog's schema.
    ///
    /// A name absent from [`Self::enabled_dogs`] is inert here.
    pub boot_first_dogs: Vec<String>,
```

In `crates/shep-daemon/src/boot.rs`, in `BootOptions`, after `known_dogs`:

```rust
    /// Which of [`Self::dogs`] run before every sheep rather than after the
    /// flock, from `[daemon] boot_first_dogs`.
    ///
    /// Assembled by the caller from the same file [`Self::dogs`] comes out
    /// of, and for the same reason: shep-daemon never reads `shep.toml`
    /// itself.
    pub boot_first_dogs: Vec<String>,
```

In `crates/shep-cli/src/commands/daemon.rs`'s `boot_options`, after the
`known_dogs` field:

```rust
        boot_first_dogs: config.daemon.boot_first_dogs.clone(),
```

Every other `BootOptions` literal in the tree needs the field too. Find them:

```bash
grep -rn "BootOptions {" crates/
```

- [ ] **Step 4: Run both crates**

Run: `cargo test --workspace --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/shep-core/src/config/daemon.rs crates/shep-daemon/src/boot.rs crates/shep-cli/
git commit -m "feat(core): add boot_first_dogs to the daemon section"
```

---

### Task 8: Wire the driver into boot and muster

**Files:**
- Modify: `crates/shep-daemon/src/snapshot.rs:352` (`muster`)
- Modify: `crates/shep-daemon/src/boot.rs:915` (the restore and dog steps)

**Interfaces:**
- Consumes: `boot_order::{nodes_for_with_dogs, start_in_stages}`;
  `BootOptions::boot_first_dogs`

`muster` currently hands the whole of `to_start` to `start_restored` in one
batch. It becomes: build the plan, run the stages, and spawn the enabled dogs
at the stage boundary the plan puts them in.

Nothing refuses here. A cycle is warned and its nodes run last, which is the
final stage `plan` already produces.

- [ ] **Step 1: Write the failing test**

Add to `crates/shep-daemon/src/snapshot.rs`'s `mod tests`:

```rust
#[tokio::test]
async fn a_restore_starts_the_roll_in_dependency_order() {
    // fails if muster still hands the whole roll over as one batch
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let mut db = AppConfig::minimal("db", "./sleep");
    db.listen_timeout = UpDuration::from_millis(50);
    let mut api = AppConfig::minimal("api", "./sleep");
    api.depends_on = vec!["db".to_string()];
    // Written to the roll in the order that would be wrong if it were used.
    write_roll(&h.ctx.snapshot_path, &[api, db]);

    let mut rx = h.ctx.events.subscribe();
    muster(&h.ctx.snapshot_path, &h.ctx.registry, &h.ctx.supervisor, &h.ctx.events, &[], &[])
        .await
        .unwrap();

    let mut order = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let BusEvent::Process { kind: ProcessEventKind::Start, info, .. } = event.as_ref() {
            order.push(info.name.clone());
        }
    }
    assert_eq!(order, vec!["db", "api"], "roll order must not decide boot order");
}

#[tokio::test]
async fn a_cyclic_roll_still_brings_the_flock_up() {
    // fails if a cycle refuses the restore, which would strand an
    // unattended boot on a typo
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let mut a = AppConfig::minimal("a", "./sleep");
    a.depends_on = vec!["b".to_string()];
    let mut b = AppConfig::minimal("b", "./sleep");
    b.depends_on = vec!["a".to_string()];
    write_roll(&h.ctx.snapshot_path, &[a, b]);

    let restored = muster(
        &h.ctx.snapshot_path,
        &h.ctx.registry,
        &h.ctx.supervisor,
        &h.ctx.events,
        &[],
        &[],
    )
    .await
    .expect("a cycle must not refuse the restore");
    assert_eq!(restored.len(), 2);
}

#[tokio::test]
async fn a_dependency_with_autostart_off_is_warned_about_and_skipped() {
    // fails if depends_on overrides autostart, which would let one app's
    // file start a sheep another app's file said not to start
    let h = harness(vec![ProcScript::never_exits()]);
    let mut db = AppConfig::minimal("db", "./sleep");
    db.autostart = false;
    let mut api = AppConfig::minimal("api", "./sleep");
    api.depends_on = vec!["db".to_string()];
    write_roll(&h.ctx.snapshot_path, &[db, api]);

    let mut rx = h.ctx.events.subscribe();
    muster(&h.ctx.snapshot_path, &h.ctx.registry, &h.ctx.supervisor, &h.ctx.events, &[], &[])
        .await
        .unwrap();

    let mut started = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let BusEvent::Process { kind: ProcessEventKind::Start, info, .. } = event.as_ref() {
            started.push(info.name.clone());
        }
    }
    assert_eq!(started, vec!["api"], "db opted out; api starts without it");
}
```

`harness` and `ProcScript` are `crate::testing::harness` and
`crate::fake::ProcScript`: `harness(vec![ProcScript::never_exits(); n])` gives
a `Harness` whose `ctx` carries `supervisor`, `events`, `registry` and
`snapshot_path`. `write_roll` stands in for whatever this file's existing
tests already use to lay a roll down; read them and reuse it rather than
adding a second one.

Note the third test asserts the skip, not the warning text. A `tracing`
assertion would pin wording nobody has agreed on; the behaviour is what
matters.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep-daemon --lib --all-features dependency_order cyclic_roll -- --skip ::slow::`
Expected: FAIL, `this function takes 3 arguments but 5 were supplied`.

- [ ] **Step 3: Stage the restore**

Change `muster`'s signature to take the dog lists and the bus, and replace its
`start_restored` tail:

```rust
pub(crate) async fn muster(
    path: &Path,
    registry: &FlockRegistry,
    supervisor: &SupervisorHandle,
    events: &Bus,
    dogs: &[String],
    boot_first_dogs: &[String],
) -> Result<Vec<String>, SnapshotError> {
```

Everything down to and including the `register_at_rest` block is unchanged.
Replace the final block, from `let to_start: Vec<ResolvedApp>` onward, with:

```rust
    let to_start: Vec<ResolvedApp> = restorable
        .to_start
        .into_iter()
        .filter(|app| !known(app))
        .collect();
    if to_start.is_empty() {
        return Ok(restored);
    }
    // Recorded whether or not the stages fully succeed: already-registered
    // entries must persist even when a later spawn fails. Only what this call
    // starts, so an app left where it stands keeps the config it is running
    // under.
    registry.record(&to_start);

    let plan = crate::boot_order::plan_for(&to_start, dogs, boot_first_dogs);
    for unresolved in &plan.unresolved {
        tracing::warn!(
            sheep = %unresolved.dependent,
            missing = %unresolved.missing,
            "a dependency names nothing this flock has; starting without it"
        );
    }
    for cycle in &plan.cycles {
        tracing::warn!(
            cycle = %shep_core::config::graph::render_cycle(cycle),
            "a dependency cycle; those sheep start last, in no particular order"
        );
    }
    for app in &to_start {
        for target in &app.config().depends_on {
            if to_start
                .iter()
                .any(|other| &other.config().name == target && !other.config().autostart)
            {
                tracing::warn!(
                    sheep = %app.config().name,
                    dependency = %target,
                    "the dependency sets autostart = false; starting without it"
                );
            }
        }
    }
    // `BatchPolicy::PerApp`, not `AllOrNothing`: `start` refuses a whole
    // batch over an app whose script provably is not there, which is right
    // for an operator typing `shep start` and wrong at an unattended boot.
    crate::boot_order::start_in_stages(
        &plan,
        to_start,
        supervisor,
        events,
        BatchPolicy::PerApp,
    )
    .await;
    Ok(restored)
}
```

`plan_for` is the helper Task 6 added beside `nodes_for_with_dogs`, so both
callers position dogs the same way.

- [ ] **Step 4: Split the dog spawn around the flock**

In `crates/shep-daemon/src/boot.rs`, replace the restore-then-dogs pair with a
version that spawns the promoted dogs first. Where the current code reads:

```rust
    if options.restore && !inherited_flock {
        restore_flock(&paths, &registry, &supervisor).await?;
    }

    crate::dogs::spawn_enabled_dogs(&options.dogs, &paths, &supervisor, &events).await;
```

write:

```rust
    // Split around the restore rather than run whole after it: a
    // log-rotation dog has to be running before a sheep starts writing,
    // while a metrics dog must not answer for a flock that is not up. Both
    // are true in one flock, so `[daemon] boot_first_dogs` says which.
    let (first, rest): (Vec<DogSpec>, Vec<DogSpec>) = options
        .dogs
        .iter()
        .cloned()
        .partition(|spec| options.boot_first_dogs.contains(&spec.name));
    crate::dogs::spawn_enabled_dogs(&first, &paths, &supervisor, &events).await;

    if options.restore && !inherited_flock {
        restore_flock(
            &paths,
            &registry,
            &supervisor,
            &events,
            &options.dogs.iter().map(|spec| spec.name.clone()).collect::<Vec<_>>(),
            &options.boot_first_dogs,
        )
        .await?;
    }

    crate::dogs::spawn_enabled_dogs(&rest, &paths, &supervisor, &events).await;
```

Give `restore_flock` the matching parameters and pass them straight through to
`snapshot::muster`. Update the `boot` rustdoc's order sentence, which
currently reads *"dogs after the restore so a metrics dog does not answer for
an empty flock"*, to:

```rust
/// The order is load-bearing: handlers before the socket (SIGUSR2 otherwise
/// terminates), the pidfile lock before the bind it makes race-free,
/// `ready_fd` on the bind not the restore, `[daemon] boot_first_dogs` before
/// the restore and every other dog after it so a metrics dog does not answer
/// for an empty flock, [`BootOptions::notify_socket`] last.
```

Update the `Request::Muster` call site in `crates/shep-daemon/src/rpc.rs` to
pass the same lists; `RpcContext` already carries `known_dogs` and `paths`, so
thread the two lists onto it if they are not reachable.

- [ ] **Step 5: Run the inner loop**

Run: `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-daemon/
git commit -m "feat(daemon): restore the muster roll in dependency order"
```

---

### Task 9: Reverse-order shutdown

**Files:**
- Modify: `crates/shep-daemon/src/boot.rs` (`RunningDaemon::run`'s teardown,
  step 4)

**Interfaces:**
- Consumes: `boot_order::{plan_for, stop_in_reverse}`

The staged stop goes before the existing `SupervisorHandle::shutdown`, which
stays as the backstop so a driver bug cannot leave a child alive. Dogs are not
in the reverse stages and stop in that backstop.

- [ ] **Step 1: Write the failing test**

Add to `crates/shep-daemon/src/boot.rs`'s `mod tests`:

```rust
#[tokio::test]
async fn a_shutdown_stops_dependents_before_their_dependencies() {
    // fails if teardown still kills everything at once, which gives a
    // worker and its database the same SIGTERM millisecond
    let h = booted_daemon_with(&["db", "api"]).await;
    let mut rx = h.ctx.events.subscribe();
    h.ctx.shutdown();
    h.finished.await.unwrap().unwrap();

    let mut order = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let BusEvent::Process { kind: ProcessEventKind::Stop, info, .. } = event.as_ref() {
            order.push(info.name.clone());
        }
    }
    let api = order.iter().position(|n| n == "api").expect("api stopped");
    let db = order.iter().position(|n| n == "db").expect("db stopped");
    assert!(api < db, "api must stop before db: {order:?}");
}

#[tokio::test]
async fn a_dog_stops_after_every_sheep() {
    // fails if dogs join the reverse stages, which would kill the bark dog
    // before the flock it reports on
    let h = booted_daemon_with_dog("metrics", &["web"]).await;
    let mut rx = h.ctx.events.subscribe();
    h.ctx.shutdown();
    h.finished.await.unwrap().unwrap();

    let mut order = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let BusEvent::Process { kind: ProcessEventKind::Stop, info, .. } = event.as_ref() {
            order.push(info.name.clone());
        }
    }
    let web = order.iter().position(|n| n == "web").expect("web stopped");
    let metrics = order.iter().position(|n| n == "metrics").expect("metrics stopped");
    assert!(web < metrics, "the dog must outlive the flock: {order:?}");
}
```

`booted_daemon_with` and `booted_daemon_with_dog` stand in for the file's
existing boot harness; read the neighbouring tests and reuse it.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep-daemon --lib --all-features stops_dependents stops_after_every -- --skip ::slow::`
Expected: FAIL on the ordering assertion, since teardown is still parallel.

- [ ] **Step 3: Stage the teardown**

`RunningDaemon` needs the two dog lists to build the same plan the boot built.
Add them to the struct beside `paths`, populated in `boot` from `options`:

```rust
    /// The dogs this shepherd holds, and which of them run first, carried so
    /// the teardown can rebuild the same plan the boot ran.
    dog_names: Vec<String>,
    boot_first_dogs: Vec<String>,
```

In `run`, between step 3's `DaemonShutdown` broadcast and step 4's kill
ladder:

```rust
        // 4. Stop the flock in reverse dependency order, so a worker drains
        //    against a database that is still answering. Every sheep is
        //    bounded by its own kill ladder under `LadderCap::Stop`, and step
        //    5 below is the backstop: a stage this misses is still killed
        //    there, so a bug here cannot leave a child alive.
        let flock = ctx.supervisor.list_checked().await.unwrap_or_default();
        let apps = ctx.registry.resolved_for(&flock);
        let plan = crate::boot_order::plan_for(&apps, &dog_names, &boot_first_dogs);
        crate::boot_order::stop_in_reverse(&plan, &ctx.supervisor).await;

        // 5. Kill ladder on whatever is still online, dogs included: they are
        //    deliberately not in the reverse stages above, because monitoring
        //    should outlive what it monitors.
        ctx.supervisor.shutdown().await;
```

`FlockRegistry::resolved_for` may not exist. If it does not, read the
registry's existing accessors and use whichever already answers with the
`ResolvedApp` for a set of names; add one only if none does, documented, in
this same commit.

- [ ] **Step 4: Run the inner loop**

Run: `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/shep-daemon/src/boot.rs
git commit -m "feat(daemon): stop the flock in reverse dependency order"
```

---

### Task 10: Staged `Start`, and the CLI's computed deadline

**Files:**
- Modify: `crates/shep-daemon/src/rpc.rs` (the `Request::Start` and
  `Request::Add` handlers)
- Modify: `crates/shep-cli/src/commands/lifecycle.rs` (`load_one`'s request
  send)

**Interfaces:**
- Consumes: `boot_order::{plan_for, start_in_stages}`
- Produces: a `shep start` that refuses a cycle with `ExitCode::InvalidConfig`

A four-stage start costs nine seconds of heuristic waits and the client's
`DEFAULT_DEADLINE` is five, so `shep start` would fail with
`DeadlineExceeded` while the daemon did exactly what it was asked. The CLI
holds every `AppConfig` it is sending, so it computes the worst case itself:
every app in its own stage.

The daemon-side refusal is `AllOrNothing`'s audience: a human is at the
keyboard.

- [ ] **Step 1: Write the failing tests**

Add to `crates/shep-cli/tests/cli_e2e.rs` (or the integration file the
neighbouring `shep start` tests live in):

```rust
#[test]
fn starting_a_flockfile_with_a_cycle_refuses_and_names_it() {
    // fails if the cycle reaches the daemon, or if the message says only
    // that a cycle exists
    let home = short_home();
    let file = write_flockfile(
        &home,
        r#"
[[apps]]
name = "a"
script = "./sleep"
depends_on = ["b"]

[[apps]]
name = "b"
script = "./sleep"
depends_on = ["a"]
"#,
    );
    let out = shep(&home).args(["start", file.to_str().unwrap()]).output().unwrap();
    assert_eq!(out.status.code(), Some(4), "InvalidConfig");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(" -> "), "the cycle must be named: {stderr}");
}

#[test]
fn a_staged_start_outlives_the_default_client_deadline() {
    // fails if the CLI sends DEFAULT_DEADLINE: three unprobed stages cost
    // nine seconds of heuristic waits against a five second deadline
    let home = short_home();
    let file = write_flockfile(
        &home,
        r#"
[[apps]]
name = "db"
script = "./sleep"

[[apps]]
name = "api"
script = "./sleep"
depends_on = ["db"]

[[apps]]
name = "web"
script = "./sleep"
depends_on = ["api"]
"#,
    );
    let out = shep(&home).args(["start", file.to_str().unwrap()]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
}
```

`short_home`, `write_flockfile` and `shep` stand in for the file's existing
harness. Every live-daemon run uses a `mktemp -d` home, since a long path
exceeds `SUN_LEN` for the control socket.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep --test cli_e2e --all-features cycle_refuses staged_start`
Expected: FAIL. The first exits 0 with an arbitrary order; the second times
out.

- [ ] **Step 3: Stage the daemon's Start handler**

In `crates/shep-daemon/src/rpc.rs`'s `Request::Start` arm, replace the direct
`supervisor.start(apps)` call. The graph spans the incoming apps and the
registered flock, so build it over both:

```rust
    // The graph spans the batch and the flock: a cycle can close through a
    // sheep this request does not carry.
    let flock = ctx.supervisor.list_checked().await.unwrap_or_default();
    let existing = ctx.registry.resolved_for(&flock);
    let together: Vec<ResolvedApp> = existing.iter().cloned().chain(apps.iter().cloned()).collect();
    let plan = crate::boot_order::plan_for(&together, &ctx.dog_names, &ctx.boot_first_dogs);
    // Refused rather than warned: an operator typed this, so a human is
    // there to fix it. A boot takes the other arm, in `snapshot::muster`.
    if let Some(cycle) = plan.cycles.first() {
        return reply(Err(RpcError {
            code: RpcErrorCode::InvalidConfig,
            message: format!(
                "dependency cycle: {}",
                shep_core::config::graph::render_cycle(cycle)
            ),
            daemon_version: None,
        }));
    }
    let names: BTreeSet<&str> = apps.iter().map(|a| a.config().name.as_str()).collect();
    let batch_plan = BootPlan {
        stages: plan
            .stages
            .iter()
            .map(|stage| {
                stage
                    .iter()
                    .filter(|n| names.contains(n.as_str()))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|stage: &Vec<String>| !stage.is_empty())
            .collect(),
        unresolved: plan.unresolved,
        cycles: Vec::new(),
    };
    let started = crate::boot_order::start_in_stages(
        &batch_plan,
        apps,
        &ctx.supervisor,
        &ctx.events,
        BatchPolicy::AllOrNothing,
    )
    .await;
```

The `RpcError` literal is the shape this arm already uses for
`normalize_all`'s own refusal at `rpc.rs:320`, so the refusal maps to
`ExitCode::InvalidConfig`, 4, client-side without any new mapping.

- [ ] **Step 4: Compute the deadline client-side**

In `crates/shep-cli/src/commands/lifecycle.rs`, beside the other constants:

```rust
/// Slack over the summed readiness deadlines of a staged start.
///
/// Covers the daemon's own per-stage slack plus the round trip, so a client
/// gives up only after the daemon has.
const STAGED_START_SLACK: Duration = Duration::from_secs(10);

/// The deadline a staged start needs.
///
/// The worst case is every app in its own stage, each held for its own
/// `listen_timeout`, so the sum is the bound. `shep logs -f` asks for a
/// longer deadline the same way, and `action_timeout`'s own rustdoc argues
/// that a caller wanting longer has to ask for it in step.
fn staged_start_deadline(apps: &[ResolvedApp]) -> Duration {
    apps.iter()
        .map(|app| app.config().listen_timeout.as_duration())
        .sum::<Duration>()
        + STAGED_START_SLACK
}
```

At the point `load_one` sends its `Request::Start` or `Request::Add`, swap
`client.request(..)` for:

```rust
    let deadline = staged_start_deadline(&apps).max(shep_client::DEFAULT_DEADLINE);
    client.request_with_deadline(body, Some(deadline)).await
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p shep --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/shep-daemon/src/rpc.rs
git commit -m "feat(daemon): start a Flockfile batch in dependency order"
git add crates/shep-cli/src/commands/lifecycle.rs
git commit -m "feat(shep): size a staged start's deadline to its stages"
```

---

### Task 11: Ordered restart and reload

**Files:**
- Modify: `crates/shep-daemon/src/rpc.rs` (the `Request::Restart` and
  `Request::Reload` handlers)

**Interfaces:**
- Consumes: `boot_order::plan_for`

Forward, dependencies first, not reverse-stop then forward-start. The rolling
version sounds more correct and behaves worse: it puts the whole fold down at
once in the middle, where forward-only never does.

A restart stage completes on `Online`. A reload stage completes on `Reloaded`
or `ReloadAbandoned`, since reload keeps choosing `Serial` or `Overlap` per
app through `ReloadMode::of`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/shep-daemon/src/rpc.rs`'s `mod tests`:

```rust
#[tokio::test]
async fn a_restart_matching_several_walks_the_stages_forward() {
    // fails if a fold restarts as one batch, which restarts api against a
    // database that has not come back
    let h = harness_with(&[("db", &[]), ("api", &["db"])]).await;
    let mut rx = h.ctx.events.subscribe();
    handle(&h.ctx, Request::Restart { selector: SelectorSpec::All }).await.unwrap();

    let mut order = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let BusEvent::Process { kind: ProcessEventKind::Restart, info, .. } = event.as_ref() {
            order.push(info.name.clone());
        }
    }
    assert_eq!(order, vec!["db", "api"]);
}

#[tokio::test]
async fn a_reload_matching_several_walks_the_stages_forward() {
    // fails if a fold reloads as one batch
    let h = harness_with(&[("db", &[]), ("api", &["db"])]).await;
    let mut rx = h.ctx.events.subscribe();
    handle(&h.ctx, Request::Reload { selector: SelectorSpec::All }).await.unwrap();

    let mut order = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let BusEvent::Process { kind: ProcessEventKind::Reload, info, .. } = event.as_ref() {
            order.push(info.name.clone());
        }
    }
    assert_eq!(order, vec!["db", "api"]);
}
```

`harness_with` and `handle` stand in for the file's existing RPC test harness.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep-daemon --lib --all-features walks_the_stages -- --skip ::slow::`
Expected: FAIL on the order assertion.

- [ ] **Step 3: Stage both handlers**

In each handler, once the selector has matched, group the matches into the
plan's stages and walk them:

```rust
    // Forward, dependencies first. Not reverse-stop then forward-start: the
    // rolling version puts the whole fold down at once in the middle, and
    // forward-only never does.
    let flock = ctx.supervisor.list_checked().await.unwrap_or_default();
    let apps = ctx.registry.resolved_for(&flock);
    let plan = crate::boot_order::plan_for(&apps, &ctx.dog_names, &ctx.boot_first_dogs);
    let matched: BTreeSet<String> = matched.iter().map(|info| info.name.clone()).collect();
    let mut results = Vec::new();
    for stage in &plan.stages {
        for name in stage {
            if !matched.contains(name) {
                continue;
            }
            let rx = ctx.events.subscribe();
            match ctx.supervisor.restart(ProcessSelector::Name(name.clone())).await {
                Ok(infos) => results.extend(infos),
                Err(err) => tracing::warn!(sheep = %name, %err, "a sheep did not restart"),
            }
            await_settled(
                rx,
                name,
                &[ProcessEventKind::Online, ProcessEventKind::Exit],
                settle_bound(name, &apps, false),
            )
            .await;
        }
    }
```

For reload, swap `supervisor.restart` for `supervisor.reload` and the awaited
kinds for `[ProcessEventKind::Reloaded, ProcessEventKind::ReloadAbandoned]`.

Add the shared wait to `boot_order.rs`:

```rust
/// Waits until `name` emits one of `kinds`, or until `bound` elapses.
///
/// The single-name half of [`await_stage`], for the verbs that walk a stage
/// one sheep at a time.
pub(crate) async fn await_settled(
    mut rx: tokio::sync::broadcast::Receiver<crate::bus::SharedEvent>,
    name: &str,
    kinds: &[ProcessEventKind],
    bound: Duration,
) {
    let settle = async {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let BusEvent::Process { kind, info, .. } = event.as_ref()
                        && info.name == name
                        && kinds.contains(kind)
                    {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    if tokio::time::timeout(bound, settle).await.is_err() {
        tracing::warn!(sheep = %name, "did not settle inside its bound; advancing anyway");
    }
}
```

`settle_bound` is the small helper both handlers share, beside them in
`rpc.rs`. A restart is bounded by the sheep's readiness deadline; a reload is
bounded by its drain and then its readiness, so it gets both:

```rust
/// How long one sheep gets to settle before its stage moves on.
///
/// A restart waits out `listen_timeout`; a reload waits out
/// `graceful_timeout` and then `listen_timeout`, which is the pair its own
/// swap is already bounded by. A name the flock does not hold falls back to
/// the slack alone, which cannot happen for a matched selector and is not
/// worth an `unwrap` either way.
fn settle_bound(name: &str, apps: &[ResolvedApp], reloading: bool) -> Duration {
    let config = apps.iter().find(|app| app.config().name == name);
    let readiness = config.map_or(Duration::ZERO, |app| {
        app.config().listen_timeout.as_duration()
    });
    let drain = match (reloading, config) {
        (true, Some(app)) => app.config().graceful_timeout.as_duration(),
        _ => Duration::ZERO,
    };
    readiness + drain + crate::boot_order::STAGE_SLACK
}
```

`STAGE_SLACK` becomes `pub(crate)` in `boot_order.rs` for this. Pass
`false` from the restart handler and `true` from the reload one.

- [ ] **Step 4: Run the inner loop**

Run: `cargo test -p shep-daemon --lib --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/shep-daemon/
git commit -m "feat(daemon): walk stages forward on a multi-sheep restart and reload"
```

---

### Task 12: `shep describe` shows the dependencies

**Files:**
- Modify: `crates/shep-core/src/protocol/request.rs` (`ProcessInfo`, its
  builder)
- Modify: `crates/shep-daemon/src/supervisor.rs` (`to_info`)
- Modify: `crates/shep-cli/src/output/mod.rs:392` (`emit_described`)

**Interfaces:**
- Produces: `ProcessInfo::depends_on: Vec<String>`

Additive, so `SCHEMA_VERSION` stays 1: that envelope moves only on a rename, a
removal, or a retype. `PROTOCOL_VERSION` already moved in Task 2 and does not
move again.

No new `shep flock` column. That table already drops columns under pressure
and this is not per-row status.

- [ ] **Step 1: Write the failing test**

Add to `crates/shep-cli/src/output/mod.rs`'s `mod tests`:

```rust
#[test]
fn describe_lists_a_sheep_s_dependencies() {
    // fails if depends_on never reaches the operator, which leaves "why did
    // web start nine seconds in" unanswerable
    let info = ProcessInfo::builder(1, "web", ProcStatus::Online)
        .depends_on(vec!["api".to_string(), "db".to_string()])
        .build();
    let mut out = Vec::new();
    emit_described(&mut out, Format::Table, "describe", vec![info], Presentation::plain()).unwrap();
    let rendered = String::from_utf8(out).unwrap();
    assert!(rendered.contains("api"), "{rendered}");
    assert!(rendered.contains("db"), "{rendered}");
}

#[test]
fn describe_says_nothing_about_dependencies_when_there_are_none() {
    // fails if an empty list prints a bare header, which every sheep in a
    // flock without ordering would then carry
    let info = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
    let mut out = Vec::new();
    emit_described(&mut out, Format::Table, "describe", vec![info], Presentation::plain()).unwrap();
    let rendered = String::from_utf8(out).unwrap();
    assert!(!rendered.to_lowercase().contains("depends"), "{rendered}");
}
```

The renderer is `shep::output::emit_described`
(`crates/shep-cli/src/output/mod.rs:392`), which is where the new row goes,
under the `Format::Table` arm beside the lamb trees. `Presentation::plain()`
stands for whichever constructor the file's neighbouring tests use for an
unstyled presentation.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p shep --lib --all-features describe_lists describe_says_nothing`
Expected: FAIL, `no method named depends_on`.

- [ ] **Step 3: Add the field, the builder setter, and the rendering**

In `ProcessInfo`, after `fold`:

```rust
    /// Names this sheep waits for at a staged start, from its
    /// `depends_on`. Empty both when the sheep declares none and when the
    /// peer daemon predates the field.
    #[serde(default)]
    pub depends_on: Vec<String>,
```

Add the matching `ProcessInfoBuilder::depends_on` setter, in the style of its
neighbours. In `supervisor.rs`'s `to_info`, populate it from
`entry.spec.config().depends_on`.

In the describe renderer, emit the row only when the list is non-empty.

- [ ] **Step 4: Run the workspace**

Run: `cargo test --workspace --all-features -- --skip ::slow::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/
git commit -m "feat(core): carry depends_on on ProcessInfo for shep describe"
```

---

### Task 13: The docs site

**Files:**
- Create: `web/src/pages/docs/boot-order.astro`
- Modify: `web/src/pages/docs/lifecycle.astro`,
  `web/src/pages/docs/first-flockfile.astro`,
  `web/src/pages/docs/dogs.astro`, and the nav component that lists the docs
  pages
- Modify: whatever `./web/scripts/generate-cli-reference.sh` regenerates

**Interfaces:**
- Consumes: everything above

A change to what an operator can type, see, or configure is not finished until
`web/` says so. Read a neighbouring page first and match its components and
prose register; `folds.astro` is the closest in shape.

- [ ] **Step 1: Write the page**

`boot-order.astro` covers, in this order: the `depends_on` field with the
`db`/`api`/`web` example and its derived stages; what ready means and the
`listen_timeout` fallback with the six-seconds-for-three-stages cost stated
plainly; the `readiness_probe` escape; `boot_first_dogs` with the log-rotate
case and the metrics counter-case; the failure table from the spec; reverse
shutdown; and the limits, that ordering applies to a batch rather than to
`shep start api`, and that promoting a dog fixes the boot window only.

Use `<Callout variant="...">`, never `kind=`. A wrong prop builds clean and
renders wrong, which is what `astro check` catches and `astro build` does not.

- [ ] **Step 2: Edit the neighbours**

`first-flockfile.astro` gains `depends_on` in its field walkthrough.
`lifecycle.astro` gains a paragraph on staged boot and staged shutdown with a
link to the new page. `dogs.astro` gains `boot_first_dogs`. Add the page to
the nav.

- [ ] **Step 3: Regenerate the CLI reference**

```bash
cargo build --release
```
```bash
./web/scripts/generate-cli-reference.sh
```

`git diff` afterwards is the check.

- [ ] **Step 4: Build and check the site**

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

Both. `check` is the one that catches a wrong prop.

- [ ] **Step 5: Commit**

```bash
git add web/
git commit -m "docs: add the boot-order page and regenerate the CLI reference"
```

---

### Task 14: The repository's own docs

**Files:**
- Modify: `docs/dogs.md`, `docs/decisions.md`, `CLAUDE.md`

- [ ] **Step 1: `docs/dogs.md`**

Add `boot_first_dogs` to the operator contract: what it does, why a dog's own
`dogs.toml` section is the wrong place for it, and that a name absent from
`enabled_dogs` is inert.

- [ ] **Step 2: `docs/decisions.md`**

Four entries, each in the file's existing "decision, then **Why:**" shape:

- `PROTOCOL_VERSION` moved to 5 for a new `AppConfig` field, because
  `deny_unknown_fields` puts that struct outside the additive rule.
- A depended-on app is gated on `ReadinessSource::Heuristic`, rather than a
  new `boot_delay` field or treating `Online` as ready.
- `autostart = false` wins over `depends_on`.
- Dogs stay last by default with two promotions, and are held out of the
  reverse shutdown.

- [ ] **Step 3: `CLAUDE.md`**

It quotes `boot.rs`'s order comment verbatim and that comment changed in Task
8. Update the quote, and add a line to the status section covering staged boot
and staged shutdown. While there, correct "Six crates plus `examples`": the
workspace has seven now that `shep-channel` is its own crate.

- [ ] **Step 4: Run the two prose skills over everything written here**

Every word in this task is prose a person reads, so the maintainer's global
rules apply: the de-slopping skill first, then the voice skill, before
committing. Task 13's page is prose too and takes the same pass.

- [ ] **Step 5: Commit**

```bash
git add docs/ CLAUDE.md
git commit -m "docs: record the boot-ordering decisions"
```

---

### Task 15: The full gate

- [ ] **Step 1: Run the four gate commands**

One at a time, each with `$?` captured directly, never through a pipe.

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

- [ ] **Step 2: Run the two cross-checks**

```bash
cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu
```
```bash
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

The Windows one needs `brew install mingw-w64` for `ring`'s build script.

- [ ] **Step 3: Run the doctests once**

```bash
cargo test -p shep-core --doc
```

- [ ] **Step 4: Fix anything red, then commit**

A fix gets its own conventional commit rather than being folded into an
earlier one.

---

## Notes for whoever executes this

- **Every `stands in for` marker names a helper to find, not to invent.** Read
  the neighbouring tests in that file and reuse what is there. A second
  harness beside an existing one is a review rejection.
- **Nothing here belongs in a `mod slow`.** Every ordering assertion runs
  against the fake runner and asserts event order, not elapsed time. Fixtures
  set `listen_timeout` to 50ms so the fast tier stays fast. If a test you write
  asserts a duration, a batch, or a count that a contended runner cannot hold
  still, it belongs in `mod slow` and the CI workflow's skip list needs it too.
- **The `!` commit is Task 2 and only Task 2.** Everything else is `feat` or
  `docs`.
- **`shep kill`'s client deadline** is checked in Task 15's full gate by way of
  the e2e suite. If a staged shutdown makes it time out, size it the way Task
  10 sizes the start deadline and commit that as its own `fix(shep):`.
