//! The shepherd's end of a dog's own connection, as a runner hands it over.

use shep_core::transport::ServerStream;

/// The daemon's end of the connection a dog was spawned holding
///
/// Opened by the runner at spawn and named for the dog it belongs to, so the
/// server that serves it never asks the peer who it is: the connection is
/// the identity. On unix the socketpair wrapped here is made at spawn; on
/// Windows it is a named pipe the dog opens by the name in `$SHEP_DOG_PIPE`.
/// `Debug` is derived: no env value reaches this type.
#[derive(Debug)]
pub struct DogLink {
    /// The dog's name, from the spec that spawned it.
    pub name: String,
    /// The pid the dog was spawned under. A socketpair's peer credentials name
    /// the shepherd itself, so this is the only way to tell the silence watch
    /// which process just made contact.
    pub pid: u32,
    /// The shepherd's end: already connected to the dog's fd 4 on unix, and
    /// the pipe instance the dog connected to on Windows.
    pub stream: ServerStream,
    /// `Some` for a connection a predecessor shepherd carried across a
    /// handover: the dog has already sent its `Hello`, so its next frame is a
    /// request. Holds the topic patterns it had subscribed to, empty for none.
    /// `None` for one opened at this spawn, and always on Windows, which has
    /// no handover.
    pub resumed: Option<Vec<String>>,
}
