//! The test side of a scripted spawn's terminal.

use std::sync::Arc;

use super::{PtyTap, ScriptedRunner};

impl ScriptedRunner {
    /// The tap on the terminal of the proc spawned at `spawn_index`.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range or that spawn had no terminal.
    #[track_caller]
    fn pty_tap(&self, spawn_index: usize) -> Arc<PtyTap> {
        self.spawned.lock().unwrap()[spawn_index]
            .pty
            .clone()
            .expect("that spawn asked for no terminal")
    }

    /// Everything the daemon has typed into the terminal of the proc spawned
    /// at `spawn_index`, in order.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range or that spawn had no terminal.
    #[must_use]
    #[track_caller]
    pub fn pty_input(&self, spawn_index: usize) -> Vec<u8> {
        self.pty_tap(spawn_index).input()
    }

    /// Every window size the daemon has set on that terminal, as
    /// `(rows, cols)`, in order.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range or that spawn had no terminal.
    #[must_use]
    #[track_caller]
    pub fn pty_sizes(&self, spawn_index: usize) -> Vec<(u16, u16)> {
        self.pty_tap(spawn_index).sizes()
    }

    /// Has the proc spawned at `spawn_index` write `bytes` to its terminal, as
    /// one read of the master.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range or that spawn had no terminal.
    #[track_caller]
    pub fn pty_emit(&self, spawn_index: usize, bytes: &[u8]) {
        self.pty_tap(spawn_index).emit(bytes);
    }

    /// Closes that terminal, as the child's last descriptor on it closing does.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range or that spawn had no terminal.
    #[track_caller]
    pub fn pty_end(&self, spawn_index: usize) {
        self.pty_tap(spawn_index).end();
    }

    /// How many receivers of that terminal's output are still alive.
    ///
    /// # Panics
    ///
    /// If `spawn_index` is out of range or that spawn had no terminal.
    #[must_use]
    #[track_caller]
    pub fn pty_subscribers(&self, spawn_index: usize) -> usize {
        self.pty_tap(spawn_index).subscribers()
    }
}
