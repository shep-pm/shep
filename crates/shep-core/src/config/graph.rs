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
        let out = plan(&[
            sheep("web", &["api"]),
            sheep("api", &["db"]),
            sheep("db", &[]),
        ]);
        assert_eq!(out.stages, vec![vec!["db"], vec!["api"], vec!["web"]]);
    }

    #[test]
    fn independent_nodes_share_a_stage_sorted_by_name() {
        // fails if a stage's order depends on input order, which would make
        // the boot plan nondeterministic
        let out = plan(&[
            sheep("cache", &[]),
            sheep("db", &[]),
            sheep("api", &["db", "cache"]),
        ]);
        assert_eq!(out.stages, vec![vec!["cache", "db"], vec!["api"]]);
    }

    #[test]
    fn a_cycle_is_named_as_a_path_and_its_nodes_run_last() {
        // fails if the cycle is merely detected, or if a cycle sinks the
        // whole plan rather than being isolated into a final stage
        let out = plan(&[
            sheep("a", &["c"]),
            sheep("b", &["a"]),
            sheep("c", &["b"]),
            sheep("lone", &[]),
        ]);
        assert_eq!(out.cycles.len(), 1, "one cycle expected: {:?}", out.cycles);
        let rendered = render_cycle(&out.cycles[0]);
        assert!(
            rendered.starts_with("a -> ")
                || rendered.starts_with("b -> ")
                || rendered.starts_with("c -> ")
        );
        assert_eq!(
            rendered.matches(" -> ").count(),
            3,
            "the path must close: {rendered}"
        );
        assert_eq!(out.stages.last().unwrap(), &vec!["a", "b", "c"]);
    }

    #[test]
    fn an_edge_to_a_name_nobody_has_is_recorded_and_dropped() {
        // fails if an unknown name refuses the plan or silently vanishes
        let out = plan(&[sheep("api", &["nope"])]);
        assert_eq!(out.stages, vec![vec!["api"]]);
        assert_eq!(
            out.unresolved,
            vec![Unresolved {
                dependent: "api".to_string(),
                missing: "nope".to_string()
            }]
        );
    }

    #[test]
    fn a_dog_nobody_depends_on_runs_last() {
        // fails if dogs join the ordinary sort, which would move every
        // existing install's boot order
        let out = plan(&[
            dog("metrics", false),
            sheep("db", &[]),
            sheep("api", &["db"]),
        ]);
        assert_eq!(out.stages, vec![vec!["db"], vec!["api"], vec!["metrics"]]);
    }

    #[test]
    fn a_boot_first_dog_runs_before_every_sheep() {
        // fails if boot_first is ignored, which is the log-rotate case
        let out = plan(&[
            dog("log-rotate", true),
            sheep("db", &[]),
            dog("metrics", false),
        ]);
        assert_eq!(
            out.stages,
            vec![vec!["log-rotate"], vec!["db"], vec!["metrics"]]
        );
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
        assert_eq!(
            out.stages,
            vec![vec!["db", "sidecar"], vec!["api"], vec!["metrics"]]
        );
    }

    #[test]
    fn an_empty_flock_plans_no_stages() {
        // fails if the sort emits an empty stage, which the driver would
        // then wait on
        assert!(plan(&[]).stages.is_empty());
    }

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
}
