use super::access_marks::mark_access;
use crate::cli::{DogsArgs, Format};
use crate::commands::rpc::typed_request_error;
use crate::dog_index::{self, AvailableDog, DogSourceKind, Origin};
use crate::exit::ExitCode;
use crate::fetch;
use crate::output::{AvailableDogRows, DogRows, Streams, emit, write_outcome};
use shep_client::Client;
use shep_core::paths::ShepPaths;

/// Lists the dogs and nothing else, marked with what `shep.toml` records of their access: the same `Request::ListFlock` [`flock`](crate::commands::query::flock_display::flock)
/// sends, filtered to the entries carrying a `testing::dog` marker
///
/// `args.filter` is a case-insensitive substring match against the dog's
/// name, the one field a running dog and a community-index entry share.
/// Not [`emit_flock`](crate::output::emit_flock), which would print the sheep table's header row over
/// a dogs-only listing.
pub async fn dogs(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &DogsArgs,
) -> ExitCode {
    let filter = args.filter.as_deref();
    match client.list_flock().await {
        Ok(mut procs) => {
            mark_access(&mut procs, paths);
            let rows = DogRows(
                procs
                    .into_iter()
                    .filter(|p| p.dog.is_some())
                    .filter(|p| filter.is_none_or(|f| matches_filter(f, &[&p.name])))
                    .collect(),
            );
            write_outcome(emit(
                &mut *streams.out,
                streams.fmt,
                "dogs",
                rows,
                streams.style,
            ))
        }
        Err(err) => typed_request_error(streams, &err),
    }
}

/// Whether `filter` matches any of `haystacks`, case-insensitively. Shared
/// by [`dogs`] (name alone) and [`available_dogs`] (name, package and
/// description).
fn matches_filter(filter: &str, haystacks: &[&str]) -> bool {
    let filter = filter.to_lowercase();
    haystacks.iter().any(|h| h.to_lowercase().contains(&filter))
}

/// Lists the dogs published in the community index: `shep dogs --available`
///
/// Reaches no [`Client`]. Reads the index through its one-day cache, which
/// `--refresh` skips; a failed fetch lists the cached copy and its age.
/// With no copy, a failed read names the URL (never one holding an `@`) and
/// exits [`ExitCode::Failure`].
pub async fn available_dogs(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &DogsArgs,
) -> ExitCode {
    let url = dog_index::index_url();
    let loaded = match dog_index::cached_index(paths, &url, args.refresh).await {
        Ok(loaded) => loaded,
        Err(err) => {
            let message = format!(
                "reading the dog index from {}: {err}",
                fetch::url_for_message(&url)
            );
            return streams.fail(ExitCode::Failure, &message);
        }
    };
    let origin = loaded.origin;
    let index = loaded.index;
    let (skipped, sanitised) = (index.skipped, index.sanitised);

    let filter = args.filter.as_deref();
    let matched: Vec<AvailableDog> = index
        .dogs
        .into_iter()
        .filter(|dog| {
            filter.is_none_or(|f| matches_filter(f, &[&dog.name, &dog.package, &dog.description]))
        })
        .collect();

    let code = if streams.fmt == Format::Table
        && matched.is_empty()
        && let Some(filter) = filter
    {
        let _ = writeln!(streams.out, "no dog matches {filter:?}");
        ExitCode::Success
    } else if streams.fmt == Format::Table
        && let [only] = matched.as_slice()
    {
        write_outcome(render_detail(&mut *streams.out, only))
    } else {
        write_outcome(emit(
            &mut *streams.out,
            streams.fmt,
            "dogs",
            AvailableDogRows(matched),
            streams.style,
        ))
    };

    note_index_costs(streams, skipped, sanitised);
    note_stale_copy(streams, &url, &origin);
    code
}

/// Says, in one line, that the listing is a cached copy because the fetch
/// failed. Silent for any other [`Origin`].
fn note_stale_copy(streams: &mut Streams<'_>, url: &str, origin: &Origin) {
    if let Origin::Stale { age, cause } = origin {
        streams.aside(
            "dogs_index_stale",
            &format!(
                "could not read the dog index from {}: {cause}; listing the copy cached {} ago",
                fetch::url_for_message(url),
                dog_index::describe_age(*age)
            ),
        );
    }
}

/// The clause both notices below end in: the counts describe the fetched
/// document, not the filtered listing they are printed beside.
const INDEX_WIDE: &str = ", across the whole index rather than this listing";

/// Prints [`dog_index::Index::skipped`]/[`dog_index::Index::sanitised`] as
/// footer notices when either is non-zero, whatever the filter matched.
fn note_index_costs(streams: &mut Streams<'_>, skipped: usize, sanitised: usize) {
    if skipped > 0 {
        streams.aside(
            "dogs_skipped",
            &format!(
                "{skipped} entr{} skipped{INDEX_WIDE}",
                if skipped == 1 { "y" } else { "ies" }
            ),
        );
    }
    if sanitised > 0 {
        streams.aside(
            "dogs_sanitised",
            &format!(
                "{sanitised} entr{} contained control characters{INDEX_WIDE}",
                if sanitised == 1 { "y" } else { "ies" }
            ),
        );
    }
}

/// The lone-match affordance [`available_dogs`] prints for `Format::Table`:
/// full detail on one dog, ending in the two copy-pasteable commands an
/// operator needs to adopt it. Never reached from `--format json`.
///
/// # Errors
/// The underlying write failed.
pub(crate) fn render_detail(
    out: &mut dyn std::io::Write,
    dog: &AvailableDog,
) -> std::io::Result<()> {
    writeln!(out, "{} . {} . {}", dog.name, dog.package, dog.category)?;
    writeln!(out, "{}", dog.description)?;
    writeln!(out, "{} . {}", dog.license, dog.repo)?;
    writeln!(out)?;
    writeln!(out, "{}", install_line(&dog.source, &dog.package))?;
    writeln!(
        out,
        "{}",
        adopt_line(&dog.source, &dog.adopt_as, &dog.package)
    )
}

/// How to build `dog` by hand, as one line with no indent: what `shep install`
/// names when it refuses a row it cannot build, in a format that has no
/// table to print the detail into.
pub(crate) fn install_instruction(dog: &AvailableDog) -> String {
    install_line(&dog.source, &dog.package).trim().to_owned()
}

/// The `$ ...` line [`render_detail`] prints for how to build `source`'s
/// binary. [`DogSourceKind::Manual`] carries prose instead of a command, so
/// it prints with no `$`, just the two-space indent the command lines share.
fn install_line(source: &DogSourceKind, package: &str) -> String {
    match source {
        DogSourceKind::Cargo {
            version: Some(version),
            ..
        } => {
            format!("  $ cargo install {package} --version {version}")
        }
        DogSourceKind::Cargo { version: None, .. } => {
            format!("  not installable: the index approves no version of {package}")
        }
        DogSourceKind::CargoGit { url } => format!("  $ cargo install --git {url}"),
        DogSourceKind::GoInstall { module } => format!("  $ go install {module}@latest"),
        DogSourceKind::Manual { instructions } => format!("  {instructions}"),
    }
}

/// The `$ shep adopt ...` line [`render_detail`] prints
///
/// Built from `adopt_as`, never `name` or `package`: a wrong name ships a
/// command that silently discards the dog's whole config section.
/// [`DogSourceKind::Manual`] has no predictable install path, so its line
/// names the placeholder literally. `--name` is always spelled, since
/// nothing enforces the naming convention on a user-contributed `package`.
fn adopt_line(source: &DogSourceKind, adopt_as: &str, package: &str) -> String {
    match source {
        DogSourceKind::Cargo { .. } | DogSourceKind::CargoGit { .. } => {
            format!("  $ shep adopt ~/.cargo/bin/{package} --name {adopt_as}")
        }
        DogSourceKind::GoInstall { .. } => {
            format!("  $ shep adopt $(go env GOPATH)/bin/{package} --name {adopt_as}")
        }
        DogSourceKind::Manual { .. } => {
            format!("  $ shep adopt <path to the binary> --name {adopt_as}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::*;
    use crate::dog_index::IndexError;
    use crate::fetch::FetchError;

    fn cargo(version: Option<&str>) -> DogSourceKind {
        DogSourceKind::Cargo {
            version: version.map(str::to_owned),
            sha256: BTreeMap::new(),
        }
    }

    #[test]
    fn a_versioned_cargo_row_installs_the_exact_version() {
        assert_eq!(
            install_line(&cargo(Some("0.2.4")), "shep-log-rotate"),
            "  $ cargo install shep-log-rotate --version 0.2.4"
        );
    }

    #[test]
    fn an_unversioned_cargo_row_says_it_is_not_installable_instead_of_a_command() {
        let line = install_line(&cargo(None), "shep-log-rotate");

        assert!(line.contains("not installable"), "{line}");
        assert!(!line.contains("cargo install"), "{line}");
    }

    #[test]
    fn a_stale_copy_is_announced_in_one_line_naming_its_age() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
        let origin = Origin::Stale {
            age: Duration::from_secs(30 * 60 * 60),
            cause: IndexError::Fetch(FetchError::Timeout),
        };

        note_stale_copy(&mut streams, "https://example.test/dogs.json", &origin);

        let printed = String::from_utf8(err).unwrap();
        assert_eq!(printed.lines().count(), 1, "{printed}");
        assert!(printed.contains("30 hours"), "{printed}");
        assert!(
            printed.contains("https://example.test/dogs.json"),
            "{printed}"
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_fetched_or_fresh_cached_listing_prints_no_stale_line() {
        for origin in [Origin::Fetched, Origin::Cache] {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);

            note_stale_copy(&mut streams, "https://example.test/dogs.json", &origin);

            assert!(err.is_empty(), "{origin:?}");
        }
    }
}
