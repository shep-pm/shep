use super::*;
use crate::fake::ScriptedSweep;
use crate::proc_table::ProcInstant;
use crate::testing::capture_logs;

// `capture_logs` follows this thread, so the paused runtime stays on it.
#[test]
fn the_driver_logs_the_report_word_for_word() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    let sweep = ScriptedSweep::new()
        .with_survivors(vec![vec![7, 8], vec![7, 8], vec![7, 8, 9]])
        .with_born(vec![vec![9]])
        .refusing(7)
        .unkillable(8);
    let snapshot = LambSnapshot::new([7, 8], ProcInstant::from_raw(1_700_000_000));

    let logs = capture_logs(|| {
        runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(10),
                sweep_lambs(&sweep, &snapshot, Duration::from_secs(5)),
            )
            .await
            .expect("the grace and kill settle finish within ten seconds");
        });
    });

    let mut lines: Vec<_> = logs
        .lines()
        .map(|line| {
            let (metadata, message) = line
                .split_once("shep_daemon::sweep: ")
                .expect("the driver logs under its own module");
            let level = if metadata.contains(" INFO ") {
                "INFO"
            } else {
                assert!(metadata.contains(" WARN "), "{line}");
                "WARN"
            };
            (level, message)
        })
        .collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        vec![
            (
                "INFO",
                "swept lambs that outlived their sheep lambs=[7, 8, 9]"
            ),
            (
                "WARN",
                "lamb signal delivery failed pid=7 signal=Kill error=the OS refused the signal: scripted refusal"
            ),
            (
                "WARN",
                "lamb signal delivery failed pid=7 signal=Term error=the OS refused the signal: scripted refusal"
            ),
            (
                "WARN",
                "lambs ignored SIGTERM for the whole grace; sending SIGKILL lambs=[7, 8, 9]"
            ),
            (
                "WARN",
                "lambs still running after SIGKILL; leaving them lambs=[7, 8]"
            ),
        ],
        "{logs}\nwalks: {:?}\nlooks: {:?}",
        sweep.walks(),
        sweep.looks(),
    );
}
