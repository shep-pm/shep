use std::io;
use std::path::Path;

/// Whether `path` exists as a directory, in the one word a config-edit
/// warning appends after printing the path — but only when the filesystem
/// gives a definite answer.
///
/// [`cwd_advisory`] and [`log_path_advisory`] share this rather than each
/// re-deriving the same three-way match: a `cwd` has to be a directory
/// itself, and a log file's parent merely has to be one to hold it, so both
/// checks are this same question asked of a different path.
///
/// Only [`io::ErrorKind::NotFound`] and "exists but is a file" are
/// conclusive enough to report. A permission error or an unsettled mount
/// answers `None` rather than a guess, the same posture
/// `tokio_runner`'s `definitely_absent` documents for the identical
/// tradeoff: a false "missing" reads worse than the one real gap this
/// misses.
fn missing_directory(path: &Path) -> Option<&'static str> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => None,
        Ok(_) => Some("exists but is not a directory"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Some("does not exist yet"),
        Err(_) => None,
    }
}

/// An operator-facing note that a `cwd` looks wrong on disk, checked when
/// the field is written rather than left to surface as the OS's own `No
/// such file or directory` at the next respawn.
///
/// Advisory, never a refusal: unlike a log file, `normalize` cannot
/// substitute an empty `cwd` for a bad one, and a directory a deploy script
/// has not created yet is a legitimate config this call cannot tell apart
/// from a typo. The check and the spawn it warns about are not atomic —
/// `check_log_ancestry`'s own doc comment names the identical window for
/// its check, and this one inherits the same gap (`docs/specs/deferred.md`).
#[must_use]
pub(crate) fn cwd_advisory(cwd: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        if let Some(reason) = windows_name_advisory(cwd) {
            return Some(format!("{} {reason}", cwd.display()));
        }
    }
    let reason = missing_directory(cwd)?;
    Some(format!("{} {reason}", cwd.display()))
}

/// [`cwd_advisory`]'s counterpart for a log file rather than a working
/// directory: `out_file`/`err_file` are opened with `O_CREAT`, so the file
/// coming and going is normal and only its parent has to already be a
/// directory.
///
/// Also catches the file's own path already being a directory — a `cwd`
/// can never be that confusion in the other direction, since nothing
/// expects a file to sit there.
#[must_use]
pub(crate) fn log_path_advisory(path: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        if let Some(reason) = windows_name_advisory(path) {
            return Some(format!("{} {reason}", path.display()));
        }
    }
    if matches!(std::fs::metadata(path), Ok(meta) if meta.is_dir()) {
        return Some(format!("{} is a directory, not a log file", path.display()));
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty())?;
    let reason = missing_directory(parent)?;
    Some(format!("{} {reason}", parent.display()))
}

/// Windows path restrictions no unix filesystem enforces, so nothing in
/// this crate checked them anywhere before this: a reserved device name, a
/// character the filesystem refuses in a path, or a full path at or over
/// the classic per-path limit.
///
/// Returns the reason alone, the same shape [`missing_directory`] returns,
/// so [`cwd_advisory`] and [`log_path_advisory`] can prefix either with the
/// path in one place rather than each formatting its own sentence.
///
/// `#[cfg(windows)]` only. Warning an operator on macOS or Linux that `CON`
/// is a reserved name would be noise: the restriction only exists where the
/// daemon evaluating it will actually resolve the path.
#[cfg(windows)]
#[must_use]
fn windows_name_advisory(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt as _;

    /// Characters no Windows filesystem accepts anywhere in a path.
    ///
    /// The printable half of the set. Windows refuses every control
    /// character below U+0020 too, which is handled separately because
    /// naming one in an advisory means printing its code point rather than
    /// the character.
    const ILLEGAL: &[char] = &['<', '>', ':', '"', '|', '?', '*'];
    /// Device names Windows reserves regardless of extension, compared
    /// case-insensitively against each path component's stem.
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    /// The classic `MAX_PATH` every Windows install still enforces unless an
    /// operator has opted into the long-path policy. That opt-in only
    /// widens what passes; it never refuses something this check accepts.
    ///
    /// `>=` rather than `>`, because the 260 counts the terminating null: a
    /// fully qualified path may be 259 units, so 260 is already over.
    /// Not measurable on the reference Windows host, which has
    /// `LongPathsEnabled` set to 1, so paths of 258 through 261 all wrote a
    /// file there. Anyone re-checking this on that box will get four passes
    /// and learn nothing about the boundary.
    const MAX_PATH: usize = 260;

    // `encode_wide`, not `OsStr::len`. Windows counts this limit in UTF-16
    // units and `len` returns WTF-8 bytes, which are only equal for ASCII.
    // `é` is two bytes and one unit, and an emoji is four bytes and two, so
    // `len` warns about paths Windows accepts: a 259-unit path of emoji
    // measures 518 by bytes and would be refused an advisory it never
    // earned.
    let length = path.as_os_str().encode_wide().count();
    if length >= MAX_PATH {
        return Some(format!(
            "is {length} characters, at or over Windows' {MAX_PATH}-character default limit"
        ));
    }
    for component in path.components() {
        let std::path::Component::Normal(part) = component else {
            // `Prefix` (`C:`) and the rest name no file of their own; only
            // a `Normal` segment can be `CON.txt` or carry a stray `?`.
            continue;
        };
        let Some(name) = part.to_str() else {
            continue;
        };
        if let Some(bad) = name.chars().find(|c| ILLEGAL.contains(c)) {
            return Some(format!("contains `{bad}`, which Windows refuses in a path"));
        }
        // Below U+0020 only, which is the range Windows documents. Not
        // `char::is_control`, which also matches U+007F and the C1 block
        // U+0080 to U+009F: those are legal in a Windows file name, so
        // warning about them would refuse a path the filesystem accepts,
        // the same way counting bytes did above.
        //
        // Named by code point, not printed. A tab or a newline reaching an
        // advisory would otherwise rearrange the operator's line rather
        // than appear in it, and a NUL would truncate it.
        if let Some(control) = name.chars().find(|&c| (c as u32) < 0x20) {
            return Some(format!(
                "contains U+{:04X}, a control character Windows refuses in a path",
                control as u32
            ));
        }
        // Up to the LAST dot, not the first. Measured on Windows 2026-09-12
        // by creating each name and asking whether a file appeared:
        // `CON.txt` did not (the device took it), `CON.my.txt` did, and so
        // did `NUL.my.log`. Splitting on the first dot would warn about
        // `CON.my.txt`, which is an ordinary file.
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        if RESERVED
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(stem))
        {
            return Some(format!("names `{name}`, a reserved Windows device name"));
        }
    }
    None
}

/// Unlike `log_path_security`'s cases, these run on every platform this crate ships
/// on: `cwd_advisory` and `log_path_advisory` read nothing platform-specific
/// except through `windows_name_advisory`, which gets its own module below.
#[cfg(test)]
mod path_advisory_tests {

    use super::*;

    #[test]
    fn a_cwd_that_exists_gets_no_warning() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cwd_advisory(dir.path()), None);
    }

    #[test]
    fn a_cwd_that_does_not_exist_yet_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-created-yet");
        let warning = cwd_advisory(&missing).expect("a missing cwd warns");
        assert!(warning.contains("does not exist"), "{warning}");
        assert!(
            warning.contains(&missing.display().to_string()),
            "names the path: {warning}"
        );
    }

    #[test]
    fn a_cwd_that_is_a_file_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"").unwrap();
        let warning = cwd_advisory(&file).expect("a file where a directory is meant warns");
        assert!(warning.contains("is not a directory"), "{warning}");
    }

    #[test]
    fn a_log_path_whose_parent_exists_gets_no_warning() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("web-out.log");
        assert_eq!(
            log_path_advisory(&log),
            None,
            "the file itself need not exist yet; `open_log_path` creates it"
        );
    }

    #[test]
    fn a_log_path_whose_parent_does_not_exist_yet_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("not-created-yet").join("web-out.log");
        let warning = log_path_advisory(&log).expect("a missing parent warns");
        assert!(warning.contains("does not exist"), "{warning}");
    }

    /// The `Ok(_)` arm of [`missing_directory`], reached through the parent
    /// rather than through the path itself. A sibling test covers the path
    /// BEING a directory; this covers its parent being an ordinary file,
    /// which is the likelier misconfiguration: an operator points `out_file`
    /// at `<something>/web.log` where `<something>` is already a file.
    #[test]
    fn a_log_path_whose_parent_is_a_file_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("not-a-directory");
        std::fs::write(&parent, b"").unwrap();
        let warning =
            log_path_advisory(&parent.join("web-out.log")).expect("a parent that is a file warns");
        assert!(
            warning.contains("exists but is not a directory"),
            "{warning}"
        );
    }

    #[test]
    fn a_log_path_that_is_already_a_directory_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let warning =
            log_path_advisory(dir.path()).expect("a directory where a file is meant warns");
        assert!(
            warning.contains("is a directory, not a log file"),
            "{warning}"
        );
    }
}

/// `windows_name_advisory` only compiles under `cfg(windows)`, so its tests
/// only run on the `windows-latest` leg of the test matrix — the one place
/// the restrictions it checks are real.
#[cfg(all(test, windows))]
mod windows_advisory_tests {
    use super::*;

    #[test]
    fn a_reserved_device_name_is_named_regardless_of_extension() {
        let warning = windows_name_advisory(Path::new(r"C:\logs\CON.log"))
            .expect("CON is reserved regardless of extension");
        assert!(
            warning.contains("reserved Windows device name"),
            "{warning}"
        );
    }

    /// fails if the stem is taken up to the first dot again. Measured on
    /// Windows 2026-09-12: writing `CON.txt` created no file, the device
    /// swallowed it, while `CON.my.txt` and `NUL.my.log` both created
    /// ordinary files. So a second dot takes the name back out of the
    /// reserved set, and warning about it would be a false alarm on a path
    /// that works.
    #[test]
    fn a_second_dot_takes_a_name_back_out_of_the_reserved_set() {
        assert_eq!(
            windows_name_advisory(Path::new(r"C:\logs\CON.my.txt")),
            None,
            "CON.my is not a device name"
        );
        assert_eq!(
            windows_name_advisory(Path::new(r"C:\logs\NUL.my.log")),
            None,
            "NUL.my is not a device name"
        );
        assert!(
            windows_name_advisory(Path::new(r"C:\logs\CON.txt")).is_some(),
            "one dot still leaves CON reserved"
        );
    }

    /// fails if the length is taken in bytes again. Windows counts this
    /// limit in UTF-16 units and the two agree only for ASCII, so a
    /// non-BMP path sits either side of the limit depending on which is
    /// counted.
    #[test]
    fn the_path_limit_counts_utf16_units_not_bytes() {
        let under = format!(r"C:\logs\{}.log", "\u{1f411}".repeat(120));
        assert_eq!(
            windows_name_advisory(Path::new(&under)),
            None,
            "252 units is under the limit, though the same path is 492 bytes"
        );

        let over = format!(r"C:\logs\{}.log", "\u{1f411}".repeat(130));
        let warning = windows_name_advisory(Path::new(&over)).expect("260 units is at the limit");
        assert!(warning.contains("260-character"), "{warning}");
    }

    /// fails if the control range widens to `char::is_control`. A tab is
    /// refused by Windows and U+0085 is not, so a check that cannot tell
    /// them apart refuses a path the filesystem accepts.
    #[test]
    fn a_control_character_is_named_by_code_point_and_c1_is_not() {
        let warning = windows_name_advisory(Path::new("C:\\logs\\web\tout.log"))
            .expect("a tab is refused anywhere in a Windows path");
        assert!(warning.contains("U+0009"), "{warning}");
        // Printed as a code point, or the advisory would carry the tab
        // itself and rearrange the operator's line.
        assert!(!warning.contains('\t'), "{warning}");

        assert_eq!(
            windows_name_advisory(Path::new("C:\\logs\\web\u{85}out.log")),
            None,
            "U+0085 is a C1 control that Windows accepts in a file name"
        );
    }

    #[test]
    fn an_ordinary_path_gets_no_warning() {
        assert_eq!(
            windows_name_advisory(Path::new(r"C:\logs\web-out.log")),
            None
        );
    }

    #[test]
    fn an_illegal_character_is_named() {
        let warning = windows_name_advisory(Path::new(r"C:\logs\web?out.log"))
            .expect("`?` is refused anywhere in a Windows path");
        assert!(warning.contains('?'), "{warning}");
    }

    #[test]
    fn an_over_length_path_is_named() {
        let long = format!(r"C:\{}", "a".repeat(300));
        let warning =
            windows_name_advisory(Path::new(&long)).expect("300 characters is over MAX_PATH");
        assert!(warning.contains("260-character"), "{warning}");
    }
}
