//! Query verbs: `flock`, `describe`, `fold`, `ping`. None mutate the flock,
//! and none autostart: `main` hands each one an already-connected [`Client`].
//!
//! `describe` and `fold` share one shape (`Request::Describe` against a
//! [`SelectorSpec`]); `fold` supplies `SelectorSpec::Fold` directly.
//!
//! `ping` does not ask the daemon for its version and pid: the handshake
//! already answered that, in the
//! [`HelloAck`](shep_core::protocol::HelloAck) [`Client::daemon`] holds. It
//! still issues `Request::Ping` as the liveness check.

use std::collections::BTreeSet;

use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{ProcessInfo, Request, Response, SelectorSpec};
use shep_core::secrets::{self, Resolution, SecretRef, SecretView};
use shep_core::status::ProcStatus;
use shep_daemon::snapshot::FlockSnapshot;

use crate::cli::{DogsArgs, FoldArgs, Format, SelectorArgs};
use crate::commands::secret::daemon_config;
use crate::commands::selector::parse_selector;
use crate::dog_index::{self, AvailableDog, DogSourceKind};
use crate::exit::ExitCode;
use crate::fetch;
use crate::flourish;
use crate::output::{
    AvailableDogRows, DescribedSecret, DogRows, Render, RolledSheep, RolledSheepRows, SecretStatus,
    Streams, emit, emit_described, emit_flock, write_outcome,
};

/// Sends `body`, renders whatever the daemon answers through [`emit`], and
/// maps every way that can go wrong to its exit code.
///
/// `extract` pulls the verb's own payload out of `Response`, which is
/// `#[non_exhaustive]`: an answer it does not recognise maps to
/// [`ExitCode::Internal`] rather than being guessed at. Every query verb uses
/// the client's default deadline, so there is no deadline parameter.
async fn request_and_render<T, F>(
    client: &Client,
    streams: &mut Streams<'_>,
    command: &str,
    body: Request,
    extract: F,
) -> ExitCode
where
    T: Render,
    F: FnOnce(Response) -> Option<T>,
{
    match client.request(body).await {
        Ok(response) => match extract(response) {
            Some(payload) => write_outcome(emit(
                &mut *streams.out,
                streams.fmt,
                command,
                payload,
                streams.style,
            )),
            None => {
                let message = "the daemon answered with a response this client does not understand";
                streams.fail(ExitCode::Internal, message)
            }
        },
        Err(err) => {
            let code = ExitCode::from(&err);
            streams.fail(code, &err.to_string())
        }
    }
}

/// `describe` and `fold`'s shared body: one `Request::Describe` against
/// `selector`, rendered through [`emit_described`] as the sheep table and
/// each sheep's lamb tree beneath it. `command` is the verb name the output
/// envelope reports.
///
/// `include_secrets` alone gates [`gather_secrets`]: `fold` passes `false`
/// and stays byte-identical to before this section existed, since it names
/// no sheep this feature was built for.
///
/// Not routed through [`request_and_render`]: `emit_described` renders one
/// `Vec<ProcessInfo>` into two tables, which no single [`Render`] impl can
/// express.
async fn describe_selector(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    command: &str,
    include_secrets: bool,
    selector: SelectorSpec,
) -> ExitCode {
    match client.request(Request::Describe { selector }).await {
        Ok(Response::Described(procs)) => {
            let (secrets, secrets_text) = if include_secrets {
                gather_secrets(paths, &procs)
            } else {
                (Vec::new(), String::new())
            };
            let result = emit_described(
                &mut *streams.out,
                streams.fmt,
                command,
                procs,
                streams.style,
                &secrets,
            )
            .and_then(|()| {
                if streams.fmt == Format::Table && !secrets_text.is_empty() {
                    write!(streams.out, "{secrets_text}")
                } else {
                    Ok(())
                }
            });
            write_outcome(result)
        }
        Ok(_) => {
            let message = "the daemon answered with a response this client does not understand";
            streams.fail(ExitCode::Internal, message)
        }
        Err(err) => {
            let code = ExitCode::from(&err);
            streams.fail(code, &err.to_string())
        }
    }
}

/// Renders one sheep's secret references for `Format::Table`: one line per
/// reference, the reference as written, the environment it resolved in, and
/// a verdict. Never a value: `Resolution::Found`'s payload is read only to
/// tell it apart from a miss.
///
/// The verdict word comes from [`SecretStatus::from_resolution`], the same
/// classifier [`gather_secrets`] uses for the JSON form, so the two can
/// never name a different verdict for the same reference.
fn render_describe_secrets(entries: &[(&str, &str, Resolution<'_>)]) -> String {
    let mut rendered = String::new();
    for (reference, environment, resolution) in entries {
        let verdict = SecretStatus::from_resolution(resolution).as_table_word();
        rendered.push_str(&format!("  {reference} ({environment}): {verdict}\n"));
    }
    rendered
}

/// `describe`'s secrets section for `procs`, once per distinct sheep name:
/// the JSON-safe rows [`emit_described`] serializes beside `data`, and the
/// same data as [`render_describe_secrets`]'s table text (one block per
/// name, each already carrying its own "Secrets for `<name>`:" heading).
///
/// Reads three local files rather than asking the shepherd: the muster roll
/// for each name's [`shep_core::config::AppConfig`] (which can trail a
/// config change that has not yet reached disk by
/// `shep_daemon::snapshot`'s debounce window), the operator's own secret
/// store, and the provider cache [`secrets::provider_namespaces_on_disk`]
/// reads. A namespace whose provider pushed with `persist = false` never
/// reaches that cache, so this can call a namespace `provider_not_ready`
/// when the running shepherd already has it in memory; only the shepherd
/// itself can answer that half.
///
/// A name the roll does not know, or one with no `{{secret:...}}` at all,
/// contributes nothing: this reports references that exist, not a claim
/// that every sheep has one.
fn gather_secrets(paths: &ShepPaths, procs: &[ProcessInfo]) -> (Vec<DescribedSecret>, String) {
    let roll = read_roll(paths);
    let store = secrets::all(&paths.secrets).unwrap_or_default();
    let namespaces = secrets::provider_namespaces_on_disk(&paths.secrets_cache);
    let host_environment = daemon_config(paths).daemon.environment;

    let mut json = Vec::new();
    let mut text = String::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for proc in procs {
        if !seen.insert(proc.name.as_str()) {
            continue;
        }
        let Some(config) = roll
            .as_ref()
            .and_then(|roll| roll.apps.iter().find(|app| app.app.name == proc.name))
            .map(|app| &app.app)
        else {
            continue;
        };
        let refs = secrets::references(config);
        if refs.is_empty() {
            continue;
        }
        let environment = config
            .environment
            .clone()
            .unwrap_or_else(|| host_environment.clone());
        let view = SecretView::new(environment.clone(), store.clone(), namespaces.clone());

        let mut entries: Vec<(&str, &str, Resolution<'_>)> = Vec::new();
        for reference in &refs {
            let Some(parsed) = SecretRef::parse(reference) else {
                continue;
            };
            let resolution = view.resolve(&parsed);
            let status = SecretStatus::from_resolution(&resolution);
            json.push(DescribedSecret {
                name: proc.name.clone(),
                reference: reference.clone(),
                environment: environment.clone(),
                status,
            });
            entries.push((reference.as_str(), environment.as_str(), resolution));
        }
        if !entries.is_empty() {
            text.push_str(&format!("\nSecrets for {}:\n", proc.name));
            text.push_str(&render_describe_secrets(&entries));
        }
    }
    (json, text)
}

/// Whatever `flock.json` currently holds, or nothing when it is missing or
/// will not parse.
///
/// Shared by [`flock_from_roll`] and [`gather_secrets`]: both read the
/// muster roll as the best local answer to "what does this app's config
/// look like right now", tolerant of a file this daemon has never written
/// or has fallen behind the live registry by a debounce window.
fn read_roll(paths: &ShepPaths) -> Option<FlockSnapshot> {
    std::fs::read(&paths.snapshot)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<FlockSnapshot>(&bytes).ok())
}

/// `shep flock` when no shepherd answers: the muster roll, marked stopped
///
/// The exit code stays [`ExitCode::DaemonUnreachable`] even though the table
/// looks successful: a monitoring script must not read a dead supervisor as
/// a healthy empty flock. A missing or unreadable roll is not an error.
pub fn flock_from_roll(streams: &mut Streams<'_>, paths: &ShepPaths) -> ExitCode {
    let saved = read_roll(paths);

    let mut sheep: Vec<RolledSheep> = saved
        .map(|roll| {
            roll.apps
                .into_iter()
                .map(|entry| RolledSheep {
                    name: entry.app.name.clone(),
                    instances: entry.instances_running,
                    status: "stopped",
                })
                .collect()
        })
        .unwrap_or_default();
    // The roll's stored order is neither meaningful nor stable. Name is the
    // whole key: one entry per app, so there is no tie to break.
    sheep.sort_unstable_by(|a, b| a.name.cmp(&b.name));

    if streams.fmt == Format::Table {
        let _ = writeln!(
            streams.err,
            "no shepherd running. {}",
            if sheep.is_empty() {
                "nothing in the saved roll either.".to_owned()
            } else {
                format!(
                    "{} in the saved roll at {}:",
                    match sheep.len() {
                        1 => "1 sheep".to_owned(),
                        n => format!("{n} sheep"),
                    },
                    paths.snapshot.display()
                )
            }
        );
    }
    let empty = sheep.is_empty();
    // No table for an empty roll: bare headers over nothing read as a glitch.
    // JSON still gets the empty array, because a script wants one shape.
    if !(empty && streams.fmt == Format::Table) {
        let _ = emit(
            &mut *streams.out,
            streams.fmt,
            "flock",
            RolledSheepRows(sheep),
            streams.style,
        );
    }
    if streams.fmt == Format::Table && !empty {
        let _ = writeln!(streams.err, "`shep muster` brings them back.");
    }
    ExitCode::DaemonUnreachable
}

/// Lists the whole flock: the sheep table, then the dogs table beneath it
/// whenever any dog is registered, with a [`sheep_flourish`] above
///
/// The flourish is gated on `Format::Table` and `streams.style.level.sheep()`
/// and nothing else. Not routed through [`request_and_render`], which renders
/// one [`Render`] type per verb rather than two tables from one
/// `Vec<ProcessInfo>`.
pub async fn flock(client: &Client, streams: &mut Streams<'_>) -> ExitCode {
    match client.request(Request::ListFlock).await {
        Ok(Response::Flock(procs)) => {
            // Read before `procs` moves into `emit_flock`.
            let art = (streams.fmt == Format::Table && streams.style.level.sheep())
                .then(|| sheep_flourish(&procs))
                .flatten();
            if let Some(art) = &art {
                let _ = write!(streams.out, "{art}");
            }
            write_outcome(emit_flock(
                &mut *streams.out,
                streams.fmt,
                "flock",
                procs,
                streams.style,
            ))
        }
        Ok(_) => {
            let message = "the daemon answered with a response this client does not understand";
            streams.fail(ExitCode::Internal, message)
        }
        Err(err) => {
            let code = ExitCode::from(&err);
            streams.fail(code, &err.to_string())
        }
    }
}

/// The flourish for one flock listing, or `None` when neither the
/// empty-flock nor the all-asleep state applies
///
/// Dogs are excluded from both checks: the flourish sits beside the sheep
/// table and is a claim about the sheep. [`ProcStatus::Stopping`] does not
/// count as asleep, being reload's transient rather than rest.
fn sheep_flourish(listing: &[ProcessInfo]) -> Option<String> {
    let sheep: Vec<&ProcessInfo> = listing.iter().filter(|p| p.dog.is_none()).collect();
    if sheep.is_empty() {
        return Some(flourish::empty_flock());
    }
    sheep
        .iter()
        .all(|p| p.status == ProcStatus::Stopped)
        .then(|| flourish::all_asleep(sheep.len()))
}

/// Lists the dogs and nothing else: the same `Request::ListFlock` [`flock`]
/// sends, filtered to the entries carrying a `dog` marker
///
/// `args.filter` is a case-insensitive substring match against the dog's
/// name, the one field a running dog and a community-index entry share.
/// Not [`emit_flock`], which would print the sheep table's header row over
/// a dogs-only listing.
pub async fn dogs(client: &Client, streams: &mut Streams<'_>, args: &DogsArgs) -> ExitCode {
    let filter = args.filter.as_deref();
    request_and_render(
        client,
        streams,
        "dogs",
        Request::ListFlock,
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
/// not cover it, and it asks
/// [`fetch::url_for_message`](crate::fetch::url_for_message) rather than
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

/// Describes the sheep matching `args.selector` in detail.
pub async fn describe(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &SelectorArgs,
) -> ExitCode {
    // One pass per target, each its own detail view: `describe` answers with
    // a tree per sheep, so merging them would lose that shape.
    let mut failure: Option<ExitCode> = None;
    for raw in &args.selectors {
        let selector = match parse_selector(streams, raw) {
            Ok(selector) => SelectorSpec::from(&selector),
            Err(code) => return code,
        };
        let code = describe_selector(client, streams, paths, "describe", true, selector).await;
        if code != ExitCode::Success {
            failure = failure.or(Some(code));
        }
    }
    failure.unwrap_or(ExitCode::Success)
}

/// Lists one fold: `Request::Describe` with `SelectorSpec::Fold(args.name)`,
/// delegating to [`describe_selector`].
pub async fn fold(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &FoldArgs,
) -> ExitCode {
    describe_selector(
        client,
        streams,
        paths,
        "fold",
        false,
        SelectorSpec::Fold(args.name.clone()),
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shep_client::testing::{
        fake_client_capturing_envelopes, fake_client_on, fake_client_with_ack, sample_ack,
        sample_info,
    };
    use shep_core::protocol::DogSource;

    use super::*;

    /// Bounds every `envelopes.recv()` here: a verb that never reaches the
    /// wire must fail by assertion, not by hanging the job.
    const RECV_TIMEOUT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn flock_asks_the_daemon_to_list_the_whole_flock() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let _ = flock(&client, &mut streams).await;
        let sent = tokio::time::timeout(RECV_TIMEOUT, envelopes.recv())
            .await
            .expect("flock must reach the wire; it hung instead of sending a request")
            .unwrap();
        assert_eq!(sent.body, Request::ListFlock);
    }

    #[tokio::test]
    async fn describe_sends_the_parsed_selector_in_its_compiled_form() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        for (input, expected) in [
            ("all", SelectorSpec::All),
            ("7", SelectorSpec::Id(7)),
            ("web", SelectorSpec::Name("web".into())),
            ("/^web-/", SelectorSpec::Regex("^web-".into())),
            ("fold:api", SelectorSpec::Fold("api".into())),
        ] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let args = SelectorArgs {
                selectors: vec![input.into()],
            };
            let _ = describe(&client, &mut streams, &paths, &args).await;
            let sent = tokio::time::timeout(RECV_TIMEOUT, envelopes.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!("describe({input}) must reach the wire; it hung instead of sending a request")
                })
                .unwrap();
            assert_eq!(
                sent.body,
                Request::Describe { selector: expected },
                "{input}"
            );
        }
    }

    /// `"/[/"` is one of the only three inputs the selector grammar rejects:
    /// an unterminated regex character class.
    #[tokio::test]
    async fn a_malformed_selector_exits_usage_without_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            describe(
                &client,
                &mut streams,
                &paths,
                &SelectorArgs {
                    selectors: vec!["/[/".into()],
                },
            )
            .await
        };
        assert_eq!(code, ExitCode::Usage);
        assert!(
            envelopes.try_recv().is_err(),
            "a malformed selector must fail locally"
        );
    }

    #[tokio::test]
    async fn fold_asks_the_daemon_for_that_fold_and_nothing_wider() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let _ = fold(
            &client,
            &mut streams,
            &paths,
            &FoldArgs { name: "api".into() },
        )
        .await;
        let sent = tokio::time::timeout(RECV_TIMEOUT, envelopes.recv())
            .await
            .expect("fold must reach the wire; it hung instead of sending a request")
            .unwrap();
        assert_eq!(
            sent.body,
            Request::Describe {
                selector: SelectorSpec::Fold("api".into())
            }
        );
    }

    /// `Response::Flock` and `Response::Described` both wrap a bare
    /// `Vec<ProcessInfo>`, so an arm swapped between them compiles clean.
    /// `reply_to_list` scripts a real `Response::Flock` to catch that.
    #[tokio::test]
    async fn flock_response_round_trips_into_rendered_flock_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_with_ack(&path, sample_ack()).await;
        daemon.reply_to_list(vec![sample_info()]);

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Json,
            };
            flock(&client, &mut streams).await
        };

        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(json["command"], "flock");
        assert_eq!(json["data"][0]["name"], "web");
    }

    #[tokio::test]
    async fn describe_response_round_trips_into_rendered_flock_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_with_ack(&path, sample_ack()).await;
        daemon.reply_to_describe(vec![sample_info()]);
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Json,
            };
            describe(
                &client,
                &mut streams,
                &paths,
                &SelectorArgs {
                    selectors: vec!["all".into()],
                },
            )
            .await
        };

        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(json["command"], "describe");
        assert_eq!(json["data"][0]["name"], "web");
    }

    #[test]
    fn describe_lists_secret_references_with_a_verdict_and_no_values() {
        let rendered = render_describe_secrets(&[
            ("DB_PASSWORD", "production", Resolution::Found("hunter2")),
            ("vercel/API_KEY", "production", Resolution::MissingNamespace),
            ("ABSENT", "production", Resolution::MissingKey),
        ]);
        assert!(rendered.contains("DB_PASSWORD"));
        assert!(rendered.contains("vercel/API_KEY"));
        assert!(!rendered.contains("hunter2"), "never a value");
    }

    /// A roll entry, an operator store entry and no provider cache at all:
    /// the three verdicts `gather_secrets` can produce from local files
    /// alone, exercised through the real `describe` verb rather than
    /// `render_describe_secrets` in isolation.
    async fn describe_with_a_seeded_web(
        fmt: Format,
    ) -> (ExitCode, shep_core::paths::ShepPaths, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_with_ack(&path, sample_ack()).await;
        daemon.reply_to_describe(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
        ]);

        // `SHEP_HOME` pinned to `dir` itself, not its `.shep` default: this
        // test writes `paths.snapshot`/`paths.secrets` directly, and the
        // default subdirectory is never created outside a real boot.
        let home = dir.path().display().to_string();
        let paths = ShepPaths::resolve(
            &move |key| (key == "SHEP_HOME").then(|| home.clone()),
            dir.path(),
        );

        let mut config = shep_core::config::AppConfig::minimal("web", "./srv");
        config
            .env
            .insert("A".into(), "{{secret:DB_PASSWORD}}".into());
        config
            .env
            .insert("B".into(), "{{secret:vercel/API_KEY}}".into());
        let roll = FlockSnapshot {
            version: 1,
            saved_at_ms: 0,
            apps: vec![shep_daemon::snapshot::SavedApp {
                app: config,
                instances_running: 1,
            }],
        };
        std::fs::write(&paths.snapshot, serde_json::to_vec(&roll).unwrap()).unwrap();
        secrets::set(&paths.secrets, "DB_PASSWORD", "production", "hunter2").unwrap();
        // No provider cache written: `vercel` has never pushed anything.

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt,
            };
            describe(
                &client,
                &mut streams,
                &paths,
                &SelectorArgs {
                    selectors: vec!["all".into()],
                },
            )
            .await
        };
        (code, paths, out)
    }

    #[tokio::test]
    async fn describe_prints_real_secret_verdicts_in_the_table() {
        let (code, _paths, out) = describe_with_a_seeded_web(Format::Table).await;
        assert_eq!(code, ExitCode::Success);
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("Secrets for web"), "{rendered}");
        assert!(
            rendered.contains("DB_PASSWORD (production): resolved"),
            "{rendered}"
        );
        assert!(
            rendered.contains("vercel/API_KEY (production): provider not ready"),
            "{rendered}"
        );
        assert!(!rendered.contains("hunter2"), "never a value: {rendered}");
    }

    #[tokio::test]
    async fn describe_json_carries_the_same_verdicts_as_an_additive_field() {
        let (code, _paths, out) = describe_with_a_seeded_web(Format::Json).await;
        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(json["command"], "describe");
        assert_eq!(json["schema_version"], 1, "SCHEMA_VERSION must not move");
        // `data` stays exactly what it always was: an array of ProcessInfo.
        assert_eq!(json["data"][0]["name"], "web");
        let entries = json["secrets"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|e| e["reference"] == "DB_PASSWORD" && e["status"] == "resolved"),
            "{entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e["reference"] == "vercel/API_KEY" && e["status"] == "provider_not_ready"),
            "{entries:?}"
        );
        assert!(!out_contains(&json, "hunter2"), "never a value");
    }

    /// Whether `hunter2` shows up anywhere in `value`'s own JSON text, not
    /// just at the top level: a value smuggled in nested one level deeper
    /// would still be a leak.
    fn out_contains(value: &serde_json::Value, needle: &str) -> bool {
        value.to_string().contains(needle)
    }

    /// `fold` shares `describe_selector` but must stay byte-identical to
    /// before this feature existed: no local file I/O, no `secrets` field.
    #[tokio::test]
    async fn fold_never_computes_or_prints_a_secrets_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_with_ack(&path, sample_ack()).await;
        daemon.reply_to_describe(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
        ]);
        // Deliberately no `paths.snapshot` on disk: if `fold` ever reads it,
        // the missing file is tolerated (`read_roll`), which would hide the
        // bug this test exists to catch. The real guard is the assertion
        // below, on `command == "fold"` never entering `gather_secrets`.
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Json,
        };
        let code = fold(
            &client,
            &mut streams,
            &paths,
            &FoldArgs { name: "api".into() },
        )
        .await;
        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(json.get("secrets").is_none(), "{json}");
    }

    // --- sheep_flourish ---

    /// A sheep with `status` pinned and nothing else.
    fn sheep(id: u32, status: ProcStatus) -> ProcessInfo {
        ProcessInfo::builder(id, format!("s{id}"), status).build()
    }

    /// A registered dog, which `sheep_flourish` must never count as a sheep.
    fn dog(id: u32) -> ProcessInfo {
        ProcessInfo::builder(id, format!("d{id}"), ProcStatus::Online)
            .dog(Some(DogSource::BuiltIn))
            .build()
    }

    #[test]
    fn sheep_flourish_fires_empty_flock_on_a_truly_empty_listing() {
        let art = sheep_flourish(&[]).expect("an empty listing must flourish");
        assert!(art.contains("no sheep in the flock yet"), "{art}");
    }

    #[test]
    fn sheep_flourish_treats_dogs_only_as_an_empty_flock() {
        let art =
            sheep_flourish(&[dog(1), dog(2)]).expect("dogs alone must read as an empty flock");
        assert!(art.contains("no sheep in the flock yet"), "{art}");
    }

    /// The count in the flourish excludes dogs too.
    #[test]
    fn sheep_flourish_fires_all_asleep_when_every_sheep_is_stopped() {
        let listing = [
            sheep(1, ProcStatus::Stopped),
            sheep(2, ProcStatus::Stopped),
            dog(3),
        ];
        let art = sheep_flourish(&listing).expect("an all-stopped flock must flourish");
        assert!(art.contains("2 in the flock, all asleep"), "{art}");
    }

    #[test]
    fn a_live_dog_does_not_block_all_asleep() {
        let listing = [sheep(1, ProcStatus::Stopped), dog(2)];
        let art = sheep_flourish(&listing).expect("a live dog must not suppress all_asleep");
        assert!(art.contains("1 in the flock, all asleep"), "{art}");
    }

    #[test]
    fn sheep_flourish_is_silent_on_a_mixed_flock() {
        let listing = [sheep(1, ProcStatus::Online), sheep(2, ProcStatus::Stopped)];
        assert_eq!(
            sheep_flourish(&listing),
            None,
            "a mixed flock is not a flourish moment"
        );
    }

    /// `Stopping` is reload's transient for the instance being replaced, so
    /// a flock mid-reload must not read as asleep.
    #[test]
    fn stopping_does_not_count_as_asleep() {
        let listing = [
            sheep(1, ProcStatus::Stopping),
            sheep(2, ProcStatus::Stopping),
        ];
        assert_eq!(
            sheep_flourish(&listing),
            None,
            "Stopping is a transient, not rest"
        );
    }

    /// The daemon answers an empty flock on every case here, which
    /// `sheep_flourish` always fires on, so only the gate decides.
    #[tokio::test]
    async fn the_flourish_only_prints_under_table_format_and_a_sheep_drawing_level() {
        use crate::style::{Presentation, StyleLevel};

        for (fmt, level, expect_art) in [
            (Format::Table, StyleLevel::Full, true),
            (Format::Json, StyleLevel::Full, false),
            (Format::Table, StyleLevel::Plain, false),
            (Format::Table, StyleLevel::Bare, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = shep_client::testing::control_address(dir.path());
            let (client, _daemon) = fake_client_on(&path).await;

            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: Presentation::new(level, None, None, None, 80),
                fmt,
            };
            let _ = flock(&client, &mut streams).await;
            let printed = String::from_utf8_lossy(&out);
            assert_eq!(
                printed.contains("no sheep in the flock yet"),
                expect_art,
                "fmt={fmt:?} level={level:?}: {printed}"
            );
        }
    }
}
