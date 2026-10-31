//! `get_metrics`' tests: the local sample, and a host's `HostUsage`.
//!
//! Split out of `read_tests.rs` under IR-48.

use shep_core::protocol::Response;

use super::*;

/// `get_metrics` is the whistle's other caller of the host sample, and
/// it crosses the same `spawn_blocking` boundary as the metrics dog's
/// scrape path. Every other tool here has a test; this is the one
/// whose host half was unpinned.
#[tokio::test]
async fn get_metrics_carries_the_host_sample() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Flock(vec![shep_client::testing::sample_info()]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams { host: None })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");

    assert!(
        result.0.host.is_some(),
        "the host sample must survive the trip to the blocking pool"
    );
    assert_eq!(result.0.flock.len(), 1);

    served.await.expect("the fake daemon task must not panic");
}

/// Both handshake fields come off the `HelloAck`, not the `Response::Flock`.
///
/// The sheep's pid is deliberately not the ack's: with both at 4242, a
/// `daemon_pid` taken from the flock would read as the handshake. The version
/// half pins only that the field is filled from the handshake, since
/// `Shepherd::connect` refuses any other version.
#[tokio::test]
async fn get_metrics_reports_the_version_and_pid_from_the_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let mut sheep = shep_client::testing::sample_info();
    sheep.pid = Some(9999);
    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Flock(vec![sheep]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams { host: None })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");

    assert_eq!(
        result.0.flock.first().and_then(|row| row.pid),
        Some(9999),
        "the sheep's own pid is on the same reply, so the two numbers \
         are tellable apart there: {:?}",
        result.0.flock.first().map(|row| row.pid)
    );
    assert_eq!(
        result.0.daemon_pid, 4242,
        "the ack's pid, not the sheep's 9999: {}",
        result.0.daemon_pid
    );
    assert_eq!(
        result.0.daemon_version,
        env!("CARGO_PKG_VERSION"),
        "filled from the handshake, not left at a placeholder: {}",
        result.0.daemon_version
    );

    served.await.expect("the fake daemon task must not panic");
}

/// What a remote shepherd reports for itself. The totals are values no real
/// machine running these tests has, so a reply that sampled this one instead
/// reads as the wrong number.
#[cfg(unix)]
fn remote_usage() -> shep_core::protocol::HostUsage {
    shep_core::protocol::HostUsage {
        cpu_percent: Some(12.5),
        memory_used_bytes: 1_000,
        memory_total_bytes: 2_000,
        disk_bytes_per_second: None,
        network_bytes_per_second: None,
    }
}

/// A `host` makes `get_metrics` ask that host's shepherd for the flock and for
/// its own `HostUsage`, over the one connection, and never sysinfo-sample the
/// machine the model is running on: the totals in the reply are the stand-in's.
#[cfg(unix)]
#[tokio::test]
async fn get_metrics_over_a_host_reads_host_usage_and_never_samples_this_machine() {
    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in_replying(
        dir.path(),
        Ok(matching_ack()),
        vec![
            Response::Flock(vec![shep_client::testing::sample_info()]),
            Response::HostUsage(Some(remote_usage())),
        ],
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams {
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a stand-in shepherd must not produce a tool error");

    assert_eq!(result.0.host_name.as_deref(), Some("amp2"));
    assert_eq!(result.0.flock.len(), 1);
    let host = result.0.host.expect("the host's usage must be reported");
    assert_eq!(
        (host.memory_total_bytes, host.memory_used_bytes),
        (2_000, 1_000),
        "the stand-in's numbers, not this machine's: {host:?}"
    );

    let bytes = written_within(TEST_TIMEOUT, &stand_in_file(dir.path(), "request-2.bin")).await;
    let envelope: shep_core::protocol::Envelope =
        shep_core::protocol::decode_frame(&bytes).unwrap();
    assert_eq!(envelope.body, Request::HostUsage);
}

/// `HostUsage` carries neither a process count nor an uptime, so a remote
/// reply leaves both keys out instead of reporting this machine's.
#[cfg(unix)]
#[tokio::test]
async fn get_metrics_over_a_host_leaves_processes_and_uptime_out() {
    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in_replying(
        dir.path(),
        Ok(matching_ack()),
        vec![
            Response::Flock(vec![shep_client::testing::sample_info()]),
            Response::HostUsage(Some(remote_usage())),
        ],
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams {
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a stand-in shepherd must not produce a tool error");

    assert_eq!(
        serde_json::to_value(&result.0).unwrap()["host"],
        serde_json::json!({"memory_total_bytes": 2000, "memory_used_bytes": 1000}),
    );
}

/// A call that names no host answers with the keys it always had: both
/// machine-wide counts present, and no `host_name`.
#[tokio::test]
async fn get_metrics_without_a_host_keeps_its_local_shape() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Flock(vec![shep_client::testing::sample_info()]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams { host: None })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");

    let reading = serde_json::to_value(&result.0).unwrap();
    assert!(reading.get("host_name").is_none(), "{reading}");
    let host = &reading["host"];
    assert!(host["processes"].is_u64(), "{host}");
    assert!(host["uptime_seconds"].is_u64(), "{host}");

    served.await.expect("the fake daemon task must not panic");
}
