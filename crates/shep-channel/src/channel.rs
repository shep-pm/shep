use crate::{ChannelError, ChildMessage, ShepherdMessage, endpoint, session};

/// The channel with no threads: you own the loop.
///
/// [`crate::serve()`] is the other, documented default: it answers messages
/// you never registered a handler for. Reach for this when your app
/// already runs its own event loop.
#[derive(Debug)]
pub struct Channel {
    pub(crate) reader: std::io::BufReader<endpoint::ReadHalf>,
    pub(crate) writer: endpoint::Transport,
    pub(crate) version: Option<String>,
    /// Buffers `recv` and `send` reuse, so a message costs no allocation.
    pub(crate) read_line: Scratch,
    pub(crate) write_line: Scratch,
}

impl Channel {
    /// Opens this process's channel, or `Ok(None)` when it has none.
    ///
    /// At most one channel exists per process. A second call returns
    /// [`ChannelError::AlreadyTaken`] rather than retake the descriptor.
    ///
    /// # Errors
    ///
    /// - [`ChannelError::Unusable`] when the environment names a channel
    ///   that cannot be opened here.
    /// - [`ChannelError::Io`] when the transport cannot be opened.
    /// - [`ChannelError::AlreadyTaken`] when this process already took its
    ///   channel.
    pub fn open() -> Result<Option<Self>, ChannelError> {
        let found = endpoint::discover()?;
        if found == endpoint::Endpoint::Absent {
            return Ok(None);
        }
        let (reader, writer) = endpoint::connect(&found)?;
        Ok(Some(Self {
            reader: std::io::BufReader::new(reader),
            writer,
            version: std::env::var(endpoint::VERSION_VAR).ok(),
            read_line: Scratch::default(),
            write_line: Scratch::default(),
        }))
    }

    /// Reads one message. `Ok(None)` is the shepherd closing its end.
    ///
    /// # Errors
    ///
    /// - [`ChannelError::Malformed`] for one unparseable line. Recoverable:
    ///   call again to resume at the next line.
    /// - [`ChannelError::Io`] when the transport fails.
    pub fn recv(&mut self) -> Result<Option<ShepherdMessage>, ChannelError> {
        session::read_message(&mut self.reader, &mut self.read_line.0)
    }

    /// Writes one message and flushes it.
    ///
    /// # Errors
    ///
    /// - [`ChannelError::Io`] when the transport fails.
    /// - [`ChannelError::Malformed`] when the message cannot be encoded.
    pub fn send(&mut self, message: &ChildMessage) -> Result<(), ChannelError> {
        session::write_messages(
            &mut self.writer,
            std::slice::from_ref(message),
            &mut self.write_line.0,
        )
    }

    /// The `SHEP_CHANNEL_VERSION` stamp, when the shepherd set one.
    ///
    /// A stamp, not a negotiation: the shepherd cannot ask what this app
    /// speaks. It is here so an app can notice a wire it has never seen.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// Takes the channel apart for the two threads that drive it.
    pub(crate) fn into_halves(
        self,
    ) -> (
        std::io::BufReader<endpoint::ReadHalf>,
        endpoint::Transport,
        Option<String>,
    ) {
        (self.reader, self.writer, self.version)
    }
}

/// A buffer a [`Channel`] keeps between messages.
///
/// Holds the last message's bytes, which can carry a reply body, so the
/// derived `Debug` is not used (IR-41).
#[derive(Default)]
pub(crate) struct Scratch(pub(crate) Vec<u8>);

impl core::fmt::Debug for Scratch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Scratch")
            .field("capacity", &self.0.capacity())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_debug_names_its_capacity_and_never_its_bytes() {
        let scratch = Scratch(b"SECRET-REPLY-BODY".to_vec());

        let rendered = format!("{scratch:?}");

        assert_eq!(
            rendered,
            format!("Scratch {{ capacity: {} }}", scratch.0.capacity())
        );
        assert!(
            !rendered.contains("SECRET-REPLY-BODY"),
            "the buffered bytes reached the Debug output: {rendered}"
        );
    }
}
