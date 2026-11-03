//! What a dog's manifest says it needs. `shep adopt` and `shep enable` warn
//! of what is unmet and go on; `shep install` plans the missing dogs and
//! refuses when the index cannot supply one.

use shep_core::dogs::manifest::Manifest;

use crate::dog_index::{AvailableDog, DogSourceKind};
use crate::exit::ExitCode;
use crate::output::Streams;

/// [`emit_notice`](crate::output::emit_notice) code for a dependency dog that
/// is not adopted; not a failure.
const DOG_DEPENDENCY_NOTICE: &str = "dog_dependency_missing";

/// [`emit_notice`](crate::output::emit_notice) code for a dog that needs a
/// newer shep than this one; not a failure.
const DOG_SHEP_TOO_OLD_NOTICE: &str = "dog_needs_newer_shep";

/// Whether the dog `name` is built in or in `adopted`.
fn is_present(name: &str, adopted: &[String]) -> bool {
    crate::dog::BUILT_IN_DOGS.contains(&name) || adopted.iter().any(|dog| dog == name)
}

/// Why `shep install` cannot supply a dog's dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The index has no dog called `dependency`.
    NotIndexed {
        dependent: String,
        dependency: String,
    },
    /// The index lists `dependency` but approves no version shep can build.
    NotInstallable {
        dependent: String,
        dependency: String,
    },
    /// `range`, as the manifest declares it, is not a version range.
    BadRange {
        dependent: String,
        dependency: String,
        range: String,
    },
    /// The index approves `version` of `dependency`, outside `range`.
    Unmet {
        dependent: String,
        dependency: String,
        range: String,
        version: String,
    },
    /// `dependency` is already being installed, so it and `dependent` wait
    /// on each other. `trail` is the dogs being installed, outermost first.
    Cycle {
        dependent: String,
        dependency: String,
        trail: Vec<String>,
    },
}

impl Refusal {
    /// The exit code `shep install` refuses with.
    pub(super) fn code(&self) -> ExitCode {
        match self {
            Self::NotIndexed { .. } => ExitCode::NotFound,
            _ => ExitCode::Failure,
        }
    }
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotIndexed {
                dependent,
                dependency,
            } => write!(
                f,
                "`{dependent}` depends on `{dependency}`, which the dog index does not carry; \
                 adopt it with `shep adopt <path>` first"
            ),
            Self::NotInstallable {
                dependent,
                dependency,
            } => write!(
                f,
                "`{dependent}` depends on `{dependency}`, which the dog index lists but approves \
                 no version shep can install; adopt it with `shep adopt <path>` first"
            ),
            Self::BadRange {
                dependent,
                dependency,
                range,
            } => write!(
                f,
                "`{dependent}` depends on `{dependency}` at {range:?}, which is not a version range"
            ),
            Self::Unmet {
                dependent,
                dependency,
                range,
                version,
            } => write!(
                f,
                "`{dependent}` needs `{dependency}` {range}, and the dog index approves \
                 {version}"
            ),
            Self::Cycle {
                dependent,
                dependency,
                trail,
            } => write!(
                f,
                "`{dependent}` depends on `{dependency}`, which is already being installed \
                 ({}): a dependency cycle",
                trail
                    .iter()
                    .map(String::as_str)
                    .chain([dependency.as_str()])
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
        }
    }
}

impl core::error::Error for Refusal {}

/// The dogs `manifest` needs that are not yet present, as index rows in
/// manifest order, for `shep install` to install first.
///
/// `dependent` is the dog being installed and `trail` every dog being
/// installed, outermost first, ending in `dependent`. A dependency adopted already is
/// skipped whatever its version, as at enable.
///
/// # Errors
/// The [`Refusal`] for the first missing dependency the index does not carry
/// or cannot supply in the declared range, or that is already in `trail`.
pub(super) fn missing<'a>(
    manifest: &Manifest,
    dependent: &str,
    trail: &[String],
    adopted: &[String],
    index: &'a [AvailableDog],
) -> Result<Vec<&'a AvailableDog>, Refusal> {
    let mut wanted: Vec<&AvailableDog> = Vec::new();
    for dependency in &manifest.depends.dogs {
        let name = dependency.name.as_str();
        if is_present(name, adopted) || wanted.iter().any(|dog| dog.adopt_as == name) {
            continue;
        }
        let (dependent, dependency_name) = (dependent.to_owned(), name.to_owned());
        if trail.iter().any(|above| above == name) {
            return Err(Refusal::Cycle {
                dependent,
                dependency: dependency_name,
                trail: trail.to_vec(),
            });
        }
        let Some(row) = index.iter().find(|dog| dog.adopt_as == name) else {
            return Err(Refusal::NotIndexed {
                dependent,
                dependency: dependency_name,
            });
        };
        let DogSourceKind::Cargo {
            version: Some(version),
            ..
        } = &row.source
        else {
            return Err(Refusal::NotInstallable {
                dependent,
                dependency: dependency_name,
            });
        };
        let range =
            semver::VersionReq::parse(&dependency.version).map_err(|_| Refusal::BadRange {
                dependent: dependent.clone(),
                dependency: dependency_name.clone(),
                range: dependency.version.clone(),
            })?;
        let met = semver::Version::parse(version).is_ok_and(|version| range.matches(&version));
        if !met {
            return Err(Refusal::Unmet {
                dependent,
                dependency: dependency_name,
                range: dependency.version.clone(),
                version: version.clone(),
            });
        }
        wanted.push(row);
    }
    Ok(wanted)
}

/// Warns of each dog `manifest` depends on that is neither built in nor in
/// `adopted`, and of a minimum shep newer than this build.
///
/// A dependency is checked by name: which version of it is installed is not
/// read. A minimum that is not a version is not judged.
pub(super) fn warn_of_unmet_depends(
    streams: &mut Streams<'_>,
    name: &str,
    manifest: &Manifest,
    adopted: &[String],
) {
    for dependency in &manifest.depends.dogs {
        if !is_present(&dependency.name, adopted) {
            let message = format!(
                "`{name}` depends on `{dep}`, which is not adopted, so `{name}` may not work \
                 until you run `shep adopt` for `{dep}`",
                dep = dependency.name
            );
            streams.aside(DOG_DEPENDENCY_NOTICE, &message);
        }
    }
    let Some(minimum) = &manifest.depends.shep else {
        return;
    };
    let this = env!("CARGO_PKG_VERSION");
    let too_old = match (
        semver::Version::parse(minimum),
        semver::Version::parse(this),
    ) {
        (Ok(minimum), Ok(this)) => this < minimum,
        _ => false,
    };
    if too_old {
        let message = format!(
            "`{name}` needs shep {minimum} or newer, and this is shep {this}, so `{name}` may \
             not work until you upgrade shep"
        );
        streams.aside(DOG_SHEP_TOO_OLD_NOTICE, &message);
    }
}
