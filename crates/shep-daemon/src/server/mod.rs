//! The connection layer: peer auth, handshake, subscriptions
//!
//! [`RpcServer`] owns the bound [`Listener`](shep_core::transport::Listener). It accepts
//! connections until told to stop, waiting out a backoff after a failing accept.
//! Each runs [`handle_conn`](crate::server::conn_protocol::handle_conn) in its own task:
//! a same-uid check ([`check_peer`](peer_auth::check_peer), unix only), a version
//! handshake, then a read loop handing envelopes to
//! [`rpc::dispatch`](crate::rpc::dispatch), which never sees a socket.
//! The OS transport lives in [`shep_core::transport`]: one implementation
//! serves a unix socket and a Windows named pipe alike.

mod accept_backoff;
mod conn_protocol;
mod dog_links;
#[cfg(unix)]
mod link_table;
#[cfg(all(test, unix))]
mod link_tests;
mod origin;
mod peer_auth;
#[cfg(all(test, unix))]
mod quiesce_tests;
#[cfg(all(test, unix))]
mod resume_grants_tests;
#[cfg(all(test, unix))]
mod resume_tests;
#[cfg(all(test, unix))]
mod revoked_subscription_tests;
mod server_lifecycle;
#[cfg(test)]
mod testing;
mod tracked_codec;
pub(crate) use dog_links::serve_dog_links;
#[cfg(unix)]
pub(crate) use link_table::DogLinks;
#[cfg(unix)]
pub use peer_auth::daemon_uid;
pub use server_lifecycle::RpcServer;
