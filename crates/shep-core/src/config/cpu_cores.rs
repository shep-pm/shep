//! A CPU quota in cores, as `docker run --cpus` spells it.

use serde::{Deserialize, Serialize};

/// A CPU quota in cores: `1.5` lets a sheep's tree use one and a half cores'
/// worth of time, across however many cores it runs on.
///
/// Holds whatever number the Flockfile wrote. `normalize` refuses a value
/// that is not finite and positive, so a config that reaches the shepherd
/// has already passed. Equality compares bits, so `NaN` equals itself and
/// the type is `Eq` like the `AppConfig` it sits in.
// wire format: changing this is a breaking change (a bare JSON/TOML number
// inside AppConfig, which travels over the client<->daemon socket)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(extend("exclusiveMinimum" = 0)))]
#[serde(transparent)]
pub struct CpuCores(f64);

impl CpuCores {
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

    /// Whether the quota is a finite number above zero, the only kind a
    /// cgroup's `cpu.max` can express.
    #[must_use]
    pub fn is_positive(self) -> bool {
        self.0.is_finite() && self.0 > 0.0
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
