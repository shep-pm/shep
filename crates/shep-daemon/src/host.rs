//! The machine the flock runs on, sampled on a tick so a listing never has
//! to.
//!
//! Three of the four numbers a host line shows are rates, so none of them
//! exists in a single moment: CPU, disk traffic and network traffic are all
//! differences between two readings of a counter the kernel only ever
//! increments. The kernel stores an odometer, and nothing anywhere reports a
//! speed.
//!
//! So something that is already running has to keep the earlier reading.
//! [`HostState`] is that something, and it is the reason a one-shot
//! `shep flock` can show the same numbers `shep flock --follow` does. The
//! shape is [`limits::stats`](crate::limits::stats)': one writer on a
//! periodic tick, and a reader that serves a listing by taking what is
//! already there.
//!
//! The invariant here is stronger than that module's, and structurally so
//! rather than by rule: the [`HostSource`] lives inside the sampling task,
//! so a request cannot reach it. Sampling twice in quick succession would
//! divide a near-zero delta by a near-zero window and report anything from
//! nothing to thousands.
//!
//! Cost is what decides the tick. Nothing here refreshes anything that is
//! not served: no process table, which is what makes
//! [`limits::sample`](crate::limits::sample)'s walk expensive, no swap, no
//! disk capacity, no component temperatures. Measured on macOS in a release
//! build at 715 us a tick over three rounds of 100, of which the block
//! devices are 450 us and the interfaces 240 us; CPU and memory together
//! are under a microsecond. Split by source on purpose, so the next person
//! to want this cheaper knows which half to cut.

use core::time::Duration;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use sysinfo::{
    CpuRefreshKind, DiskRefreshKind, Disks, IpNetwork, MemoryRefreshKind, NetworkData, Networks,
    RefreshKind, System,
};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use shep_core::protocol::HostUsage;

/// How often the shepherd reads the host counters.
///
/// One second, which is the interval three other things in this workspace
/// already run on: `shep flock --follow`'s default redraw, its floor, and
/// `shep lookout`'s own host sampler. A followed listing therefore keeps
/// moving at the cadence it moved at when it sampled for itself.
///
/// Comfortably above `sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`, the 200 ms
/// floor [`is_measurable`] holds every rate to, and cheap enough at 715 us a
/// tick that an idle shepherd spends 0.07% of one core on it.
pub(crate) const HOST_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Where one host reading comes from.
///
/// A seam so [`HostState`]'s loop can be driven by a hand-rolled fake under
/// a paused clock. The real one walks `sysinfo`; a fake answers from a
/// script, which is the only way to assert what the loop does with a
/// reading rather than what this machine happened to be doing.
pub(crate) trait HostSource: Send + 'static {
    /// The machine now, against the window since the previous call.
    fn sample(&mut self) -> HostUsage;
}

/// The latest reading, and the task that is the only thing allowed to
/// replace it.
///
/// Held as an `Arc` by the RPC layer, which reads it and never writes. The
/// task is aborted when the last handle drops, the same stop mechanism
/// [`PollingEnforcer`](crate::limits::PollingEnforcer) uses and the only one
/// this type needs.
#[derive(Debug)]
pub(crate) struct HostState {
    // `std::sync::Mutex`: the critical section is one clone of five numbers,
    // never held across an `.await`, and there is no second lock to order
    // against.
    latest: Arc<Mutex<Option<HostUsage>>>,
    /// `None` on a platform `sysinfo` cannot read, where there is nothing to
    /// poll and [`Self::latest`] stays empty for the life of the daemon.
    task: Option<JoinHandle<()>>,
}

impl HostState {
    /// The production wiring: `sysinfo` on [`HOST_POLL_INTERVAL`].
    ///
    /// Must be called from within a Tokio runtime context: it spawns the
    /// sampling task at once.
    #[must_use]
    pub(crate) fn real() -> Arc<Self> {
        Self::start(
            HostWatch::install().map(|watch| Box::new(watch) as Box<dyn HostSource>),
            HOST_POLL_INTERVAL,
        )
    }

    /// The reading from the last tick, or `None` where there has not been
    /// one.
    ///
    /// Two reasons for `None` and they are not the same: a platform
    /// `sysinfo` cannot read at all, and a daemon whose sampling task has
    /// not reached its first line yet. The second lasts microseconds and is
    /// over before the control socket accepts anything, so the answer a
    /// client can actually observe is the first.
    ///
    /// Never samples. That is the whole point of the type.
    #[must_use]
    pub(crate) fn latest(&self) -> Option<HostUsage> {
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A state that answers `reading` forever and polls nothing.
    ///
    /// For a test that needs the RPC layer to have something to serve.
    /// Construction rather than a setter: the production type has exactly
    /// one writer, and a test-only way to write over a live one would be a
    /// second.
    #[cfg(test)]
    pub(crate) fn fixed(reading: Option<HostUsage>) -> Arc<Self> {
        Arc::new(Self {
            latest: Arc::new(Mutex::new(reading)),
            task: None,
        })
    }

    /// Starts the sampling task over `source`, or none at all when there is
    /// nothing to sample.
    fn start(source: Option<Box<dyn HostSource>>, interval: Duration) -> Arc<Self> {
        let latest = Arc::new(Mutex::new(None));
        let task = source.map(|mut source| {
            let writes_to = Arc::clone(&latest);
            tokio::spawn(async move {
                // The seeding read, before the first tick rather than after
                // it: its window is zero so every rate comes back absent,
                // but memory is not a rate and this is what puts a real
                // figure in front of a listing that arrives in the first
                // second of a shepherd's life.
                *writes_to.lock().unwrap_or_else(PoisonError::into_inner) = Some(source.sample());

                let mut ticker = tokio::time::interval(interval);
                // The first tick of a tokio interval is immediate, and the
                // seeding read above has just taken it.
                ticker.tick().await;
                // A runtime that stalled must not then burst every tick it
                // banked: each burst tick would divide a real delta by a
                // near-zero window. `Delay` measures the next interval from
                // the reading that just finished.
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    let reading = source.sample();
                    *writes_to.lock().unwrap_or_else(PoisonError::into_inner) = Some(reading);
                }
            })
        });
        Arc::new(Self { latest, task })
    }
}

impl Drop for HostState {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Bytes one block device moved: what it has moved since boot, and what it
/// moved over the last window.
///
/// The lifetime pair is not served. It is the device's identity, for
/// [`distinct_disk_io`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiskIo {
    /// Bytes read and written since boot.
    lifetime: (u64, u64),
    /// Bytes read and written since the previous refresh.
    window: (u64, u64),
}

/// Totals `entries` over distinct devices, counting each one once.
///
/// The entries are mount points, and several of them can sit on one device.
/// macOS is where this bites: `sysinfo` walks each APFS volume up to the
/// `IOBlockStorageDriver` behind it, so `/` and `/System/Volumes/Data`
/// report the same counters and a plain sum doubles every number. Measured
/// 12 times out of 12 under a 4 GB write, both volumes byte-identical every
/// round.
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
/// A host line counts what left the machine. On a box where a sheep answers
/// a local proxy, loopback carries every request twice and swamps the
/// interface an operator is actually watching. Judged by address rather than
/// by name, since `lo` and `lo0` are two spellings of a set with no promised
/// end. An interface with no addresses at all is not loopback; it also moves
/// no bytes.
fn is_loopback(interface: &NetworkData) -> bool {
    addresses_are_loopback(interface.ip_networks())
}

/// The judgement [`is_loopback`] makes, over the addresses alone.
///
/// Split out because `sysinfo::NetworkData` has no public constructor, so
/// the rule above it is untestable while it is wired to one. It decides
/// which interfaces reach the network rate at all, and a regression would
/// inflate or deflate every number a host line carries without failing
/// anything.
fn addresses_are_loopback(addresses: &[IpNetwork]) -> bool {
    !addresses.is_empty() && addresses.iter().all(|network| network.addr.is_loopback())
}

/// Whether `window` is long enough to report a rate over.
///
/// `MINIMUM_CPU_UPDATE_INTERVAL` is the floor for all three rates, not only
/// for CPU. It is the shortest window `sysinfo` promises a CPU reading over,
/// and dividing a handful of bytes by a handful of milliseconds is no more
/// honest for the other two.
fn is_measurable(window: Duration) -> bool {
    window >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL
}

/// `bytes` over `window`, as bytes a second.
///
/// `u128` throughout: a window of a few milliseconds against a large delta
/// overflows `u64` on the multiply long before the divide brings it back.
fn per_second(bytes: u64, window: Duration) -> u64 {
    let millis = window.as_millis().max(1);
    u64::try_from(u128::from(bytes) * 1000 / millis).unwrap_or(u64::MAX)
}

/// The earlier reading, held between ticks.
///
/// Not `Default` and not `new()`: [`HostWatch::install`] answers `None` on a
/// target `sysinfo` cannot read, which is a real state a caller has to serve
/// rather than an error.
///
/// `tokio::time::Instant`, not `std::time::Instant`, for
/// [`limits::stats`](crate::limits::stats)' reason: a paused clock is what
/// lets a test advance a window and get a rate out, where a wall clock would
/// make every test assert on however long a CI runner took.
#[derive(Debug)]
struct HostWatch {
    system: System,
    disks: Disks,
    networks: Networks,
    sampled_at: Instant,
}

impl HostWatch {
    /// Takes the anchor reading, or answers `None` where `sysinfo` reads
    /// nothing.
    ///
    /// The reading taken here is never served. It is what the first served
    /// one subtracts from.
    fn install() -> Option<Self> {
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
    /// two host readings in shep-cli use: swap is not on this line.
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
}

impl HostSource for HostWatch {
    /// Refreshes every source and reports against the window since the last
    /// refresh.
    ///
    /// Refreshing always, reporting conditionally: a refresh that is skipped
    /// leaves the next window measuring from the wrong instant, so the short
    /// window is spent rather than avoided. What [`is_measurable`]
    /// suppresses is the arithmetic, not the reading.
    fn sample(&mut self) -> HostUsage {
        let now = Instant::now();
        let window = now.saturating_duration_since(self.sampled_at);
        self.sampled_at = now;

        self.system.refresh_specifics(Self::refresh());
        // `true` on both: drop whatever this enumeration no longer lists.
        // The flag is `remove_not_listed`, and with `false` a device or an
        // interface that goes away keeps its entry, frozen at the last
        // window's delta, which the sums below then re-add on every later
        // sample for the life of the daemon. A disk keeps its lifetime pair
        // too, so it also stays a distinct `distinct_disk_io` key. Nothing
        // is lost by dropping them: a device seen for the first time starts
        // at `read_bytes == old_read_bytes`, so its first reported delta is
        // zero rather than its whole lifetime counter (sysinfo 0.38.4).
        self.disks.refresh_specifics(true, Self::disk_refresh());
        self.networks.refresh(true);

        let measured = is_measurable(window);
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

        HostUsage {
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

    /// fails if a seeding reading starts reporting rates over a window too
    /// short to divide by, or if the floor drifts off `sysinfo`'s own.
    ///
    /// A pure function rather than a real reading, deliberately: asserting
    /// that [`HostWatch::install`] and the reading after it fall inside
    /// 200 ms would be asserting that a CI runner never deschedules a
    /// thread, and the rule under test has nothing to do with how fast the
    /// machine is.
    #[test]
    fn a_window_under_sysinfos_own_floor_carries_no_rate() {
        assert!(!is_measurable(Duration::ZERO));
        assert!(!is_measurable(Duration::from_millis(5)));
        assert!(!is_measurable(
            sysinfo::MINIMUM_CPU_UPDATE_INTERVAL - Duration::from_nanos(1)
        ));
        assert!(is_measurable(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL));
        assert!(
            is_measurable(HOST_POLL_INTERVAL),
            "the tick this daemon actually runs on"
        );
    }

    fn net(addr: &str) -> IpNetwork {
        IpNetwork {
            addr: addr.parse().expect("a literal address"),
            prefix: 8,
        }
    }

    /// fails if loopback stops being judged by address. Judging by name was
    /// the alternative, and `lo` and `lo0` are two spellings of a set with no
    /// promised end.
    #[test]
    fn an_interface_is_loopback_only_when_every_address_is() {
        assert!(addresses_are_loopback(&[net("127.0.0.1")]));
        assert!(addresses_are_loopback(&[net("127.0.0.1"), net("::1")]));
        assert!(!addresses_are_loopback(&[net("192.168.1.4")]));
        assert!(
            !addresses_are_loopback(&[net("127.0.0.1"), net("192.168.1.4")]),
            "one routable address is enough to make an interface count"
        );
    }

    /// An interface with no addresses is not loopback, so it stays in the
    /// total; it also moves no bytes, so it contributes nothing either way.
    #[test]
    fn an_interface_with_no_addresses_is_not_loopback() {
        assert!(!addresses_are_loopback(&[]));
    }

    /// fails if the real sampler stops reading this machine's memory. Memory
    /// is not a rate, so it is the one number a seeding reading must carry.
    #[test]
    fn a_real_reading_reads_this_machines_memory() {
        let Some(mut watch) = HostWatch::install() else {
            return;
        };

        let reading = watch.sample();

        assert!(reading.memory_total_bytes > 0, "a machine has memory");
        assert!(reading.memory_used_bytes > 0);
    }

    /// A source that answers from a script and counts how often it was
    /// asked.
    ///
    /// The count is what makes [`a_listing_never_samples`] mean anything: a
    /// reader that took its own reading would look identical from the
    /// numbers alone.
    struct ScriptedSource {
        readings: std::sync::mpsc::Receiver<HostUsage>,
        calls: Arc<Mutex<u32>>,
    }

    impl HostSource for ScriptedSource {
        fn sample(&mut self) -> HostUsage {
            *self.calls.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            self.readings
                .try_recv()
                .expect("the script ran out of readings")
        }
    }

    /// A reading whose memory figure is `used`, so a test can tell one
    /// reading from the next by looking at one number.
    fn reading(used: u64, cpu: Option<f32>) -> HostUsage {
        HostUsage {
            cpu_percent: cpu,
            memory_used_bytes: used,
            memory_total_bytes: 51_539_607_552,
            disk_bytes_per_second: cpu.map(|_| (1_258_291, 491_520)),
            network_bytes_per_second: cpu.map(|_| (24_594, 9_260)),
        }
    }

    /// A state over `script`, already past its seeding reading, and the call
    /// counter behind it.
    async fn scripted(script: Vec<HostUsage>) -> (Arc<HostState>, Arc<Mutex<u32>>) {
        let (tx, readings) = std::sync::mpsc::channel();
        for entry in script {
            tx.send(entry).expect("the receiver is alive");
        }
        let calls = Arc::new(Mutex::new(0));
        let source = ScriptedSource {
            readings,
            calls: Arc::clone(&calls),
        };
        let state = HostState::start(Some(Box::new(source)), HOST_POLL_INTERVAL);
        // The task has to reach its seeding read and commit to the first
        // tick before the clock moves, or the jump below lands before the
        // timer it is meant to fire exists.
        tokio::task::yield_now().await;
        (state, calls)
    }

    /// fails if a listing that arrives in the first second of a shepherd's
    /// life gets nothing at all. Memory is not a rate, so it is readable
    /// before any window has passed, and a shepherd with no window yet is
    /// not the same claim as a platform that cannot be read.
    #[tokio::test(start_paused = true)]
    async fn the_seeding_reading_lands_before_the_first_tick() {
        let (state, calls) = scripted(vec![reading(1, None)]).await;

        let seeded = state.latest().expect("a seeding reading");
        assert_eq!(seeded.memory_used_bytes, 1);
        assert_eq!(seeded.cpu_percent, None, "no window has passed yet");
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    /// fails if the tick stops replacing what a listing reads, which would
    /// leave every `shep flock` quoting the shepherd's boot instant forever.
    #[tokio::test(start_paused = true)]
    async fn each_tick_replaces_what_a_listing_reads() {
        let (state, calls) = scripted(vec![
            reading(1, None),
            reading(2, Some(11.5)),
            reading(3, Some(22.5)),
        ])
        .await;

        // `sleep`, not `advance`: `tokio::time::advance` does not promise
        // every pending timer has been processed by the time it returns, and
        // the enforcer's own tests were caught by that.
        tokio::time::sleep(HOST_POLL_INTERVAL + Duration::from_millis(1)).await;
        assert_eq!(state.latest().expect("tick 1").memory_used_bytes, 2);

        tokio::time::sleep(HOST_POLL_INTERVAL).await;
        assert_eq!(state.latest().expect("tick 2").memory_used_bytes, 3);
        assert_eq!(
            *calls.lock().unwrap(),
            3,
            "the seeding read plus one per tick, and nothing else"
        );
    }

    /// The invariant the whole module exists for. A reader that took its own
    /// reading would divide a near-zero delta by a near-zero window and
    /// report anything from nothing to thousands, and it would look exactly
    /// like this test's subject from the outside.
    ///
    /// fails if `latest` ever reaches the source.
    #[tokio::test(start_paused = true)]
    async fn a_listing_never_samples() {
        let (state, calls) = scripted(vec![reading(1, None), reading(2, Some(11.5))]).await;

        tokio::time::sleep(HOST_POLL_INTERVAL + Duration::from_millis(1)).await;
        let after_one_tick = *calls.lock().unwrap();

        for _ in 0..100 {
            assert_eq!(state.latest().expect("a reading").memory_used_bytes, 2);
        }

        assert_eq!(
            *calls.lock().unwrap(),
            after_one_tick,
            "a hundred listings must cost the machine nothing"
        );
    }

    /// fails if a platform `sysinfo` cannot read starts spawning a task that
    /// has nothing to poll. `None` here is what the RPC layer turns into
    /// `Response::HostUsage(None)`, which a reader spells as unavailable
    /// rather than as unmeasured.
    #[tokio::test(start_paused = true)]
    async fn a_platform_that_cannot_be_read_polls_nothing() {
        let state = HostState::start(None, HOST_POLL_INTERVAL);

        assert!(state.task.is_none());
        tokio::time::sleep(HOST_POLL_INTERVAL * 5).await;
        assert_eq!(state.latest(), None);
    }

    /// fails if the sampling task outlives the daemon that owns it. Every
    /// other stop mechanism in this crate is a `Drop`, and a task left
    /// running after a handover would have two shepherds reading the same
    /// counters.
    #[tokio::test(start_paused = true)]
    async fn dropping_the_last_handle_stops_the_task() {
        let (state, calls) = scripted(vec![reading(1, None), reading(2, Some(11.5))]).await;

        tokio::time::sleep(HOST_POLL_INTERVAL + Duration::from_millis(1)).await;
        let before = *calls.lock().unwrap();
        drop(state);

        // Well past several ticks. The script holds nothing more, so a task
        // still running would panic in `ScriptedSource::sample` rather than
        // merely raise the count.
        tokio::time::sleep(HOST_POLL_INTERVAL * 5).await;

        assert_eq!(*calls.lock().unwrap(), before);
    }
}
