//! The client<->daemon wire protocol (version 7).
//!
//! Typed request/response enums plus bus events. Framing lives in
//! [`wire`]; a serialized shape change bumps [`PROTOCOL_VERSION`].
//! Version 4 bumped on an addition. Version 5 bumped on a new `AppConfig`
//! field: that struct is `deny_unknown_fields`, so the additive rule below
//! does not cover it and an older daemon cannot decode `depends_on`.
//! Version 6 bumped on a retype: [`Response::Reloading`] became a struct
//! variant to carry the apps a staged reload refused, so it serializes as
//! an object where an older peer reads an array. Version 7 bumped on the
//! same retype applied to [`Response::Restarted`], which a staged restart
//! refuses apps of for the same reason and had nowhere to name them.
//!
//! A `*_wire_v7` test pins today's shape. A
//! `v1_*_fixture_still_deserializes` test pins an old peer's payload and
//! never renames.

pub mod events;
pub mod frame;
pub mod request;
/// Frame encoding shared by daemon and client
pub mod wire;

pub use events::{BusEvent, ProcessEventKind};
pub use frame::ServerFrame;
pub use request::{
    ActionOutcome, ActionReply, DogSectionToml, DogSource, EnvValue, Envelope, ExitInfo, Hello,
    HelloAck, HelloReply, Lamb, LineOutcome, LineReply, ProcessInfo, ProcessInfoBuilder, Reply,
    Request, Response, RpcError, RpcErrorCode, SelectorSpec, SheepApplied, SheepConfigView,
    SheepDrift, SheepRefusal, SignalOutcome, SignalReply, Smit, SmitError, sort_flock,
};
pub use shep_channel::{CHANNEL_VERSION, ChildMessage, ShepherdMessage};
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
/// CHANGELOG. Byte fixtures in each protocol module pin the deserialize
/// direction.
pub const PROTOCOL_VERSION: u32 = 7;

/// The oldest protocol this build accepts from a peer.
///
/// The handshake compares against this rather than [`PROTOCOL_VERSION`],
/// so a change that only adds does not refuse anyone. This rises only
/// when a message shape changes such that an older peer cannot read it,
/// and raising it refuses every peer built below it, which is why the
/// rules in the spec exist to make that rare.
pub const MIN_SUPPORTED: u32 = 7;

#[cfg(test)]
mod tests {
    use super::{MIN_SUPPORTED, PROTOCOL_VERSION};

    #[test]
    fn a_retyped_restarted_forced_the_protocol_version_up() {
        // fails if `Response::Restarted` becomes a struct variant without
        // the bump, the same way `Response::Reloading` forced 6. The
        // variant serializes as an object now where it used to serialize
        // as an array, so an older peer decodes neither, and the handshake
        // is the only place that can say so.
        assert_eq!(PROTOCOL_VERSION, 7);
    }

    #[test]
    fn the_floor_never_outruns_the_ceiling() {
        // A floor above the current version would refuse every peer,
        // including one built from this exact commit. Both sides are
        // `const`, so clippy wants the check itself const-evaluated.
        const { assert!(MIN_SUPPORTED <= PROTOCOL_VERSION) };
    }
}
