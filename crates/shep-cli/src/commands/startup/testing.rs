//! Fixtures and helpers shared by this module's tests.

use super::target_resolution::StartupPlan;
use super::unit::UnitSpec;
use super::*;
use crate::cli::Init;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The unit path a `target_resolution::StartupPlan` built for a test points at, so `step_execution::install`
/// can be driven without writing into `/etc` or `/Library`. Every field
/// but `home` is fixed; `home` is what each case varies.
///
/// `with_file_name` rather than a path under `home`: the two cases that
/// matter pass a `home` that does not exist, and a unit path inside it
/// could not be written even by the run this is meant to prove writes
/// nothing.
pub(super) fn plan_for_test(home: &Path) -> StartupPlan {
    StartupPlan {
        init: Init::Systemd,
        spec: UnitSpec {
            user: "deploy".to_string(),
            exec: PathBuf::from("/usr/local/bin/shep"),
            home: home.to_path_buf(),
            path: OsString::from("/usr/local/bin:/usr/bin:/bin"),
            working_dir: PathBuf::from("/home/deploy"),
        },
        unit_path: home.with_file_name("shep-deploy.service"),
        label: unit::launchd_label("deploy"),
        sudo_user: None,
        own_default_home: None,
    }
}
