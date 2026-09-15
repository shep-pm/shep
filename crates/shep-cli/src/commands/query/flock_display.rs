use super::secret_inspection::describe_selector;
use super::terminal_fitting::fit_rows;
use crate::cli::{FoldArgs, Format};
use crate::commands::rpc::{client_error, unexpected_response};
use crate::exit::ExitCode;
use crate::flourish;
use crate::host;
use crate::lookout::term;
use crate::output::{RolledSheep, RolledSheepRows, Streams, emit, emit_flock, write_outcome};
use crate::shutdown::Interrupt;
use crate::style::Presentation;
use crossterm::QueueableCommand as _;
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::terminal::{Clear, ClearType};
use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{HostUsage, ProcessInfo, Request, Response, SelectorSpec};
use shep_core::status::ProcStatus;
use shep_daemon::snapshot::FlockSnapshot;
use std::io::{self, Write as _};
use std::time::Duration;

/// Whatever `flock.json` currently holds, or nothing when it is missing or
/// will not parse.
///
/// Shared by [`flock_from_roll`] and [`gather_secrets`](crate::commands::query::secret_inspection::gather_secrets): both read the
/// muster roll as the best local answer to "what does this app's config
/// look like right now", tolerant of a file this daemon has never written
/// or has fallen behind the live registry by a debounce window.
pub(crate) fn read_roll(paths: &ShepPaths) -> Option<FlockSnapshot> {
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
/// and nothing else. Not routed through [`request_and_render`](crate::commands::rpc::request_and_render), which renders
/// one [`crate::output::Render`] type per verb rather than two tables from one
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
            let host = host_usage(client).await;
            if streams.fmt == Format::Table
                && let Some(usage) = host
            {
                // Above the table and separated from it, exactly where a
                // followed listing puts the same line. `Presentation::width`
                // is the terminal's, resolved once at the seam.
                let strip = host::strip(usage, streams.style, streams.style.width);
                let _ = writeln!(streams.out, "{strip}\n");
            }
            write_outcome(emit_flock(
                &mut *streams.out,
                streams.fmt,
                "flock",
                procs,
                host,
                streams.style,
            ))
        }
        Ok(_unrecognised) => unexpected_response(streams),
        Err(err) => client_error(streams, &err),
    }
}

/// What the shepherd says the machine is doing, or `None` where it will not
/// say.
///
/// The two answers inside the `Some` are different claims and a reader
/// renders them differently: a reading, and a platform `sysinfo` cannot read
/// at all.
///
/// The outer `None` is a shepherd that refused the request, which today
/// means one built before `Request::HostUsage` existed. It decodes
/// `Request::Unrecognized` and answers `RpcErrorCode::Unsupported` by name,
/// and the listing then prints no strip: exactly what `shep flock` printed
/// before this existed. Not a warning line, which would fire on every
/// listing for as long as the skew lasts, on the verb an operator types
/// most.
pub(super) async fn host_usage(client: &Client) -> Option<Option<HostUsage>> {
    match client.request(Request::HostUsage).await {
        Ok(Response::HostUsage(usage)) => Some(usage),
        // A shepherd that answered something else, and one that refused.
        Ok(_unrecognised) => None,
        Err(_unsupported) => None,
    }
}

/// The flourish for one flock listing, or `None` when neither the
/// empty-flock nor the all-asleep state applies
///
/// Dogs are excluded from both checks: the flourish sits beside the sheep
/// table and is a claim about the sheep. [`ProcStatus::Stopping`] does not
/// count as asleep, being reload's transient rather than rest.
pub(super) fn sheep_flourish(listing: &[ProcessInfo]) -> Option<String> {
    let sheep: Vec<&ProcessInfo> = listing.iter().filter(|p| p.dog.is_none()).collect();
    if sheep.is_empty() {
        return Some(flourish::empty_flock());
    }
    sheep
        .iter()
        .all(|p| p.status == ProcStatus::Stopped)
        .then(|| flourish::all_asleep(sheep.len()))
}

/// `shep flock --follow`: the same listing, painted over itself every
/// `interval` until the operator interrupts it or the shepherd goes.
///
/// Each redraw is one `Request::ListFlock` rendered into a buffer and then
/// written over the screen in a single write. Rendering before clearing is
/// what keeps the terminal from sitting blank for the length of the round
/// trip. The main screen, not the alternate one, so the last frame is still
/// there afterwards. No flourish, which redrawn every second is noise.
///
/// An interrupt ends the follow at [`ExitCode::Success`], a shepherd that
/// goes away mid-follow ends it carrying that refusal's own code. The two
/// must not read as the same thing.
///
/// Why each of those beat its alternative: `docs/decisions.md`, "Following
/// the flock".
pub(crate) async fn flock_follow(
    client: &Client,
    streams: &mut Streams<'_>,
    interval: Duration,
) -> ExitCode {
    let mut interrupt = match Interrupt::install() {
        Ok(interrupt) => interrupt,
        Err(err) => {
            let message = format!("listening for an interrupt: {err}");
            return streams.fail(ExitCode::Failure, &message);
        }
    };
    // The hook covers a panic, the guard covers every other way out of this
    // function, and `term::restore` is documented idempotent because a panic
    // fires both. The guard shows the cursor and stops there: hiding it is
    // the only change this verb makes to the terminal, where `lookout` also
    // takes raw mode and the alternate screen.
    term::install_panic_hook();
    let _cursor = term::RestoreGuard::with_action(|| {
        let _ = crossterm::execute!(io::stdout(), Show);
    });
    let _ = streams.out.queue(Hide);

    let mut ticker = tokio::time::interval(interval);
    // A shepherd slower to answer than the interval would otherwise bank
    // every tick it missed and redraw them back to back the moment it
    // answered. `Delay` measures the next interval from the redraw that just
    // finished, so a slow shepherd slows the cadence instead of bursting it.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = interrupt.recv() => return ExitCode::Success,
        }
        // The request is raced too, so a shepherd that stops answering does
        // not hold the terminal until it does.
        let listing = tokio::select! {
            listing = client.request(Request::ListFlock) => listing,
            _ = interrupt.recv() => return ExitCode::Success,
        };
        let procs = match listing {
            Ok(Response::Flock(procs)) => procs,
            Ok(_unrecognised) => return unexpected_response(streams),
            Err(err) => return client_error(streams, &err),
        };
        let host = tokio::select! {
            host = host_usage(client) => host,
            _ = interrupt.recv() => return ExitCode::Success,
        };
        // One reading, used twice. Measured before the frame is built
        // rather than after it, because the strip fits itself to the window
        // and a window resized between redraws has to be read again. Two
        // readings would let a resize land between them, fitting the strip
        // to the old width and then trimming the frame to the new one.
        let size = crossterm::terminal::size().ok();
        // A width of zero is not a window. `fit_rows` answers that case for
        // itself, further down, by declining to trim at all.
        let columns = size
            .filter(|&(columns, _)| columns > 0)
            .map_or(streams.style.width, |(columns, _)| usize::from(columns));
        let frame = follow_frame(
            procs,
            streams.style,
            host.map(|usage| host::strip(usage, streams.style, columns)),
        );
        let frame = match size {
            Some((columns, rows)) => fit_rows(&frame, columns, rows),
            // A terminal that will not say how big it is gets the frame
            // whole. Scrolling beats hiding a sheep.
            None => frame,
        };
        // Not discarded. A follow whose terminal has gone (the emulator
        // closed, an ssh session dropped) writes into a dead descriptor
        // forever otherwise, and keeps asking the shepherd for a listing it
        // cannot paint. `write_outcome` keeps a broken pipe at
        // `ExitCode::Success`, which is the reader leaving rather than a
        // failure.
        let painted = paint(streams.out, &frame);
        if painted.is_err() {
            return write_outcome(painted);
        }
    }
}

/// One redraw's four writes, as one `io::Result`.
///
/// Separate so the loop above reads as "paint, and stop if that failed"
/// rather than four discarded results in a row.
pub(super) fn paint(out: &mut dyn io::Write, frame: &str) -> io::Result<()> {
    out.queue(MoveTo(0, 0))?;
    out.queue(Clear(ClearType::FromCursorDown))?;
    write!(out, "{frame}")?;
    out.flush()
}

/// One redraw's worth of text: the host line, then the tables [`flock`]
/// would have printed.
///
/// The host line goes above rather than below so it holds still while the
/// tables under it change length, and so it is the last thing [`fit_rows`]
/// gives up.
pub(super) fn follow_frame(
    listing: Vec<ProcessInfo>,
    style: Presentation,
    strip: Option<String>,
) -> String {
    let mut frame = Vec::new();
    if let Some(strip) = strip {
        // Writing to a `Vec` cannot fail, here or below.
        let _ = writeln!(frame, "{strip}\n");
    }
    // `None`: a followed frame is always a table, and the host block rides
    // above it as text rather than inside the payload.
    let _ = emit_flock(&mut frame, Format::Table, "flock", listing, None, style);
    String::from_utf8_lossy(&frame).into_owned()
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

    use shep_core::paths::ShepPaths;
    use shep_core::protocol::{HostUsage, ProcessInfo, Request, SelectorSpec};

    use shep_core::status::ProcStatus;

    use crate::cli::{FoldArgs, Format};

    use crate::exit::ExitCode;

    use crate::host;

    use crate::output::Streams;

    use super::super::testing::*;
    use super::*;
    use crate::style::Presentation;
    use shep_client::testing::{
        fake_client_capturing_envelopes, fake_client_on, fake_client_replying_err,
        fake_client_with_ack, sample_ack, sample_info,
    };
    use shep_core::protocol::RpcErrorCode;

    /// fails if the host strip stops leading the frame. It has to be first:
    /// it holds still while the tables under it change length, and it is
    /// what survives a window too short for the rest.
    #[test]
    fn a_followed_frame_leads_with_the_host_strip() {
        let usage = HostUsage {
            cpu_percent: Some(11.0),
            memory_used_bytes: 39_963_869_184,
            memory_total_bytes: 51_539_607_552,
            disk_bytes_per_second: Some((0, 0)),
            network_bytes_per_second: Some((0, 0)),
        };
        let strip = host::strip(Some(usage), Presentation::BARE, 200);

        let frame = follow_frame(vec![sample_info()], Presentation::BARE, Some(strip));

        let mut lines = frame.lines();
        assert!(lines.next().unwrap().starts_with("host  cpu  "));
        assert_eq!(lines.next().unwrap(), "");
        assert!(frame.contains("web"), "the table still follows: {frame}");
    }

    /// fails if a shepherd that will not answer for the machine starts
    /// printing a blank strip instead of no strip.
    #[test]
    fn a_frame_without_a_host_strip_is_the_tables_alone() {
        let frame = follow_frame(vec![sample_info()], Presentation::BARE, None);

        assert!(!frame.contains("host  cpu"), "{frame}");
        assert!(frame.contains("web"), "{frame}");
    }

    /// fails if the flourish comes back into a followed frame. It is art
    /// above an empty flock, and a redraw every second turns it into noise.
    #[test]
    fn a_followed_frame_of_an_empty_flock_carries_no_flourish() {
        let frame = follow_frame(Vec::new(), Presentation::BARE, None);

        assert!(!frame.contains("no sheep in the flock yet"), "{frame}");
    }

    /// The version-skew arm, and the reason a refusal is silent: a shepherd
    /// built before `Request::HostUsage` existed decodes
    /// `Request::Unrecognized` and answers `RpcErrorCode::Unsupported` by
    /// name.
    ///
    /// fails if a refusal starts failing the verb, or starts being told
    /// apart from a reading of `None`, which is a platform answer rather
    /// than a version one.
    #[tokio::test]
    async fn a_shepherd_that_refuses_the_host_request_answers_for_no_strip() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_client_replying_err(
            &path,
            RpcErrorCode::Unsupported,
            "this shepherd does not implement the request the client sent",
        )
        .await;

        assert_eq!(host_usage(&client).await, None);
    }

    /// fails if a reply this client cannot read starts being rendered as
    /// something. The fake answers `Response::Pong` to everything.
    #[tokio::test]
    async fn a_shepherd_answering_something_else_answers_for_no_strip() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = fake_client_capturing_envelopes(&path).await;

        assert_eq!(host_usage(&client).await, None);
    }

    /// fails if a one-shot listing stops asking for the machine. The whole
    /// point of the shepherd holding a baseline is that a listing which
    /// never asks gets nothing for it.
    #[tokio::test]
    async fn flock_asks_for_the_machine_as_well_as_the_flock() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = shepherd_answering(&path, Some(sample_host())).await;

        let _ = listing(&client, Format::Table).await;

        let mut sent = Vec::new();
        for _ in 0..2 {
            let envelope = tokio::time::timeout(RECV_TIMEOUT, envelopes.recv())
                .await
                .expect("flock must reach the wire; it hung instead of sending a request")
                .unwrap();
            sent.push(envelope.body);
        }
        assert_eq!(sent, vec![Request::ListFlock, Request::HostUsage]);
    }

    /// The door an operator actually knocks on. `host::strip` has its own
    /// tests for what the line says; this one is about a one-shot listing
    /// carrying it at all, which is the whole ask.
    ///
    /// fails if the strip stops leading a one-shot table, or stops being
    /// separated from it.
    #[tokio::test]
    async fn a_one_shot_listing_leads_with_the_host_strip() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = shepherd_answering(&path, Some(sample_host())).await;

        let (code, text) = listing(&client, Format::Table).await;

        assert_eq!(code, ExitCode::Success);
        let mut lines = text.lines();
        assert!(
            lines.next().unwrap().starts_with("host  cpu  "),
            "got {text:?}"
        );
        assert_eq!(lines.next().unwrap(), "");
        assert!(text.contains("web"), "the table still follows: {text:?}");
    }

    /// fails if the two formats stop answering the same question. The table
    /// shows the machine, so the JSON says so too, beside `data` rather
    /// than inside it: an existing `data[0].name` script sees no change.
    #[tokio::test]
    async fn the_json_surface_carries_the_host_beside_the_flock() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _envelopes) = shepherd_answering(&path, Some(sample_host())).await;

        let (_code, text) = listing(&client, Format::Json).await;

        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["data"][0]["name"], "web");
        assert_eq!(json["host"]["memory_used_bytes"], 39_963_869_184u64);
        assert_eq!(json["host"]["cpu_percent"], 11.459_433);
        // A strip is a terminal thing, and JSON is not a terminal.
        assert!(!text.contains("host  cpu"), "got {text:?}");
    }

    /// Three answers, and a reader has to tell them apart: absent means
    /// this shepherd would not say, `null` means a platform that cannot be
    /// read, and an object with `null` rates means one that has not been
    /// read for long enough yet.
    ///
    /// fails if any two of them collapse onto one spelling.
    #[tokio::test]
    async fn the_json_host_key_spells_its_three_answers_apart() {
        let json = async |host: Option<HostUsage>| {
            let dir = tempfile::tempdir().unwrap();
            let path = shep_client::testing::control_address(dir.path());
            let (client, _envelopes) = shepherd_answering(&path, host).await;
            let (_code, text) = listing(&client, Format::Json).await;
            serde_json::from_str::<serde_json::Value>(&text).unwrap()
        };

        // Read, and read long enough ago to have a window: an object whose
        // rates are numbers.
        let read = json(Some(sample_host())).await;
        assert_eq!(read["host"]["cpu_percent"], 11.459_433);

        // A platform `sysinfo` cannot read: present and null, which is not
        // the same claim as a rate that has no window yet.
        let unreadable = json(None).await;
        assert!(unreadable["host"].is_null(), "got {unreadable:?}");

        // Read, but with no window yet: an object, with nulls where the
        // rates will be.
        let unmeasured = json(Some(HostUsage {
            cpu_percent: None,
            disk_bytes_per_second: None,
            network_bytes_per_second: None,
            ..sample_host()
        }))
        .await;
        assert!(unmeasured["host"].is_object(), "got {unmeasured:?}");
        assert!(unmeasured["host"]["cpu_percent"].is_null());
        assert_eq!(unmeasured["host"]["memory_used_bytes"], 39_963_869_184u64);
    }

    /// The third spelling: no key at all, from a shepherd that would not
    /// answer the question. A script has to be able to tell "this shepherd
    /// is too old" from "this machine cannot be read".
    ///
    /// fails if a refusal starts writing `host: null`, which would claim a
    /// platform answer the shepherd never gave.
    #[tokio::test]
    async fn a_refused_host_request_leaves_no_json_key_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_with_ack(&path, sample_ack()).await;
        daemon.reply_to_list(vec![sample_info()]);

        // `FakeDaemon` answers anything it has no script for with
        // `Response::Pong`, which this client can no more use than a
        // refusal.
        let (code, text) = listing(&client, Format::Json).await;

        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["data"][0]["name"], "web");
        assert!(json.get("host").is_none(), "got {json:?}");
    }

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
