//! Spawn assembly: pure functions that build [`SpawnSpec`] from app config.
//!
//! The assembler takes a validated `ResolvedApp` and produces a fully-resolved
//! [`SpawnSpec`] ready for [`ProcessRunner::spawn`](crate::runner::ProcessRunner::spawn).
//! No I/O here: the defaults, the process env, the paths, the credentials and
//! the secret store are all read by the daemon before the assembler is called.
//!
//! Two builders, and only one of them may be spawned. [`assemble`] resolves
//! every `{{secret:...}}` and refuses the spec when one will not; the private
//! `describe` is for the callers that read a spec's log paths or build its
//! prober without ever starting a process.
//!
//! Public for its two out-of-crate readers and nothing else: `tests/real_runner.rs`
//! calls [`assemble`] to build a spec it then spawns for real, and
//! [`instance_slots`]'s doc example is compiled as its own crate.

use core::convert::Infallible;
use core::fmt;
use std::collections::BTreeMap;
use std::path::PathBuf;

use shep_core::config::ResolvedApp;
use shep_core::config::template::{self, RenderError};
use shep_core::paths::ShepPaths;
use shep_core::secrets::SecretView;

use crate::privilege::Credentials;
use crate::runner::SpawnSpec;

/// Finds the `count` lowest-free instance slot numbers from an existing set.
///
/// Used to allocate instance slots for clustered apps. Assumes `existing` is
/// sorted; returns a new vector of `count` distinct slots, smallest first,
/// none of which appear in `existing`.
///
/// # Examples
///
/// ```
/// use shep_daemon::assemble::instance_slots;
///
/// assert_eq!(instance_slots(&[], 3), vec![0, 1, 2]);
/// assert_eq!(instance_slots(&[0, 2], 2), vec![1, 3]);
/// ```
#[must_use]
pub fn instance_slots(existing: &[u32], count: u32) -> Vec<u32> {
    let mut result = Vec::with_capacity(count as usize);
    let mut candidate = 0u32;

    for _ in 0..count {
        while existing.contains(&candidate) || result.contains(&candidate) {
            candidate += 1;
        }
        result.push(candidate);
        candidate += 1;
    }

    result
}

/// The env every spawned child starts from, before the app's own `env` map
/// is folded on top (app config always wins on conflict).
///
/// Without a `PATH` here, a bare program or interpreter name can never be
/// found by exec. Reads the daemon's own environment once, so this stays a
/// pure function of process state, not I/O.
fn base_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    // An empty PATH ("PATH=") is treated as absent: `Ok("")` would
    // otherwise slip through `unwrap_or_else`, and an empty PATH resolves a
    // bare program against the cwd instead of searching.
    let path = std::env::var("PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_PATH.to_string());
    env.insert("PATH".to_string(), path);
    for key in INHERITED {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    env
}

/// The `PATH` a child gets when the daemon itself has none.
#[cfg(unix)]
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// The `PATH` a child gets when the daemon itself has none.
///
/// Not expanded via `%SystemRoot%`: these literal paths are correct on
/// every standard Windows install and need no variable to resolve.
#[cfg(windows)]
const DEFAULT_PATH: &str = r"C:\Windows\system32;C:\Windows;C:\Windows\System32\Wbem";

/// Variables inherited from the daemon's own environment, on top of `PATH`.
#[cfg(unix)]
const INHERITED: &[&str] = &["HOME", "USER", "LANG", "TZ"];

/// Variables inherited from the daemon's own environment, on top of `PATH`.
///
/// Longer than the unix list because Windows children need it: many Win32
/// APIs read `%SystemRoot%` directly, and `PATHEXT`/`COMSPEC` let a child
/// resolve and run `.cmd` files at all. `TEMP`/`TMP` and the
/// `USERPROFILE`/`APPDATA`/`LOCALAPPDATA` trio are where most runtimes keep
/// per-user state. Still a closed allowlist, not inherit-everything.
#[cfg(windows)]
const INHERITED: &[&str] = &[
    "SystemRoot",
    "windir",
    "SystemDrive",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "LANG",
    "TZ",
];

/// Which of an app's fields carried a `{{secret:...}}` that would not
/// resolve, and why.
///
/// Redacted by construction (IR-41): `field` is an env key or a field's own
/// name, and [`RenderError`] quotes only the reference, the namespace and
/// the environment. Neither half can hold a secret's value.
#[non_exhaustive]
#[derive(Debug)]
pub enum AssembleError {
    /// A template in `field` could not be rendered.
    Template {
        /// The env key it was in, or the field's own name (`args`,
        /// `out_file`, `err_file`).
        field: String,
        /// Why.
        source: RenderError,
    },
}

impl AssembleError {
    /// Whether waiting could make this spec assemble.
    ///
    /// `true` only for a namespace no provider dog has pushed to yet; see
    /// [`RenderError::is_retriable`]. A caller that calls every refusal
    /// retriable turns a key nobody has set into a crash loop.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::Template { source, .. } => source.is_retriable(),
        }
    }
}

impl fmt::Display for AssembleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Template { field, source } => write!(f, "`{field}`: {source}"),
        }
    }
}

impl core::error::Error for AssembleError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Template { source, .. } => Some(source),
        }
    }
}

/// Assembles a [`SpawnSpec`] from a validated app config and instance slot.
///
/// `credentials` and `secrets` are both resolved by the caller, since a
/// passwd lookup and a store read are real I/O and this function otherwise
/// stays pure. `interpreter = None` or `Some("none")` runs the script
/// directly; `Some(path)` runs `path` with `[script, ...args]`.
///
/// Explicit `out_file`/`err_file` win over the default log path and render
/// through the same grammar as `env` and `args`; normalize already refused a
/// path that collides across instances unless `merge_logs` asked for it.
/// `SpawnSpec::stdin` carries `config.stdin` straight through: unlike
/// `channel`, nothing else turns it on.
///
/// The spec a child is spawned from, and the only one that may be: every
/// `{{secret:...}}` in it resolved. The crate-private `describe` is for a
/// caller reading a spec it will not spawn.
///
/// # Errors
///
/// - [`AssembleError::Template`]: an `env` value, an arg or an explicit log
///   path names a secret this view cannot resolve.
///   [`AssembleError::is_retriable`] says whether waiting would help.
pub fn assemble(
    app: &ResolvedApp,
    instance: u32,
    paths: &ShepPaths,
    credentials: Option<Credentials>,
    secrets: &SecretView,
) -> Result<SpawnSpec, AssembleError> {
    let name = app.config().name.clone();
    build(
        app,
        instance,
        paths,
        credentials,
        secrets.environment(),
        |value, field| {
            template::render(value, &name, instance, secrets).map_err(|source| {
                AssembleError::Template {
                    field: field.to_string(),
                    source,
                }
            })
        },
    )
}

/// [`assemble`] for a caller reading a spec rather than spawning one: a
/// value holding a `{{secret:...}}` this view cannot resolve keeps its
/// references as written instead of refusing.
///
/// Never spawn what this returns. Its callers name a sheep's log files,
/// preflight the program exec will find, or build a prober, and a refusal
/// there would cost an operator the sheep itself: `shep add` exists to
/// register a template whose secrets nobody has filled in yet, and an
/// adoption that refused one would strand a running flock.
///
/// The whole value falls back, not the one reference in it that missed, so a
/// caller cannot read a half-resolved value as a resolved one.
#[must_use]
pub(crate) fn describe(
    app: &ResolvedApp,
    instance: u32,
    paths: &ShepPaths,
    credentials: Option<Credentials>,
    secrets: &SecretView,
) -> SpawnSpec {
    let name = app.config().name.clone();
    let built: Result<SpawnSpec, Infallible> = build(
        app,
        instance,
        paths,
        credentials,
        secrets.environment(),
        |value, _| {
            Ok(template::render(value, &name, instance, secrets)
                .unwrap_or_else(|_| template::render_positional(value, &name, instance)))
        },
    );
    match built {
        Ok(spec) => spec,
        Err(never) => match never {},
    }
}

/// [`assemble`] and [`describe`] over one body: `render` is handed each
/// templated value with the field name to blame, and decides what an
/// unresolvable `{{secret:...}}` costs.
///
/// # Errors
///
/// Whatever `render` returns, at the first value it refuses.
fn build<E>(
    app: &ResolvedApp,
    instance: u32,
    paths: &ShepPaths,
    credentials: Option<Credentials>,
    environment: &str,
    mut render: impl FnMut(&str, &str) -> Result<String, E>,
) -> Result<SpawnSpec, E> {
    let config = app.config();
    let name = config.name.clone();

    // Args carry the same grammar as `env`, rendered once here before the
    // interpreter logic below decides where they land.
    let mut rendered_args = Vec::with_capacity(config.args.len());
    for value in &config.args {
        rendered_args.push(render(value, "args")?);
    }

    let (program, args) = match &config.interpreter {
        None => (config.script.clone(), rendered_args),
        Some(interp) if interp == "none" => (config.script.clone(), rendered_args),
        Some(interp) => {
            let mut interp_args = vec![config.script.clone()];
            interp_args.extend(rendered_args);
            (interp.clone(), interp_args)
        }
    };

    // Anything not seeded here is invisible to the child: tokio_runner.rs
    // calls env_clear() then envs(&spec.env). Each value renders through the
    // grammar as it is inserted.
    let mut env = base_env();
    for (key, value) in &config.env {
        let value = render(value, key)?;
        env.insert(key.clone(), value);
    }
    // Fixed names, always injected: an app that wants the slot under its
    // own var can template it, e.g. `MY_VAR = "{{instance}}"`. After the
    // app's own map, and refused by normalize, so neither can be shadowed.
    env.insert("SHEP_INSTANCE".to_string(), instance.to_string());
    env.insert("SHEP_NAME".to_string(), name.clone());
    env.insert("SHEP_ENVIRONMENT".to_string(), environment.to_string());

    let cwd = config.cwd.as_ref().map(PathBuf::from);

    let log_stem = if config.merge_logs {
        format!("{}-", name)
    } else {
        format!("{}-{}-", name, instance)
    };

    let out_file = match &config.out_file {
        Some(explicit) => PathBuf::from(render(explicit, "out_file")?),
        None => paths.logs.join(format!("{}out.log", log_stem)),
    };

    let err_file = match &config.err_file {
        Some(explicit) => PathBuf::from(render(explicit, "err_file")?),
        None => paths.logs.join(format!("{}err.log", log_stem)),
    };

    // Also implied by wait_ready or shutdown_with_message: widening this
    // must keep every term, or dropping one silently stops opening fd 3 for
    // an app that relied on it implying the channel.
    let channel = config.channel || config.wait_ready || config.shutdown_with_message;

    Ok(SpawnSpec {
        name,
        program,
        args,
        cwd,
        env,
        out_file,
        err_file,
        channel,
        stdin: config.stdin,
        credentials,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shep_core::config::{AppConfig, normalize};

    fn test_paths() -> ShepPaths {
        ShepPaths {
            home: PathBuf::from("/home/ada/.shep"),
            daemon_config: PathBuf::from("/home/ada/.shep/shep.toml"),
            dogs_config: PathBuf::from("/home/ada/.shep/dogs.toml"),
            snapshot: PathBuf::from("/home/ada/.shep/flock.json"),
            logs: PathBuf::from("/home/ada/.shep/logs"),
            pids: PathBuf::from("/home/ada/.shep/pids"),
            run: PathBuf::from("/home/ada/.shep/run"),
            socket: PathBuf::from("/home/ada/.shep/run/shep.sock"),
            barks: PathBuf::from("/home/ada/.shep/barks.jsonl"),
            kv: PathBuf::from("/home/ada/.shep/kv.json"),
            overrides: PathBuf::from("/home/ada/.shep/overrides.json"),
            secrets: PathBuf::from("/home/ada/.shep/secrets.json"),
            secrets_cache: PathBuf::from("/home/ada/.shep/secrets-cache.json"),
        }
    }

    /// A view holding nothing, in the environment a host defaults to.
    fn no_secrets() -> SecretView {
        SecretView::empty("production".to_string())
    }

    /// A view holding exactly `key` in `environment`, and nothing else.
    fn view_with(environment: &str, key: &str, value: &str) -> SecretView {
        SecretView::new(
            environment.to_string(),
            BTreeMap::from([(
                key.to_string(),
                BTreeMap::from([(environment.to_string(), value.to_string())]),
            )]),
            BTreeMap::new(),
        )
    }

    #[test]
    fn slots_empty_request() {
        let result = instance_slots(&[], 3);
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn slots_skip_occupied() {
        let result = instance_slots(&[0, 2], 2);
        assert_eq!(result, vec![1, 3]);
    }

    #[test]
    fn env_adds_shep_instance() {
        let app_config = AppConfig {
            name: "web".to_string(),
            script: "/usr/bin/python3".to_string(),
            args: vec!["app.py".to_string()],
            interpreter: Some("none".to_string()),
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 1, &paths, None, &no_secrets()).unwrap();

        assert!(spec.env.contains_key("SHEP_INSTANCE"));
        assert_eq!(spec.env.get("SHEP_INSTANCE").map(|s| s.as_str()), Some("1"));
    }

    #[test]
    fn every_child_learns_its_slot_and_its_name() {
        let app = normalize(AppConfig {
            name: "worker".to_string(),
            script: "bin/worker".to_string(),
            ..Default::default()
        })
        .unwrap();
        let spec = assemble(&app, 3, &test_paths(), None, &no_secrets()).unwrap();
        assert_eq!(spec.env.get("SHEP_INSTANCE").map(String::as_str), Some("3"));
        assert_eq!(
            spec.env.get("SHEP_NAME").map(String::as_str),
            Some("worker")
        );
    }

    #[test]
    fn interpreter_none_runs_script_directly() {
        let app_config = AppConfig {
            name: "app".to_string(),
            script: "/opt/bin/server".to_string(),
            args: vec!["--port".to_string(), "8080".to_string()],
            interpreter: None,
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 0, &paths, None, &no_secrets()).unwrap();

        assert_eq!(spec.program, "/opt/bin/server");
        assert_eq!(spec.args, vec!["--port", "8080"]);
    }

    #[test]
    fn interpreter_explicit_none_runs_script_directly() {
        let app_config = AppConfig {
            name: "app".to_string(),
            script: "server.py".to_string(),
            args: vec!["--verbose".to_string()],
            interpreter: Some("none".to_string()),
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 0, &paths, None, &no_secrets()).unwrap();

        assert_eq!(spec.program, "server.py");
        assert_eq!(spec.args, vec!["--verbose"]);
    }

    #[test]
    fn interpreter_path_prepends_script() {
        let app_config = AppConfig {
            name: "app".to_string(),
            script: "app.js".to_string(),
            args: vec!["--debug".to_string(), "true".to_string()],
            interpreter: Some("node".to_string()),
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 0, &paths, None, &no_secrets()).unwrap();

        assert_eq!(spec.program, "node");
        assert_eq!(spec.args, vec!["app.js", "--debug", "true"]);
    }

    #[test]
    fn merge_logs_false_uses_instance_suffix() {
        let app_config = AppConfig {
            name: "web".to_string(),
            script: "app".to_string(),
            args: vec![],
            merge_logs: false,
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 2, &paths, None, &no_secrets()).unwrap();

        assert_eq!(
            spec.out_file,
            PathBuf::from("/home/ada/.shep/logs/web-2-out.log")
        );
        assert_eq!(
            spec.err_file,
            PathBuf::from("/home/ada/.shep/logs/web-2-err.log")
        );
    }

    #[test]
    fn merge_logs_true_omits_instance_suffix() {
        let app_config = AppConfig {
            name: "api".to_string(),
            script: "api".to_string(),
            args: vec![],
            merge_logs: true,
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 1, &paths, None, &no_secrets()).unwrap();

        assert_eq!(
            spec.out_file,
            PathBuf::from("/home/ada/.shep/logs/api-out.log")
        );
        assert_eq!(
            spec.err_file,
            PathBuf::from("/home/ada/.shep/logs/api-err.log")
        );
    }

    #[test]
    fn explicit_out_file_wins() {
        let app_config = AppConfig {
            name: "app".to_string(),
            script: "app".to_string(),
            args: vec![],
            merge_logs: false,
            out_file: Some("/var/log/myapp.log".to_string()),
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 0, &paths, None, &no_secrets()).unwrap();

        assert_eq!(spec.out_file, PathBuf::from("/var/log/myapp.log"));
        assert_eq!(
            spec.err_file,
            PathBuf::from("/home/ada/.shep/logs/app-0-err.log")
        );
    }

    // fails if the gate drops the `channel` term from the disjunction
    #[test]
    fn channel_enabled_by_its_own_field() {
        let app_config = AppConfig {
            name: "app".to_string(),
            script: "app".to_string(),
            args: vec![],
            channel: true,
            wait_ready: false,
            shutdown_with_message: false,
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 0, &paths, None, &no_secrets()).unwrap();

        assert!(spec.channel);
    }

    // fails if the gate drops the `wait_ready` term from the disjunction
    #[test]
    fn channel_enabled_by_wait_ready() {
        let app_config = AppConfig {
            name: "app".to_string(),
            script: "app".to_string(),
            args: vec![],
            channel: false,
            wait_ready: true,
            shutdown_with_message: false,
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 0, &paths, None, &no_secrets()).unwrap();

        assert!(spec.channel);
    }

    // fails if the gate drops the `shutdown_with_message` term from the
    // disjunction
    #[test]
    fn channel_enabled_by_shutdown_with_message() {
        let app_config = AppConfig {
            name: "app".to_string(),
            script: "app".to_string(),
            args: vec![],
            channel: false,
            wait_ready: false,
            shutdown_with_message: true,
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 0, &paths, None, &no_secrets()).unwrap();

        assert!(spec.channel);
    }

    // Catches a stuck-open gate (e.g. `|| true`), since all three flags are
    // false here, unlike the three positive tests above.
    #[test]
    fn channel_disabled_when_all_three_flags_are_false() {
        let app_config = AppConfig {
            name: "app".to_string(),
            script: "app".to_string(),
            args: vec![],
            channel: false,
            wait_ready: false,
            shutdown_with_message: false,
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let paths = test_paths();

        let spec = assemble(&app, 0, &paths, None, &no_secrets()).unwrap();

        assert!(!spec.channel);
    }

    #[test]
    fn assembled_env_always_carries_a_path() {
        // tokio_runner.rs's env_clear() + envs(&spec.env) means this map IS
        // the child's whole env: no PATH here, and a bare interpreter name
        // (node, python3, sh, ...) can never be found by exec.
        let app_config = AppConfig {
            name: "web".to_string(),
            script: "app.js".to_string(),
            args: vec![],
            interpreter: Some("node".to_string()),
            ..Default::default()
        };
        let app = normalize(app_config).unwrap();
        let spec = assemble(&app, 0, &test_paths(), None, &no_secrets()).unwrap();
        let path = spec
            .env
            .get("PATH")
            .expect("PATH must survive env_clear()+envs(&spec.env)");
        assert!(
            !path.is_empty(),
            "an empty PATH is exactly the ENOENT failure mode"
        );
    }

    #[test]
    fn an_explicit_app_path_overrides_the_seeded_default() {
        let mut app_config = AppConfig {
            name: "web".to_string(),
            script: "app.js".to_string(),
            args: vec![],
            interpreter: Some("node".to_string()),
            ..Default::default()
        };
        app_config
            .env
            .insert("PATH".to_string(), "/opt/custom/bin".to_string());
        let app = normalize(app_config).unwrap();
        let spec = assemble(&app, 0, &test_paths(), None, &no_secrets()).unwrap();
        assert_eq!(
            spec.env.get("PATH").map(String::as_str),
            Some("/opt/custom/bin")
        );
    }

    /// The one field on the way to the runner whose default is "closed":
    /// a regression here would silently give an opted-in app `/dev/null`.
    #[test]
    fn the_stdin_flag_reaches_the_spawn_spec() {
        let mut app = AppConfig::minimal("repl", "./repl");
        app.stdin = true;
        let spec = assemble(
            &normalize(app).unwrap(),
            0,
            &test_paths(),
            None,
            &no_secrets(),
        )
        .unwrap();
        assert!(spec.stdin);
    }

    #[test]
    fn templates_render_per_instance_in_env_and_args() {
        let mut config = AppConfig {
            name: "z-worker".to_string(),
            script: "bin/worker".to_string(),
            instances: 4,
            args: vec!["--metrics-port".to_string(), "91{{instance}}".to_string()],
            ..Default::default()
        };
        config
            .env
            .insert("Z_WORKER_ID".to_string(), "z-{{instance}}".to_string());
        config.env.insert(
            "Z_DEVICE_ID".to_string(),
            "{{name}}-{{instance}}d".to_string(),
        );

        let app = normalize(config).unwrap();
        let spec = assemble(&app, 2, &test_paths(), None, &no_secrets()).unwrap();

        assert_eq!(spec.env.get("Z_WORKER_ID").map(String::as_str), Some("z-2"));
        assert_eq!(
            spec.env.get("Z_DEVICE_ID").map(String::as_str),
            Some("z-worker-2d")
        );
        assert!(spec.args.contains(&"912".to_string()), "{:?}", spec.args);
    }

    #[test]
    fn a_templated_log_path_renders_per_instance() {
        let app = normalize(AppConfig {
            name: "web".to_string(),
            script: "./srv".to_string(),
            instances: 3,
            out_file: Some("/var/log/web-{{instance}}.log".to_string()),
            ..Default::default()
        })
        .unwrap();
        let spec = assemble(&app, 2, &test_paths(), None, &no_secrets()).unwrap();
        assert_eq!(spec.out_file, PathBuf::from("/var/log/web-2.log"));
    }

    /// Unlike `channel` (implied by `wait_ready`/`shutdown_with_message`),
    /// nothing implies `stdin`.
    #[test]
    fn nothing_else_turns_stdin_on() {
        let mut app = AppConfig::minimal("web", "./srv");
        app.channel = true;
        app.wait_ready = true;
        app.shutdown_with_message = true;
        let spec = assemble(
            &normalize(app).unwrap(),
            0,
            &test_paths(),
            None,
            &no_secrets(),
        )
        .unwrap();
        assert!(spec.channel, "the fixture should still open a channel");
        assert!(!spec.stdin);
    }

    #[test]
    fn a_resolved_secret_reaches_the_child_env() {
        let mut config = AppConfig::minimal("web", "./srv");
        config
            .env
            .insert("PW".into(), "{{secret:DB_PASSWORD}}".into());
        let app = normalize(config).unwrap();
        let spec = assemble(
            &app,
            0,
            &test_paths(),
            None,
            &view_with("production", "DB_PASSWORD", "hunter2"),
        )
        .unwrap();
        assert_eq!(spec.env.get("PW").map(String::as_str), Some("hunter2"));
    }

    #[test]
    fn shep_environment_is_injected_and_matches_the_view() {
        let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
        let spec = assemble(
            &app,
            0,
            &test_paths(),
            None,
            &SecretView::empty("staging".into()),
        )
        .unwrap();
        assert_eq!(
            spec.env.get("SHEP_ENVIRONMENT").map(String::as_str),
            Some("staging")
        );
    }

    #[test]
    fn a_missing_key_refuses_the_spawn_and_names_the_field() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("PW".into(), "{{secret:ABSENT}}".into());
        let app = normalize(config).unwrap();
        let err = assemble(
            &app,
            0,
            &test_paths(),
            None,
            &SecretView::empty("production".into()),
        )
        .unwrap_err();
        assert!(!err.is_retriable());
        let rendered = err.to_string();
        assert!(rendered.contains("PW"), "names the env key: {rendered}");
        assert!(rendered.contains("ABSENT"), "{rendered}");
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );
    }

    #[test]
    fn an_unready_namespace_refuses_retriably() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("PW".into(), "{{secret:vault/K}}".into());
        let app = normalize(config).unwrap();
        let err = assemble(
            &app,
            0,
            &test_paths(),
            None,
            &SecretView::empty("production".into()),
        )
        .unwrap_err();
        assert!(err.is_retriable());
    }

    #[test]
    fn a_secret_resolves_in_args_and_in_an_explicit_log_path() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.args = vec!["--token={{secret:K}}".into()];
        config.out_file = Some("/tmp/{{secret:K}}.log".into());
        config.merge_logs = true;
        let app = normalize(config).unwrap();
        let spec = assemble(
            &app,
            0,
            &test_paths(),
            None,
            &view_with("production", "K", "v"),
        )
        .unwrap();
        assert_eq!(spec.args, vec!["--token=v".to_string()]);
        assert!(spec.out_file.to_string_lossy().contains("/tmp/v.log"));
    }

    #[test]
    fn an_app_cannot_set_shep_environment_by_hand() {
        let mut config = AppConfig::minimal("web", "./srv");
        config
            .env
            .insert("SHEP_ENVIRONMENT".into(), "sneaky".into());
        // normalize refuses it, the same way it refuses SHEP_NAME.
        assert!(normalize(config).is_err());
    }

    /// The half of the store a namespace reads is refused apart from the
    /// operator's own, since only one of the two clears itself.
    #[test]
    fn the_two_refusal_shapes_stay_apart() {
        let mut absent = AppConfig::minimal("a", "./srv");
        absent.env.insert("K".into(), "{{secret:NOPE}}".into());
        let mut unready = AppConfig::minimal("b", "./srv");
        unready.env.insert("K".into(), "{{secret:ns/NOPE}}".into());
        let view = SecretView::empty("production".to_string());
        let of = |config| {
            assemble(&normalize(config).unwrap(), 0, &test_paths(), None, &view)
                .unwrap_err()
                .is_retriable()
        };
        assert!(!of(absent), "a key nobody set waits on a person");
        assert!(of(unready), "a namespace no dog pushed to clears itself");
    }

    /// `describe` is what registration, adoption and every prober site read,
    /// and each of those has a live sheep to lose if a store read refuses.
    #[test]
    fn describe_keeps_an_unresolvable_reference_rather_than_refusing() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("PW".into(), "{{secret:ABSENT}}".into());
        config.out_file = Some("/tmp/{{name}}.log".into());
        config.merge_logs = true;
        let app = normalize(config).unwrap();
        let spec = describe(
            &app,
            0,
            &test_paths(),
            None,
            &SecretView::empty("staging".into()),
        );
        assert_eq!(
            spec.env.get("PW").map(String::as_str),
            Some("{{secret:ABSENT}}")
        );
        assert_eq!(spec.out_file, PathBuf::from("/tmp/web.log"));
        assert_eq!(
            spec.env.get("SHEP_ENVIRONMENT").map(String::as_str),
            Some("staging"),
            "a described spec still names its environment"
        );
    }

    /// A value that resolves is identical either way, so the two builders
    /// cannot drift on anything but a refusal.
    #[test]
    fn describe_matches_assemble_when_every_reference_resolves() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("PW".into(), "{{secret:K}}".into());
        let app = normalize(config).unwrap();
        let view = view_with("production", "K", "v");
        let assembled = assemble(&app, 2, &test_paths(), None, &view).unwrap();
        let described = describe(&app, 2, &test_paths(), None, &view);
        assert_eq!(assembled.env, described.env);
        assert_eq!(assembled.out_file, described.out_file);
        assert_eq!(assembled.args, described.args);
    }
}
