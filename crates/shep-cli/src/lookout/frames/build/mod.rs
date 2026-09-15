//! Builds one gallery scene's rendered buffer, a step per module.
//!
//! [`scene_with`] is the only entry point, and the modules under it are
//! the steps it walks, in the order it walks them. [`flock`] decides who
//! is in the flock, [`prepare`] replays what the operator had already
//! done, [`link`] replays what arrived from the machine and the shepherd,
//! and [`armed`] replays the state that has to wait until after the tick
//! because its own expiry is shorter than the age the frame is drawn at.
//! [`probe`] is how the tests read a finished frame back.
//!
//! Between them these tests cover every [`super::scene::Scene`] but two.
//! `Folds` has nothing anywhere pinning what its caption claims beyond
//! its own `.snap` file. `Secrets` does, in
//! `armed::tests::the_secrets_scene_shows_a_revealed_row_not_a_mask`,
//! rather than beside the rest.
//!
//! The thirteen tests split out of
//! `every_scene_shows_the_thing_it_is_named_for` are `#[cfg(unix)]`. The
//! eight that already stood on their own are not, and this split changed
//! neither. The gate was there for a synthetic signalled exit that
//! `signal_label` resolves against the running platform's own table,
//! which is now
//! `flock::tests::the_errored_scene_shows_the_exit_it_is_named_for`
//! alone. The other twelve inherited it, and which of them could drop it
//! has never been measured.

mod armed;
mod flock;
mod link;
mod prepare;
mod probe;
mod scene;

pub use scene::scene_with;
