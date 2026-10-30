//! Process-clock and targeted process-table reads shared with the lamb sweep
//!
//! A handover reads the epoch second an adopted sheep started. The sweep
//! dates its snapshots with [`ProcInstant`], using the same clock as the
//! process-table reads in `shep-tree`.

pub(crate) use shep_tree::ProcInstant;
#[cfg(unix)]
pub(crate) use shep_tree::instant::read_pids;
