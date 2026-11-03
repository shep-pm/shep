//! A CPU quota in cores, as `docker run --cpus` spells it.

use serde::{Deserialize, Serialize};

/// A CPU quota in cores: `1.5` lets a sheep's tree use one and a half cores'
/// worth of time, across however many cores it runs on.
///
/// Holds whatever number the Flockfile wrote. `normalize` refuses a value
/// under 0.01 and any that is not a finite number, so a config that reaches
/// the shepherd has already passed. Equality compares bits, so `NaN` equals itself and
/// the type is `Eq` like the `AppConfig` it sits in.
// wire format: changing this is a breaking change (a bare JSON/TOML number
// inside AppConfig, which travels over the client<->daemon socket)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(extend("minimum" = 0.01)))]
#[serde(transparent)]
pub struct CpuCores(f64);

impl CpuCores {
    /// The scheduling period in microseconds that `cpu.max` quotas are
    /// measured against: the kernel's own default, and what `docker run
    /// --cpus` uses.
    pub const PERIOD_US: u64 = 100_000;

    /// The smallest quota in microseconds the kernel accepts in `cpu.max`.
    pub const MIN_QUOTA_US: u64 = 1_000;

    /// Wraps a core count, unchecked
    #[inline]
    #[must_use]
    pub const fn from_cores(cores: f64) -> Self {
        Self(cores)
    }

    /// Returns the quota in cores
    #[inline]
    #[must_use]
    pub const fn cores(self) -> f64 {
        self.0
    }

    /// The microseconds of CPU time per [`Self::PERIOD_US`] this quota allows,
    /// rounded to the nearest microsecond. Saturates at the ends of `u64`.
    #[must_use]
    pub fn quota_us(self) -> u64 {
        // Saturating cast: a quota past `u64` is far above any kernel ceiling.
        (self.0 * Self::PERIOD_US as f64).round() as u64
    }

    /// Whether a cgroup's `cpu.max` can hold the quota: finite, and at least
    /// [`Self::MIN_QUOTA_US`] per period, which is 0.01 cores.
    #[must_use]
    pub fn is_enforceable(self) -> bool {
        self.0.is_finite() && self.quota_us() >= Self::MIN_QUOTA_US
    }
}

impl PartialEq for CpuCores {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for CpuCores {}

#[cfg(test)]
mod tests {
    use super::CpuCores;

    #[test]
    fn it_reads_a_whole_number_and_a_decimal() {
        assert_eq!(
            serde_json::from_str::<CpuCores>("2").unwrap(),
            CpuCores::from_cores(2.0)
        );
        assert_eq!(
            serde_json::from_str::<CpuCores>("1.5").unwrap(),
            CpuCores::from_cores(1.5)
        );
    }

    #[test]
    fn the_smallest_enforceable_quota_is_one_hundredth_of_a_core() {
        assert!(CpuCores::from_cores(0.01).is_enforceable());
        assert!(!CpuCores::from_cores(0.009).is_enforceable());
        assert!(!CpuCores::from_cores(0.0).is_enforceable());
        assert!(!CpuCores::from_cores(f64::NAN).is_enforceable());
        assert!(!CpuCores::from_cores(f64::INFINITY).is_enforceable());
    }

    #[test]
    fn it_refuses_a_string() {
        assert!(serde_json::from_str::<CpuCores>(r#""1.5""#).is_err());
    }

    #[test]
    fn it_writes_a_bare_number() {
        assert_eq!(
            serde_json::to_string(&CpuCores::from_cores(0.5)).unwrap(),
            "0.5"
        );
    }
}
