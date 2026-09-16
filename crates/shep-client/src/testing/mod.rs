//! Test doubles for a scripted daemon peer, shared with shep-cli via the
//! `test-support` feature. The only module that defines a `fake_daemon`.
//!
//! Every helper takes the socket path as `&Path`; the caller owns the
//! `TempDir`. No dev-dependencies, so `missing_docs` and
//! `missing_debug_implementations` apply here like any other public module.
//!
//! [`fake_daemon`], [`sample_ack`] and [`sample_info`] are handshake-only
//! primitives; [`FakeDaemon`] and the `fake_client_*` helpers connect a
//! real [`Client`](crate::client::Client) against a scripted peer; [`fast_opts`],
//! [`start_fake_daemon_answering_on`] and [`child_exiting_with`] serve the
//! autostart tests.

mod autostart;
mod client;
mod handover;
mod handshake;
mod scripted;
mod shared;
pub use autostart::{child_exiting_with, fast_opts, start_fake_daemon_answering_on};
pub use client::{
    fake_client_answering, fake_client_capturing_envelopes, fake_client_event_then_reply,
    fake_client_on, fake_client_out_of_order, fake_client_replying_err,
    fake_client_that_closes_after_handshake, fake_client_that_dies_mid_request,
    fake_client_that_never_replies, fake_client_with_ack, fake_client_with_push,
    fake_daemon_answering_with_ack, fake_daemon_scripted_on, fake_reconnecting_client_on,
};
pub use handover::{Handovers, Handshake, fake_daemon_across_handovers};
pub use handshake::{
    fake_daemon, fake_daemon_accepting_repeatedly, fake_daemon_accepting_repeatedly_with_ack,
    fake_daemon_wedged_after_handshake, serve_one_request,
};
pub use scripted::FakeDaemon;
use scripted::{SCRIPT_CHANNEL_CAPACITY, ScriptCommand, serve_scripted};
use shared::{
    bind, handshake, read_envelope, send_sample_event, write_err, write_event, write_reply,
};
pub use shared::{control_address, sample_ack, sample_info};
