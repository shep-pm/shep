//! Environment inherited by spawned children
//!
//! Use [`INHERITED`] for this target, or either platform list to assemble another target's environment.
//! [`DEFAULT_PATH`] supplies a search path when the parent has none.

/// The `PATH` a child gets when the daemon itself has none.
#[cfg(unix)]
pub const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The `PATH` a child gets when the daemon itself has none.
///
/// Not expanded via `%SystemRoot%`: these literal paths are correct on
/// every standard Windows install and need no variable to resolve.
#[cfg(windows)]
pub const DEFAULT_PATH: &str = r"C:\Windows\system32;C:\Windows;C:\Windows\System32\Wbem";

/// Variables inherited from the daemon's own environment, on top of `PATH`.
pub const INHERITED_UNIX: &[&str] = &["HOME", "USER", "LANG", "TZ"];

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
pub const INHERITED_WINDOWS: &[&str] = &[
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

/// Inherited variables selected for this target
///
/// `cfg!(windows)` picks [`INHERITED_WINDOWS`]; other targets use [`INHERITED_UNIX`].
/// The daemon's spawn assembly (`build` and `inherited_env`) and the CLI's
/// dog vetting share this selection. Both lists remain available so tests
/// can exercise either platform on any host.
pub const INHERITED: &[&str] = if cfg!(windows) {
    INHERITED_WINDOWS
} else {
    INHERITED_UNIX
};

#[cfg(test)]
mod tests {
    #[test]
    fn windows_inherited_variables_are_pinned() {
        assert_eq!(
            super::INHERITED_WINDOWS,
            &[
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
            ]
        );
    }
}
