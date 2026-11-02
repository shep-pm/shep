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

/// Rebuild every sheep's `listen` sockets the blob carries.
///
/// A descriptor that is not open, or is not a listening TCP socket, is
/// replaced by a fresh bind of the address it carried, with one `warn!`
/// naming the sheep and the address. A replacement that cannot bind is left
/// out, and the sheep's next start binds it or fails naming it.
pub(super) fn adopt_listeners(blob: &Handover) -> Vec<AdoptedListeners> {
    let mut by_sheep: BTreeMap<&str, Vec<(String, SocketAddr, TcpListener)>> = BTreeMap::new();
    for carried in blob.listeners() {
        let configured = carried.address().parse::<SocketAddr>();
        // Taken before anything else is checked, so a socket that is refused
        // below is closed rather than left open and unowned.
        let adopted = adopt_fd(
            carried.fd(),
            &format!(
                "the listen socket '{}' of sheep '{}'",
                carried.name(),
                carried.sheep()
            ),
        )
        .and_then(|file| listening_tcp(OwnedFd::from(file)));
        let socket = match (adopted, &configured) {
            (Ok(socket), Ok(_)) => Ok(socket),
            (Ok(_), Err(error)) => Err(io::Error::other(format!(
                "its address does not parse: {error}"
            ))),
            (Err(error), _) => Err(error),
        };
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
