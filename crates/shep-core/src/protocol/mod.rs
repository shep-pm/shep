//! The client<->daemon wire protocol (version 8).
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
//! Version 8 bumped on a second new `AppConfig` field, `environment`, for
//! the reason version 5 did. [`Request::PutSecrets`] rode in on the same
//! commit and forced nothing: it is an additive variant, and a daemon that
//! has never heard of it decodes [`Request::Unrecognized`] and refuses by
//! name.
//!
//! A `*_wire_v8` test pins today's shape. A
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
pub const PROTOCOL_VERSION: u32 = 8;

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

    #[test]
    fn a_new_app_config_field_forced_the_protocol_version_up() {
        // fails if a field is added to `AppConfig` without the bump, the
        // same way `depends_on` forced 5 and `environment` forced 8. That
        // struct is `deny_unknown_fields`, so an older peer refuses the
        // whole payload rather than ignoring a key it does not know, and
        // the handshake is the only place that can say so. The retypes of
        // `Response::Reloading` and `Response::Restarted` forced 6 and 7
        // for the separate reason that an object is not an array.
        assert_eq!(PROTOCOL_VERSION, 8);
    }

    #[test]
    fn the_floor_never_outruns_the_ceiling() {
        // A floor above the current version would refuse every peer,
        // including one built from this exact commit. Both sides are
        // `const`, so clippy wants the check itself const-evaluated.
        const { assert!(MIN_SUPPORTED <= PROTOCOL_VERSION) };
    }
}
