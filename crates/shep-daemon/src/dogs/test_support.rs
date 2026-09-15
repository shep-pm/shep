//! Test fixtures shared by more than one dogs submodule's own tests.
//!
//! [`start_test_dog`] is used by both `silent`'s ladder tests and `narrate`'s
//! log test, so it lives here rather than in either.

use core::time::Duration;

use shep_core::protocol::DogSource;

use super::{DogSpec, dog_app};

/// How long [`start_test_dog`] waits on the supervisor before calling it a
/// hang rather than a slow start.
///
/// Generous on purpose: a deadlock guard, not a timing assertion.
const DOG_FIXTURE_START_BUDGET: Duration = Duration::from_secs(10);

pub(super) async fn start_test_dog(ctx: &crate::rpc::RpcContext, name: &str) {
    let spec = DogSpec {
        name: name.to_string(),
        source: DogSource::BuiltIn,
    };
    let app = dog_app(&spec, &ctx.paths).expect("the dog fixture must assemble");
    // Bounded, because the callers below run under a paused clock and this
    // await is the one thing in them not already forced. `start_paused`
    // auto-advances to the next deadline once every task is idle, so the
    // timeout fires rather than waiting on a wall clock.
    tokio::time::timeout(
        DOG_FIXTURE_START_BUDGET,
        ctx.supervisor.start_dog(app, DogSource::BuiltIn),
    )
    .await
    .expect("the dog fixture must start inside its budget")
    .expect("the dog fixture must start");
}
