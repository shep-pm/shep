//! Builds one gallery scene's rendered buffer, a step per module.
//!
//! [`scene_with`] is the only entry point, and the modules under it are
//! the steps it walks, in the order it walks them. [`flock`] decides who
//! is in the flock, [`prepare`] replays what the operator had already
//! done, [`link`] replays what arrived from the machine and the shepherd,
//! and [`armed`] replays the state that has to wait until after the tick
//! because its own expiry is shorter than the age the frame is drawn at.
//! [`probe`] is how the tests read a finished frame back.

mod armed;
mod flock;
mod link;
mod prepare;
mod probe;
mod scene;

pub use scene::scene_with;
