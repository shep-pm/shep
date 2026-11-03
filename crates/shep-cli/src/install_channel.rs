//! Which package manager installed the running shep, read from the path of
//! the binary alone, so detection starts no process.
//!
//! Paths are matched as text with `\` read as `/`, a `\\?\` prefix dropped
//! and case folded, so the Windows layouts are tested on every platform.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The identifier `winget upgrade` takes.
const WINGET_IDENTIFIER: &str = "shep-pm.shep";

/// The one file a Debian package of shep installs, as dpkg lists it.
const DEB_BINARY: &str = "/usr/bin/shep";

/// Where the dpkg database keeps its file lists.
const DPKG_INFO: &str = "/var/lib/dpkg/info";

/// How shep got onto this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    /// A Homebrew keg under a `Cellar`.
    Homebrew,
    /// `cargo install`, into `$CARGO_HOME/bin`.
    Cargo,
    /// A Scoop app directory.
    Scoop,
    /// A WinGet package directory.
    Winget,
    /// A Chocolatey package directory.
    Chocolatey,
    /// A `.deb` that dpkg lists `/usr/bin/shep` for.
    Deb,
    /// Anything else, such as a release archive unpacked by hand.
    Archive,
}

/// What [`Channel::of`] reads besides the path.
#[derive(Debug, Default)]
pub(crate) struct Probe {
    /// Where `cargo install` puts its `bin`, `$CARGO_HOME` or `~/.cargo`.
    pub(crate) cargo_home: Option<PathBuf>,
    /// `$SCOOP` and `$SCOOP_GLOBAL`, whichever are set.
    pub(crate) scoop_roots: Vec<PathBuf>,
    /// `$ChocolateyInstall`.
    pub(crate) chocolatey_root: Option<PathBuf>,
    /// The directory of dpkg's `<package>.list` files.
    pub(crate) dpkg_info: PathBuf,
}

impl Probe {
    /// The probe `var` describes.
    fn from_env(var: &dyn Fn(&str) -> Option<OsString>) -> Self {
        let set = |key: &str| {
            var(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
            cargo_home: set("CARGO_HOME")
                .or_else(|| shep_core::paths::user_home(var).map(|home| home.join(".cargo"))),
            scoop_roots: ["SCOOP", "SCOOP_GLOBAL"]
                .into_iter()
                .filter_map(set)
                .collect(),
            chocolatey_root: set("ChocolateyInstall"),
            dpkg_info: PathBuf::from(DPKG_INFO),
        }
    }
}

impl Channel {
    /// The channel of the shep installed at `exe`, a resolved path.
    pub(crate) fn of(exe: &Path, probe: &Probe) -> Self {
        let path = Components::of(exe);
        if path.has_run(&["cellar", "shep"]) {
            Self::Homebrew
        } else if probe
            .cargo_home
            .as_deref()
            .is_some_and(|home| path.is_directly_in(&Components::of(home).child("bin")))
        {
            Self::Cargo
        } else if path.has_run(&["scoop", "apps", "shep"])
            || probe
                .scoop_roots
                .iter()
                .any(|root| path.is_under(root, &["apps", "shep"]))
        {
            Self::Scoop
        } else if path.has_winget_packages() {
            Self::Winget
        } else if path.has_run(&["chocolatey", "lib", "shep"])
            || probe
                .chocolatey_root
                .as_deref()
                .is_some_and(|root| path.is_under(root, &["lib", "shep"]))
        {
            Self::Chocolatey
        } else if path.is_exactly(DEB_BINARY) && dpkg_names_shep(&probe.dpkg_info) {
            Self::Deb
        } else {
            Self::Archive
        }
    }

    /// The channel of the running binary, `Archive` when it cannot be
    /// resolved.
    pub(crate) fn running() -> Self {
        let Ok(exe) = std::env::current_exe().and_then(|exe| exe.canonicalize()) else {
            return Self::Archive;
        };
        Self::of(&exe, &Probe::from_env(&|key| std::env::var_os(key)))
    }

    /// The command that upgrades shep through this channel, `None` when none
    /// does.
    pub(crate) fn command(self) -> Option<String> {
        match self {
            Self::Homebrew => Some("brew upgrade shep".to_owned()),
            Self::Cargo => Some("cargo install --locked shep".to_owned()),
            Self::Scoop => Some("scoop update shep".to_owned()),
            Self::Winget => Some(format!("winget upgrade {WINGET_IDENTIFIER}")),
            Self::Chocolatey => Some("choco upgrade shep".to_owned()),
            Self::Deb | Self::Archive => None,
        }
    }
}

/// A path's components as text: lower case, `/` for either separator, no
/// verbatim prefix, no empty parts.
struct Components(Vec<String>);

impl Components {
    fn of(path: &Path) -> Self {
        let text = path.to_string_lossy().to_lowercase().replace('\\', "/");
        let text = text.strip_prefix("//?/").unwrap_or(&text);
        Self(
            text.split('/')
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    }

    fn child(mut self, name: &str) -> Self {
        self.0.push(name.to_owned());
        self
    }

    /// Whether `run` appears as consecutive whole components.
    fn has_run(&self, run: &[&str]) -> bool {
        self.0.windows(run.len()).any(|window| window == run)
    }

    /// Whether this is a file whose parent directory is `dir`.
    fn is_directly_in(&self, dir: &Self) -> bool {
        self.0.len() == dir.0.len() + 1 && self.0.starts_with(&dir.0)
    }

    /// Whether this sits under `root` joined with `run`.
    fn is_under(&self, root: &Path, run: &[&str]) -> bool {
        let mut prefix = Components::of(root).0;
        prefix.extend(run.iter().map(|part| (*part).to_owned()));
        self.0.len() > prefix.len() && self.0.starts_with(&prefix)
    }

    fn is_exactly(&self, path: &str) -> bool {
        self.0 == Self::of(Path::new(path)).0
    }

    /// A `WinGet` component with a `Packages` one somewhere after it.
    fn has_winget_packages(&self) -> bool {
        self.0
            .iter()
            .position(|part| part == "winget")
            .is_some_and(|at| self.0[at + 1..].iter().any(|part| part == "packages"))
    }
}

/// Whether dpkg's `shep.list`, or a multiarch `shep:<arch>.list`, names
/// [`DEB_BINARY`].
fn dpkg_names_shep(info: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(info) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let is_shep = name.to_str().is_some_and(|name| {
            name == "shep.list"
                || name
                    .strip_prefix("shep:")
                    .is_some_and(|arch| arch.strip_suffix(".list").is_some_and(|a| !a.is_empty()))
        });
        is_shep
            && std::fs::read_to_string(entry.path())
                .is_ok_and(|list| list.lines().any(|line| line.trim() == DEB_BINARY))
    })
}

#[cfg(test)]
mod tests;
