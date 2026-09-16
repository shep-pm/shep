//! RPC frames: requests, responses, envelopes, and structured errors

mod config_reply;
mod envelope;
mod handshake;
mod outcomes;
mod process;
mod redacted;
mod response;
mod smit;
mod verbs;

pub use config_reply::{SheepApplied, SheepConfigView, SheepDrift, SheepRefusal};
pub use envelope::{Envelope, HelloReply, Reply, RpcError, RpcErrorCode};
pub use handshake::{Hello, HelloAck};
pub use outcomes::{
    ActionOutcome, ActionReply, LineOutcome, LineReply, SignalOutcome, SignalReply,
};
pub use process::{DogSource, ExitInfo, Lamb, ProcessInfo, ProcessInfoBuilder, sort_flock};
pub use redacted::{DogSectionToml, EnvValue};
pub use response::{HostUsage, Response};
pub use smit::{Smit, SmitError};
pub use verbs::{Request, SelectorSpec};
