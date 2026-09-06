//! A supervised app that answers on the shepherd channel.
//!
//! Run it under shep with `channel = true` in the Flockfile. Then
//! `shep trigger <name> gc` reaches the action handler below.

fn main() {
    let shepherd = shep_channel::serve();
    shepherd.on_action("gc", |params, _name| {
        format!("collected, params={params:?}")
    });
    shepherd.on_shutdown(|| std::process::exit(0));
    shepherd
        .ready()
        .expect("failed to send the readiness message");
    shepherd.metric("rps", 42.0);

    // The reader thread does the work, so main only has to stay alive.
    loop {
        std::thread::park();
    }
}
