//! Rebuilding a sheep's `listen` sockets from the descriptors that crossed
//! the exec.
//!
//! Unlike every other descriptor in the blob, a socket that cannot be adopted
//! here does not refuse the rehydrate: the sheep's instances still hold the
//! port, and the successor can replace its own end of it.

use std::collections::BTreeMap;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::os::fd::OwnedFd;

use super::super::{CarriedListener, Handover};
use super::adopt_descriptors::adopt_fd;
use crate::listen::ListenSockets;
use crate::sys;

/// One sheep's `listen` sockets, as the successor holds them.
///
/// `Debug` shows names and addresses only.
#[derive(Debug)]
pub struct AdoptedListeners {
    /// The sheep whose `listen` table the sockets were bound from.
    pub sheep: String,
    /// Its sockets, in name order.
    pub sockets: ListenSockets,
}

/// The first pass over a blob's `listen` sockets: every carried descriptor
/// taken, none replaced yet.
///
/// Split from [`Self::rebind`] so every number the blob names is adopted
/// before any bind can be handed one. A bind takes the lowest free number,
/// and one taken early could be a number the blob still names.
#[derive(Debug)]
pub(super) struct TakenListeners<'a> {
    entries: Vec<(
        &'a CarriedListener,
        Result<SocketAddr, core::net::AddrParseError>,
        io::Result<TcpListener>,
    )>,
}

/// Take every `listen` descriptor the blob carries, checking each is a
/// listening socket bound where the blob says.
///
/// A descriptor that did not arrive under its number is not taken at all: the
/// number may now name something this process opened.
pub(super) fn take_listeners(blob: &Handover) -> TakenListeners<'_> {
    let entries = blob
        .listeners()
        .iter()
        .map(|carried| {
            let configured = carried.address().parse::<SocketAddr>();
            let socket = take(carried, &configured);
            (carried, configured, socket)
        })
        .collect();
    TakenListeners { entries }
}

fn take(
    carried: &CarriedListener,
    configured: &Result<SocketAddr, core::net::AddrParseError>,
) -> io::Result<TcpListener> {
    if carried.unclaimed() {
        return Err(io::Error::other("it did not arrive under its number"));
    }
    // Taken before anything else is checked, so a socket that is refused
    // below is closed rather than left open and unowned.
    let socket = adopt_fd(
        carried.fd(),
        &format!(
            "the listen socket '{}' of sheep '{}'",
            carried.name(),
            carried.sheep()
        ),
    )
    .and_then(|file| listening_tcp(OwnedFd::from(file)))?;
    let configured = configured
        .as_ref()
        .map_err(|error| io::Error::other(format!("its address does not parse: {error}")))?;
    let local = socket.local_addr()?;
    if serves(local, *configured) {
        Ok(socket)
    } else {
        Err(io::Error::other(format!("it is bound to {local}")))
    }
}

/// Whether a socket bound at `local` is the one `configured` asked for, where
/// port 0 asked for any port.
pub(super) fn serves(local: SocketAddr, configured: SocketAddr) -> bool {
    local.ip() == configured.ip() && (configured.port() == 0 || local.port() == configured.port())
}

impl TakenListeners<'_> {
    /// Replace each socket that could not be taken and group the rest by
    /// sheep.
    ///
    /// A descriptor that is not open, not a listening TCP socket, or not
    /// bound where the blob says is replaced by a fresh bind of its address,
    /// with one `warn!` naming the sheep and the address. A replacement that
    /// cannot bind is left out, and the sheep's next start binds it.
    pub(super) fn rebind(self) -> Vec<AdoptedListeners> {
        let mut by_sheep: BTreeMap<&str, Vec<(String, SocketAddr, TcpListener)>> = BTreeMap::new();
        for (carried, configured, socket) in self.entries {
            let socket = socket
                .or_else(|reason| replace(carried, &configured, &reason))
                .ok();
            if let (Some(socket), Ok(configured)) = (socket, configured) {
                by_sheep.entry(carried.sheep()).or_default().push((
                    carried.name().to_string(),
                    configured,
                    socket,
                ));
            }
        }
        by_sheep
            .into_iter()
            .map(|(sheep, entries)| AdoptedListeners {
                sheep: sheep.to_string(),
                sockets: ListenSockets::from_adopted(entries),
            })
            .collect()
    }
}

/// `fd` as a TCP listener, if it is a listening socket of an internet family.
fn listening_tcp(fd: OwnedFd) -> io::Result<TcpListener> {
    let listener = TcpListener::from(fd);
    if !sys::is_listening_socket(&listener) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "it is not a listening socket",
        ));
    }
    listener.local_addr().map_err(|error| {
        io::Error::new(error.kind(), format!("it is not a TCP socket: {error}"))
    })?;
    Ok(listener)
}

/// Binds `carried`'s address afresh in place of a descriptor that failed with
/// `reason`, logging the one `warn!` for it.
///
/// `Ok(socket)` replaces the carried one. `Err` after the log is a socket
/// that could not be replaced either.
fn replace(
    carried: &CarriedListener,
    configured: &Result<SocketAddr, core::net::AddrParseError>,
    reason: &io::Error,
) -> io::Result<TcpListener> {
    let bound = match configured {
        Ok(address) => TcpListener::bind(address),
        Err(error) => Err(io::Error::other(error.to_string())),
    };
    let outcome = match &bound {
        Ok(_) => "bound afresh".to_string(),
        Err(error) => format!("not bound ({error}), so its next start binds it"),
    };
    tracing::warn!(
        sheep = carried.sheep(),
        name = carried.name(),
        address = carried.address(),
        "the carried listen socket was unusable ({reason}); {outcome}"
    );
    bound
}
