//! Vetting a binary before `shep adopt` writes it to `shep.toml`: spawning
//! it under two probe flags, reading what it answers, and judging whether
//! it is safe and able to speak to this shepherd.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use shep_core::dogs::{DogVersion, SCHEMA_FLAG, VERSION_FLAG, parse_version_answer};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{MIN_SUPPORTED, SelectorSpec};

use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;
use crate::output::Streams;

/// Why a binary cannot be adopted.
///
/// The modes `enable` cannot have: a dog that ships inside this binary has
/// no path to be missing, no permission bit to be unset, and nobody else
/// who can write it.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub enum AdoptRefusal {
    /// Nothing exists at that path.
    Missing,
    /// It exists and is not a file (a directory, most often a `bin/` the
    /// operator meant to point inside of).
    NotAFile,
    /// It exists and no execute bit is set for anyone.
    NotExecutable,
    /// The binary, or the directory holding it, can be written by any user
    /// on this system. An adopted dog is exec'd at the shepherd's own trust
    /// level on every restart without being re-vetted. A writable directory
    /// counts too: the binary can be renamed away and replaced.
    WorldWritable {
        /// The offending path: the binary itself, or its directory.
        path: PathBuf,
    },
    /// It exists, is executable, and this kernel will not exec it: the
    /// wrong architecture, or an interpreter line naming something absent.
    WillNotExec {
        /// What `exec` reported.
        reason: String,
    },
    /// It answered `--version` (see [`DogVersion`]) with a `shep-protocol`
    /// below [`MIN_SUPPORTED`], the oldest this shep's handshake still
    /// accepts, so adopting it would register a dog that connects to
    /// nothing. A dog built against a newer protocol than this shep is
    /// fine: the handshake has no upper bound, only a floor. Only a stated
    /// protocol reaches this: a dog that names none is
    /// [`DogVersion::protocol`]'s `None` and is adopted.
    ProtocolMismatch {
        /// What the candidate said it speaks.
        dog: u32,
        /// [`MIN_SUPPORTED`], the oldest protocol this shep still accepts.
        min: u32,
    },
}

impl std::fmt::Display for AdoptRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "no file exists at that path"),
            Self::NotAFile => write!(f, "that path is not a file"),
            Self::NotExecutable => write!(f, "no execute bit is set on that file"),
            Self::WorldWritable { path } => write!(
                f,
                "{} is writable by any user on this system, and an adopted dog runs \
                 with the shepherd's own privileges",
                path.display()
            ),
            Self::WillNotExec { reason } => {
                write!(f, "this kernel refused to run that file: {reason}")
            }
            Self::ProtocolMismatch { dog, min } => write!(
                f,
                "this dog was built for shep protocol {dog}, and this shep needs {min} or \
                 newer; reinstall the dog without --locked so it builds against the current \
                 shep-core, or run a shep that accepts protocol {dog}"
            ),
        }
    }
}

impl core::error::Error for AdoptRefusal {}

/// Vets `path` as a dog binary, before anything is written to `shep.toml`.
///
/// Returns the absolute, canonicalized path: the daemon exec's it after a
/// reboot, from whatever directory the init system gave it. Checks in order,
/// each refusing before the next: existence, file-ness, the execute bit,
/// [`writability`], then one spawn per probe flag, since a probe runs the
/// binary under `home` and `name`, the environment the adopted dog gets.
///
/// # Errors
/// [`AdoptRefusal`] when the path does not resolve, is not a file this
/// kernel will exec, or answers a protocol this shep cannot speak. Silence
/// is not an error: unknown protocol, [`DogSchema::Silent`] schema.
pub fn vet_binary(path: &Path, home: &Path, name: &str) -> Result<VettedBinary, AdoptRefusal> {
    vet_binary_within(path, home, name, VERSION_BUDGET)
}

/// [`vet_binary`], against a caller-chosen budget for the probes.
///
/// Production has one budget and [`vet_binary`] passes it. A test whose
/// question has nothing to do with timing can pass a generous one: the probe
/// spawns a real child and bounds the wait on a wall clock.
///
/// `budget` bounds one wait, not the call. [`answer_text`] gives the full
/// budget to each of two waits, the child's exit and its output, so a probe
/// costs roughly twice `budget` and a vet runs two probes.
///
/// # Errors
/// The same [`AdoptRefusal`] set [`vet_binary`] raises.
pub fn vet_binary_within(
    path: &Path,
    home: &Path,
    name: &str,
    budget: Duration,
) -> Result<VettedBinary, AdoptRefusal> {
    let metadata = std::fs::metadata(path).map_err(|_| AdoptRefusal::Missing)?;
    if !metadata.is_file() {
        return Err(AdoptRefusal::NotAFile);
    }
    // No execute bit set for anyone: owner (0o100), group (0o010), or
    // other (0o001).
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(AdoptRefusal::NotExecutable);
    }
    // Only a symlink loop or a race with a delete can fail here, and neither
    // has anything more specific than `Missing`. The verbatim prefix is
    // stripped because the path is recorded in a file operators edit.
    let canonical = path
        .canonicalize()
        .map(|abs| shep_core::paths::strip_verbatim_prefix(&abs).into_owned())
        .map_err(|_| AdoptRefusal::Missing)?;
    let group_writable = writability(&canonical)?;
    let answer = ask_version(&canonical, home, name, budget)?;
    // `answer.version` is never compared: a third-party dog's crate version
    // has no relationship to shep's own. Only the protocol decides whether
    // the dog can connect.
    if let Some(dog) = answer.as_ref().and_then(|answer| answer.protocol)
        && dog < MIN_SUPPORTED
    {
        return Err(AdoptRefusal::ProtocolMismatch {
            dog,
            min: MIN_SUPPORTED,
        });
    }
    // After the protocol refusal: a candidate shep is about to refuse is
    // not run a second time.
    let schema = ask_schema(&canonical, home, name, budget);
    Ok(VettedBinary {
        path: canonical,
        group_writable,
        answer,
        schema,
    })
}

/// The whole environment a dog is run with here: what the daemon would give
/// it, and nothing else.
///
/// Every caller pairs this with `env_clear`, and both of them run a binary
/// shep did not write: `ask`'s adopt probe, and `hook::run_on_remove`.
/// `SHEP_HOME` and `SHEP_DOG_NAME` are in here rather than left to the
/// caller so a third variable cannot reach one spawn and miss the other.
///
/// Mirrors `shep_daemon::assemble::base_env`, which is private to a crate
/// the CLI does not reach into. The lists are duplicated: if the daemon's
/// allowlist grows, this one has to follow, or a candidate is vetted under
/// conditions its supervised run will not have.
pub(crate) fn dog_env(home: &Path, name: &str) -> Vec<(String, OsString)> {
    #[cfg(unix)]
    const INHERITED: &[&str] = &["HOME", "USER", "LANG", "TZ"];
    #[cfg(unix)]
    const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
    #[cfg(windows)]
    const INHERITED: &[&str] = &[
        "SystemRoot",
        "SystemDrive",
        "windir",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "COMSPEC",
        "PATHEXT",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
    ];
    #[cfg(windows)]
    const DEFAULT_PATH: &str = r"C:\Windows\system32;C:\Windows;C:\Windows\System32\Wbem";

    let path = std::env::var("PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| DEFAULT_PATH.to_string());
    let mut env = vec![("PATH".to_string(), OsString::from(path))];
    env.extend(
        INHERITED
            .iter()
            .filter_map(|key| std::env::var_os(key).map(|v| ((*key).to_string(), v))),
    );
    // Last, and as `OsString`: a home that is not UTF-8 still reaches the
    // dog whole, and the two shep owns cannot be shadowed by an inherited
    // one of the same name.
    env.push(("SHEP_HOME".to_string(), home.as_os_str().to_owned()));
    env.push(("SHEP_DOG_NAME".to_string(), OsString::from(name)));
    env
}

/// Runs `path` with `flag`, one of [`VERSION_FLAG`] or [`SCHEMA_FLAG`], and
/// hands back what it printed on stdout, within `budget`.
///
/// `Ok(None)` is no answer, and never a fault: silence, a run that failed,
/// and a run still going when the budget ran out all arrive that way.
/// Answering either flag is optional. `Ok(Some(text))` is only ever the
/// output of a run that exited 0; what counts as a usable answer is each
/// caller's own question.
///
/// # Errors
/// [`AdoptRefusal::WillNotExec`], and only that: nothing here judges the
/// answer it read.
fn ask(
    path: &Path,
    flag: &str,
    home: &Path,
    name: &str,
    budget: Duration,
) -> Result<Option<String>, AdoptRefusal> {
    // `env_clear` and the daemon's own allowlist, never the operator's
    // environment: this runs a stranger's binary. `SHEP_HOME` is the home
    // this invocation resolved, or the candidate finds the live daemon's
    // socket. Stdout is piped so it cannot write on the operator's terminal.
    let mut command = Command::new(path);
    command
        .arg(flag)
        .env_clear()
        .envs(dog_env(home, name))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A group of the probe's own, whose id is its own pid: without one
    // `kill_probe_tree` has no group to name and reaches only the leader.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }

    match command.spawn() {
        Err(err) => Err(AdoptRefusal::WillNotExec {
            reason: err.to_string(),
        }),
        Ok(mut child) => {
            // Started before any wait: a candidate that writes more than a
            // pipe buffer holds would otherwise block on its own `write`,
            // indistinguishable from one that never exits.
            let reading = child.stdout.take().map(read_in_background);
            if let Some(reason) = macos_deferred_exec_failure(&mut child) {
                let _ = child.wait();
                return Err(AdoptRefusal::WillNotExec { reason });
            }
            let answer = answer_text(&mut child, reading, budget);
            kill_probe_tree(&mut child);
            let _ = child.wait();
            Ok(answer)
        }
    }
}

/// SIGKILLs the probe, and on unix everything it forked
///
/// A dog that does not recognise the flag runs its ordinary job instead, so
/// a probe can fork before it answers. `Child::kill` reaches only the
/// leader. A descendant that calls `setsid` leaves the group and survives.
///
/// Failure is never reported: an empty group answers `ESRCH`, and the
/// caller's answer is the same either way.
fn kill_probe_tree(child: &mut Child) {
    // `Child` skips the syscall once it holds a status, so a child already
    // reaped here is never signalled through a pid the OS may have recycled.
    let _ = child.kill();
    // POSIX holds a group id out of the pool while the group has members, so
    // a sweep with forks to reach cannot name a stranger's group. An empty
    // one answers ESRCH, short of a pid wrap inside the microseconds since
    // the reap. `-0` is `0`, this process's own group, so zero must not pass.
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id())
        && pid > 0
    {
        let group = nix::unistd::Pid::from_raw(-pid);
        let _ = nix::sys::signal::kill(group, nix::sys::signal::Signal::SIGKILL);
    }
}

/// Asks `path` for its version with [`VERSION_FLAG`] and parses the answer.
///
/// `Ok(None)` is an unknown protocol and never a fault: everything [`ask`]
/// answers `None` for, plus output with no line 1 to read a version from.
/// [`warn_of_a_dog_a_restart_would_break`] asks a dog adopted long ago,
/// since the binary changes on disk with nothing watching. Neither caller
/// writes the answer down.
///
/// # Errors
/// [`AdoptRefusal::WillNotExec`], and only that.
fn ask_version(
    path: &Path,
    home: &Path,
    name: &str,
    budget: Duration,
) -> Result<Option<DogVersion>, AdoptRefusal> {
    Ok(ask(path, VERSION_FLAG, home, name, budget)?
        .as_deref()
        .and_then(parse_version_answer))
}

/// Asks `path` for its config schema with [`SCHEMA_FLAG`], and reads the
/// answer as JSON.
///
/// `pub(crate)`: `shep lookout`'s dog config pane calls this too.
///
/// No `Result`: nothing a candidate does to this probe can refuse an adopt.
/// A failure to spawn arrives as [`DogSchema::Silent`], like a dog that has
/// never heard of the flag.
///
/// The answer is written down by nothing. `cargo install` replaces a dog's
/// binary with nothing watching, and a stale schema mislabels which field is
/// a credential.
pub(crate) fn ask_schema(path: &Path, home: &Path, name: &str, budget: Duration) -> DogSchema {
    // Empty output is a dog with no schema, not a schema that failed to
    // parse: empty input is invalid JSON, so without the guard the ordinary
    // case earns the warning meant for a broken one.
    match ask(path, SCHEMA_FLAG, home, name, budget) {
        Ok(Some(text)) if !text.trim().is_empty() => match serde_json::from_str(&text) {
            Ok(schema) => DogSchema::Published(schema),
            Err(_) => DogSchema::Unreadable,
        },
        Ok(_) | Err(_) => DogSchema::Silent,
    }
}

/// A binary [`vet_binary`] accepted, and what an operator should still be
/// told about it.
#[derive(Debug, PartialEq, Eq)]
pub struct VettedBinary {
    /// The absolute, canonicalized path: the one `adopt` records and the
    /// daemon later exec's.
    pub path: PathBuf,
    /// The paths [`writability`] found group-writable: the binary, its
    /// directory, both, or neither. One notice each, never a refusal.
    pub group_writable: Vec<PathBuf>,
    /// What it answered when asked for its version, and `None` when it
    /// answered nothing shep could read. Reported, never recorded.
    pub answer: Option<DogVersion>,
    /// What it answered when asked for its config schema. Written down by
    /// nobody, for the reason [`ask_schema`] gives.
    pub schema: DogSchema,
}

/// What a candidate answered when asked for its config schema. Three
/// answers, since only one of the two ways of having no schema is worth
/// telling an operator about. Nothing here is a refusal.
#[derive(PartialEq, Eq)]
pub enum DogSchema {
    /// The dog printed JSON, exactly as it wrote it. Not validated past
    /// being JSON: the dog is the authority on its own config.
    Published(serde_json::Value),
    /// The dog answered nothing shep can use: it printed nothing, its run
    /// failed, it never exited, or it could not be spawned. Earns no
    /// warning.
    Silent,
    /// The dog printed something that is not JSON: a bug in that dog, and
    /// the one shape `adopt` warns about.
    Unreadable,
}

/// Reports that there is a schema, never what is in it.
///
/// A schema carries the dog's own defaults, the same field the secret
/// marker exists to keep off a screen, so a derive would put a credential
/// into any `{vetted:?}`.
impl core::fmt::Debug for DogSchema {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Published(_) => f.write_str("Published(..)"),
            Self::Silent => f.write_str("Silent"),
            Self::Unreadable => f.write_str("Unreadable"),
        }
    }
}

/// How long [`ask`] gives a binary to answer one probe flag, and separately
/// how long it then waits for that answer to reach the reader thread. Each
/// flag is asked in its own spawn and gets the whole budget.
///
/// One second, against the milliseconds a `println!` and an exit take. The
/// headroom is for a cold, dynamically linked binary on a loaded machine,
/// where a probe takes 180 to 300ms against single-digit milliseconds idle.
/// Too short records an unknown protocol for a slow dog; too long stalls
/// every adopt of the dogs that exist today, none of which answer at all.
///
/// `restart` asks with the same number. A binary that hangs is killed at the
/// budget and the restart proceeds unwarned.
pub(crate) const VERSION_BUDGET: Duration = Duration::from_secs(1);

/// How often [`answer_text`] polls within [`VERSION_BUDGET`].
const VERSION_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// The most a probe will read from a candidate, per spawn.
///
/// One mebibyte, against a version answer of two lines and a JSON Schema of
/// single-digit kilobytes. Without a bound the read is limited only by the
/// budget and the candidate's write speed, which reached roughly 290MB
/// resident for a second of spew.
///
/// Truncation is never a refusal: a cut-off version answer is the unknown
/// protocol it already was, and a cut-off schema is
/// [`DogSchema::Unreadable`].
const PROBE_OUTPUT_LIMIT: u64 = 1024 * 1024;

/// Drains `stdout` to end on a thread, handing the text back through the
/// returned channel.
///
/// A thread, because neither read can be bounded: before the candidate
/// exits a read blocks until it writes, and after it exits a read blocks
/// for as long as anything it spawned still holds the inherited pipe open.
/// On the timeout path the thread ends when the pipe closes, holding only a
/// `String` and a sender.
fn read_in_background(stdout: std::process::ChildStdout) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut text = String::new();
        // The drop that follows is half of what the bound buys: the read
        // end closes, so a candidate still writing takes an EPIPE. Non-UTF-8
        // bytes read as silence, a character the cap cut in half included:
        // `read_to_string` leaves `text` empty when it fails.
        let _ = stdout.take(PROBE_OUTPUT_LIMIT).read_to_string(&mut text);
        let _ = tx.send(text);
    });
    rx
}

/// Waits, bounded by `budget`, for `child` to answer, and returns what it
/// printed.
///
/// Twice `budget` is the worst case: the wait for the exit and the wait for
/// the reader thread's text are bounded separately. Only a child that has
/// already exited successfully reaches the second, which is there for a
/// grandchild still holding the inherited pipe open.
///
/// `None` is no answer, never a refusal: no pipe to read, no exit inside the
/// budget, or a non-zero exit. `docs/dogs.md` asks a dog to answer on stdout
/// and exit 0, so lines from a run that then failed are not an answer.
/// `Some` may still be empty.
fn answer_text(
    child: &mut Child,
    reading: Option<Receiver<String>>,
    budget: Duration,
) -> Option<String> {
    let reading = reading?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() >= budget => return None,
            Ok(None) => std::thread::sleep(VERSION_POLL_INTERVAL),
        }
    }
    reading.recv_timeout(budget).ok()
}

/// Who besides the owner can write `canonical` and the directory holding it.
///
/// World-writable is unambiguous, so it refuses. Group-writable is not: a
/// deployment directory owned by a trusted deploy group is a normal
/// arrangement, so it comes back as a path to warn about and the adopt
/// proceeds. The sticky bit is not an exemption. A path with no parent
/// (`/` itself) cannot be a file and never reaches here.
///
/// # Errors
/// [`AdoptRefusal::WorldWritable`], naming whichever of the two paths it
/// found first, the binary before its directory, so the more specific thing
/// to fix is the one reported.
fn writability(canonical: &Path) -> Result<Vec<PathBuf>, AdoptRefusal> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // Only the `cfg(unix)` push below writes this.
    #[cfg_attr(windows, allow(unused_mut))]
    let mut group_writable = Vec::new();
    for candidate in [Some(canonical), canonical.parent()].into_iter().flatten() {
        // Unreadable metadata is not a refusal: this check has nothing to
        // say about a directory whose mode cannot be read.
        let Ok(metadata) = std::fs::metadata(candidate) else {
            continue;
        };
        // The Windows analogue is an ACE granting write to a broad group,
        // which needs a real ACL read. `shep adopt` does not check it
        // there, and the operator docs say so.
        #[cfg(windows)]
        let _ = &metadata;
        #[cfg(unix)]
        let mode = metadata.permissions().mode();
        #[cfg(unix)]
        if mode & 0o002 != 0 {
            return Err(AdoptRefusal::WorldWritable {
                path: candidate.to_path_buf(),
            });
        }
        #[cfg(unix)]
        if mode & 0o020 != 0 {
            group_writable.push(candidate.to_path_buf());
        }
    }
    Ok(group_writable)
}

/// How long [`macos_deferred_exec_failure`] gives a spawned probe to prove
/// it cannot run, before treating it as a real, running binary.
///
/// 50ms, next to the ~3ms this module's tests observe the kernel's fallback
/// taking; generous against scheduler contention. A binary that does run
/// polls for the whole 50ms, since nothing here can prove a negative early.
#[cfg(target_os = "macos")]
const PROBE_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

/// How often [`macos_deferred_exec_failure`] polls within [`PROBE_BUDGET`].
#[cfg(target_os = "macos")]
const PROBE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_micros(500);

/// Catches the one way [`vet_binary`]'s `Command::spawn` can succeed for a
/// file this kernel cannot actually run.
///
/// macOS's `posix_spawn` fast path is not synchronous for an exec-format
/// failure: the fork has already happened when `spawn` returns `Ok`, and a
/// file the kernel cannot recognize is re-executed through `/bin/sh`, which
/// refuses it and exits `126`. That happens within a few milliseconds, well
/// inside [`PROBE_BUDGET`], while a runnable binary is still running.
#[cfg(target_os = "macos")]
fn macos_deferred_exec_failure(child: &mut std::process::Child) -> Option<String> {
    let start = std::time::Instant::now();
    while start.elapsed() < PROBE_BUDGET {
        match child.try_wait() {
            Ok(Some(status)) if status.code() == Some(126) => {
                return Some(
                    "this kernel could not recognize the file as an executable".to_string(),
                );
            }
            // A real, if fast, run, or a failed wait: neither is this
            // function's to report.
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => std::thread::sleep(PROBE_POLL_INTERVAL),
        }
    }
    None
}

/// Every kernel but macOS reports an exec-format failure through
/// `Command::spawn`'s `Err` arm, so there is nothing to catch.
#[cfg(not(target_os = "macos"))]
fn macos_deferred_exec_failure(_child: &mut std::process::Child) -> Option<String> {
    None
}

/// Renders `refusal` and returns the exit code an unvettable binary
/// reports: [`ExitCode::InvalidConfig`] for every mode, since what is wrong
/// is the argument `adopt` was given, not shep's own state.
pub(super) fn fail_adopt(
    streams: &mut Streams<'_>,
    path: &Path,
    refusal: &AdoptRefusal,
) -> ExitCode {
    let code = ExitCode::InvalidConfig;
    let message = format!("{}: {refusal}", path.display());
    streams.fail(code, &message)
}

/// [`emit_notice`] code for the group-writable warning: caller-defined, and
/// not one of [`ExitCode::code_str`]'s, since `adopt` still succeeds here.
const GROUP_WRITABLE_NOTICE: &str = "group_writable";

/// Warns that `path` is group-writable, and lets the adopt proceed.
///
/// Goes out through [`emit_notice`] rather than [`emit_error`] so a
/// `--format json` consumer can tell a diagnostic on a successful command
/// from a failure.
pub(super) fn warn_group_writable(streams: &mut Streams<'_>, path: &Path) {
    let message = format!(
        "{} is writable by its group; anyone in that group can replace the binary \
         this dog runs, and it runs with the shepherd's own privileges",
        path.display()
    );
    streams.aside(GROUP_WRITABLE_NOTICE, &message);
}

/// [`emit_notice`] code for the version report; like
/// [`GROUP_WRITABLE_NOTICE`], not a failure.
const DOG_VERSION_NOTICE: &str = "dog_version";

/// Tells the operator what the candidate answered.
///
/// The version is reported, never compared. The protocol is reported only
/// when it is missing, the operator's one chance to hear that this dog's
/// compatibility is unknown until it connects. A dog that answered nothing
/// gets no notice: that is the ordinary case.
pub(super) fn report_dog_version(streams: &mut Streams<'_>, name: &str, answer: &DogVersion) {
    let message = match answer.protocol {
        Some(protocol) => format!(
            "{name} reports version {}, shep protocol {protocol}",
            answer.version
        ),
        None => format!(
            "{name} reports version {} and names no shep protocol, so whether it can \
             speak to this shep is unknown until it connects",
            answer.version
        ),
    };
    streams.aside(DOG_VERSION_NOTICE, &message);
}

/// [`emit_notice`] code for the unreadable-schema warning; not a failure.
const DOG_SCHEMA_UNREADABLE_NOTICE: &str = "dog_schema_unreadable";

/// Warns that `name` answered the schema flag with something that is not
/// JSON, and lets the adopt proceed.
///
/// The one schema answer worth a line. Silence is the ordinary case, and a
/// notice for each silent dog is how an operator learns to skip the one that
/// matters.
pub(super) fn warn_unreadable_schema(streams: &mut Streams<'_>, name: &str) {
    let message = format!(
        "{name} answered `{SCHEMA_FLAG}` with something that is not JSON, so shep has \
         no description of its settings and they stay a hand-edited section. The dog \
         is adopted and runs normally; this is a bug to report to whoever wrote it"
    );
    streams.aside(DOG_SCHEMA_UNREADABLE_NOTICE, &message);
}

/// [`emit_notice`](crate::output::emit_notice) code for the warning
/// `restart` prints before it restarts a dog whose binary on disk cannot
/// speak to this shepherd. Not a failure: the restart still happens.
const DOG_BINARY_SKEW_NOTICE: &str = "dog_binary_skew";

/// Warns, before `restart` sends anything, about a dog whose binary on disk
/// speaks a protocol below [`MIN_SUPPORTED`], the floor this shepherd's
/// handshake still accepts.
///
/// The running dog works, the binary it would come back from does not, and
/// the two meet at the next restart. A warning, never a refusal: the binary
/// may be exactly what the operator just installed. A dog built against a
/// newer protocol than this shep is inside the window and gets no warning.
/// A dog that does not answer [`VERSION_FLAG`] is unknown rather than stale
/// and gets nothing.
///
/// Only a `Name` selector is probed: a built-in dog has no `[daemon]
/// adopted_dogs` entry, an `all` or `/regex/` sweep names no dog, and an
/// `Id` would cost a round trip. `budget` is a parameter for the reason
/// [`vet_binary_within`] is.
pub fn warn_of_a_dog_a_restart_would_break(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    selectors: &[SelectorSpec],
    budget: Duration,
) {
    for selector in selectors {
        let SelectorSpec::Name(name) = selector else {
            continue;
        };
        // `None` for every name `[daemon] adopted_dogs` has never heard
        // of, which is every built-in dog and every sheep.
        let Ok(Some(binary)) = ShepToml::adopted_dog_path_readonly(&paths.daemon_config, name)
        else {
            continue;
        };
        // Asked, never remembered: a protocol recorded at adopt time would
        // copy a number that changes on disk with nothing watching. A dog
        // that ignores `--version` runs its ordinary job instead, for up to
        // `budget`; `docs/dogs.md` names that cost.
        let Ok(Some(answer)) = ask_version(&binary, &paths.home, name, budget) else {
            continue;
        };
        let Some(disk) = answer.protocol else {
            continue;
        };
        if disk >= MIN_SUPPORTED {
            continue;
        }
        let message = format!(
            "`{name}`'s binary at {} was built for shep protocol {disk}, and this shep \
             needs {MIN_SUPPORTED} or newer; restarting it brings it back on that binary, \
             unable to connect. Run a shep that accepts protocol {disk}, or reinstall the \
             dog against protocol {MIN_SUPPORTED}, and restart it again",
            binary.display()
        );
        streams.aside(DOG_BINARY_SKEW_NOTICE, &message);
    }
}

// `unix` because the adopt-vetting cases read execute bits and the
// world-writable bit. What Windows claims instead is covered by
// `tests/cli_e2e.rs`.
#[cfg(all(test, unix))]
mod tests;
