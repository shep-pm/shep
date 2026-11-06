//! Serving an attach to a sheep's pty: the subscription, and the keys and
//! sizes that go back.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// The open terminal of the lowest-id instance named `name`, with that id.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::NoSuchSheep`]: nothing is registered under `name`.
    /// - [`SupervisorError::NoPty`]: no instance of it has an open terminal.
    fn open_pty_of(&self, name: &str) -> Result<(u32, PtyHandle), SupervisorError> {
        let ids = self.matching_ids(&ProcessSelector::Name(name.to_owned()));
        if ids.is_empty() {
            return Err(SupervisorError::NoSuchSheep(name.to_owned()));
        }
        ids.into_iter()
            .find_map(|id| Some((id, self.sheep.get(&id)?.open_pty()?.clone())))
            .ok_or_else(|| SupervisorError::NoPty {
                name: name.to_owned(),
            })
    }

    /// Subscribes to `name`'s terminal, answering on the actor loop: a
    /// subscription is a lookup and a clone, and awaits nothing.
    pub(super) fn pty_attach(
        &self,
        name: &str,
        reply: oneshot::Sender<Result<PtyAttached, SupervisorError>>,
    ) {
        let attached = self.open_pty_of(name).map(|(id, pty)| PtyAttached {
            id,
            output: pty.subscribe(),
        });
        let _ = reply.send(attached);
    }

    /// Types `bytes` into `name`'s terminal from a task of its own.
    ///
    /// Not on the actor loop, as [`Self::begin_send_line`] is not: a write
    /// waits on a program that may have stopped reading.
    pub(super) fn begin_pty_input(
        &self,
        name: &str,
        bytes: Vec<u8>,
        reply: oneshot::Sender<Result<u32, SupervisorError>>,
    ) {
        self.begin_pty_ask(name, reply, |pty| async move { pty.send(bytes).await });
    }

    /// Sizes `name`'s terminal from a task of its own, for the reason
    /// [`Self::begin_pty_input`] gives.
    pub(super) fn begin_pty_resize(
        &self,
        name: &str,
        rows: u16,
        cols: u16,
        reply: oneshot::Sender<Result<u32, SupervisorError>>,
    ) {
        self.begin_pty_ask(name, reply, move |pty| async move {
            pty.resize(rows, cols).await
        });
    }

    fn begin_pty_ask<F>(
        &self,
        name: &str,
        reply: oneshot::Sender<Result<u32, SupervisorError>>,
        ask: impl FnOnce(PtyHandle) -> F,
    ) where
        F: Future<Output = Result<(), PtyError>> + Send + 'static,
    {
        let (id, pty) = match self.open_pty_of(name) {
            Ok(found) => found,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let name = name.to_owned();
        let asked = ask(pty);
        tokio::spawn(async move {
            let result = match tokio::time::timeout(STDIN_WRITE_TIMEOUT, asked).await {
                Ok(Ok(())) => Ok(id),
                Ok(Err(PtyError::Gone)) => Err(SupervisorError::NoPty { name }),
                Ok(Err(PtyError::Failed(msg))) => Err(SupervisorError::PtyFailed(msg)),
                Err(_elapsed) => Err(SupervisorError::PtyFailed(format!(
                    "sheep {name} did not take the write within {}s",
                    STDIN_WRITE_TIMEOUT.as_secs()
                ))),
            };
            let _ = reply.send(result);
        });
    }
}
