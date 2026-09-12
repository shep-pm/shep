//! The machine the flock runs on, as one line above a followed listing.
//!
//! Three of the four numbers are rates, so none of them exists in a single
//! moment: CPU, disk traffic and network traffic are all differences between
//! two samples. [`HostWatch`] holds the earlier sample between redraws, which
//! is the only reason `shep flock --follow` can show them and a one-shot
//! `shep flock` cannot.
//!
//! Cost is the constraint. `lookout`'s own sampler carries a comment saying a
//! process walk is what makes `dog::metrics` expensive, and this runs on the
//! same cadence, so nothing here refreshes anything that is not printed: no
//! process table, no swap, no disk capacity, no component temperatures.
//! Measured on macOS at 5.6 ms a tick, against a floor of one second between
//! ticks.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use sysinfo::{
    CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, NetworkData, Networks, RefreshKind,
    System,
};

use crate::output::human_bytes;

/// What one redraw reads off the machine.
///
/// Every rate is `Option`: the window between two samples can be too short to
/// divide by, and on the first redraw it always is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HostSample {
    /// Every core's usage, as one percentage of one whole machine.
    pub cpu_percent: Option<f32>,
    /// Memory in use, as the platform reports it.
    pub memory_used_bytes: u64,
    /// Total physical memory.
    pub memory_total_bytes: u64,
    /// Bytes a second the block devices read and wrote over the window.
    pub disk_bytes_per_second: Option<(u64, u64)>,
    /// Bytes a second the non-loopback interfaces received and transmitted
    /// over the window.
    pub network_bytes_per_second: Option<(u64, u64)>,
}

impl HostSample {
    /// The one line that sits above a followed listing.
    ///
    /// Rates that have no window yet render as `-` rather than as zero: a
    /// machine doing nothing and a machine not yet measured are different
    /// claims, and the first redraw is always the second one.
    pub(crate) fn line(&self) -> String {
        let cpu = match self.cpu_percent {
            Some(percent) => format!("{percent:.0}%"),
            None => "-".to_owned(),
        };
        let (disk_read, disk_write) = rate_pair(self.disk_bytes_per_second);
        let (received, transmitted) = rate_pair(self.network_bytes_per_second);
        format!(
            "host  cpu {cpu}  mem {}/{}  disk r {disk_read} w {disk_write}  net rx {received} tx {transmitted}",
            human_bytes(self.memory_used_bytes),
            human_bytes(self.memory_total_bytes),
        )
    }
}

/// One rate pair rendered, or a pair of dashes where there is no window.
fn rate_pair(rate: Option<(u64, u64)>) -> (String, String) {
    match rate {
        Some((first, second)) => (
            format!("{}/s", human_bytes(first)),
            format!("{}/s", human_bytes(second)),
        ),
        None => ("-".to_owned(), "-".to_owned()),
    }
}

/// Bytes one block device moved: what it has moved since boot, and what it
/// moved over the last window.
///
/// The lifetime pair is not displayed. It is the device's identity, for
/// [`distinct_disk_io`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiskIo {
    /// Bytes read and written since boot.
    pub lifetime: (u64, u64),
    /// Bytes read and written since the previous refresh.
    pub window: (u64, u64),
}

/// Totals `entries` over distinct devices, counting each one once.
///
/// The entries are mount points, and several of them can sit on one device.
/// macOS is where this bites: `sysinfo` walks each APFS volume up to the
/// `IOBlockStorageDriver` behind it, so `/` and `/System/Volumes/Data` report
/// the same counters and a plain sum doubles every number. Measured 12 times
/// out of 12 under a 4 GB write, both volumes byte-identical every round.
///
/// Lifetime counters are the identity because `sysinfo` exposes no device
/// name to group by. Two genuinely separate devices agreeing on both 64-bit
/// counters would collapse into one, which is only reachable at boot with
/// both at zero, where they contribute nothing either way.
fn distinct_disk_io(entries: impl IntoIterator<Item = DiskIo>) -> (u64, u64) {
    let mut seen = BTreeSet::new();
    let mut read = 0u64;
    let mut written = 0u64;
    for entry in entries {
        if seen.insert(entry.lifetime) {
            read = read.saturating_add(entry.window.0);
            written = written.saturating_add(entry.window.1);
        }
    }
    (read, written)
}

/// Whether every address on `interface` is a loopback address.
///
/// A followed listing counts what left the machine. On a box where a sheep
/// answers a local proxy, loopback carries every request twice and swamps the
/// interface an operator is actually watching. Judged by address rather than
/// by name, since `lo` and `lo0` are two spellings of a set with no promised
/// end. An interface with no addresses at all is not loopback; it also moves
/// no bytes.
fn is_loopback(interface: &NetworkData) -> bool {
    let addresses = interface.ip_networks();
    !addresses.is_empty() && addresses.iter().all(|network| network.addr.is_loopback())
}

/// `bytes` over `window`, as bytes a second.
///
/// `u128` throughout: a window of a few milliseconds against a large delta
/// overflows `u64` on the multiply long before the divide brings it back.
fn per_second(bytes: u64, window: Duration) -> u64 {
    let millis = window.as_millis().max(1);
    u64::try_from(u128::from(bytes) * 1000 / millis).unwrap_or(u64::MAX)
}

/// The earlier sample, held between redraws.
///
/// Not `Default` and not `new()`: [`HostWatch::install`] answers `None` on a
/// target `sysinfo` cannot read, which is a real state a caller has to render
/// rather than an error (`dog::metrics::sample_host` makes the same call).
#[derive(Debug)]
pub(crate) struct HostWatch {
    system: System,
    disks: Disks,
    networks: Networks,
    sampled_at: Instant,
}

impl HostWatch {
    /// Takes the first sample, or answers `None` where `sysinfo` reads
    /// nothing.
    ///
    /// The sample taken here is never displayed. It is the anchor the first
    /// displayed sample subtracts from.
    pub(crate) fn install() -> Option<Self> {
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return None;
        }
        Some(Self {
            system: System::new_with_specifics(Self::refresh()),
            disks: Disks::new_with_refreshed_list_specifics(Self::disk_refresh()),
            networks: Networks::new_with_refreshed_list(),
            sampled_at: Instant::now(),
        })
    }

    /// CPU usage and memory in use, and nothing else `System` can be asked
    /// for.
    ///
    /// `with_ram()` rather than `MemoryRefreshKind::everything()`, which the
    /// two older samplers use: swap is not on this line.
    fn refresh() -> RefreshKind {
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram())
    }

    /// Bytes moved, and not capacity: `total_space` costs a `statvfs` per
    /// mount point and appears nowhere on the line.
    fn disk_refresh() -> DiskRefreshKind {
        DiskRefreshKind::nothing().with_io_usage()
    }

    /// Refreshes every source and reports the window since the last refresh.
    ///
    /// Refreshing always, reporting conditionally: a refresh that is skipped
    /// leaves the next window measuring from the wrong instant, so the short
    /// window is spent rather than avoided. What a short window suppresses is
    /// the arithmetic, not the sample.
    ///
    /// `MINIMUM_CPU_UPDATE_INTERVAL` is the floor for all three rates, not
    /// only for CPU. It is the shortest window `sysinfo` promises a CPU
    /// reading over, and dividing a handful of bytes by a handful of
    /// milliseconds is no more honest for the other two.
    pub(crate) fn sample(&mut self) -> HostSample {
        let now = Instant::now();
        let window = now.saturating_duration_since(self.sampled_at);
        self.sampled_at = now;

        self.system.refresh_specifics(Self::refresh());
        self.disks.refresh_specifics(false, Self::disk_refresh());
        self.networks.refresh(false);

        let measured = window >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL;
        let disk = measured.then(|| {
            let (read, written) = distinct_disk_io(self.disks.list().iter().map(|disk| {
                let usage = disk.usage();
                DiskIo {
                    lifetime: (usage.total_read_bytes, usage.total_written_bytes),
                    window: (usage.read_bytes, usage.written_bytes),
                }
            }));
            (per_second(read, window), per_second(written, window))
        });
        let network = measured.then(|| {
            let (received, transmitted) = self
                .networks
                .list()
                .values()
                .filter(|interface| !is_loopback(interface))
                .fold((0u64, 0u64), |(received, transmitted), interface| {
                    (
                        received.saturating_add(interface.received()),
                        transmitted.saturating_add(interface.transmitted()),
                    )
                });
            (
                per_second(received, window),
                per_second(transmitted, window),
            )
        });

        HostSample {
            cpu_percent: measured.then(|| self.system.global_cpu_usage()),
            memory_used_bytes: self.system.used_memory(),
            memory_total_bytes: self.system.total_memory(),
            disk_bytes_per_second: disk,
            network_bytes_per_second: network,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io(lifetime: (u64, u64), window: (u64, u64)) -> DiskIo {
        DiskIo { lifetime, window }
    }

    /// fails if the macOS double-count comes back: two mount points on one
    /// device carry one device's counters, and summing them reports twice
    /// the traffic the machine did.
    #[test]
    fn two_mount_points_on_one_device_are_counted_once() {
        let volumes = [
            io((1_089_321_177_088, 1_750_606_974_976), (2_641_920, 4_096)),
            io((1_089_321_177_088, 1_750_606_974_976), (2_625_536, 4_096)),
        ];

        assert_eq!(distinct_disk_io(volumes), (2_641_920, 4_096));
    }

    /// fails if the dedupe over-reaches: two devices really are two devices,
    /// and Linux reports each partition's own `/proc/diskstats` line.
    #[test]
    fn two_devices_are_counted_twice() {
        let devices = [io((100, 200), (10, 20)), io((300, 400), (30, 40))];

        assert_eq!(distinct_disk_io(devices), (40, 60));
    }

    /// fails if an empty list panics rather than reporting nothing moved.
    #[test]
    fn no_devices_move_no_bytes() {
        assert_eq!(distinct_disk_io([]), (0, 0));
    }

    /// fails if the rate arithmetic overflows instead of scaling. A tenth of
    /// a second holding 100 MiB is 1000 MiB a second.
    #[test]
    fn a_rate_scales_a_window_up_to_a_second() {
        assert_eq!(
            per_second(100 * 1024 * 1024, Duration::from_millis(100)),
            1000 * 1024 * 1024
        );
        assert_eq!(per_second(u64::MAX, Duration::from_millis(1)), u64::MAX);
        assert_eq!(per_second(512, Duration::from_secs(2)), 256);
    }

    /// fails if a zero window divides by zero.
    #[test]
    fn a_window_of_no_time_does_not_divide_by_zero() {
        assert_eq!(per_second(4, Duration::ZERO), 4000);
    }

    /// fails if the first redraw starts printing zeroes. A rate with no
    /// window is absent, and absent is not idle.
    #[test]
    fn a_sample_with_no_window_dashes_every_rate_and_still_prints_memory() {
        let sample = HostSample {
            cpu_percent: None,
            memory_used_bytes: 39_963_869_184,
            memory_total_bytes: 51_539_607_552,
            disk_bytes_per_second: None,
            network_bytes_per_second: None,
        };

        assert_eq!(
            sample.line(),
            "host  cpu -  mem 37.2G/48.0G  disk r - w -  net rx - tx -"
        );
    }

    /// fails if the measured line loses a unit or a number.
    #[test]
    fn a_measured_sample_names_all_four() {
        let sample = HostSample {
            cpu_percent: Some(11.459_433),
            memory_used_bytes: 39_963_869_184,
            memory_total_bytes: 51_539_607_552,
            disk_bytes_per_second: Some((1_258_291, 491_520)),
            network_bytes_per_second: Some((24_594, 9_260)),
        };

        assert_eq!(
            sample.line(),
            "host  cpu 11%  mem 37.2G/48.0G  disk r 1.2M/s w 480.0K/s  net rx 24.0K/s tx 9.0K/s"
        );
    }

    /// fails if the real sampler stops reading this machine's memory. The
    /// rates are not asserted: the window here is microseconds, which is the
    /// case that has to report nothing.
    #[test]
    fn the_first_sample_reads_memory_and_no_rate() {
        let Some(mut watch) = HostWatch::install() else {
            return;
        };

        let sample = watch.sample();

        assert!(sample.memory_total_bytes > 0, "a machine has memory");
        assert!(sample.memory_used_bytes > 0);
        assert_eq!(sample.cpu_percent, None, "no window, no rate");
        assert_eq!(sample.disk_bytes_per_second, None);
        assert_eq!(sample.network_bytes_per_second, None);
    }
}
