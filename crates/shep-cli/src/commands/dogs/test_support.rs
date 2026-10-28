//! The fixture every dog verb's tests share.

use crate::cli::Format;
use crate::output::Streams;

/// A dog verb's streams under `--format table`, bare style; a test reading
/// a JSON field sets `fmt` itself.
pub(super) fn streams<'a>(out: &'a mut Vec<u8>, err: &'a mut Vec<u8>) -> Streams<'a> {
    Streams::bare(out, err, Format::Table)
}

/// A handshake answer from this build, which is the only one a dog verb writes
/// to: [`crate::version_guard`] refuses a shepherd of another one before a
/// request leaves. [`shep_client::testing::sample_ack`]'s `9.9.9` is such a
/// shepherd, so a test that wants its request to reach the wire cannot use it.
pub(super) fn this_builds_ack() -> shep_core::protocol::HelloAck {
    shep_core::protocol::HelloAck {
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        ..shep_client::testing::sample_ack()
    }
}
