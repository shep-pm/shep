//! `shep import`: reading somebody else's file into shep's own state.
//!
//! Two subcommands with nothing in common but a noun. [`mod@pm2`] reads a
//! `dump.pm2` and writes a Flockfile, connecting to nothing. [`mod@dotenv`]
//! reads a `.env` into the secret store and one sheep's env, which needs a
//! running shepherd. They are separate verbs because their inputs, their
//! outputs and their flags are all separate; see the design doc.

pub(crate) mod dotenv;
pub(crate) mod pm2;
