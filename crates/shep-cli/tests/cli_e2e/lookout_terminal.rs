//! Driving lookout through a terminal, including its real schema probe.

#![cfg(unix)]

use super::*;
use std::io::Write as _;
use std::sync::mpsc::{self, Receiver};

const TERMINAL_ROWS: u16 = 40;
const TERMINAL_COLS: u16 = 120;

struct TerminalScreen {
    parser: vt100::Parser,
}

impl std::fmt::Debug for TerminalScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalScreen")
            .field("contents", &self.contents())
            .finish_non_exhaustive()
    }
}

impl Default for TerminalScreen {
    fn default() -> Self {
        Self {
            parser: vt100::Parser::new(TERMINAL_ROWS, TERMINAL_COLS, 0),
        }
    }
}

impl TerminalScreen {
    fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    fn contents(&self) -> String {
        self.parser.screen().contents()
    }
}

/// `script` supplies the controlling terminal that crossterm reads.
#[derive(Debug)]
struct LookoutTerminal {
    child: Child,
    output: Receiver<std::io::Result<Vec<u8>>>,
    screen: TerminalScreen,
}

impl LookoutTerminal {
    fn open(home: &Path) -> Self {
        let binary = env!("CARGO_BIN_EXE_shep");
        let shell = format!(
            "stty cols {TERMINAL_COLS} rows {TERMINAL_ROWS}; exec \"$LOOKOUT_TEST_BINARY\" --home \"$SHEP_HOME\" lookout"
        );
        let mut command = std::process::Command::new("script");
        command.arg("-q");
        #[cfg(target_os = "linux")]
        command.args(["-e", "-c", shell.as_str(), "/dev/null"]);
        #[cfg(not(target_os = "linux"))]
        command.args(["/dev/null", "/bin/sh", "-c", shell.as_str()]);
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
            screen: TerminalScreen::default(),
        }
    }

    fn read_until(&mut self, text: &str) {
        let deadline = Instant::now() + CMD_TIMEOUT;
        while !self.screen.contents().contains(text) {
            let bytes = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "lookout did not draw {text:?}: {error}; output: {}",
                        self.screen.contents()
                    )
                })
                .expect("read lookout's terminal");
            self.screen.process(&bytes);
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
    terminal.press(b"/kelpie\re");
    terminal.read_until("+ add a key");
    assert!(
        terminal.screen.contents().contains("(dog config)"),
        "the env row must be drawn in the dog's pane"
    );
    terminal.press(b"q");
    let status = common::wait_bounded(&mut terminal.child, CMD_TIMEOUT, "lookout's terminal");
    assert!(status.success(), "{status:?}");
}

#[test]
fn terminal_output_recognizes_a_label_whose_spaces_are_unchanged_cells() {
    use ratatui::backend::{Backend as _, CrosstermBackend};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    let before = Buffer::empty(Rect::new(0, 0, TERMINAL_COLS, TERMINAL_ROWS));
    let mut after = before.clone();
    after.set_string(100, 39, "control enabled", Style::default());
    let mut bytes = Vec::new();
    CrosstermBackend::new(&mut bytes)
        .draw(before.diff(&after).into_iter())
        .unwrap();
    let mut screen = TerminalScreen::default();
    screen.process(&bytes);
    assert!(
        screen.contents().contains("control enabled"),
        "the visible label must match despite cursor movements: {:?}",
        screen.contents()
    );
}

#[test]
fn terminal_output_tracks_redraws_across_fragmented_escape_and_utf8_sequences() {
    use ratatui::backend::{Backend as _, CrosstermBackend};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    let blank = Buffer::empty(Rect::new(0, 0, TERMINAL_COLS, TERMINAL_ROWS));
    let mut flock = blank.clone();
    flock.set_string(0, 0, "kelpie", Style::default());
    let mut bytes = Vec::new();
    CrosstermBackend::new(&mut bytes)
        .draw(blank.diff(&flock).into_iter())
        .unwrap();
    let mut screen = TerminalScreen::default();
    screen.process(&bytes);
    assert!(screen.contents().contains("kelpie"));

    let mut pane = blank;
    pane.set_string(0, 0, "🐕 env", Style::default());
    bytes.clear();
    CrosstermBackend::new(&mut bytes)
        .draw(flock.diff(&pane).into_iter())
        .unwrap();
    for byte in bytes {
        screen.process(&[byte]);
    }
    let contents = screen.contents();
    assert!(contents.contains("🐕 env"), "{contents:?}");
    assert!(!contents.contains("kelpie"), "{contents:?}");
}
