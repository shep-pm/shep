//! A `cargo` row's prebuilt binary: the URL built from the row and the
//! hash the index pins for it.

use super::*;

/// The triple this binary was compiled for, set by `build.rs`.
pub const BUILD_TARGET: &str = env!("SHEP_BUILD_TARGET");

/// A prebuilt binary the index approves for one target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prebuilt {
    /// `<repo>/releases/download/v<version>/<package>-<version>-<target>`,
    /// with `.exe` appended on Windows. Built, never read from the index.
    pub url: String,
    /// The lowercase hex sha256 the download must hash to.
    pub sha256: String,
}

impl AvailableDog {
    /// Whether this is a `cargo` row with no approved version, which is
    /// listed but cannot be installed.
    pub fn is_unversioned_cargo(&self) -> bool {
        matches!(&self.source, DogSourceKind::Cargo { version: None, .. })
    }

    /// Whether `shep install` can build this row: a `cargo` row with an
    /// approved version.
    pub fn is_installable(&self) -> bool {
        matches!(
            &self.source,
            DogSourceKind::Cargo {
                version: Some(_),
                ..
            }
        )
    }

    /// The prebuilt binary for the triple this binary was compiled for, or
    /// `None` when the index hashes none for it.
    pub fn prebuilt(&self) -> Option<Prebuilt> {
        self.prebuilt_for(BUILD_TARGET)
    }

    /// The prebuilt binary for `target`, which must match a hashed triple
    /// exactly. `None` for a row with no version, no hash for `target`, or
    /// a package or version that cannot sit in a URL path.
    pub fn prebuilt_for(&self, target: &str) -> Option<Prebuilt> {
        let DogSourceKind::Cargo {
            version: Some(version),
            sha256,
        } = &self.source
        else {
            return None;
        };
        let sha256 = sha256.get(target)?;
        if !is_url_segment(&self.package) || !is_url_segment(version) {
            return None;
        }
        let repo = self.repo.trim_end_matches('/');
        let package = &self.package;
        let exe = if target.contains("windows") {
            ".exe"
        } else {
            ""
        };
        Some(Prebuilt {
            url: format!("{repo}/releases/download/v{version}/{package}-{version}-{target}{exe}"),
            sha256: sha256.clone(),
        })
    }
}

/// Whether `text` is safe to splice into a URL path: crate-name and semver
/// characters only.
fn is_url_segment(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn cargo_dog(version: Option<&str>, hashes: &[(&str, &str)]) -> AvailableDog {
        AvailableDog {
            name: "Bella".to_owned(),
            package: "shep-log-rotate".to_owned(),
            adopt_as: "log-rotate".to_owned(),
            description: "Rotates logs.".to_owned(),
            repo: "https://github.com/shep-pm/shep-log-rotate".to_owned(),
            license: "MIT".to_owned(),
            category: "logs".to_owned(),
            permissions: None,
            needs: Vec::new(),
            source: DogSourceKind::Cargo {
                version: version.map(str::to_owned),
                sha256: hashes
                    .iter()
                    .map(|(target, hash)| ((*target).to_owned(), (*hash).to_owned()))
                    .collect(),
            },
        }
    }

    #[test]
    fn a_hash_for_the_build_target_yields_the_built_url_and_that_hash() {
        let dog = cargo_dog(Some("0.2.4"), &[(BUILD_TARGET, HASH)]);

        let prebuilt = dog.prebuilt().expect("the build target is hashed");

        let exe = if BUILD_TARGET.contains("windows") {
            ".exe"
        } else {
            ""
        };
        assert_eq!(
            prebuilt.url,
            format!(
                "https://github.com/shep-pm/shep-log-rotate/releases/download/v0.2.4/\
                 shep-log-rotate-0.2.4-{BUILD_TARGET}{exe}"
            )
        );
        assert_eq!(prebuilt.sha256, HASH);
    }

    #[test]
    fn a_row_hashing_only_other_targets_yields_nothing() {
        let dog = cargo_dog(
            Some("0.2.4"),
            &[
                ("x86_64-unknown-linux-gnu", HASH),
                ("aarch64-apple-darwin", HASH),
            ],
        );

        assert_eq!(dog.prebuilt_for("x86_64-unknown-linux-musl"), None);
        assert_eq!(dog.prebuilt_for("x86_64-unknown-linux"), None);
    }

    #[test]
    fn a_windows_target_gets_an_exe_suffix_and_a_unix_one_does_not() {
        let dog = cargo_dog(
            Some("0.2.4"),
            &[
                ("x86_64-pc-windows-msvc", HASH),
                ("aarch64-apple-darwin", HASH),
            ],
        );

        let windows = dog.prebuilt_for("x86_64-pc-windows-msvc").expect("hashed");
        assert!(
            windows
                .url
                .ends_with("/shep-log-rotate-0.2.4-x86_64-pc-windows-msvc.exe")
        );
        let mac = dog.prebuilt_for("aarch64-apple-darwin").expect("hashed");
        assert!(
            mac.url
                .ends_with("/shep-log-rotate-0.2.4-aarch64-apple-darwin")
        );
    }

    #[test]
    fn a_row_with_hashes_but_no_version_yields_nothing() {
        let dog = cargo_dog(None, &[("aarch64-apple-darwin", HASH)]);

        assert_eq!(dog.prebuilt_for("aarch64-apple-darwin"), None);
    }

    #[test]
    fn a_version_that_could_rewrite_the_url_path_yields_nothing() {
        let dog = cargo_dog(Some("1.0.0/../../x"), &[("aarch64-apple-darwin", HASH)]);

        assert_eq!(dog.prebuilt_for("aarch64-apple-darwin"), None);
    }

    #[test]
    fn a_repo_with_a_trailing_slash_builds_the_same_url() {
        let mut dog = cargo_dog(Some("0.2.4"), &[("aarch64-apple-darwin", HASH)]);
        let plain = dog.prebuilt_for("aarch64-apple-darwin");
        dog.repo.push('/');

        assert_eq!(dog.prebuilt_for("aarch64-apple-darwin"), plain);
    }

    #[test]
    fn only_a_cargo_row_with_no_version_is_unversioned() {
        assert!(cargo_dog(None, &[]).is_unversioned_cargo());
        assert!(!cargo_dog(Some("0.2.4"), &[]).is_unversioned_cargo());
    }
}
