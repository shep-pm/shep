//! The sockets a sheep's `listen` table asks the shepherd to hold.
//!
//! One set per app, bound when the first instance starts and handed to every
//! instance after it, so a reload overlaps on a socket that never closes.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// The sockets `config`'s next instance is spawned with: the app's held
    /// set if it matches `config.listen`, otherwise a set bound now.
    ///
    /// Called before the spawn it serves, so a bind failure fails that spawn
    /// and a reload that cannot bind leaves its drainee serving. A changed
    /// table keeps the sockets of every address it still names and binds the
    /// rest; what it no longer names closes here, though the drainee goes on
    /// serving it from its own copy until it exits.
    ///
    /// # Errors
    /// - [`AssembleError::Listen`] if an address cannot be bound. The held set
    ///   is left as it was.
    pub(super) fn listen_for(
        &mut self,
        config: &AppConfig,
    ) -> Result<Option<Arc<ListenSockets>>, AssembleError> {
        if config.listen.is_empty() {
            self.listeners.remove(&config.name);
            return Ok(None);
        }
        let held = self.listeners.get(&config.name);
        if let Some(sockets) = held.filter(|sockets| sockets.is_bound_from(&config.listen)) {
            return Ok(Some(Arc::clone(sockets)));
        }
        let bound = ListenSockets::bind_keeping(held.map(|sockets| &**sockets), &config.listen)
            .map(Arc::new)
            .map_err(AssembleError::Listen)?;
        self.listeners
            .insert(config.name.clone(), Arc::clone(&bound));
        Ok(Some(bound))
    }

    /// Puts back the set a reload replaced, once the reload is abandoned.
    ///
    /// The drainee still holds those addresses, so the next reload has to
    /// find them held or its bind of an address the drainee serves fails. A
    /// drainee with no sockets gets none back: the set the reload bound has
    /// no process to accept on it.
    pub(super) fn restore_listeners(&mut self, name: &str) {
        match self.listeners_before.remove(name) {
            Some(Some(before)) => {
                self.listeners.insert(name.to_string(), before);
            }
            Some(None) => {
                self.listeners.remove(name);
            }
            None => {}
        }
    }

    /// Closes the sockets of every app with no instance left that is running
    /// or due to run again, so a stopped or deleted app frees its ports and
    /// binds them afresh on its next start.
    pub(super) fn prune_listeners(&mut self) {
        if self.listeners.is_empty() && self.listeners_before.is_empty() {
            return;
        }
        let wanted: BTreeSet<&str> = self
            .sheep
            .values()
            .filter(|slot| {
                matches!(
                    slot.entry.status,
                    ProcStatus::Starting
                        | ProcStatus::Online
                        | ProcStatus::Stopping
                        | ProcStatus::WaitingRestart
                )
            })
            .map(|slot| slot.entry.spec.config().name.as_str())
            .collect();
        self.listeners
            .retain(|name, _| wanted.contains(name.as_str()));
        self.listeners_before
            .retain(|name, _| self.reloads.contains_key(name));
    }
}
