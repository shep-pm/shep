//! Replace a process's image with `execve` and carry on. Named descriptors
//! and a state blob cross, and the children the old image started stay the
//! new image's children. Unix only: elsewhere these answer `Unsupported`.
//!
//! [`Handover`] is the outgoing side and [`boot`] the incoming one, which a
//! successor calls first thing in `main`. [`wait_for_child`] reaps a child the
//! old image started, by pid.
//!
//! A failed handover leaves this image running and returns why; a completed
//! one cannot be undone.
//! ```no_run
//! # #[cfg(unix)]
//! use serde::{Deserialize, Serialize};
//! # #[cfg(unix)]
//! use shep_handover::{Handover, State, Verdict};
//!
//! # #[cfg(unix)]
//! #[derive(Serialize, Deserialize)]
//! struct Carry {
//!     child: u32,
//! }
//!
//! # #[cfg(unix)]
//! impl State for Carry {
//!     const FORMAT: u32 = 1;
//! }
//!
//! # #[cfg(unix)]
//! #[tokio::main(flavor = "current_thread")]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     if let Some(mut arrival) = shep_handover::boot::<Carry>()? {
//!         let output = arrival.fds.take("child-out").expect("carried").into_reader()?;
//!         let exit = shep_handover::wait_for_child(arrival.state.child).await?;
//!         # let _ = (output, exit);
//!         return Ok(());
//!     }
//!
//!     let child_out = std::io::pipe()?.0;
//!     let state = Carry { child: 4242 };
//!     let refusal = Handover::new(&state)
//!         .carry("child-out", &child_out)
//!         .exec(&|_: &std::path::Path| Verdict::Carryable)
//!         .unwrap_err();
//!     eprintln!("staying on this image: {refusal}");
//!     Ok(())
//! }
//! # #[cfg(not(unix))]
//! # fn main() {}
//! ```

#![doc(test(attr(deny(warnings))))]
// Not `forbid`: `sys` adopts the descriptors a handover left open, the one
// thing `std` cannot express without taking a raw number.
#![deny(unsafe_code)]

mod carried;
#[cfg(unix)]
mod envelope;
mod error;
#[cfg(unix)]
mod exec;
mod fitness;
mod incoming;
mod outgoing;
#[cfg(unix)]
mod probe;
mod reap;
mod state;
#[cfg(unix)]
#[allow(unsafe_code, reason = "adopting an inherited descriptor; see `sys`")]
mod sys;
mod target;

pub use carried::Carried;
#[cfg(unix)]
pub use carried::CarriedFd;
pub use error::{BootError, HandOverError};
pub use fitness::{Fitness, Verdict};
pub use incoming::{Arrival, boot};
pub use outgoing::Handover;
pub use reap::{ChildExit, wait_for_child};
pub use state::{HANDOVER_ENV, State};
pub use target::{exec_target, record_launch_path};
