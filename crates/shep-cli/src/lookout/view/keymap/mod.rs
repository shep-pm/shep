//! The keymap overlay, frame 1k: every key lookout binds, grouped by what
//! it does, boxed over the dimmed body.
//!
//! The rows come from [`crate::lookout::keymap::rows`], which derives them
//! by running `map_key`. Nothing here decides which keys exist.

mod keymap_rows;
mod shed;
#[cfg(test)]
mod testing;
pub(super) use shed::draw;
