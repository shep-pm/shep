//! RPC frames: requests, responses, envelopes, and structured errors

mod config_reply;
mod dog_source;
mod envelope;
mod handshake;
mod outcomes;
mod process;
mod question;
mod redacted;
#[cfg(test)]
mod request_wire;
mod response;
mod response_name;
mod selector_spec;
mod smit;
mod verbs;

pub use config_reply::{SheepApplied, SheepConfigView, SheepDrift, SheepRefusal};
pub use dog_source::DogSource;
pub use envelope::{Envelope, HelloReply, Reply, RpcError, RpcErrorCode};
pub use handshake::{Hello, HelloAck};
pub use outcomes::{
    ActionOutcome, ActionReply, LineOutcome, LineReply, SignalOutcome, SignalReply,
};
pub use process::{ExitInfo, Lamb, ProcessInfo, ProcessInfoBuilder, sort_flock};
pub use question::{OpenQuestion, Settled};
pub use redacted::{DogSectionToml, EnvValue};
pub use response::{HostUsage, Response};
pub use selector_spec::SelectorSpec;
pub use smit::{Smit, SmitError};
pub use verbs::Request;
