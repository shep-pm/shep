//! The client<->daemon wire protocol (version 9).
//!
//! Typed request/response enums plus bus events. Framing lives in
//! [`wire`]; a serialized shape change bumps [`PROTOCOL_VERSION`].
//! Version 4 bumped on an addition. Version 5 bumped on a new `AppConfig`
//! field: that struct was `deny_unknown_fields` then, so the additive rule
//! below did not cover it and an older daemon could not decode
//! `depends_on`. The denial has since moved to `Flockfile::parse`, so a
//! field change now costs an older peer the field rather than the whole
//! payload, and still bumps.
//! Version 6 bumped on a retype: [`Response::Reloading`] became a struct
//! variant to carry the apps a staged reload refused, so it serializes as
//! an object where an older peer reads an array. Version 7 bumped on the
//! same retype applied to [`Response::Restarted`], which a staged restart
//! refuses apps of for the same reason and had nowhere to name them.
//! Version 8 bumped on a second new `AppConfig` field, `environment`, for
//! the reason version 5 did. [`Request::PutSecrets`] rode in on the same
//! commit and forced nothing: it is an additive variant, and a daemon that
//! has never heard of it decodes [`Request::Unrecognized`] and refuses by
//! name. Version 9 bumped on removing `increment_var`, the first shape
//! change here to subtract a field rather than add one.
//!
//! A `*_wire_v9` test pins today's shape. A
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
pub const PROTOCOL_VERSION: u32 = 9;

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

    /// Fails whenever `PROTOCOL_VERSION` moves, which makes a bump a
    /// deliberate edit rather than a reflex. It does not detect a shape
    /// change that forgot to bump: the `*_wire_v9` snapshots do that, by
    /// gaining or losing the key.
    ///
    /// A bump moves four things together, and only this one fails on its
    /// own: the constant, this literal, the module doc's header and its
    /// version list, and the three `*_wire_vN` snapshots with the names
    /// that pin them.
    ///
    /// `depends_on` forced 5, `environment` 8, dropping `increment_var` 9.
    /// The `Response::Reloading` and `Response::Restarted` retypes forced 6
    /// and 7, an object not being an array.
    #[test]
    fn a_removed_app_config_field_forced_the_protocol_version_up() {
        assert_eq!(PROTOCOL_VERSION, 9);
    }

    #[test]
    fn the_floor_never_outruns_the_ceiling() {
        // A floor above the current version would refuse every peer,
        // including one built from this exact commit. Both sides are
        // `const`, so clippy wants the check itself const-evaluated.
        const { assert!(MIN_SUPPORTED <= PROTOCOL_VERSION) };
    }
}
