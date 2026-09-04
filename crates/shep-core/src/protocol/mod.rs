//! The client<->daemon wire protocol (version 4)
//!
//! Typed request/response enums plus bus events. Framing lives in [`wire`];
//! every type here is snapshot-pinned, so changing a serialized shape bumps
//! [`PROTOCOL_VERSION`].
//!
//! Version 4 bumped on an addition, against the rule below. An older daemon
//! cannot decode the new config requests, so the handshake must catch the
//! skew.
//!
//! Two test-name conventions assert opposite things: a `*_wire_v4` snapshot
//! pins the shape this crate serializes today, so it follows
//! [`PROTOCOL_VERSION`] and gets renamed when that moves; a
//! `v1_*_fixture_still_deserializes` test pins a literal payload from an old
//! peer, and its name must never move since it records where the bytes
//! came from.

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
    SheepDrift, SignalOutcome, SignalReply, Smit, SmitError, sort_flock,
};
pub use shep_channel::{CHANNEL_VERSION, ChildMessage, ShepherdMessage};
pub use wire::{MAX_FRAME_BYTES, WireError, codec, decode_frame, encode_frame};

/// The shepherd channel's wire types. Moved to the `shep-channel` crate;
/// this path is kept so consumers of 0.1.x do not break.
///
/// `docs/specs/deferred.md` told app authors to reach for it by this path
/// and this crate's own CHANGELOG advertised it by name, so removing it
/// would break anyone who listened. It is also what `cargo-semver-checks`
/// sees on a release pull request: a removed public module is a major bump,
/// and `version_group = "shep"` would carry all five crates with it.
///
/// No `since`, deliberately. The attribute takes a literal, so any value is
/// a guess about which release this branch lands in, and it has already
/// been wrong twice: written as 0.1.26 when the workspace was at 0.1.25,
/// then 0.1.28, while 0.1.26 through 0.1.31 all shipped without this
/// module. A version that names a release the item was never in is worse
/// than no version, and the note carries the part a caller acts on.
#[deprecated(note = "use `shep_core::protocol` directly")]
pub mod channel {
    pub use shep_channel::{CHANNEL_VERSION, ChildMessage, ShepherdMessage};
}

/// Wire protocol version.
///
/// Evolution rule: ADDITIVE optional fields (new serde-defaulted `Option<T>`
/// fields, new variants behind `#[non_exhaustive]`) keep the version.
/// Removing, renaming, or retyping anything serialized bumps it, recorded in
/// the CHANGELOG. Byte fixtures in each protocol module pin the deserialize
/// direction.
pub const PROTOCOL_VERSION: u32 = 4;
