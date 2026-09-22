//! A supervised app that answers on the shepherd channel.
//!
//! Run it under shep with `channel = true` in the Flockfile. Then
//! `shep trigger <name> gc` reaches the action handler below.

use std::time::Duration;

/// How long the shutdown handler waits for queued replies. Well inside
/// the default `kill_timeout`, which the shepherd is counting down while
/// this waits.
const DRAIN_BUDGET: Duration = Duration::from_secs(2);

fn main() {
    let shepherd = shep_channel::serve();
    shepherd.on_action("gc", |params, _name| {
        format!("collected, params={params:?}")
    });

    let exiting = shepherd.clone();
    shepherd.on_shutdown(move || {
        // Sending queues. Exiting without this drops whatever the writer
        // thread had not reached, which for an action the operator is
        // waiting on costs them the whole `action_timeout`.
        if let Err(error) = exiting.flush(DRAIN_BUDGET) {
            eprintln!("{} replies never reached shep: {error}", exiting.pending());
        }
        std::process::exit(0);
    });

    shepherd
        .ready()
        .expect("failed to send the readiness message");
    shepherd.metric("rps", 42.0);

    // The reader thread does the work, so main only has to stay alive.
    loop {
        std::thread::park();
    }
}
