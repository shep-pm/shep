//! The client<->daemon wire protocol (version 6).
//!
//! Typed request/response enums plus bus events. Framing lives in
//! [`wire`]; a serialized shape change bumps [`PROTOCOL_VERSION`].
//! Version 4 bumped on an addition. Version 5 bumped on a new `AppConfig`
//! field: that struct is `deny_unknown_fields`, so the additive rule below
//! does not cover it and an older daemon cannot decode `depends_on`.
//! Version 6 bumped on a retype: [`Response::Reloading`] became a struct
//! variant to carry the apps a staged reload refused, so it serializes as
//! an object where an older peer reads an array.
//!
//! A `*_wire_v6` test pins today's shape. A
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
pub use wire::{MAX_FRAME_BYTES, WireError, codec, decode_frame, encode_frame};

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
pub const PROTOCOL_VERSION: u32 = 6;

#[cfg(test)]
mod tests {
    use super::PROTOCOL_VERSION;

    #[test]
    fn a_retyped_reloading_forced_the_protocol_version_up() {
        // fails if `Response::Reloading` becomes a struct variant without
        // the bump. The variant serializes as an object now where it used
        // to serialize as an array, so an older peer decodes neither, and
        // the handshake is the only place that can say so.
        assert_eq!(PROTOCOL_VERSION, 6);
    }
}
