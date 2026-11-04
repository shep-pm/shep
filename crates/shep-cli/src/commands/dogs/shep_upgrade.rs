//! Bare `shep upgrade`: shep through the channel it came from, the shepherd
//! reload, then every installed dog the index has a newer version of.
//!
//! One confirm lists the lot. The channel's manager runs in the foreground
//! with the terminal, so its prompts and `sudo` work. The reload runs the
//! `shep` found on `PATH`, because this process holds the old code and the
//! manager may have removed its path.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use semver::Version;
use shep_client::Client;
use shep_core::paths::ShepPaths;

use crate::cli::{Format, UpgradeArgs};
use crate::commands::shep_toml::{ShepToml, ShepTomlError};
use crate::dog_index::{AvailableDog, BUILD_TARGET};
use crate::dog_origin::Origins;
use crate::exit::ExitCode;
use crate::install_channel::Channel;
use crate::output::Streams;

use super::adopt::lookup_on_path;
use super::consent::Input;
use super::dog_upgrade::{Goal, upgrade_dog};
use super::fail_config;
use super::install::{Reach, load_index};
use super::install_depends::Catalog;

/// The command that reloads the shepherd onto the shep on `PATH`.
const RELOAD_COMMAND: &str = "shep daemon reload";

/// What this shep is and how it was installed, as far as upgrading it asks.
#[derive(Debug)]
pub(super) struct Host {
    /// How the running shep got here.
    pub(super) channel: Channel,
    /// Whether this is Windows, where no manager runs.
    pub(super) windows: bool,
    /// The triple the running shep was built for, which names its archive.
    pub(super) target: &'static str,
    /// The `PATH` the manager and the new `shep` are looked up on.
    pub(super) path_var: Option<OsString>,
    /// The version of the running shep.
    pub(super) running: Version,
}

impl Host {
    /// This process.
    pub(super) fn real() -> Self {
        Self {
            channel: Channel::running(),
            windows: cfg!(windows),
            target: BUILD_TARGET,
            path_var: std::env::var_os("PATH"),
            running: Version::parse(env!("CARGO_PKG_VERSION"))
                .expect("Cargo only builds a package with a semver version"),
        }
    }
}

/// Everything one bare `shep upgrade` reads.
#[derive(Debug)]
pub(super) struct Upgrade<'a> {
    pub(super) paths: &'a ShepPaths,
    pub(super) host: &'a Host,
    pub(super) reach: &'a Reach,
    pub(super) catalog: &'a Catalog<'a>,
    /// Where each adopted dog stands against the index.
    pub(super) origins: &'a Origins<'a>,
    /// The latest shep the index names, if it names one.
    pub(super) shep: Option<&'a Version>,
    /// `--reload`: reload the shepherd without asking.
    pub(super) reload: bool,
}

/// `shep upgrade` with no dog and no `--dogs`.
pub(super) async fn bare(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &UpgradeArgs,
) -> ExitCode {
    if streams.fmt == Format::Json {
        return streams.fail(
            ExitCode::Usage,
            "bare `shep upgrade` asks before it changes anything and has no JSON form; \
             `shep upgrade --dogs --format json` upgrades the dogs and reports each",
        );
    }
    let (url, loaded) = match load_index(streams, paths, args.refresh).await {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    let rows: Vec<AvailableDog> = loaded.index.dogs.clone();
    let latest = loaded.index.shep.clone();
    let origins = Origins::over(paths, Some(loaded.index));
    let host = Host::real();
    run(
        streams,
        &mut Input::stdin(),
        &Upgrade {
            paths,
            host: &host,
            reach: &Reach::real(),
            catalog: &Catalog::new(&url, &rows),
            origins: &origins,
            shep: latest.as_ref(),
            reload: args.reload,
        },
    )
    .await
}

/// What the confirm lists.
struct Plan {
    /// The latest shep, when it is newer than the running one.
    shep: Option<Version>,
    dogs: Vec<DogStep>,
}

/// One dog the index has a newer version of.
struct DogStep {
    name: String,
    /// The installed version, `-` when nothing recorded it.
    from: String,
    to: String,
}

impl Plan {
    fn of(up: &Upgrade<'_>) -> Result<Self, ShepTomlError> {
        let cfg = ShepToml::read_only(&up.paths.daemon_config)?;
        let dogs = cfg
            .adopted_dog_names()
            .into_iter()
            .filter_map(|name| {
                let path = cfg.adopted_dog_path(&name)?;
                let origin = up.origins.of(&name, Some(&path));
                Some(DogStep {
                    from: origin.version.unwrap_or_else(|| "-".to_owned()),
                    to: origin.newer?,
                    name,
                })
            })
            .collect();
        Ok(Self {
            shep: up.shep.filter(|latest| **latest > up.host.running).cloned(),
            dogs,
        })
    }

    fn is_empty(&self) -> bool {
        self.shep.is_none() && self.dogs.is_empty()
    }

    /// One line per step: names, old versions and new versions in columns.
    fn listing(&self, host: &Host) -> String {
        let mut rows = Vec::new();
        if let Some(latest) = &self.shep {
            rows.push((
                "shep",
                host.running.to_string(),
                latest.to_string(),
                host.channel.command(),
            ));
        }
        for dog in &self.dogs {
            rows.push((dog.name.as_str(), dog.from.clone(), dog.to.clone(), None));
        }
        let name_width = rows.iter().map(|row| row.0.len()).max().unwrap_or_default();
        let from_width = rows.iter().map(|row| row.1.len()).max().unwrap_or_default();
        let mut text = String::new();
        for (name, from, to, command) in rows {
            text.push_str(&format!(
                "{name:<name_width$}  {from:<from_width$} \u{2192} {to}"
            ));
            if let Some(command) = command {
                text.push_str(&format!("   {command}"));
            }
            text.push('\n');
        }
        text
    }
}

/// Plans, confirms, upgrades shep, offers the reload, then upgrades the dogs.
/// The exit code is the first failure, or success.
pub(super) async fn run(
    streams: &mut Streams<'_>,
    input: &mut Input,
    up: &Upgrade<'_>,
) -> ExitCode {
    let plan = match Plan::of(up) {
        Ok(plan) => plan,
        Err(err) => return fail_config(streams, &err),
    };
    if plan.is_empty() {
        let _ = writeln!(streams.out, "shep and every dog are current");
        return ExitCode::Success;
    }
    let _ = write!(streams.out, "{}", plan.listing(up.host));
    let _ = write!(streams.err, "Upgrade these? [Y/n] ");
    let _ = streams.err.flush();
    match yes_by_default(input) {
        Ok(true) => {}
        Ok(false) => return ExitCode::Success,
        Err(reason) => {
            return streams.fail(
                ExitCode::Failure,
                &format!("nothing was changed: the question could not be read ({reason})"),
            );
        }
    }
    let mut first_failure = ExitCode::Success;
    if let Some(latest) = &plan.shep {
        match upgrade_shep(streams, input, up, latest).await {
            Ok(code) => first_failure = code,
            Err(code) => return code,
        }
    }
    for dog in &plan.dogs {
        let code = upgrade_dog(
            streams,
            up.paths,
            up.reach,
            input,
            up.catalog,
            &dog.name,
            &Goal::Follow,
        )
        .await;
        if first_failure == ExitCode::Success {
            first_failure = code;
        }
    }
    first_failure
}

/// Reads a line: Enter, `y` and `yes` are a yes, end of input and anything
/// else a no.
fn yes_by_default(input: &mut Input) -> Result<bool, String> {
    match input.read_line() {
        Ok(Some(line)) => Ok(matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "" | "y" | "yes"
        )),
        Ok(None) => Ok(false),
        Err(err) => Err(err.to_string()),
    }
}

/// Upgrades shep itself, then offers the reload. `Err` stops the run before
/// the dogs; `Ok` carries a failure that does not.
async fn upgrade_shep(
    streams: &mut Streams<'_>,
    input: &mut Input,
    up: &Upgrade<'_>,
    latest: &Version,
) -> Result<ExitCode, ExitCode> {
    let host = up.host;
    if host.windows {
        // The running shepherd holds shep.exe, and with no `execve` a reload
        // stops and starts the flock.
        match host.channel.command() {
            Some(command) => {
                let _ = writeln!(
                    streams.out,
                    "Run `{command}` to upgrade shep. shep does not run it here: the shepherd \
                     holds shep.exe."
                );
            }
            None => print_steps(streams, host),
        }
        let _ = writeln!(
            streams.out,
            "Then run `{RELOAD_COMMAND}`. The flock restarts, because Windows cannot hand it over."
        );
        return Ok(ExitCode::Success);
    }
    let Some(command) = host.channel.command() else {
        print_steps(streams, host);
        let _ = writeln!(
            streams.out,
            "Then `{RELOAD_COMMAND}` moves the shepherd onto {latest}."
        );
        return Ok(ExitCode::Success);
    };
    run_manager(streams, host, &command).await?;
    let (new_shep, installed) = installed_shep(streams, host, &command).await?;
    Ok(reload(streams, input, up, &installed, &new_shep).await)
}

/// The by-hand steps of a channel with no command.
fn print_steps(streams: &mut Streams<'_>, host: &Host) {
    let _ = writeln!(streams.out, "To upgrade shep:");
    for (at, step) in host.channel.manual_steps(host.target).iter().enumerate() {
        let _ = writeln!(streams.out, "  {}. {step}", at + 1);
    }
}

/// Runs `command` with this process's terminal and waits for it.
async fn run_manager(
    streams: &mut Streams<'_>,
    host: &Host,
    command: &str,
) -> Result<(), ExitCode> {
    let mut words = command.split_whitespace();
    let program = words.next().unwrap_or_default();
    let Some(program) = lookup_on_path(Path::new(program), host.path_var.as_deref()) else {
        return Err(streams.fail(
            ExitCode::Failure,
            &format!("`{program}` is not on PATH; run `{command}` yourself"),
        ));
    };
    let _ = writeln!(streams.out, "Running `{command}`");
    let _ = streams.out.flush();
    match tokio::process::Command::new(&program)
        .args(words)
        .status()
        .await
    {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(streams.fail(
            ExitCode::Failure,
            &format!("`{command}` failed ({status}); the dogs were left alone"),
        )),
        Err(err) => Err(streams.fail(
            ExitCode::SpawnFailed,
            &format!("running {}: {err}", program.display()),
        )),
    }
}

/// The `shep` on `PATH` after the manager ran, with the version it reports,
/// once that is newer than this one.
async fn installed_shep(
    streams: &mut Streams<'_>,
    host: &Host,
    command: &str,
) -> Result<(PathBuf, Version), ExitCode> {
    let Some(found) = lookup_on_path(Path::new("shep"), host.path_var.as_deref()) else {
        return Err(streams.fail(
            ExitCode::Failure,
            &format!("`{command}` finished but there is no shep on PATH; the dogs were left alone"),
        ));
    };
    let output = tokio::process::Command::new(&found)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    let reported = output
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .find_map(|word| Version::parse(word).ok())
        });
    match reported {
        Some(version) if version > host.running => Ok((found, version)),
        Some(version) => Err(streams.fail(
            ExitCode::Failure,
            &format!(
                "`{command}` finished but the shep on PATH is still {version}: the channel has \
                 not caught up with the release yet. The dogs were left alone; try again later"
            ),
        )),
        None => Err(streams.fail(
            ExitCode::Failure,
            &format!(
                "`{command}` finished but {} did not report a version; the dogs were left alone",
                found.display()
            ),
        )),
    }
}

/// Reloads the shepherd onto `new_shep`, which reports `installed`, when one
/// answers and the operator agrees. A failed reload is the exit code but does
/// not stop the dogs.
async fn reload(
    streams: &mut Streams<'_>,
    input: &mut Input,
    up: &Upgrade<'_>,
    installed: &Version,
    new_shep: &Path,
) -> ExitCode {
    if Client::connect(&up.paths.socket).await.is_err() {
        return ExitCode::Success;
    }
    let go = if up.reload {
        true
    } else if input.is_terminal() {
        let _ = write!(
            streams.err,
            "Reload the shepherd onto {installed} now? [Y/n] "
        );
        let _ = streams.err.flush();
        yes_by_default(input).unwrap_or(false)
    } else {
        false
    };
    if !go {
        let _ = writeln!(
            streams.out,
            "The shepherd still runs the old shep. `{RELOAD_COMMAND}` moves it onto {installed}."
        );
        return ExitCode::Success;
    }
    match tokio::process::Command::new(new_shep)
        .args(["daemon", "reload"])
        .status()
        .await
    {
        Ok(status) if status.success() => ExitCode::Success,
        Ok(status) => streams.fail(
            ExitCode::Failure,
            &format!("`{RELOAD_COMMAND}` failed ({status}); run it again once the cause is fixed"),
        ),
        Err(err) => streams.fail(
            ExitCode::SpawnFailed,
            &format!("running {}: {err}", new_shep.display()),
        ),
    }
}
