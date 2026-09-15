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

/// The `PATH` a child gets when the daemon itself has none.
#[cfg(unix)]
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The `PATH` a child gets when the daemon itself has none.
///
/// Not expanded via `%SystemRoot%`: these literal paths are correct on
/// every standard Windows install and need no variable to resolve.
#[cfg(windows)]
const DEFAULT_PATH: &str = r"C:\Windows\system32;C:\Windows;C:\Windows\System32\Wbem";

/// Variables inherited from the daemon's own environment, on top of `PATH`.
const INHERITED_UNIX: &[&str] = &["HOME", "USER", "LANG", "TZ"];

/// Variables inherited from the daemon's own environment, on top of `PATH`.
///
/// Longer than the unix list because Windows children need it: many Win32
/// APIs read `%SystemRoot%` directly, and `PATHEXT`/`COMSPEC` let a child
/// resolve and run `.cmd` files at all. `TEMP`/`TMP` and the
/// `USERPROFILE`/`APPDATA`/`LOCALAPPDATA` trio are where most runtimes keep
/// per-user state. Still a closed allowlist, not inherit-everything.
///
/// `PYTHONUTF8`/`PYTHONIOENCODING` are here for one specific failure: a
/// non-console stdio handle (a pipe, which is what every spawned child gets)
/// makes CPython up to 3.14 fall back to the legacy ANSI code page for
/// `sys.stdout`/`sys.stderr` on Windows, and any non-ASCII byte an app prints
/// then raises `UnicodeEncodeError`. CPython 3.15 turns UTF-8 mode on by
/// default (PEP 686), so these two matter for an older interpreter and for
/// anything that sets `PYTHONUTF8=0`. Neither has a unix equivalent to piggyback
/// on. `LANG` is what decides a child's stdio encoding there and
/// [`INHERITED_UNIX`] forwards it, which is not the same as setting one: a
/// daemon started with no `LANG`, or with one naming a non-UTF-8 locale,
/// hands that to its children. Windows has no variable in that role to
/// forward at all. Setting either in the daemon's own environment now
/// reaches every spawned app; an app can still set them itself, per-app, in
/// its Flockfile `env`.
pub(super) const INHERITED_WINDOWS: &[&str] = &[
    "SystemRoot",
    "windir",
    "SystemDrive",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "LANG",
    "TZ",
    "PYTHONUTF8",
    "PYTHONIOENCODING",
];

/// The list [`build`](crate::assemble::spawn_spec::build) actually reads on this build. Picked with `cfg!`
/// rather than `#[cfg(...)]` gating [`INHERITED_UNIX`]/[`INHERITED_WINDOWS`]
/// themselves: both stay referenced on every target this way, so a test can
/// run either platform's list (and [`inherited_env`]'s filtering) on any
/// host, the same way this crate's unit-file renderers are pinned by text
/// on a Mac without a systemd host, and `-D warnings` never calls the other
/// platform's list dead code.
pub(super) const INHERITED: &[&str] = if cfg!(windows) {
    INHERITED_WINDOWS
} else {
    INHERITED_UNIX
};

/// [`inherited_env`]'s two arguments bundled into one: a key list plus a
/// reader, so [`build`](crate::assemble::spawn_spec::build) takes one parameter for this instead of two, and a
/// test can substitute both without touching real process env.
pub(super) struct InheritedEnv<'a> {
    pub(super) keys: &'a [&'a str],
    pub(super) reader: &'a dyn Fn(&str) -> Option<String>,
}
