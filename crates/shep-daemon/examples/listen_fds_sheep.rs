//! A sheep that serves the sockets shep passed it, and says how it got them.
//!
//! Used by `tests/real_runner`, since `/bin/sh` cannot accept on a
//! descriptor. It reads systemd's contract: `LISTEN_PID` must be its own pid,
//! `LISTEN_FDS` sockets sit from fd 3, and `SHEP_CHANNEL_FD` names the shepherd
//! channel, which it greets with a `ready`.
//!
//! Every connection to the socket on fd `N` is answered with one line:
//! `fd=N pid=<pid> listen_pid=<LISTEN_PID> listen_fds=<LISTEN_FDS>
//! names=<LISTEN_FDNAMES> channel=<SHEP_CHANNEL_FD>`, then closed.

#[cfg(unix)]
fn main() {
    unix::serve();
}

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::os::fd::{FromRawFd as _, RawFd};
    use std::thread;

    /// The first descriptor of systemd's socket-activation layout.
    const LISTEN_FDS_START: RawFd = 3;

    fn var(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| "unset".to_string())
    }

    /// Greets the shepherd on the channel, on the descriptor it names.
    fn say_ready() {
        let Ok(channel) = std::env::var("SHEP_CHANNEL_FD") else {
            return;
        };
        let fd: RawFd = channel.parse().expect("SHEP_CHANNEL_FD must be a number");
        // SAFETY: shep opened this descriptor for the child and named it in
        // `SHEP_CHANNEL_FD`; nothing else in this process owns it.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(b"{\"kind\":\"ready\"}\n")
            .expect("the channel must take a line");
        // Held open: closing it would hang the channel up on the shepherd.
        std::mem::forget(file);
    }

    pub fn serve() {
        let pid = std::process::id();
        assert_eq!(
            var("LISTEN_PID"),
            pid.to_string(),
            "LISTEN_PID must be this process's own pid"
        );
        let count: RawFd = var("LISTEN_FDS")
            .parse()
            .expect("LISTEN_FDS must be a number");
        let line = |fd: RawFd| {
            format!(
                "fd={fd} pid={pid} listen_pid={} listen_fds={} names={} channel={}\n",
                var("LISTEN_PID"),
                var("LISTEN_FDS"),
                var("LISTEN_FDNAMES"),
                var("SHEP_CHANNEL_FD"),
            )
        };

        let mut servers = Vec::new();
        for fd in LISTEN_FDS_START..LISTEN_FDS_START + count {
            // SAFETY: `LISTEN_FDS` says the shepherd passed a listening socket
            // on every descriptor from 3 up, and nothing here owns them yet.
            let listener = unsafe { TcpListener::from_raw_fd(fd) };
            let reply = line(fd);
            servers.push(thread::spawn(move || {
                for conn in listener.incoming() {
                    let _ = conn.and_then(|mut conn| conn.write_all(reply.as_bytes()));
                }
            }));
        }
        say_ready();
        for server in servers {
            let _ = server.join();
        }
    }
}

#[cfg(not(unix))]
fn main() {
    // Reached by nothing: the only caller is a `#![cfg(unix)]` test binary.
    // Present so the Windows leg of the matrix compiles this target.
    eprintln!("listen_fds_sheep needs inherited descriptors");
    std::process::exit(1);
}
