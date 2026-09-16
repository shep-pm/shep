//! The shapes of child a case starts, from a bare sleeper to one that
//! answers on the shepherd channel.

use super::*;

/// The interpreter a fixture's inline script is written for, and the flag
/// that makes it read one: `/bin/sh -c` on unix, `cmd /C` on Windows.
pub(crate) fn shell() -> (&'static str, &'static str) {
    #[cfg(unix)]
    {
        ("/bin/sh", "-c")
    }
    #[cfg(windows)]
    {
        ("cmd", "/C")
    }
}

/// An [`AppConfig`] running `script` under [`shell`].
///
/// `interpreter = "none"`: the script is already written for a shell, so shep
/// must not also resolve one from the program's extension.
pub(crate) fn shell_app(name: &str, script: String) -> AppConfig {
    let (program, flag) = shell();
    let mut app = AppConfig::minimal(name, program);
    app.interpreter = Some("none".to_string());
    app.args = vec![flag.to_string(), script];
    app
}

/// A sheep that stays up until something stops it.
///
/// `ping` on Windows: `timeout.exe` refuses to run with stdin redirected,
/// which every sheep's is, and `cmd` has no `sleep`.
pub(crate) fn forever_app(name: &str) -> AppConfig {
    #[cfg(unix)]
    let script = "while :; do sleep 1; done".to_string();
    #[cfg(windows)]
    let script = "ping -n 9999 127.0.0.1 >nul".to_string();
    shell_app(name, script)
}

/// A sheep that writes `line` to stdout and then stays up long enough to be
/// observed.
pub(crate) fn announce_app(name: &str, line: &str) -> AppConfig {
    #[cfg(unix)]
    let script = format!("echo {line}; sleep 5");
    #[cfg(windows)]
    let script = format!("echo {line}& ping -n 6 127.0.0.1 >nul");
    shell_app(name, script)
}

/// A sheep that sends one `ready` on the shepherd channel, then stays up.
///
/// fd 3 on unix, so a `>&3` redirect is the whole contract. Windows has no
/// fd-3 inheritance: the daemon exports the pipe name as
/// `%SHEP_CHANNEL_PIPE%`. The caller still sets `wait_ready`, which is what
/// makes `assemble` open the channel.
pub(crate) fn ready_app(name: &str, dir: &std::path::Path) -> AppConfig {
    #[cfg(unix)]
    {
        let _ = dir;
        shell_app(
            name,
            r#"printf '{"kind":"ready"}
' >&3; while :; do sleep 1; done"#
                .to_string(),
        )
    }
    // A `.cmd` file, not `cmd /C <script>`: `std::process::Command` escapes an
    // argument's inner quotes as `\"`, which `cmd.exe` takes literally, so the
    // redirect target arrives malformed.
    #[cfg(windows)]
    {
        let script = dir.join(format!("{name}-ready.cmd"));
        let mut body = String::new();
        for line in [
            "@echo off",
            "(echo {\"kind\":\"ready\"}) > \"%SHEP_CHANNEL_PIPE%\"",
            "ping -n 9999 127.0.0.1 >nul",
        ] {
            body.push_str(line);
            body.push('\r');
            body.push('\n');
        }
        std::fs::write(&script, body).unwrap();
        let mut app = AppConfig::minimal(name, &script.display().to_string());
        app.interpreter = Some("none".to_string());
        app
    }
}

/// A sheep that writes `before`, waits for `marker` to appear, writes
/// `after`, then stays up. The fixture the log-rotation cases drive.
pub(crate) fn gated_announce_app(name: &str, marker: &std::path::Path) -> AppConfig {
    #[cfg(unix)]
    {
        let script = format!(
            "echo before; while [ ! -f {} ]; do sleep 1; done; echo after; sleep 5",
            marker.display()
        );
        let mut app = shell_app(name, script);
        app.autorestart = false;
        app
    }
    // A batch file, not a `cmd /C` one-liner: `goto` needs labels, which exist
    // only in a file, so an inline loop does not loop. `ping -n 2` is the
    // sleep, for `forever_app`'s reason.
    #[cfg(windows)]
    {
        const CRLF: &str = "\r\n";
        let script = marker.with_file_name("gated.cmd");
        let body = [
            "@echo off".to_string(),
            "echo before".to_string(),
            ":wait".to_string(),
            format!("if exist \"{}\" goto ready", marker.display()),
            "ping -n 2 127.0.0.1 >nul".to_string(),
            "goto wait".to_string(),
            ":ready".to_string(),
            "echo after".to_string(),
            "ping -n 6 127.0.0.1 >nul".to_string(),
            String::new(),
        ];
        std::fs::write(&script, body.join(CRLF))
            .expect("the gated fixture script must be writable");
        let mut app = shell_app(name, script.display().to_string());
        // The script exits and `autorestart` is on by default, so without this
        // the log gains a second `before` and `after`.
        app.autorestart = false;
        app
    }
}
