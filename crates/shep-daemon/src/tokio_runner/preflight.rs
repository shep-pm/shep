//! What exec will make of a spec's program, before anything is spawned.

use std::fs;
use std::io;
use std::path::Path;

use crate::runner::{Preflight, SpawnSpec};

/// What exec will make of `spec.program`, before anything is spawned.
///
/// A `/` makes it a path, absolute or relative to `spec.cwd`, whose absence is
/// [`Preflight::Impossible`] and refuses the caller's whole batch. Without one
/// it is a bare command resolved through `spec.env`'s own `PATH`, at most
/// [`Preflight::Doubtful`]: a `shep startup` unit's `PATH` is not the
/// operator's shell's. Read through [`definitely_absent`], not [`Path::exists`].
pub(super) fn what_exec_will_find(spec: &SpawnSpec) -> Preflight {
    if spec.program.is_empty() {
        return Preflight::Unknown;
    }
    let program = Path::new(&spec.program);
    // A `/` is the claim that this is a path; Windows spells it `\`. Missing
    // that claim costs only the clear refusal: the fall-through looks the
    // program up on PATH, misses, and the spawn proceeds anyway.
    if spec.program.contains('/') || spec.program.contains(std::path::MAIN_SEPARATOR) {
        let full = if program.is_absolute() {
            program.to_path_buf()
        } else {
            match &spec.cwd {
                Some(cwd) => cwd.join(program),
                None => return Preflight::Unknown,
            }
        };
        if !definitely_absent(&full) {
            return Preflight::Unknown;
        }
        return Preflight::Impossible(format!("no such file: {}", full.display()));
    }
    let Some(path) = spec.env.get("PATH").filter(|value| !value.is_empty()) else {
        return Preflight::Unknown;
    };
    // Absent only if every entry answers a plain `NotFound`: one unreadable
    // directory means exec may still find the program there. `split_paths`
    // rather than a separator of our own, since a Windows entry carries a `:`
    // of its own and may be quoted.
    for dir in std::env::split_paths(path).filter(|dir| !dir.as_os_str().is_empty()) {
        match fs::metadata(dir.join(program)) {
            Ok(_) => return Preflight::Unknown,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Preflight::Unknown,
        }
    }
    Preflight::Doubtful(format!(
        "`{}` is not on the shepherd's PATH ({})",
        spec.program,
        summarise_path(path)
    ))
}

/// How many `PATH` entries a preflight message names before summarising the
/// rest.
///
/// Four: `assemble::environment_inheritance::inherited_env`'s fallback for a
/// startup unit with no `PATH` is three entries, so the case an operator hits
/// prints in full with one to spare. What gets cut off is an interactive
/// shell's `PATH`, which is unreadable in a terminal error.
const PATH_ENTRIES_IN_MESSAGE: usize = 4;

/// What separates one `PATH` entry from the next.
///
/// Display only: the lookup in `what_exec_will_find` goes through
/// `std::env::split_paths`, which also understands quoting.
pub(super) const PATH_LIST_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

/// `path` as a message should print it: in full when short, and otherwise its
/// first [`PATH_ENTRIES_IN_MESSAGE`] entries with a count of the rest.
pub(super) fn summarise_path(path: &str) -> String {
    let entries: Vec<&str> = path
        .split(PATH_LIST_SEPARATOR)
        .filter(|dir| !dir.is_empty())
        .collect();
    if entries.len() <= PATH_ENTRIES_IN_MESSAGE {
        return path.to_string();
    }
    format!(
        "{} and {} more entries",
        entries[..PATH_ENTRIES_IN_MESSAGE].join(&PATH_LIST_SEPARATOR.to_string()),
        entries.len() - PATH_ENTRIES_IN_MESSAGE,
    )
}

/// Whether the filesystem says, without qualification, that `path` is not
/// there. `NotFound` and nothing else.
///
/// [`Path::exists`] returns `false` on any [`fs::metadata`] error, so a
/// permission error or an unsettled mount would read as absent and have
/// `what_exec_will_find` refuse a whole batch over a filesystem that was
/// unavailable for a moment. Follows symlinks, as exec does.
fn definitely_absent(path: &Path) -> bool {
    matches!(fs::metadata(path), Err(err) if err.kind() == io::ErrorKind::NotFound)
}
