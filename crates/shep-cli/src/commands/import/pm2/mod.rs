//! `shep import pm2`: reading a pm2 dump into a Flockfile.
//!
//! [`dump`] parses a `dump.pm2` document into rows; [`mod@env`] splits a
//! row's environment into what a Flockfile carries and what the operator
//! must decide about; [`convert`] collapses rows into apps; [`render`]
//! serializes them as Flockfile TOML. [`import`] glues the four together.

pub(crate) mod convert;
pub(crate) mod dump;
pub(crate) mod env;
mod render;

use std::path::PathBuf;

use convert::ImportNote;

use crate::cli::ImportPm2Args;
use crate::exit::ExitCode;
use crate::output::{ImportRow, ImportRows, Streams, emit, write_outcome};

/// Reads a pm2 dump into apps and writes them out as a Flockfile.
///
/// Connects to nothing: reads one file, writes another.
///
/// Source is `args.from`, or `$HOME/.pm2/dump.pm2`. Output is `args.out`, or
/// `./Flockfile.toml`, never overwritten without `args.force`. Neither
/// resolving, and an existing output, are [`ExitCode::Usage`]. `args.dry_run`
/// prints the Flockfile to stdout with no envelope and writes nothing: the
/// output must parse back as a Flockfile.
///
/// Every [`ImportNote`] goes to stderr under both formats, after a read
/// summary line.
pub fn import(streams: &mut Streams<'_>, args: &ImportPm2Args) -> ExitCode {
    let source = match resolve_source(args) {
        Ok(source) => source,
        Err(message) => {
            return streams.fail(ExitCode::Usage, &message);
        }
    };

    let text = match std::fs::read_to_string(&source) {
        Ok(text) => text,
        Err(err) => {
            let message = if err.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "no dump at {}; `pm2 save` writes one there",
                    source.display()
                )
            } else {
                format!("could not read {}: {err}", source.display())
            };
            return streams.fail(ExitCode::Usage, &message);
        }
    };

    let rows = match dump::parse(&text) {
        Ok(rows) => rows,
        Err(err) => {
            return streams.fail(ExitCode::InvalidConfig, &err.to_string());
        }
    };
    let instance_count = rows.len();

    let imported = match convert::convert(rows) {
        Ok(imported) => imported,
        Err(err) => {
            return streams.fail(ExitCode::InvalidConfig, &err.to_string());
        }
    };

    streams.aside(
        "read",
        &format!(
            "read {instance_count} instance rows for {} apps from {}",
            imported.apps.len(),
            source.display(),
        ),
    );
    for note in &imported.notes {
        let (code, message) = describe_note(note);
        streams.aside(code, &message);
    }

    let rendered = match render::flockfile(&imported.apps) {
        Ok(rendered) => rendered,
        Err(err) => {
            return streams.fail(ExitCode::Failure, &err.to_string());
        }
    };

    if args.dry_run {
        return write_outcome(streams.out.write_all(rendered.as_bytes()));
    }

    let out_path = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("Flockfile.toml"));
    // `--force` truncates unconditionally: `std::fs::write` is exactly right.
    // Without it, a plain `exists()` check followed by a write is a
    // check-then-act race, so a file created between the two would be
    // clobbered even though `--force` was never passed. `create_new(true)`
    // makes the refusal and the write one atomic syscall instead.
    if args.force {
        if let Err(err) = std::fs::write(&out_path, &rendered) {
            return streams.fail(ExitCode::Failure, &err.to_string());
        }
    } else {
        let result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out_path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, rendered.as_bytes()));
        if let Err(err) = result {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                let message = format!(
                    "{} already exists; pass --force to overwrite it",
                    out_path.display()
                );
                return streams.fail(ExitCode::Usage, &message);
            }
            return streams.fail(ExitCode::Failure, &err.to_string());
        }
    }

    let rows = ImportRows(
        imported
            .apps
            .iter()
            .map(|app| ImportRow {
                name: app.name.clone(),
                script: app.script.clone(),
                instances: app.instances,
                reuse_port: app.reuse_port,
            })
            .collect(),
    );
    write_outcome(emit(
        &mut *streams.out,
        streams.fmt,
        "import",
        rows,
        streams.style,
    ))
}

/// `args.from`, or `~/.pm2/dump.pm2` if it names nothing.
///
/// Which variables name that `~` is [`shep_core::paths::user_home`]'s
/// question, and differs by platform.
///
/// # Errors
/// A message naming both `--from` and the home variable when neither
/// resolves a path. `--home`/`$SHEP_HOME` can resolve `ShepPaths` while no
/// home directory resolves at all.
fn resolve_source(args: &ImportPm2Args) -> Result<PathBuf, String> {
    #[cfg(not(windows))]
    const NO_DUMP: &str = "neither --from nor $HOME names a dump to read; pass --from, or set \
                           $HOME so ~/.pm2/dump.pm2 resolves";
    #[cfg(windows)]
    const NO_DUMP: &str = "neither --from nor %USERPROFILE% names a dump to read; pass --from, \
                           or set %USERPROFILE% so ~/.pm2/dump.pm2 resolves";

    if let Some(from) = &args.from {
        return Ok(from.clone());
    }
    shep_core::paths::user_home(&|key| std::env::var_os(key))
        .map(|home| home.join(".pm2").join("dump.pm2"))
        .ok_or_else(|| NO_DUMP.to_string())
}

/// [`ImportNote`]'s stderr rendering: a stable code for `--format json`'s
/// `notice.code`, and the human message.
fn describe_note(note: &ImportNote) -> (&'static str, String) {
    match note {
        ImportNote::ClusterMode { app, instances } => (
            "cluster_mode",
            format!(
                "{app} ran {instances} instances in pm2 cluster mode; shep binds nothing, so \
                 {app} must set SO_REUSEPORT itself (Node's reusePort: true, needing Node >= \
                 22.12) or every instance past the first hits EADDRINUSE at start"
            ),
        ),
        ImportNote::InheritedEnv { app, key } => (
            "inherited_env",
            format!(
                "{app}: {key} was running but neither declared nor recognized session junk; \
                 decide whether it belongs in the Flockfile, the unit, or nowhere"
            ),
        ),
        ImportNote::UnrepresentableEnv { app, key } => (
            "unrepresentable_env",
            format!(
                "{app}: {key}'s value is not a string, number, or boolean, which a Flockfile \
                 env cannot hold, so it was dropped"
            ),
        ),
        ImportNote::InstanceVar { app, var } => (
            "instance_var",
            format!(
                "{app}: reads its instance number from ${var}; imported as \
                 {var} = \"{{{{instance}}}}\" under [app.env] rather than copied as a value"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use shep_core::config::{FlockFormat, Flockfile};

    use super::*;
    use crate::cli::Format;

    fn args(dump: &Path, out: &Path, dry_run: bool, force: bool) -> ImportPm2Args {
        ImportPm2Args {
            from: Some(dump.to_path_buf()),
            out: Some(out.to_path_buf()),
            dry_run,
            force,
        }
    }

    /// Runs `import` against a bare table-format `Streams` and hands back
    /// its exit code beside everything it wrote.
    fn run_import(args: &ImportPm2Args) -> (ExitCode, Vec<u8>, Vec<u8>) {
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out_buf,
                err: &mut err_buf,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            import(&mut streams, args)
        };
        (code, out_buf, err_buf)
    }

    #[test]
    fn an_existing_flockfile_is_not_overwritten_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("Flockfile.toml");
        std::fs::write(&out, "[[app]]\nname = \"mine\"\nscript = \"./mine\"\n").unwrap();
        let dump = dir.path().join("dump.pm2");
        std::fs::write(&dump, include_str!("testdata/dump.pm2.json")).unwrap();

        let (code, _out_buf, _err_buf) = run_import(&args(&dump, &out, false, false));
        assert_eq!(code, ExitCode::Usage);
        assert!(std::fs::read_to_string(&out).unwrap().contains("mine"));
    }

    #[test]
    fn dry_run_prints_a_parseable_flockfile_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("dump.pm2");
        std::fs::write(&dump, include_str!("testdata/dump.pm2.json")).unwrap();
        let out = dir.path().join("Flockfile.toml");

        let (code, out_buf, _err_buf) = run_import(&args(&dump, &out, true, false));
        assert_eq!(code, ExitCode::Success);
        assert!(!out.exists(), "--dry-run writes nothing");
        let printed = String::from_utf8(out_buf).unwrap();
        assert_eq!(
            Flockfile::parse(&printed, FlockFormat::Toml)
                .unwrap()
                .apps
                .len(),
            3,
            "`shep import --dry-run > Flockfile.toml` must produce a file \
             shep can read back: {printed}"
        );
    }

    #[test]
    fn the_report_names_every_cluster_app_and_every_ambiguous_key() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("dump.pm2");
        std::fs::write(&dump, include_str!("testdata/dump.pm2.json")).unwrap();
        let out = dir.path().join("Flockfile.toml");

        let (_code, _out_buf, err_buf) = run_import(&args(&dump, &out, true, false));
        let report = String::from_utf8(err_buf).unwrap();
        assert!(report.contains("api"), "{report}");
        assert!(report.contains("SO_REUSEPORT"), "{report}");
        for key in ["BUN_INSTALL", "JAVA_HOME", "DATABASE_URL"] {
            assert!(report.contains(key), "{key} was never named: {report}");
        }
    }

    #[test]
    fn a_missing_dump_is_a_named_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("nope.pm2");
        let out = dir.path().join("Flockfile.toml");

        let (code, _out_buf, err_buf) = run_import(&args(&dump, &out, false, false));
        assert_eq!(code, ExitCode::Usage);
        let report = String::from_utf8(err_buf).unwrap();
        assert!(report.contains(&dump.display().to_string()), "{report}");
        assert!(report.contains("pm2 save"), "{report}");
    }

    #[test]
    fn a_malformed_dump_is_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("dump.pm2");
        std::fs::write(&dump, "not json").unwrap();
        let out = dir.path().join("Flockfile.toml");

        let (code, _out_buf, _err_buf) = run_import(&args(&dump, &out, false, false));
        assert_eq!(code, ExitCode::InvalidConfig);
    }
}
