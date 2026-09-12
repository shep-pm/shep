//! Shared by `fixtures.rs` and `wire_export.rs`, the two targets that hold a
//! committed file against what the code produces.

use std::fs;
use std::path::Path;

/// The environment variable that rewrites a committed file instead of
/// asserting against it. One name, so blessing one corpus cannot silently
/// take a different door from blessing the other.
const BLESS: &str = "SHEP_CHANNEL_BLESS";

/// Writes `produced` to `path` under [`BLESS`], and otherwise asserts the
/// committed bytes match it.
///
/// `stale_hint` says who else reads these bytes, since the assertion fires
/// on a change that is correct in Rust and breaking downstream.
///
/// # Panics
///
/// If the file cannot be written under [`BLESS`], if it cannot be read
/// without it, or if the committed bytes differ.
pub fn bless_or_compare(path: &Path, produced: &str, stale_hint: &str) {
    if std::env::var_os(BLESS).is_some() {
        let parent = path.parent().expect("a committed file has a parent");
        fs::create_dir_all(parent).expect("create the corpus directory");
        fs::write(path, produced).expect("write the committed file");
        return;
    }
    let committed = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "{}: {error}. Run with {BLESS}=1 to create it.",
            path.display()
        )
    });
    assert_eq!(
        committed,
        produced,
        "{} is stale. {stale_hint}",
        path.display()
    );
}
