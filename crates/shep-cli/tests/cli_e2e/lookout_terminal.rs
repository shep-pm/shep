//! Driving lookout through a terminal, including its real schema probe.

#![cfg(unix)]

use super::*;
use std::io::Write as _;
use std::sync::mpsc::{self, Receiver};

/// `script` supplies the controlling terminal that crossterm reads.
#[derive(Debug)]
struct LookoutTerminal {
    child: Child,
    output: Receiver<std::io::Result<Vec<u8>>>,
    received: Vec<u8>,
}

impl LookoutTerminal {
    fn open(home: &Path) -> Self {
        let binary = shep(home).get_program().to_owned();
        let shell =
            "stty cols 120 rows 40; exec \"$LOOKOUT_TEST_BINARY\" --home \"$SHEP_HOME\" lookout";
        let mut command = std::process::Command::new("script");
        command.arg("-q");
        #[cfg(target_os = "linux")]
        command.args(["-e", "-c", shell, "/dev/null"]);
        #[cfg(not(target_os = "linux"))]
        command.args(["/dev/null", "/bin/sh", "-c", shell]);
        let mut child = command
            .env("LOOKOUT_TEST_BINARY", binary)
            .env("SHEP_HOME", home)
            .env("NO_COLOR", "1")
            .env("TERM", "xterm-256color")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start lookout in a terminal");
        let mut stdout = child.stdout.take().unwrap();
        let (send, output) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let mut bytes = vec![0; 4096];
                let result = match stdout.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        bytes.truncate(count);
                        Ok(bytes)
                    }
                    Err(error) => Err(error),
                };
                let failed = result.is_err();
                if send.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Self {
            child,
            output,
            received: Vec::new(),
        }
    }

    fn read_until(&mut self, text: &str) {
        let deadline = Instant::now() + CMD_TIMEOUT;
        while !String::from_utf8_lossy(&self.received).contains(text) {
            let bytes = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "lookout did not draw {text:?}: {error}; output: {}",
                        String::from_utf8_lossy(&self.received)
                    )
                })
                .expect("read lookout's terminal");
            self.received.extend(bytes);
        }
    }

    fn press(&mut self, keys: &[u8]) {
        self.child.stdin.as_mut().unwrap().write_all(keys).unwrap();
        self.child.stdin.as_mut().unwrap().flush().unwrap();
    }
}

impl Drop for LookoutTerminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Opens the real dog's pane and waits for its first-env-key control.
pub(super) fn assert_adopted_dog_pane_draws_env(home: &Path) {
    let mut terminal = LookoutTerminal::open(home);
    terminal.read_until("control enabled");
    terminal.read_until("kelpie");
    terminal.received.clear();
    terminal.press(b"/kelpie\re");
    terminal.read_until("+ add a key");
    assert!(
        String::from_utf8_lossy(&terminal.received).contains("(dog config)"),
        "the env row must be drawn in the dog's pane"
    );
    terminal.press(b"q");
    let status = common::wait_bounded(&mut terminal.child, CMD_TIMEOUT, "lookout's terminal");
    assert!(status.success(), "{status:?}");
}
