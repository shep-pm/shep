//! The client<->daemon wire protocol (version 12).
//!
//! Typed request/response enums plus bus events. Framing lives in
//! [`wire`]; a serialized shape change bumps [`PROTOCOL_VERSION`].
//!
//! An additive change keeps the version: an older peer decodes
//! [`Request::Unrecognized`] and refuses by name. `AppConfig` always bumps.
//!
//! A `*_wire_v12` test pins today's shape. A
//! `v1_*_fixture_still_deserializes` test pins an old peer's payload.

pub mod events;
pub mod frame;
pub mod request;
/// Frame encoding shared by daemon and client
pub mod wire;

pub use events::{BusEvent, ProcessEventKind};
pub use frame::ServerFrame;
pub use request::{
    ActionOutcome, ActionReply, DogAccess, DogSectionToml, DogSource, EnvCopyChange, EnvCopyKind,
    EnvValue, Envelope, ExitInfo, Hello, HelloAck, HelloReply, HostUsage, Lamb, LineOutcome,
    LineReply, LogStream, LogTail, ProcessInfo, ProcessInfoBuilder, Reply, Request, Response,
    RpcError, RpcErrorCode, SelectorSpec, SheepApplied, SheepConfigView, SheepDrift, SheepRefusal,
    SignalOutcome, SignalReply, Smit, SmitError, Vcs, sort_flock,
};
pub use shep_channel::{
    CHANNEL_VERSION, ChildMessage, LambLabel, LambLabelError, MetricType, ShepherdMessage,
};
pub use wire::{MAX_FRAME_BYTES, WireError, codec, decode_frame, encode_frame, reply_id};

/// The shepherd channel's wire types. Moved to the `shep-channel` crate;
/// this path is kept so consumers of 0.1.x do not break. Use
/// `shep_core::protocol` directly instead.
#[deprecated(note = "use `shep_core::protocol` directly")]
pub mod channel {
    pub use shep_channel::{CHANNEL_VERSION, ChildMessage, ShepherdMessage};
}

/// Wire protocol version.
///
/// Additive optional fields (new serde-defaulted `Option<T>` fields, new
/// variants behind `#[non_exhaustive]`) keep the version. Removing,
/// renaming, or retyping anything serialized bumps it, recorded in the
/// CHANGELOG. `AppConfig` is the exception: an older peer ignores a key
/// it does not know, so it runs a config the operator did not write.
pub const PROTOCOL_VERSION: u32 = 12;

/// The oldest protocol this build accepts from a peer.
///
/// The handshake compares against this rather than [`PROTOCOL_VERSION`],
/// so a change that only adds does not refuse anyone. This rises only
/// when a message shape changes such that an older peer cannot read it,
/// and raising it refuses every peer built below it, which is why the
/// rules in the spec exist to make that rare.
pub const MIN_SUPPORTED: u32 = 8;

#[cfg(test)]
mod tests {
    use super::{MIN_SUPPORTED, PROTOCOL_VERSION};

    // Fails whenever `PROTOCOL_VERSION` moves, so a bump is a deliberate
    // edit. A shape change that forgot to bump shows up in the
    // `*_wire_v12` snapshots instead. A bump moves the constant, this
    // literal, this test's name, the doc header and the snapshots.
    #[test]
    fn an_added_app_config_field_forced_the_protocol_version_up() {
        assert_eq!(PROTOCOL_VERSION, 12);
    }

    #[test]
    fn the_floor_never_outruns_the_ceiling() {
        // A floor above the current version would refuse every peer,
        // including one built from this exact commit. Both sides are
        // `const`, so clippy wants the check itself const-evaluated.
        const { assert!(MIN_SUPPORTED <= PROTOCOL_VERSION) };
    }
}
