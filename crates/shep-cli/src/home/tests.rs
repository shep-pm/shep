//! The home gate's tests: path resolution, the refusals' exact wording, and
//! first-run creation.

use super::*;

/// fails if the copyable remedy stops being one shell word. A path with
/// a space rendered `mkdir -p /tmp/my shep`, which creates two
/// directories and reports no error, leaving exactly the empty
/// invisible flock this refusal exists to prevent.
#[cfg(not(windows))]
#[test]
fn the_mkdir_hint_survives_a_path_a_shell_would_split() {
    let refusal = HomeRefusal::Missing(PathBuf::from("/tmp/my shep home"));
    let text = refusal.to_string();
    assert!(
        text.contains("mkdir -p '/tmp/my shep home'"),
        "the remedy must name one word: {text}"
    );
    // The line above it names the path as prose, and is not a command.
    assert!(text.contains("no flock at /tmp/my shep home"), "{text}");
}

/// fails if the refusal offers to drop a knob the operator may never
/// have typed. `$SHEP_HOME` reaches this variant through clap's `env`,
/// so naming the flag alone sends half of them looking for something
/// that is not on their command line.
#[test]
fn the_missing_home_refusal_reads_exactly_this() {
    let text = HomeRefusal::Missing(PathBuf::from("/srv/api")).to_string();
    // Whole message, not a fragment: a substring passes while a line
    // outside it regresses, and all three lines are per-platform.
    let expected = if cfg!(windows) {
        "no flock at /srv/api\n\
         did you mean to drop --home/%SHEP_HOME%? the default is %USERPROFILE%\\.shep\n\
         to set up a flock there deliberately: mkdir \"/srv/api\""
    } else {
        "no flock at /srv/api\n\
         did you mean to drop --home/$SHEP_HOME? the default is ~/.shep\n\
         to set up a flock there deliberately: mkdir -p '/srv/api'"
    };
    assert_eq!(text, expected);
}

/// fails if a path can add a line to a refusal. The table emitter keeps
/// shep's own line breaks, so a `\n` inside an interpolated value would
/// write a line under `error[usage]:` that reads as shep's own.
#[test]
fn a_newline_in_a_missing_home_cannot_forge_a_line() {
    let hostile = PathBuf::from("/tmp/forge-a\nnotice[ok]: your flock is fine");
    let text = HomeRefusal::Missing(hostile).to_string();
    assert_eq!(
        text.lines().count(),
        3,
        "the refusal has three lines of its own: {text:?}"
    );
    assert!(
        !text.lines().any(|line| line.starts_with("notice[")),
        "a line was forged: {text:?}"
    );
    assert!(
        text.starts_with("no flock at /tmp/forge-a notice[ok]: your flock is fine\n"),
        "the newline must become a space, not vanish: {text:?}"
    );
}

/// fails if the relative-home refusal takes a line from its path. Same
/// defect as the missing-home one, two variants over, and the reason to
/// check it separately is that it interpolates twice.
#[test]
fn a_newline_in_a_relative_home_cannot_forge_a_line() {
    let refusal = HomeRefusal::Relative {
        knob: "--home/$SHEP_HOME",
        given: PathBuf::from("forge-b\nerror[internal]: shepherd compromised"),
        absolute: Some(PathBuf::from(
            "/w/forge-b\nerror[internal]: shepherd compromised",
        )),
    };
    let text = refusal.to_string();
    assert_eq!(text.lines().count(), 3, "{text:?}");
    assert!(
        !text.lines().any(|line| line.starts_with("error[")),
        "a line was forged: {text:?}"
    );
}

/// fails if the io refusal lets either half add a line. Its `source` is
/// written by the OS rather than by an operator, and is collapsed for
/// the same reason: the layout is shep's and the values are not.
#[test]
fn neither_half_of_an_io_refusal_can_forge_a_line() {
    let refusal = HomeRefusal::Io {
        path: PathBuf::from("/tmp/forge-c\nnotice[ok]: created"),
        source: std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied\nnotice[ok]: retried and it worked",
        ),
    };
    let text = refusal.to_string();
    assert_eq!(text.lines().count(), 1, "{text:?}");
    assert!(!text.contains('\n'), "{text:?}");
}

/// fails if an apostrophe in a path breaks out of the quoting and turns
/// the rest of the hint into shell the operator did not mean to run.
#[cfg(not(windows))]
#[test]
fn an_apostrophe_in_a_path_cannot_escape_the_mkdir_hint() {
    let refusal = HomeRefusal::Missing(PathBuf::from("/tmp/rin's flock"));
    let text = refusal.to_string();
    assert!(
        text.contains(r"mkdir -p '/tmp/rin'\''s flock'"),
        "an embedded quote must close and reopen: {text}"
    );
}

/// fails if a Windows operator is handed a remedy from another
/// platform. `mkdir -p` parses in neither shell here, and a single
/// quote is an ordinary character to `cmd.exe`, so it would split the
/// path rather than hold it together.
#[cfg(windows)]
#[test]
fn the_missing_home_remedy_is_one_a_windows_shell_can_run() {
    let spaced = HomeRefusal::Missing(PathBuf::from(r"C:\tmp\my shep home")).to_string();
    assert_eq!(
        spaced,
        "no flock at C:\\tmp\\my shep home\n\
         did you mean to drop --home/%SHEP_HOME%? the default is %USERPROFILE%\\.shep\n\
         to set up a flock there deliberately: mkdir \"C:\\tmp\\my shep home\""
    );

    // A Windows path can hold an apostrophe, and the POSIX escaping
    // would break it apart. Nothing inside a double-quoted wrap needs
    // escaping, because a Windows path cannot hold a `"`.
    let quoted = HomeRefusal::Missing(PathBuf::from(r"C:\tmp\rin's flock")).to_string();
    assert_eq!(
        quoted,
        "no flock at C:\\tmp\\rin's flock\n\
         did you mean to drop --home/%SHEP_HOME%? the default is %USERPROFILE%\\.shep\n\
         to set up a flock there deliberately: mkdir \"C:\\tmp\\rin's flock\""
    );
}

/// A [`ShepPaths`] rooted at `root`, so the rule can be exercised without
/// touching the process-global `$HOME`.
#[cfg(unix)]
fn paths_at(root: &std::path::Path) -> ShepPaths {
    let home = root.join(".shep").to_string_lossy().into_owned();
    let env = |key: &str| (key == "SHEP_HOME").then(|| home.clone());
    ShepPaths::resolve(&env, std::path::Path::new("/nonexistent"))
}

/// `~/.shep` is a name shep chose, so shep may create it.
#[cfg(unix)]
#[test]
fn a_missing_default_home_is_created_and_reported_as_new() {
    let root = tempfile::tempdir().unwrap();

    let (paths, created) =
        ensure_home_at(paths_at(root.path()), false).expect("a default home is created");
    assert_eq!(paths.home, root.path().join(".shep"));
    assert!(
        created,
        "the first call must report that it created the home"
    );
    assert!(
        paths.home.is_dir(),
        "the home must exist on disk afterwards"
    );

    let (_, created_again) =
        ensure_home_at(paths_at(root.path()), false).expect("second call succeeds");
    assert!(
        !created_again,
        "a home that was already there is not newly created"
    );
}

#[cfg(unix)]
#[test]
fn creating_startups_default_home_scaffolds_it_like_the_shared_gate() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths_at(root.path());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = Streams::bare(&mut out, &mut err, cli::Format::Table);

    create_default_home(&mut streams, paths.clone()).expect("a default home is created");
    assert!(paths.home.is_dir());
    let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
    assert!(written.contains("[interpreters]"), "{written}");

    create_default_home(&mut streams, paths).expect("a home already there is left alone");
}

#[cfg(unix)]
#[test]
fn an_explicitly_named_missing_home_is_refused_and_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths_at(&root.path().join("srv").join("typo"));
    let named = paths.home.clone();

    let refusal = ensure_home_at(paths, true).expect_err("a named missing home is refused");
    assert_eq!(refusal.code(), ExitCode::Usage);
    let message = refusal.to_string();
    assert!(
        message.contains(&named.display().to_string()),
        "the refusal must name the path it refused: {message}"
    );
    assert!(
        message.contains("~/.shep"),
        "the refusal must point at the default as the way out: {message}"
    );
    assert!(
        !named.exists(),
        "a refused path must be left on disk exactly as it was found"
    );
}

#[cfg(unix)]
#[test]
fn a_created_home_is_owner_only_from_the_moment_it_exists() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let (paths, _) = ensure_home_at(paths_at(root.path()), false).unwrap();
    let mode = std::fs::metadata(&paths.home).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "a fresh $SHEP_HOME must be owner-only");
}

/// A rooted path on the platform running the test. `/tmp/explicit` has
/// no drive prefix, so Windows reads it as relative and the gate in
/// `resolve_paths` refuses it.
#[cfg(not(windows))]
const EXPLICIT_HOME: &str = "/tmp/explicit";

/// A rooted path on the platform running the test.
#[cfg(windows)]
const EXPLICIT_HOME: &str = r"C:\tmp\explicit";

/// Pins `resolve_paths`'s folding of an already-populated
/// `GlobalArgs::home` only. `$SHEP_HOME` reaches that field through clap,
/// and is pinned in `cli.rs`.
#[test]
fn explicit_home_field_resolves_to_the_expected_shep_paths() {
    let paths = resolve_paths(&global_with_home(Some(EXPLICIT_HOME))).unwrap();
    assert_eq!(paths.home, std::path::Path::new(EXPLICIT_HOME));
    // The control address is a socket file on unix and a named-pipe name
    // on Windows, so `--home` is asserted to reach both derivations.
    #[cfg(unix)]
    assert_eq!(
        paths.socket,
        std::path::Path::new("/tmp/explicit/run/shep.sock")
    );
    #[cfg(windows)]
    assert_eq!(paths.socket, std::path::Path::new(&paths.pipe_name()));
}

fn global_with_home(home: Option<&str>) -> cli::GlobalArgs {
    cli::GlobalArgs {
        host: None,
        home: home.map(Into::into),
        format: cli::Format::Table,
        quiet: false,
        style: None,
    }
}

/// An absolute path whose bytes are not valid UTF-8, which only unix can
/// spell. Windows paths are UTF-16, so the same hole there is an unpaired
/// surrogate and needs its own constructor.
#[cfg(unix)]
fn non_utf8_home() -> std::path::PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    std::path::PathBuf::from(std::ffi::OsString::from_vec(
        b"/tmp/shep-\xff-home".to_vec(),
    ))
}

/// A home shep cannot carry as text is refused at the door rather than
/// replaced silently.
///
/// Without this, `to_string_lossy` turns the byte into U+FFFD and every
/// path in the layout is built from a directory the operator never
/// named. The refusal has to come before `ShepPaths::resolve`, since that
/// is where the conversion happens.
#[cfg(unix)]
#[test]
fn a_home_that_is_not_utf8_is_refused_before_any_path_is_derived() {
    let home = non_utf8_home();
    let global = cli::GlobalArgs {
        host: None,
        home: Some(home.clone()),
        format: cli::Format::Table,
        quiet: false,
        style: None,
    };
    let Err(refusal) = resolve_paths(&global) else {
        panic!("a home that is not UTF-8 must not resolve a layout");
    };
    assert!(
        matches!(&refusal, HomeRefusal::NotUtf8 { knob, given }
            if *knob == HOME_KNOB && given == &home),
        "its own refusal, not the relative or unresolved one: {refusal:?}"
    );
    assert_eq!(refusal.code(), ExitCode::Usage);

    let rendered = refusal.to_string();
    assert!(
        rendered.contains(HOME_KNOB),
        "names the spelling to fix: {rendered}"
    );
    assert!(
        rendered.contains("UTF-8"),
        "says what is wrong with it: {rendered}"
    );
    assert!(
        !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
        "no em or en dash in copy a user reads: {rendered}"
    );
}

/// The neighbouring gate still answers first for a path that is both
/// relative and not UTF-8, so one bad home reports one reason.
#[cfg(unix)]
#[test]
fn a_relative_home_is_refused_as_relative_even_when_it_is_also_not_utf8() {
    use std::os::unix::ffi::OsStringExt as _;
    let home = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"rel-\xff-home".to_vec()));
    let global = cli::GlobalArgs {
        host: None,
        home: Some(home),
        format: cli::Format::Table,
        quiet: false,
        style: None,
    };
    let Err(refusal) = resolve_paths(&global) else {
        panic!("a relative home must not resolve a layout");
    };
    assert!(
        matches!(refusal, HomeRefusal::Relative { .. }),
        "the rootless reason wins, since it is the one an operator hits first"
    );
}

/// The whole point of the gate: a home with no root puts the control
/// socket at `rel-home/run/shep.sock`, which names one flock from the
/// directory it was started in and a different, absent one from
/// anywhere else.
#[test]
fn a_relative_home_is_refused_before_any_path_is_derived() {
    let Err(refusal) = resolve_paths(&global_with_home(Some("rel-home"))) else {
        panic!("a relative --home must not resolve a layout");
    };
    assert!(
        matches!(&refusal, HomeRefusal::Relative { knob, given, .. }
            if *knob == HOME_KNOB && given == std::path::Path::new("rel-home")),
        "a rootless home is its own refusal, not the unresolved one"
    );
    assert_eq!(
        refusal.code(),
        ExitCode::Usage,
        "a path shep cannot act on is the operator's to fix"
    );

    let rendered = refusal.to_string();
    assert!(
        rendered.contains("rel-home"),
        "the refusal must quote the path as typed: {rendered}"
    );
    assert!(
        rendered.contains(HOME_KNOB),
        "the refusal must name the spelling to fix: {rendered}"
    );
    let cwd = std::env::current_dir().expect("a current directory");
    assert!(
        rendered.contains(&cwd.join("rel-home").display().to_string()),
        "the remedy must name the absolute form of what was typed: {rendered}"
    );
}

/// Windows reads a path with no drive prefix as relative, so `\shep`
/// carries the same defect as `rel-home` and has to be refused with it.
#[cfg(windows)]
#[test]
fn a_drive_relative_home_is_refused_on_windows() {
    assert!(
        matches!(
            resolve_paths(&global_with_home(Some(r"\shep"))),
            Err(HomeRefusal::Relative { .. })
        ),
        r"`\shep` resolves against whichever drive is current"
    );
}

#[cfg(unix)]
#[test]
fn a_home_directory_that_is_not_utf8_is_refused_and_names_its_own_variable() {
    use std::os::unix::ffi::OsStringExt as _;
    // Every variable `user_home` reads, so the arm under test is the one
    // that answered rather than a fallback.
    let mangled = |key: &str| {
        matches!(key, "HOME" | "USERPROFILE" | "HOMEDRIVE" | "HOMEPATH")
            .then(|| OsString::from_vec(b"/home/\xff".to_vec()))
    };
    let Err(refusal) = resolve_paths_in(&global_with_home(None), &mangled) else {
        panic!("a home directory that is not UTF-8 must not resolve a layout");
    };
    assert!(
        matches!(&refusal, HomeRefusal::NotUtf8 { knob, .. } if *knob == HOME_DIR_VAR),
        "names the variable that supplied it, not the --home knob: {refusal:?}"
    );
    assert_eq!(refusal.code(), ExitCode::Usage);
    assert!(
        refusal.to_string().contains("UTF-8"),
        "says what is wrong with it"
    );
}

/// rootless home directory is the same defect one door over.
#[test]
fn a_relative_home_directory_is_refused_and_names_its_own_variable() {
    // Every variable `user_home` reads on either platform, so the arm
    // under test is the one that answered rather than a fallback.
    let rootless = |key: &str| {
        matches!(key, "HOME" | "USERPROFILE" | "HOMEDRIVE" | "HOMEPATH")
            .then(|| OsString::from("ada"))
    };
    let Err(refusal) = resolve_paths_in(&global_with_home(None), &rootless) else {
        panic!("a rootless home directory must not resolve a layout");
    };
    // Pinned by variant, not by rendered text alone: `UNRESOLVED_HOME`
    // also names `$HOME`, so the assertions below pass on a refusal that
    // never noticed the rootless path.
    assert!(
        matches!(&refusal, HomeRefusal::Relative { knob, given, .. }
            if *knob == HOME_DIR_VAR && given == std::path::Path::new("ada")),
        "the refusal must carry the home directory as supplied, not the joined `.shep`"
    );

    let rendered = refusal.to_string();
    assert!(
        rendered.contains(HOME_DIR_VAR),
        "an operator cannot fix `--home` when `--home` is not what said it: {rendered}"
    );
    assert!(
        !rendered.contains(HOME_KNOB),
        "naming a knob the operator did not touch sends them to the wrong fix: {rendered}"
    );
    let cwd = std::env::current_dir().expect("a current directory");
    assert!(
        rendered.contains(&cwd.join("ada").display().to_string()),
        "the remedy must name the absolute form of what was supplied: {rendered}"
    );
}

#[test]
fn an_explicit_home_resolves_with_no_home_directory_at_all() {
    let nothing = |_: &str| None;
    let paths = resolve_paths_in(&global_with_home(Some(EXPLICIT_HOME)), &nothing)
        .expect("--home names a root on its own");
    assert_eq!(
        paths.home,
        std::path::Path::new(EXPLICIT_HOME),
        "the empty placeholder must not reach the resolved home"
    );
    assert!(
        !paths.snapshot.starts_with(".shep"),
        "a path rooted at the empty placeholder would start with `.shep`: {}",
        paths.snapshot.display()
    );
}

/// The gate must not refuse the ordinary case it sits in front of.
#[test]
fn an_absolute_home_directory_still_resolves_the_default_home() {
    let rooted = |key: &str| (key == "HOME").then(|| OsString::from(EXPLICIT_HOME));
    // Windows reads `HOME` only after `USERPROFILE`, which this closure
    // leaves unset, so one absolute answer serves both platforms.
    let paths = resolve_paths_in(&global_with_home(None), &rooted)
        .expect("an absolute home directory resolves the default home");
    assert_eq!(
        paths.home,
        std::path::Path::new(EXPLICIT_HOME).join(".shep")
    );
}

/// fails if the status line comes back for an invocation that named a
/// shepherd: `shep --host amp2` would then print whether *this* machine's
/// shepherd is running, an answer to a question nobody asked.
#[test]
fn a_status_line_is_skipped_for_any_argv_that_names_a_home_or_a_host() {
    let argv = |tokens: &[&str]| tokens.iter().map(OsString::from).collect::<Vec<_>>();
    for named in [
        &["shep", "--host", "amp2"][..],
        &["shep", "--host=amp2"],
        &["shep", "--host", "amp2", "--help"],
        &["shep", "--format", "json", "--host", "amp2"],
        &["shep", "--home", "/tmp/h"],
        &["shep", "--home=/tmp/h"],
    ] {
        assert!(names_another_shepherd(&argv(named)), "{named:?}");
    }
    for unnamed in [
        &["shep"][..],
        &["shep", "--help"],
        &["shep", "--format", "json"],
        &["shep", "--hostile"],
        &["shep", "--homes=1"],
    ] {
        assert!(!names_another_shepherd(&argv(unnamed)), "{unnamed:?}");
    }
}
