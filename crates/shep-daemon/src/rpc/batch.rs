//! Validation and dependency ordering for a multi-app batch before it
//! reaches the supervisor.
//!
//! [`staged_plan`] turns a resolved batch into a [`BootPlan`] or the
//! cycle that refuses it; [`duplicate_name`] catches a batch naming one
//! app twice. The `From` impl is the wire shape a `Start`/`Add`/
//! `ApplyConfig` reply carries.

use std::collections::BTreeSet;
use std::path::Path;

use shep_core::config::graph::BootPlan;
use shep_core::config::{DeclaredApp, ResolvedApp};
use shep_core::protocol::{RpcError, RpcErrorCode, SheepApplied};

use crate::supervisor::Applied;

use super::context::RpcContext;

/// Whether `namespace`'s push may reach the cache file: the `persist` key
/// of its own `[<namespace>]` table in `dogs.toml`, or `true`.
///
/// Read per push rather than at boot, so an operator who edits the file
/// does not have to restart the shepherd for it to take.
///
/// Every way of not finding a boolean answers `true`: no file, no section,
/// no key, a key of some other type, or a file that will not parse. The
/// cache is what makes a pushed value survive a restart, and a dog whose
/// config says nothing about it wants one. An operator who does not says
/// so explicitly.
pub(super) fn persists(dogs_config: &Path, namespace: &str) -> bool {
    let Ok(source) = std::fs::read_to_string(dogs_config) else {
        return true;
    };
    let Ok(config) = shep_core::config::DogsConfig::load(Some(&source)) else {
        return true;
    };
    config
        .dog
        .get(namespace)
        .and_then(|table| table.get("persist"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
}

/// The stages `apps` starts in, or the cycle that refuses the whole batch.
///
/// The graph spans the batch AND the registered flock: a cycle can close
/// through a sheep this request does not carry, so a Flockfile naming an
/// `api` that waits for `db` is a cycle against a flock whose `db` already
/// waits for `api`, with neither document showing one on its own.
///
/// The stages it answers with cover the batch alone. Everything else in the
/// graph is registered already and is not this request's to start; it is
/// there to be ordered around and to close a cycle.
///
/// Refused rather than warned about, which is the opposite of what a boot
/// does with the same graph in `snapshot::muster`. A boot has nobody at the
/// keyboard and must not strand a machine over a typo; an operator typed
/// this and is there to fix it.
///
/// # Errors
///
/// - [`RpcErrorCode::InvalidConfig`]: the graph holds a cycle ONE OF THIS
///   BATCH'S APPS IS IN, named as the path to break. A knot standing
///   elsewhere in the registry is left to whichever request drew it. The
///   same code `normalize_all`'s own refusal carries a few lines up, so it
///   reaches the operator as `ExitCode::InvalidConfig`.
pub(super) fn staged_plan(ctx: &RpcContext, apps: &[ResolvedApp]) -> Result<BootPlan, RpcError> {
    let mut edges = ctx.registry.depends_on_by_name();
    // A dog is a node with no edges of its own, so an edge naming one
    // resolves instead of reading as a typo. `or_default` rather than
    // `insert`, for `nodes_for_with_dogs`' reason: a sheep already holding
    // that name is the node, and a second one would be started twice.
    // Where the dogs sort is immaterial here, since they are running before
    // any request arrives and the stages are filtered to the batch anyway.
    for dog in &ctx.dog_names {
        edges.entry(dog.clone()).or_default();
    }
    // The batch's own edges win over whatever the registry holds for the same
    // name: this document is the newer statement about it.
    for app in apps {
        edges.insert(app.config().name.clone(), app.config().depends_on.clone());
    }
    let plan = crate::boot_order::plan_for_names(&edges);
    let batch: BTreeSet<&str> = apps.iter().map(|app| app.config().name.as_str()).collect();
    // Only a cycle this batch is in, for the reason the `unresolved` loop
    // below gives: the graph spans the whole registry, so taking the first
    // cycle refuses a request over a knot no app it names is part of. Two
    // Flockfile loads, neither drawing a cycle, can leave one standing
    // elsewhere in the flock, and `shep start` would then refuse every app
    // in the fold with an error naming apps the operator never mentioned.
    // A cycle that closes THROUGH the batch still has a batch member in it
    // and is still refused.
    // Membership, not the path: `cycles` holds one representative path per
    // knot, so a knot of three reached by two edges names two of them and a
    // batch holding only the third would pass a test against the path.
    // `knots` is index-aligned with `cycles`, so the path is still what the
    // message renders.
    let cycle = plan
        .knots
        .iter()
        .position(|knot| knot.iter().any(|name| batch.contains(name.as_str())))
        .and_then(|knot| plan.cycles.get(knot));
    if let Some(cycle) = cycle {
        return Err(RpcError {
            code: RpcErrorCode::InvalidConfig,
            message: format!(
                "dependency cycle: {}",
                shep_core::config::graph::render_cycle(cycle)
            ),
            daemon_version: None,
        });
    }
    // Warned, not refused: a dependency on an app whose Flockfile lives in
    // another repository is legitimate, and the boot path takes the same view
    // in `snapshot::warn_about_the_graph`. Only edges this batch drew, so a
    // request is never blamed for the rest of the flock's.
    for unresolved in &plan.unresolved {
        if batch.contains(unresolved.dependent.as_str()) {
            tracing::warn!(
                sheep = %unresolved.dependent,
                missing = %unresolved.missing,
                "a dependency names nothing this flock has; starting without it"
            );
        }
    }
    Ok(BootPlan {
        stages: plan
            .stages
            .iter()
            .map(|stage| {
                stage
                    .iter()
                    .filter(|name| batch.contains(name.as_str()))
                    .cloned()
                    .collect::<Vec<String>>()
            })
            .filter(|stage| !stage.is_empty())
            .collect(),
        unresolved: plan.unresolved,
        cycles: Vec::new(),
        knots: Vec::new(),
    })
}

/// The first name two entries of an `ApplyConfig` share, if any.
///
/// `handle_apply_config` reads the override store once for the whole request
/// and writes it once at the end, so a second entry of the same name merges
/// against the store as the first entry found it: the first entry's record is
/// overwritten and nothing says so.
///
/// Refused whole rather than per app, since a document naming one app twice
/// is malformed rather than partly wrong.
///
/// Linear in a `BTreeSet`, matching `normalize_all`: a request carries the
/// apps one Flockfile declared.
pub(super) fn duplicate_name(apps: &[DeclaredApp]) -> Option<String> {
    let mut seen = BTreeSet::new();
    apps.iter()
        .find(|app| !seen.insert(app.config.name.as_str()))
        .map(|app| app.config.name.clone())
}

/// The wire form of one app's load, with the merged config dropped.
///
/// `Applied` carries the whole merged [`ResolvedApp`] because `rpc` hands
/// it to the registry; [`SheepApplied`] does not. A client has no use for the
/// config, and `env` is in it, so the conversion is where the config stops.
impl From<Applied> for SheepApplied {
    fn from(applied: Applied) -> Self {
        Self::new(
            applied.name,
            applied.applied,
            applied.pending,
            applied.refused,
        )
    }
}

