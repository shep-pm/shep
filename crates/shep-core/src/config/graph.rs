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
    /// One cycle for every knot in the graph, as a path with the first name
    /// not repeated at the end. [`render_cycle`] closes it for display.
    /// Several cycles can run through the same knot; the one named here is
    /// a representative, and breaking it is what an operator does about it.
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
/// Nodes in a cycle are lifted out of the sort into one final unordered
/// stage, and one cycle through each knot they form is recorded in
/// [`BootPlan::cycles`]. Refusing here would strand an unattended boot; the
/// caller decides whether to refuse, and only the operator-facing callers
/// do.
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

    let knots = knots(&edges);
    let in_a_cycle: BTreeSet<&str> = knots.iter().flatten().copied().collect();
    let cycles: Vec<Vec<String>> = knots
        .iter()
        .map(|members| representative_cycle(&edges, members))
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
    if !in_a_cycle.is_empty() {
        // One stage for every cyclic node, never one per reported cycle: a
        // node several cycles run through is still started once.
        stages.push(in_a_cycle.iter().map(|n| (*n).to_string()).collect());
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

/// Every knot in `edges`: a strongly connected component of two or more
/// nodes, or a lone node that depends on itself. Each component's members,
/// sorted, and the components themselves ordered by their first member.
///
/// Tarjan's algorithm rather than a search for a back edge. A back-edge walk
/// answers "does this one walk close on itself", and marking a node explored
/// the first time it is reached is what makes that walk finite: a second path
/// arriving at the same node turns back without retraversing it, so of two
/// cycles sharing a node only one is ever seen. The question this module asks
/// is which nodes sit in a component larger than themselves, and only a
/// component algorithm answers it.
///
/// Recursive, since the depth is bounded by the size of one flock.
fn knots<'a>(edges: &BTreeMap<&'a str, BTreeSet<&'a str>>) -> Vec<BTreeSet<&'a str>> {
    let mut tarjan = Tarjan {
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        next: 0,
        components: Vec::new(),
    };
    for name in edges.keys().copied() {
        if !tarjan.index.contains_key(name) {
            tarjan.connect(name, edges);
        }
    }
    let mut found: Vec<BTreeSet<&str>> = tarjan
        .components
        .into_iter()
        .filter(|members| {
            members.len() > 1
                || members
                    .iter()
                    .next()
                    .is_some_and(|only| edges.get(only).is_some_and(|deps| deps.contains(only)))
        })
        .collect();
    found.sort();
    found
}

/// Tarjan's bookkeeping: the visit number each node was reached at, the
/// lowest number it can reach, and the nodes whose component is still open.
struct Tarjan<'a> {
    index: BTreeMap<&'a str, usize>,
    low: BTreeMap<&'a str, usize>,
    stack: Vec<&'a str>,
    on_stack: BTreeSet<&'a str>,
    next: usize,
    components: Vec<BTreeSet<&'a str>>,
}

impl<'a> Tarjan<'a> {
    fn connect(&mut self, name: &'a str, edges: &BTreeMap<&'a str, BTreeSet<&'a str>>) {
        self.index.insert(name, self.next);
        self.low.insert(name, self.next);
        self.next += 1;
        self.stack.push(name);
        self.on_stack.insert(name);

        if let Some(deps) = edges.get(name) {
            for dep in deps.iter().copied() {
                let reachable = if self.index.contains_key(dep) {
                    // An edge into a closed component says nothing about
                    // this one, so only a node still on the stack counts.
                    self.on_stack.contains(dep).then(|| self.index[dep])
                } else {
                    self.connect(dep, edges);
                    Some(self.low[dep])
                };
                if let Some(reachable) = reachable {
                    let low = self.low.entry(name).or_insert(reachable);
                    *low = (*low).min(reachable);
                }
            }
        }

        if self.low[name] == self.index[name] {
            let mut members = BTreeSet::new();
            while let Some(member) = self.stack.pop() {
                self.on_stack.remove(member);
                members.insert(member);
                if member == name {
                    break;
                }
            }
            self.components.push(members);
        }
    }
}

/// One cycle through `members`, as the path a walk closed on.
///
/// [`BootPlan::cycles`] names a path rather than a set because
/// [`render_cycle`] prints `a -> b -> c -> a`, and a bare set of the names
/// involved is not something an operator can act on.
fn representative_cycle<'a>(
    edges: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    members: &BTreeSet<&'a str>,
) -> Vec<String> {
    let Some(start) = members.iter().copied().next() else {
        return Vec::new();
    };
    let mut path = vec![start];
    let mut seen: BTreeSet<&str> = BTreeSet::from([start]);
    if !close_on(start, start, edges, members, &mut path, &mut seen) {
        // Every member of a knot has a path back to every other, so the
        // walk closes. Naming the node alone is the honest fallback.
        return vec![start.to_string()];
    }
    path.iter().map(|n| (*n).to_string()).collect()
}

/// Walks from `at`, inside `members` only, until it finds an edge back to
/// `start`. `path` holds the walk so far and is the cycle when this answers
/// `true`.
fn close_on<'a>(
    at: &'a str,
    start: &'a str,
    edges: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    members: &BTreeSet<&'a str>,
    path: &mut Vec<&'a str>,
    seen: &mut BTreeSet<&'a str>,
) -> bool {
    let Some(deps) = edges.get(at) else {
        return false;
    };
    for dep in deps.iter().copied().filter(|dep| members.contains(dep)) {
        if dep == start {
            return true;
        }
        if seen.insert(dep) {
            path.push(dep);
            if close_on(dep, start, edges, members, path, seen) {
                return true;
            }
            path.pop();
        }
    }
    false
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

    #[test]
    fn two_cycles_sharing_a_node_put_every_member_in_the_last_stage() {
        // fails if the cycle search marks a node done the first time it is
        // reached, which leaves the second path through it unwalked: "c" is
        // as cyclic as "b" and used to plan into the first stage, ahead of
        // the "d" it depends on
        let out = plan(&[
            sheep("a", &["b", "c"]),
            sheep("b", &["d"]),
            sheep("c", &["d"]),
            sheep("d", &["a"]),
        ]);
        assert_eq!(
            out.cycles.len(),
            1,
            "one component expected: {:?}",
            out.cycles
        );
        assert_eq!(out.stages, vec![vec!["a", "b", "c", "d"]]);
    }

    #[test]
    fn a_node_two_cycles_run_through_is_planned_into_one_stage() {
        // fails if the final stage is built once per reported cycle rather
        // than once from the set of cyclic nodes, which starts the shared
        // node twice
        let out = plan(&[
            sheep("a", &["b", "c"]),
            sheep("b", &["a"]),
            sheep("c", &["a"]),
        ]);
        assert_eq!(
            out.cycles.len(),
            1,
            "one component expected: {:?}",
            out.cycles
        );
        assert_eq!(out.stages, vec![vec!["a", "b", "c"]]);
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

        #[test]
        fn every_cyclic_node_is_reported_and_no_node_is_planned_twice(
            edges in proptest::collection::vec((0usize..6, 0usize..6), 0..18)
        ) {
            // Arbitrary edges, so most draws are cyclic: the DAG-by-
            // construction case above gives the cyclic path no coverage at
            // all. Ground truth is a transitive closure computed here, not
            // anything the module does, so the test cannot agree with a bug
            // by sharing its logic.
            const N: usize = 6;
            let mut adjacent = [[false; N]; N];
            for (from, to) in edges {
                adjacent[from][to] = true;
            }
            let mut reaches = adjacent;
            for k in 0..N {
                for i in 0..N {
                    for j in 0..N {
                        if reaches[i][k] && reaches[k][j] {
                            reaches[i][j] = true;
                        }
                    }
                }
            }
            let nodes: Vec<BootNode> = (0..N)
                .map(|i| BootNode {
                    name: format!("n{i}"),
                    depends_on: (0..N)
                        .filter(|j| adjacent[i][*j])
                        .map(|j| format!("n{j}"))
                        .collect(),
                    kind: NodeKind::Sheep,
                })
                .collect();
            let out = plan(&nodes);

            let planned: Vec<String> = out.stages.iter().flatten().cloned().collect();
            let mut once = planned.clone();
            once.sort();
            once.dedup();
            proptest::prop_assert_eq!(
                once.len(),
                planned.len(),
                "a node is planned twice: {:?}",
                out.stages
            );
            proptest::prop_assert_eq!(once.len(), N, "a node is planned nowhere: {:?}", out.stages);

            // A node is truly cyclic when it reaches itself, and every one
            // of them belongs in the final stage.
            let last = out.stages.len() - 1;
            for (i, reached) in reaches.iter().enumerate() {
                if reached[i] {
                    proptest::prop_assert!(
                        out.stages[last].contains(&format!("n{i}")),
                        "n{} is cyclic and is planned elsewhere: {:?}",
                        i,
                        out.stages
                    );
                }
            }

            // One reported path per cyclic component, and every reported
            // path is a real cycle rather than a set of involved names.
            let mut components: Vec<Vec<usize>> = Vec::new();
            for (i, reached) in reaches.iter().enumerate() {
                if reached[i] && !components.iter().any(|c| c.contains(&i)) {
                    components.push((0..N).filter(|j| reached[*j] && reaches[*j][i]).collect());
                }
            }
            proptest::prop_assert_eq!(
                out.cycles.len(),
                components.len(),
                "reported {:?} for components {:?}",
                out.cycles,
                components
            );
            for cycle in &out.cycles {
                for (at, name) in cycle.iter().enumerate() {
                    let from: usize = name[1..].parse().unwrap();
                    let to: usize = cycle[(at + 1) % cycle.len()][1..].parse().unwrap();
                    proptest::prop_assert!(
                        adjacent[from][to],
                        "{} is not a path anything can walk",
                        render_cycle(cycle)
                    );
                }
            }
        }
    }
}
