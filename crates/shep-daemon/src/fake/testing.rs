//! Fixtures and helpers shared by this module's tests.

use crate::runner::SpawnSpec;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) fn spec() -> SpawnSpec {
    SpawnSpec {
        name: "web".to_string(),
        program: "/bin/true".to_string(),
        args: vec![],
        cwd: None,
        env: BTreeMap::new(),
        out_file: PathBuf::from("/tmp/shep-test-out.log"),
        err_file: PathBuf::from("/tmp/shep-test-err.log"),
        channel: true,
        stdin: false,
        credentials: None,
    }
}
