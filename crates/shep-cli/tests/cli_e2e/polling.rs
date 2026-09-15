//! Polling a real daemon until it says what a case is waiting for, rather
//! than sleeping a guess.

use super::*;

/// A port with nothing on it: bind `:0`, read what the OS chose, release it.
///
/// A stranger taking it before the dog binds is a loud loss: the dog refuses
/// to run and `shep dogs` reports it errored.
pub(crate) fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the OS must have a free loopback port")
        .local_addr()
        .expect("a bound listener has an address")
        .port()
}

/// One attempt at a `GET /metrics` scrape against `addr`, over a plain
/// `std::net::TcpStream`: this workspace carries no HTTP crate.
///
/// Reads to EOF: the dog answers one request per connection and then drops the
/// stream, so the peer closing ends the response.
///
/// # Errors
/// Connection refused (nothing bound yet), or no full response within
/// [`METRICS_SCRAPE_READ_TIMEOUT`].
pub(crate) fn scrape_metrics(addr: std::net::SocketAddr) -> std::io::Result<String> {
    use std::io::{Read as _, Write as _};
    let mut stream = std::net::TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(METRICS_SCRAPE_READ_TIMEOUT))?;
    stream.write_all(b"GET /metrics HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
    let mut body = String::new();
    stream.read_to_string(&mut body)?;
    Ok(body)
}

/// [`scrape_metrics`], retried until it answers or [`METRICS_SCRAPE_DEADLINE`]
/// expires, returning the last attempt's body (`""` if none connected). A
/// target that never comes up fails as an assertion on an empty string, never
/// a hang.
pub(crate) fn poll_metrics(addr: std::net::SocketAddr) -> String {
    let start = Instant::now();
    loop {
        if let Ok(body) = scrape_metrics(addr) {
            return body;
        }
        if start.elapsed() >= METRICS_SCRAPE_DEADLINE {
            return String::new();
        }
        std::thread::sleep(METRICS_SCRAPE_POLL_INTERVAL);
    }
}

/// One attempt at a request against a `shep serve` worker. `serve::worker`
/// answers `Connection: close`, so reading to EOF reads the whole response.
///
/// Returns the status code off the first line and everything after the blank
/// line as the body. Not a real HTTP parser: nothing this tier produces is
/// chunked.
///
/// # Errors
/// Connection refused (nothing bound yet), or no full response within
/// [`SERVE_HTTP_READ_TIMEOUT`].
pub(crate) fn http_get(
    addr: std::net::SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
) -> std::io::Result<(u16, String)> {
    use std::io::{Read as _, Write as _};
    let mut stream = std::net::TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(SERVE_HTTP_READ_TIMEOUT))?;
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = raw
        .split_once("\r\n\r\n")
        .map_or("", |(_, body)| body)
        .to_string();
    Ok((status, body))
}

/// [`http_get`], retried until it answers or [`SERVE_HTTP_DEADLINE`]
/// expires, returning the last attempt's status and body either way
/// (`(0, "")` if every attempt failed to connect at all).
pub(crate) fn poll_http_get(
    addr: std::net::SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let start = Instant::now();
    loop {
        if let Ok(answer) = http_get(addr, path, headers) {
            return answer;
        }
        if start.elapsed() >= SERVE_HTTP_DEADLINE {
            return (0, String::new());
        }
        std::thread::sleep(SERVE_HTTP_POLL_INTERVAL);
    }
}

#[cfg(unix)]
/// Runs `shep flock --format json` until it answers a `pid` for the dog named
/// `name`, or [`FLOCK_DEADLINE`] expires. `shep enable` returning success means
/// the `EnableDog` RPC landed, not that a pid is recorded.
///
/// `flock`, not `dogs`: `Response::Flock` carries both populations in one
/// array. Panics on expiry, since a `None` would leave a running dog
/// unregistered with the guard that exists to reap it.
pub(crate) fn wait_for_dog_pid(home: &Path, name: &str) -> nix::unistd::Pid {
    let flock = poll_flock_data(home, FLOCK_DEADLINE, |data| {
        data.as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|e| e["name"] == name && !e["pid"].is_null())
        })
    });
    let dog = flock
        .as_array()
        .and_then(|entries| entries.iter().find(|e| e["name"] == name))
        .unwrap_or_else(|| panic!("no entry named {name} in `shep flock`: {flock}"));
    let pid = dog["pid"]
        .as_i64()
        .unwrap_or_else(|| panic!("dog {name} has no pid after {FLOCK_DEADLINE:?}: {dog}"));
    nix::unistd::Pid::from_raw(i32::try_from(pid).expect("a real OS pid fits i32"))
}

/// Runs `shep bleats --no-follow` with `args` appended until its stdout is
/// non-empty or [`BLEATS_DEADLINE`] expires, returning the last `Output`.
///
/// The retry covers the gap between `shep start` returning and the daemon's
/// log pump writing the child's first line. Reading the log file directly
/// would tie this tier to a path rule an app's `out_file` overrides.
pub(crate) fn bleats_no_follow_until_written(home: &Path, args: &[&str]) -> Output {
    bleats_no_follow_until(home, args, |stdout| !stdout.is_empty())
}

/// [`bleats_no_follow_until_written`] for a caller that needs more than one
/// line: waits until every string in `needles` is in one reading.
///
/// A sheep's two streams reach two files the pump fills independently, so a
/// caller asserting on both would otherwise take the first reading with
/// either.
pub(crate) fn bleats_no_follow_until_contains(
    home: &Path,
    args: &[&str],
    needles: &[&str],
) -> Output {
    bleats_no_follow_until(home, args, |stdout| {
        needles.iter().all(|needle| stdout.contains(needle))
    })
}

/// The shared retry loop: runs the command until `done` accepts its stdout or
/// [`BLEATS_DEADLINE`] expires.
pub(crate) fn bleats_no_follow_until(
    home: &Path,
    args: &[&str],
    done: impl Fn(&str) -> bool,
) -> Output {
    let start = Instant::now();
    loop {
        let output = shep(home)
            .arg("bleats")
            .arg("--no-follow")
            .args(args)
            .output()
            .unwrap();
        if done(&String::from_utf8_lossy(&output.stdout)) || start.elapsed() >= BLEATS_DEADLINE {
            return output;
        }
        std::thread::sleep(BLEATS_POLL_INTERVAL);
    }
}

/// Runs `shep flock --format json` until `done` accepts the whole `data`
/// array, or `deadline` expires, returning the last observation either way.
///
/// Returning rather than panicking on expiry keeps the failure the caller's
/// own assertion. The deadline is a parameter because the two real-clock cases
/// need one an order of magnitude past [`FLOCK_DEADLINE`].
/// Every attempt must succeed; the one case that signals a handover itself
/// polls through [`poll_flock_data_across_a_handover`].
pub(crate) fn poll_flock_data(
    home: &Path,
    deadline: Duration,
    done: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    poll_flock_until(home, deadline, false, done)
}

/// [`poll_flock_data`] for the one case that signals a handover itself and
/// then polls the shepherd being replaced.
///
/// The attempt whose reply is in flight at the `execve` fails with
/// [`DROPPED_REPLY`]; asserting on it turns a tolerated event into a panic.
/// One drop, and a second is still fatal: the poll is serial and the exec
/// happens once, so at most one accepted connection is open at the swap.
#[cfg(unix)]
pub(crate) fn poll_flock_data_across_a_handover(
    home: &Path,
    deadline: Duration,
    done: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    poll_flock_until(home, deadline, true, done)
}

/// The loop the two above share. `tolerate_one_drop` lets one attempt fail
/// with a connection the shepherd closed after accepting it.
///
/// A tolerated attempt costs a retry and nothing else: it consults neither
/// `done` nor `deadline`. The retry can land after `deadline` by one poll
/// interval and one command, and that is wanted: a drop at the edge that
/// ended the poll would be the flake this closes, one window narrower.
pub(crate) fn poll_flock_until(
    home: &Path,
    deadline: Duration,
    tolerate_one_drop: bool,
    done: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let start = Instant::now();
    let mut tolerance = tolerate_one_drop;
    loop {
        let output = shep(home)
            .arg("--format")
            .arg("json")
            .arg("flock")
            .output()
            .unwrap();
        if tolerance && !output.status.success() && closed_by_a_handover(&output) {
            tolerance = false;
            std::thread::sleep(FLOCK_POLL_INTERVAL);
            continue;
        }
        assert_success(&output);
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("flock stdout was not JSON: {e}"));
        let data = envelope["data"].clone();
        if done(&data) || start.elapsed() >= deadline {
            return data;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

/// Whether `output` is a client whose connection the shepherd had accepted
/// when it replaced its own image. The two sentences and nothing else, so a
/// shepherd that is gone or refusing still fails the caller.
pub(crate) fn closed_by_a_handover(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains(DROPPED_REPLY) || stderr.contains(DROPPED_HANDSHAKE)
}

/// [`poll_flock_data`] for the single-sheep cases: waits [`FLOCK_DEADLINE`]
/// and hands `done`, and the caller, that one sheep's `ProcessInfo` rather
/// than the array around it.
pub(crate) fn poll_flock(
    home: &Path,
    done: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    poll_flock_data(home, FLOCK_DEADLINE, |data| done(&data[0]))[0].clone()
}

/// Runs `shep --format json describe <name>` until its lamb tree is non-empty
/// or `deadline` expires, returning the last `Output` either way.
///
/// `describe` walks the live process tree in its own handler, so the first
/// call after `start` races `/bin/sh` forking and exec'ing its trailing
/// `sleep`.
pub(crate) fn poll_describe_lambs(home: &Path, name: &str, deadline: Duration) -> Output {
    let start = Instant::now();
    loop {
        let output = shep(home)
            .arg("--format")
            .arg("json")
            .arg("describe")
            .arg(name)
            .output()
            .unwrap();
        assert_success(&output);
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("describe stdout was not JSON: {e}"));
        let has_lamb = envelope["data"][0]["lambs"]
            .as_array()
            .is_some_and(|lambs| !lambs.is_empty());
        if has_lamb || start.elapsed() >= deadline {
            return output;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    }
}

/// The `data[]` element named `name`, for the cases that run a control sheep
/// beside the one under test. By name, since `data[0]`/`data[1]` would swap
/// meanings if id or app ordering moved.
pub(crate) fn sheep_named<'a>(data: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    data.as_array()
        .unwrap_or_else(|| panic!("flock data must be an array: {data}"))
        .iter()
        .find(|info| info["name"] == name)
        .unwrap_or_else(|| panic!("no sheep named {name} in the flock: {data}"))
}
