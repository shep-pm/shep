use crate::cli::{DogsArgs, Format};
use crate::commands::rpc::request_and_render;
use crate::dog_index::{self, AvailableDog, DogSourceKind};
use crate::exit::ExitCode;
use crate::fetch;
use crate::output::{AvailableDogRows, DogRows, Streams, emit, write_outcome};
use shep_client::Client;
use shep_core::protocol::{Request, Response};

/// Lists the dogs and nothing else: the same `Request::ListFlock` [`flock`](crate::commands::query::flock_display::flock)
/// sends, filtered to the entries carrying a `testing::dog` marker
///
/// `args.filter` is a case-insensitive substring match against the dog's
/// name, the one field a running dog and a community-index entry share.
/// Not [`emit_flock`](crate::output::emit_flock), which would print the sheep table's header row over
/// a dogs-only listing.
pub async fn dogs(client: &Client, streams: &mut Streams<'_>, args: &DogsArgs) -> ExitCode {
    let filter = args.filter.as_deref();
    request_and_render(
        client,
        streams,
        "dogs",
        Request::ListFlock,
        None,
        |response| match response {
            Response::Flock(procs) => Some(DogRows(
                procs
                    .into_iter()
                    .filter(|p| p.dog.is_some())
                    .filter(|p| filter.is_none_or(|f| matches_filter(f, &[&p.name])))
                    .collect(),
            )),
            _ => None,
        },
    )
    .await
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
/// Reaches no [`Client`], so it answers with no shepherd running. Under
/// `Format::Table` a filter matching one dog prints its detail view, and a
/// filter matching nothing prints `no dog matches "<filter>"` and still
/// exits [`ExitCode::Success`]. `--format json` always renders the array.
///
/// # Errors reaching the operator
/// A failure to read or parse the index names the URL and exits
/// [`ExitCode::Failure`]: [`dog_index::IndexError`] carries it on one
/// variant only.
///
/// The one URL not named is one holding an `@`. A dog index URL is a
/// public location, which is why this quotes it at all, but
/// `SHEP_DOG_INDEX` is an operator's own string and nothing stops a
/// password reaching it. This message is built here rather than by
/// [`dog_index::IndexError`], so the refusals inside [`crate::fetch`] do
/// not cover it, and it asks [`fetch::url_for_message`] rather than
/// deciding for itself: an earlier version asked
/// `url_carries_credentials` instead and printed urls that `parse_url`
/// had just withheld.
pub async fn available_dogs(streams: &mut Streams<'_>, args: &DogsArgs) -> ExitCode {
    let url = dog_index::index_url();
    let index = match dog_index::fetch_index(&url).await {
        Ok(index) => index,
        Err(err) => {
            let message = format!(
                "reading the dog index from {}: {err}",
                fetch::url_for_message(&url)
            );
            return streams.fail(ExitCode::Failure, &message);
        }
    };
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
    code
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
fn render_detail(out: &mut dyn std::io::Write, dog: &AvailableDog) -> std::io::Result<()> {
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

/// The `$ ...` line [`render_detail`] prints for how to build `source`'s
/// binary. [`DogSourceKind::Manual`] carries prose instead of a command, so
/// it prints with no `$`, just the two-space indent the command lines share.
fn install_line(source: &DogSourceKind, package: &str) -> String {
    match source {
        DogSourceKind::Cargo {
            version: Some(version),
        } => {
            format!("  $ cargo install {package} --version {version}")
        }
        DogSourceKind::Cargo { version: None } => format!("  $ cargo install {package}"),
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
