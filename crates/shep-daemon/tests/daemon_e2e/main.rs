//! Real-daemon integration tier: boots shep-daemon on a temp `$SHEP_HOME`,
//! talks to it over the control socket with shep-core's own codec, and
//! drives real child processes.
//!
//! Real time throughout: a paused clock's auto-advance would expire timeouts
//! before IO wakeups arrive.

// Many cases here are `#[cfg(unix)]`, so on Windows those items are unreached.
#![cfg_attr(windows, allow(dead_code))]
// And so are the imports only those cases use.
#![cfg_attr(windows, allow(unused_imports))]

use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use shep_core::transport::{self, ClientStream};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use shep_core::config::{AppConfig, ProbeConfig, ProbeKind};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{
    ActionOutcome, BusEvent, ChildMessage, Envelope, Hello, HelloAck, HelloReply, LineOutcome,
    MIN_SUPPORTED, PROTOCOL_VERSION, ProcessEventKind, ProcessInfo, Reply, Request, Response,
    RpcError, RpcErrorCode, SelectorSpec, ServerFrame, codec, decode_frame, encode_frame,
};
use shep_core::status::ProcStatus;
use shep_core::values::UpDuration;

use shep_daemon::boot::{BootError, BootOptions, DIR_MODE, boot};
use shep_daemon::rpc::RpcContext;
use shep_daemon::tokio_runner::TokioRunner;

const RECV_TIMEOUT: Duration = Duration::from_secs(10);

mod app_configs;
mod channel;
mod daemon_lifetime;
mod harness;
mod lifecycle;
mod probed_reload;
mod protocol;
mod reload;
mod smit;

pub(crate) use app_configs::*;
pub(crate) use harness::*;
pub(crate) use reload::*;
