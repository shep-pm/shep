//! Which hints an operator turned off, from `$SHEP_HOME/hints.json`, and the
//! `shep hints` verb that lists and changes them.

use std::io::{self, Write as _};
use std::path::Path;

use serde::{Deserialize, Serialize};
use shep_core::paths::ShepPaths;

use super::{Hint, Offer, OfferState, offered_names, offers_of};
use crate::cli::{HintsArgs, HintsCommand};
use crate::dog_index;
use crate::exit::ExitCode;
use crate::output::{Render, Streams, emit, write_outcome};

/// The file's name under `$SHEP_HOME`.
const FILE: &str = "hints.json";

/// The hints turned off, shaped `{"off": ["logs"]}` on disk.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct Off {
    /// Names as written: one this build does not know is never matched.
    #[serde(default)]
    off: Vec<String>,
}

impl Off {
    /// What `home`'s `hints.json` turns off.
    ///
    /// A missing, unreadable or unparsable file turns nothing off: a hint is
    /// never worth failing a listing over.
    pub(super) fn read(home: &Path) -> Self {
        std::fs::read(home.join(FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// What `home`'s `hints.json` turns off, for a caller about to rewrite it.
    ///
    /// A missing file turns nothing off. One that cannot be read or parsed
    /// is an error, so a rewrite never discards what the operator wrote.
    fn read_to_change(home: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(home.join(FILE)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err.to_string()),
        };
        serde_json::from_slice(&bytes).map_err(|err| err.to_string())
    }

    /// Whether `hint` is turned off.
    pub(super) fn contains(&self, hint: Hint) -> bool {
        self.off
            .iter()
            .any(|name| Hint::from_name(name) == Some(hint))
    }

    /// Turns `hint` off, and says whether that changed anything.
    fn turn_off(&mut self, hint: Hint) -> bool {
        let was_off = self.contains(hint);
        if !was_off {
            self.off.push(hint.name().to_owned());
        }
        !was_off
    }

    /// Turns `hint` on, and says whether that changed anything.
    fn turn_on(&mut self, hint: Hint) -> bool {
        let before = self.off.len();
        self.off.retain(|name| Hint::from_name(name) != Some(hint));
        self.off.len() != before
    }

    /// Writes the file through a temp file in `home` and a rename, so a
    /// reader never sees half of it.
    fn write(&self, home: &Path) -> io::Result<()> {
        std::fs::create_dir_all(home)?;
        let mut staged = tempfile::Builder::new()
            .prefix(".hints.")
            .suffix(".tmp")
            .tempfile_in(home)?;
        serde_json::to_writer(&mut staged, self)?;
        staged.write_all(b"\n")?;
        staged.persist(home.join(FILE)).map_err(|err| err.error)?;
        Ok(())
    }
}

/// One line of `shep hints`.
#[derive(Debug, Serialize)]
struct HintRow {
    hint: &'static str,
    state: &'static str,
    /// Each dog the hint offers, worded `name (state)`.
    for_it: Vec<String>,
}

/// `shep hints`' listing, in [`Hint::ALL`] order.
#[derive(Debug, Serialize)]
#[serde(transparent)]
struct HintRows(Vec<HintRow>);

impl Render for HintRows {
    fn headers() -> &'static [&'static str] {
        &["HINT", "STATE", "FOR IT"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|row| {
                let for_it = if row.for_it.is_empty() {
                    "-".to_owned()
                } else {
                    row.for_it.join(", ")
                };
                vec![row.hint.to_owned(), row.state.to_owned(), for_it]
            })
            .collect()
    }

    fn json_key_for(header: &str) -> String {
        header.to_lowercase().replace(' ', "_")
    }

    // Every column is short and none is worth losing.
    const PRIORITIES: &'static [u8] = &[0, 0, 0];
}

/// The word `shep hints` puts after an offered dog's name.
const fn state_word(state: OfferState) -> &'static str {
    match state {
        OfferState::NotInstalled => "not installed",
        OfferState::Installed => "installed",
        OfferState::BuiltIn => "built-in",
    }
}

/// `hint`'s offers as `shep hints` words them: index dogs first, built-in last.
fn offered_words(paths: &ShepPaths, index: Option<&dog_index::Index>, hint: Hint) -> Vec<String> {
    let (built_in, rest): (Vec<Offer>, Vec<Offer>) = offers_of(paths, offered_names(hint, index))
        .into_iter()
        .partition(|offer| offer.state == OfferState::BuiltIn);
    rest.into_iter()
        .chain(built_in)
        .map(|offer| format!("{} ({})", offer.name, state_word(offer.state)))
        .collect()
}

/// `shep hints`: lists the hints, or turns some off or on.
///
/// `index_url` names which cached index the listing reads. Needs no
/// shepherd and never fetches.
pub(crate) fn run(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    index_url: &str,
    args: &HintsArgs,
) -> ExitCode {
    match &args.command {
        None => list(streams, paths, index_url),
        Some(HintsCommand::Off { hint }) => change(streams, paths, hint.as_deref(), false),
        Some(HintsCommand::On { hint }) => change(streams, paths, hint.as_deref(), true),
    }
}

fn list(streams: &mut Streams<'_>, paths: &ShepPaths, index_url: &str) -> ExitCode {
    let off = Off::read(&paths.home);
    let index = dog_index::cached_only(paths, index_url);
    let rows = Hint::ALL
        .into_iter()
        .map(|hint| HintRow {
            hint: hint.name(),
            state: if off.contains(hint) { "off" } else { "on" },
            for_it: offered_words(paths, index.as_ref(), hint),
        })
        .collect();
    write_outcome(emit(
        &mut *streams.out,
        streams.fmt,
        "hints",
        HintRows(rows),
        streams.style,
    ))
}

/// Turns `name`, or every hint with `None`, on or off.
fn change(streams: &mut Streams<'_>, paths: &ShepPaths, name: Option<&str>, on: bool) -> ExitCode {
    let targets: Vec<Hint> = match name {
        None => Hint::ALL.to_vec(),
        Some(name) => match Hint::from_name(name) {
            Some(hint) => vec![hint],
            None => {
                let valid: Vec<&str> = Hint::ALL.iter().map(|hint| hint.name()).collect();
                let message = format!("unknown hint {name:?}; the hints are {}", valid.join(", "));
                return streams.fail(ExitCode::Usage, &message);
            }
        },
    };
    let mut off = match Off::read_to_change(&paths.home) {
        Ok(off) => off,
        Err(err) => {
            let message = format!(
                "cannot use {}: {err}. Fix or remove it, then try again",
                paths.home.join(FILE).display()
            );
            return streams.fail(ExitCode::InvalidConfig, &message);
        }
    };
    let changed: Vec<&str> = targets
        .into_iter()
        .filter(|hint| {
            if on {
                off.turn_on(*hint)
            } else {
                off.turn_off(*hint)
            }
        })
        .map(Hint::name)
        .collect();
    let now = if on { "on" } else { "off" };
    if changed.is_empty() {
        let message = match name {
            Some(name) => format!("{name} is already {now}"),
            None => format!("every hint is already {now}"),
        };
        streams.note("hints", &message);
        return ExitCode::Success;
    }
    if let Err(err) = off.write(&paths.home) {
        let message = format!("cannot write {}: {err}", paths.home.join(FILE).display());
        return streams.fail(ExitCode::Failure, &message);
    }
    streams.note("hints", &format!("turned {now}: {}", changed.join(", ")));
    ExitCode::Success
}

#[cfg(test)]
mod tests {
    use super::super::tests::{URL, cache_index, install, off, paths_in};
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    use super::*;
    use crate::cli::{Format, HintsCommand};

    struct Ran {
        code: ExitCode,
        out: String,
        err: String,
    }

    fn hints(paths: &ShepPaths, fmt: Format, command: Option<HintsCommand>) -> Ran {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            &mut Streams::bare(&mut out, &mut err, fmt),
            paths,
            URL,
            &HintsArgs { command },
        );
        Ran {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    fn turn_off(paths: &ShepPaths, hint: Option<&str>) -> Ran {
        let hint = hint.map(str::to_owned);
        hints(paths, Format::Table, Some(HintsCommand::Off { hint }))
    }

    fn turn_on(paths: &ShepPaths, hint: Option<&str>) -> Ran {
        let hint = hint.map(str::to_owned);
        hints(paths, Format::Table, Some(HintsCommand::On { hint }))
    }

    fn off_list(paths: &ShepPaths) -> Vec<String> {
        let bytes = std::fs::read(paths.home.join(FILE)).unwrap();
        let file: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        serde_json::from_value(file["off"].clone()).unwrap()
    }

    #[test]
    fn hints_off_writes_the_named_hint_to_hints_json() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());

        let ran = turn_off(&paths, Some("logs"));

        assert_eq!(ran.code, ExitCode::Success, "{}", ran.err);
        assert_eq!(off_list(&paths), ["logs"]);
        assert_eq!(ran.out, "notice[hints]: turned off: logs\n");
    }

    #[test]
    fn hints_off_with_no_name_turns_every_hint_off() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());

        let ran = turn_off(&paths, None);

        assert_eq!(ran.code, ExitCode::Success, "{}", ran.err);
        assert_eq!(off_list(&paths), ["logs", "alerts", "upgrade"]);
    }

    #[test]
    fn hints_on_removes_a_hint_from_the_off_list() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        off(&paths, &["logs", "alerts"]);

        let ran = turn_on(&paths, Some("logs"));

        assert_eq!(ran.code, ExitCode::Success, "{}", ran.err);
        assert_eq!(off_list(&paths), ["alerts"]);
        assert_eq!(ran.out, "notice[hints]: turned on: logs\n");
    }

    #[test]
    fn hints_on_with_no_name_turns_every_hint_on() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        off(&paths, &["logs", "alerts"]);

        let ran = turn_on(&paths, None);

        assert_eq!(ran.code, ExitCode::Success, "{}", ran.err);
        assert!(off_list(&paths).is_empty());
    }

    #[test]
    fn a_file_that_does_not_parse_is_refused_and_left_as_it_is() {
        for broken in [r#"{"off": ["logs",]}"#, r#"{"off": "logs"}"#] {
            let dir = tempfile::tempdir().unwrap();
            let paths = paths_in(dir.path());
            std::fs::write(paths.home.join(FILE), broken).unwrap();

            let ran = turn_off(&paths, Some("alerts"));

            assert_eq!(ran.code, ExitCode::InvalidConfig, "{broken}");
            assert!(ran.err.contains("hints.json"), "{}", ran.err);
            assert_eq!(
                std::fs::read_to_string(paths.home.join(FILE)).unwrap(),
                broken
            );
        }
    }

    #[test]
    fn a_name_this_build_does_not_know_survives_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        off(&paths, &["nonesuch"]);

        turn_off(&paths, Some("logs"));

        assert_eq!(off_list(&paths), ["nonesuch", "logs"]);
    }

    #[test]
    fn an_unknown_hint_name_is_a_usage_error_naming_the_valid_ones() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());

        let ran = turn_off(&paths, Some("nonesuch"));

        assert_eq!(ran.code, ExitCode::Usage);
        assert!(ran.err.contains("nonesuch"), "{}", ran.err);
        assert!(ran.err.contains("logs, alerts, upgrade"), "{}", ran.err);
        assert!(!paths.home.join(FILE).exists());
    }

    #[test]
    fn turning_off_an_off_hint_changes_nothing_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        off(&paths, &["logs"]);
        let before = std::fs::read(paths.home.join(FILE)).unwrap();

        let ran = turn_off(&paths, Some("logs"));

        assert_eq!(ran.code, ExitCode::Success, "{}", ran.err);
        assert_eq!(ran.out, "notice[hints]: logs is already off\n");
        assert_eq!(std::fs::read(paths.home.join(FILE)).unwrap(), before);
    }

    #[test]
    fn the_table_lists_each_hint_with_its_state_and_what_it_offers() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        cache_index(
            &paths,
            &[("log-rotate", &["logs"]), ("discord", &["alerts"])],
        );
        install(&paths, "discord");
        off(&paths, &["alerts"]);

        let ran = hints(&paths, Format::Table, None);

        assert_eq!(ran.code, ExitCode::Success, "{}", ran.err);
        assert_eq!(
            ran.out,
            "\
HINT     STATE  FOR IT
logs     on     log-rotate (not installed)
alerts   off    discord (installed), bark (built-in)
upgrade  on     -
"
        );
    }

    #[test]
    fn with_no_cached_index_a_row_shows_a_dash_unless_it_has_the_built_in() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());

        let ran = hints(&paths, Format::Table, None);

        assert_eq!(
            ran.out,
            "\
HINT     STATE  FOR IT
logs     on     -
alerts   on     bark (built-in)
upgrade  on     -
"
        );
    }

    #[test]
    fn the_json_form_has_one_row_per_hint() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        cache_index(&paths, &[("log-rotate", &["logs"])]);
        off(&paths, &["logs"]);

        let ran = hints(&paths, Format::Json, None);

        let envelope: serde_json::Value = serde_json::from_str(&ran.out).unwrap();
        assert_eq!(envelope["command"], "hints");
        assert_eq!(
            envelope["data"],
            serde_json::json!([
                { "hint": "logs", "state": "off", "for_it": ["log-rotate (not installed)"] },
                { "hint": "alerts", "state": "on", "for_it": ["bark (built-in)"] },
                { "hint": "upgrade", "state": "on", "for_it": [] },
            ])
        );
    }

    #[test]
    fn an_off_hint_no_longer_prints_under_the_listing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let flock = [ProcessInfo::builder(1, "api", ProcStatus::Online)
            .restarts(3)
            .build()];
        assert!(super::super::next(&paths, URL, &flock).is_some());

        turn_off(&paths, Some("alerts"));

        assert!(super::super::next(&paths, URL, &flock).is_none());
    }

    #[test]
    fn a_missing_file_leaves_every_hint_on() {
        let dir = tempfile::tempdir().unwrap();

        let off = Off::read(dir.path());

        assert!(Hint::ALL.iter().all(|hint| !off.contains(*hint)));
    }

    #[test]
    fn an_unparsable_file_leaves_every_hint_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"{"off": ["logs""#).unwrap();

        let off = Off::read(dir.path());

        assert!(Hint::ALL.iter().all(|hint| !off.contains(*hint)));
    }

    #[test]
    fn a_hint_named_off_is_off_and_an_unknown_name_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), r#"{"off": ["logs", "nonesuch"]}"#).unwrap();

        let off = Off::read(dir.path());

        assert!(off.contains(Hint::Logs));
        assert!(!off.contains(Hint::Alerts));
    }
}
