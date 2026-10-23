use shep_core::inherited_env::DEFAULT_PATH;
pub(super) use shep_core::inherited_env::INHERITED;
#[cfg(any(test, doc))]
pub(super) use shep_core::inherited_env::INHERITED_WINDOWS;
use std::collections::BTreeMap;

/// The env every spawned child starts from, before the app's own `env` map
/// is folded on top (app config always wins on conflict), plus `PATH`:
/// without one, a bare program or interpreter name can never be found by
/// exec, so it's seeded unconditionally rather than left to `keys`.
///
/// [`build`](crate::assemble::spawn_spec::build) calls this with the platform-selected [`INHERITED`] and a
/// `std::env::var` reader; a test can pass [`INHERITED_WINDOWS`] and a fake
/// reader instead, on any host, without touching real process env.
/// Mutating that from a test is unsound under a parallel test binary, and
/// `std::env::set_var` is itself `unsafe` since edition 2024.
pub(super) fn inherited_env(
    keys: &[&str],
    env_reader: &dyn Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    // An empty PATH ("PATH=") is treated as absent: `Some("")` would
    // otherwise slip through `unwrap_or_else`, and an empty PATH resolves a
    // bare program against the cwd instead of searching.
    let path = env_reader("PATH")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_PATH.to_string());
    env.insert("PATH".to_string(), path);
    for key in keys {
        if let Some(value) = env_reader(key) {
            env.insert((*key).to_string(), value);
        }
    }
    env
}

/// [`inherited_env`]'s two arguments bundled into one: a key list plus a
/// reader, so [`build`](crate::assemble::spawn_spec::build) takes one parameter for this instead of two, and a
/// test can substitute both without touching real process env.
pub(super) struct InheritedEnv<'a> {
    pub(super) keys: &'a [&'a str],
    pub(super) reader: &'a dyn Fn(&str) -> Option<String>,
}
