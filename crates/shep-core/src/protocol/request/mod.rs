//! RPC frames: requests, responses, envelopes, and structured errors

mod config_reply;
mod dog_access;
mod dog_source;
mod env_copy;
mod envelope;
mod handshake;
mod log_tail;
mod outcomes;
mod process;
mod redacted;
mod response;
mod response_name;
mod sandbox;
mod selector_spec;
mod smit;
mod vcs;
mod verbs;

#[cfg(test)]
mod reset_field_tests;

pub use config_reply::{SheepApplied, SheepConfigView, SheepDrift, SheepRefusal};
pub use dog_access::DogAccess;
pub use dog_source::DogSource;
pub use env_copy::{EnvCopyChange, EnvCopyKind};
pub use envelope::{Envelope, HelloReply, Reply, RpcError, RpcErrorCode};
pub use handshake::{Hello, HelloAck};
pub use log_tail::{LogStream, LogTail};
pub use outcomes::{
    ActionOutcome, ActionReply, LineOutcome, LineReply, SignalOutcome, SignalReply,
};
pub use process::{ExitInfo, Lamb, ProcessInfo, ProcessInfoBuilder, sort_flock};
pub use redacted::{DogSectionToml, EnvValue};
pub use response::{HostUsage, Response};
pub use selector_spec::SelectorSpec;
pub use smit::{Smit, SmitError};
pub use vcs::Vcs;
pub use verbs::Request;
