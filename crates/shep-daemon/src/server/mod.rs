//! The connection layer: peer auth, handshake, subscriptions
//!
//! [`RpcServer`] owns the bound [`Listener`](shep_core::transport::Listener) and accepts connections until
//! told to stop. Each runs `handle_conn` in its own task: a same-uid check
//! ([`check_peer`](peer_auth::check_peer), unix only), a version handshake, then a read loop that
//! decodes envelopes and hands them to
//! [`rpc::dispatch`](crate::rpc::dispatch), which never sees a socket.
//!
//! The OS transport lives in [`shep_core::transport`], so everything here is
//! one implementation over a unix socket and a Windows named pipe alike;
//! [`check_peer`](peer_auth::check_peer) is the only genuine platform difference left.
//! [`RpcServer`]'s doc is the daemon's security writeup.

mod conn_protocol;
mod peer_auth;
mod server_lifecycle;
#[cfg(test)]
mod testing;
pub use peer_auth::daemon_uid;
pub use server_lifecycle::RpcServer;
