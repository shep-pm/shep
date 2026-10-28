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
/// A closed allowlist, not inherit-everything. Windows needs the longer
/// list. Many Win32 APIs read `%SystemRoot%` directly, and `PATHEXT` and
/// `COMSPEC` let a child run `.cmd` files. `PYTHONUTF8` and
/// `PYTHONIOENCODING` cover a pipe stdio handle. An older CPython falls
/// back to the ANSI code page there.
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
