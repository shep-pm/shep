// use schemars::generate
use crate::values::UpDuration;
use serde::{Deserialize, Serialize};

/// How a health probe checks a sheep
// wire format: changing these strings is a breaking change
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// HTTP GET must return 2xx
    Http,
    /// TCP connect must succeed
    Tcp,
    /// Command must exit 0
    Exec,
}

/// Readiness/liveness probe configuration (spec §7)
// wire format: changing field names/defaults is a breaking change
// No `deny_unknown_fields` here: this type rides the wire, where an
// unknown field means a newer peer. The denial lives in `Flockfile::parse`.
// `schemars(deny_unknown_fields)` below shapes a schema only, never a parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub struct ProbeConfig {
    /// Probe mechanism
    pub kind: ProbeKind,
    /// URL (http), `host:port` (tcp), or command line (exec)
    pub target: String,
    /// Time between probes (default 10s)
    #[serde(default = "default_probe_interval")]
    pub interval: UpDuration,
    /// Per-probe timeout (default 5s)
    #[serde(default = "default_probe_timeout")]
    pub timeout: UpDuration,
    /// Consecutive failures before the probe reports unhealthy (default 3)
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
}

pub(super) fn default_probe_interval() -> UpDuration {
    UpDuration::from_millis(10_000)
}

pub(super) fn default_probe_timeout() -> UpDuration {
    UpDuration::from_millis(5_000)
}

pub(super) fn default_failure_threshold() -> u32 {
    3
}
