//! The TCP sockets the shepherd binds for a sheep's `listen` table.
//!
//! One [`ListenSockets`] is one sheep's whole set, bound once and handed to
//! every instance the way systemd's socket activation does: the descriptors
//! sit at fd 3 upward in name order, and `LISTEN_FDS` and `LISTEN_FDNAMES`
//! describe them. A reload that leaves `listen` alone keeps the same value,
//! so a connecting client meets one open socket from the old instance to the
//! new.

use core::fmt;
use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};

/// A sheep's bound listening sockets, in name order.
///
/// Dropping the last handle closes them in the shepherd. A running instance
/// holds its own inherited descriptors, so the socket outlives this value for
/// as long as an instance does.
pub struct ListenSockets {
    entries: Vec<Entry>,
}

struct Entry {
    name: String,
    /// The address as parsed, so two spellings of one address compare equal.
    configured: SocketAddr,
    listener: TcpListener,
}

/// Why a sheep's `listen` table could not be bound.
///
/// `#[non_exhaustive]`: a later key (a unix socket path) would add a variant.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenError {
    /// The shepherd could not bind one address.
    Bind {
        /// The entry's name in the `listen` table.
        name: String,
        /// The address as the Flockfile wrote it.
        address: String,
        /// The OS's reason, or why the address did not parse.
        reason: String,
    },
}

impl fmt::Display for ListenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind {
                name,
                address,
                reason,
            } => write!(f, "could not listen on {address} for `{name}`: {reason}"),
        }
    }
}

impl core::error::Error for ListenError {}

impl ListenSockets {
    /// Binds every entry of `table`, name to address, in name order.
    ///
    /// The std listener sets `SO_REUSEADDR` on unix before it binds, so a
    /// restart is not held off by connections left in `TIME_WAIT`.
    ///
    /// # Errors
    /// - [`ListenError::Bind`]: an address does not parse, or the OS refuses
    ///   the bind. Nothing stays bound: the sockets opened so far close.
    pub fn bind(table: &BTreeMap<String, String>) -> Result<Self, ListenError> {
        Self::bind_keeping(None, table)
    }

    /// [`Self::bind`], except an address `previous` already holds keeps that
    /// socket instead of being bound a second time.
    ///
    /// A reload that adds an address to `listen` binds only the new one: the
    /// rest would refuse to bind while the old set holds them, and closing
    /// them to bind again would drop connections queued on them. The address
    /// is what is kept, not its name, so a renamed entry keeps its socket.
    ///
    /// # Errors
    /// - [`ListenError::Bind`], as [`Self::bind`]. `previous` is untouched.
    pub fn bind_keeping(
        previous: Option<&Self>,
        table: &BTreeMap<String, String>,
    ) -> Result<Self, ListenError> {
        let mut entries = Vec::with_capacity(table.len());
        for (name, address) in table {
            let fail = |reason: String| ListenError::Bind {
                name: name.clone(),
                address: address.clone(),
                reason,
            };
            let parsed: SocketAddr = address
                .parse()
                .map_err(|error: core::net::AddrParseError| fail(error.to_string()))?;
            let kept = previous
                .and_then(|held| held.entries.iter().find(|e| e.configured == parsed))
                .and_then(|e| e.listener.try_clone().ok());
            let listener = match kept {
                Some(listener) => listener,
                None => TcpListener::bind(parsed).map_err(|error| fail(error.to_string()))?,
            };
            entries.push(Entry {
                name: name.clone(),
                configured: parsed,
                listener,
            });
        }
        Ok(Self { entries })
    }

    /// Whether these sockets were bound from exactly `table`.
    #[must_use]
    pub fn is_bound_from(&self, table: &BTreeMap<String, String>) -> bool {
        self.entries.len() == table.len()
            && self.entries.iter().all(|e| {
                table
                    .get(&e.name)
                    .and_then(|a| a.parse::<SocketAddr>().ok())
                    == Some(e.configured)
            })
    }

    /// The entry names, in the fd order they are passed.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// How many sockets there are, which is `LISTEN_FDS`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The address each socket is bound to, in fd order.
    ///
    /// Differs from the configured address for port 0, where the OS picks.
    ///
    /// # Errors
    /// - The OS's error if a socket's local address cannot be read.
    pub fn local_addrs(&self) -> std::io::Result<Vec<SocketAddr>> {
        self.entries
            .iter()
            .map(|e| e.listener.local_addr())
            .collect()
    }

    /// A fresh descriptor for each socket, in fd order, for a child to
    /// inherit. The shepherd keeps the originals.
    ///
    /// # Errors
    /// - The OS's error if a descriptor cannot be duplicated.
    #[cfg(unix)]
    pub(crate) fn duplicates(&self) -> std::io::Result<Vec<std::os::fd::OwnedFd>> {
        self.entries
            .iter()
            .map(|e| e.listener.try_clone().map(std::os::fd::OwnedFd::from))
            .collect()
    }
}

#[cfg(all(test, unix))]
impl ListenSockets {
    /// A set whose "sockets" are any descriptors, for tests that check where a
    /// child finds them and cannot bind in a sandbox.
    ///
    /// Taken in name order, as [`Self::bind`] leaves them. Nothing here ever
    /// accepts on one.
    pub(crate) fn from_descriptors(entries: Vec<(&str, std::os::fd::OwnedFd)>) -> Self {
        let mut entries: Vec<Entry> = entries
            .into_iter()
            .map(|(name, fd)| Entry {
                name: name.to_string(),
                configured: SocketAddr::from(([0, 0, 0, 0], 0)),
                listener: TcpListener::from(fd),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Self { entries }
    }
}

/// Compared by what each was bound from, since an open socket has no value to
/// compare.
impl PartialEq for ListenSockets {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .zip(&other.entries)
                .all(|(a, b)| a.name == b.name && a.configured == b.configured)
    }
}

impl Eq for ListenSockets {}

impl fmt::Debug for ListenSockets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(
                self.entries
                    .iter()
                    .map(|e| (&e.name, e.configured.to_string())),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn it_binds_each_entry_in_name_order() {
        let sockets =
            ListenSockets::bind(&table(&[("web", "127.0.0.1:0"), ("admin", "127.0.0.1:0")]))
                .unwrap();
        assert_eq!(sockets.names().collect::<Vec<_>>(), ["admin", "web"]);
        assert_eq!(sockets.len(), 2);
        let addrs = sockets.local_addrs().unwrap();
        assert_ne!(addrs[0].port(), 0, "the OS picked a real port");
        assert_ne!(addrs[0], addrs[1]);
    }

    #[test]
    fn a_taken_address_fails_the_bind_and_names_it() {
        let held = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = held.local_addr().unwrap().to_string();

        let error = ListenSockets::bind(&table(&[("http", &address)])).unwrap_err();

        let ListenError::Bind {
            name,
            address: named,
            ..
        } = &error;
        assert_eq!((name.as_str(), named.as_str()), ("http", address.as_str()));
        assert!(
            error
                .to_string()
                .starts_with(&format!("could not listen on {address} for `http`: ")),
            "{error}"
        );
    }

    #[test]
    fn an_unparsable_address_fails_the_bind_and_names_it() {
        let error = ListenSockets::bind(&table(&[("http", "not-an-address")])).unwrap_err();
        assert!(error.to_string().contains("not-an-address"), "{error}");
    }

    #[test]
    fn a_failed_bind_leaves_the_earlier_entries_closed() {
        let held = TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = held.local_addr().unwrap().to_string();
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let free_addr = free.local_addr().unwrap();
        drop(free);

        // "a" sorts first and binds, then "b" fails.
        ListenSockets::bind(&table(&[("a", &free_addr.to_string()), ("b", &taken)])).unwrap_err();

        TcpListener::bind(free_addr).expect("the first entry's socket closed with the failure");
    }

    #[test]
    fn it_remembers_what_it_was_bound_from() {
        let from = table(&[("http", "127.0.0.1:0")]);
        let sockets = ListenSockets::bind(&from).unwrap();
        assert!(sockets.is_bound_from(&from));
        assert!(!sockets.is_bound_from(&table(&[("http", "127.0.0.1:1")])));
        assert!(!sockets.is_bound_from(&table(&[("web", "127.0.0.1:0")])));
        assert!(!sockets.is_bound_from(&BTreeMap::new()));
    }

    #[test]
    fn an_address_the_previous_set_holds_keeps_its_socket_under_a_new_name() {
        let held = ListenSockets::bind(&table(&[("http", "127.0.0.1:0")])).unwrap();
        let addr = held.local_addrs().unwrap()[0];
        // Fixed to the port the OS chose, so the two tables name one address.
        let first = table(&[("http", &addr.to_string())]);
        drop(held);
        let held = ListenSockets::bind(&first).unwrap();

        let renamed = table(&[("web", &addr.to_string())]);
        let next = ListenSockets::bind_keeping(Some(&held), &renamed)
            .expect("the held address is not bound a second time");

        assert_eq!(next.names().collect::<Vec<_>>(), ["web"]);
        assert_eq!(next.local_addrs().unwrap(), [addr]);
        assert!(
            ListenSockets::bind(&renamed).is_err(),
            "the old set still holds the address, so a plain bind would fail"
        );
    }

    #[test]
    fn a_respelled_address_is_the_same_address() {
        let held = ListenSockets::bind(&table(&[("http", "127.0.0.1:0")])).unwrap();
        let port = held.local_addrs().unwrap()[0].port();
        drop(held);
        let held = ListenSockets::bind(&table(&[("http", &format!("127.0.0.1:{port}"))])).unwrap();

        let respelled = table(&[("http", &format!("127.0.0.1:0{port}"))]);
        assert!(held.is_bound_from(&respelled));
        let next = ListenSockets::bind_keeping(Some(&held), &respelled)
            .expect("the held address is kept, not bound again");
        assert_eq!(next.local_addrs().unwrap(), held.local_addrs().unwrap());
    }

    #[test]
    fn debug_shows_names_and_addresses_only() {
        let sockets = ListenSockets::bind(&table(&[("http", "127.0.0.1:0")])).unwrap();
        assert_eq!(format!("{sockets:?}"), r#"{"http": "127.0.0.1:0"}"#);
    }

    #[cfg(unix)]
    #[test]
    fn a_duplicate_is_the_same_listening_socket() {
        let sockets = ListenSockets::bind(&table(&[("http", "127.0.0.1:0")])).unwrap();
        let addr = sockets.local_addrs().unwrap()[0];
        let duplicates = sockets.duplicates().unwrap();
        assert_eq!(duplicates.len(), 1);

        let accepting = TcpListener::from(duplicates.into_iter().next().unwrap());
        let _client = std::net::TcpStream::connect(addr).unwrap();
        let (_conn, _) = accepting.accept().unwrap();
    }
}
