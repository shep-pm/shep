//! The muster tests' one flock: a fresh `$SHEP_HOME`, a shepherd over a
//! scripted runner, a registry, and the bus that shepherd publishes on.

use super::super::*;

use crate::fake::ScriptedRunner;
use crate::supervisor::spawn_supervisor;
use crate::testing::test_paths;
use shep_core::paths::ShepPaths;

/// A shepherd, a registry and a laid-out `$SHEP_HOME`, built one way for
/// every test that drives a muster restore.
///
/// A test names its roll and its runner scripts, and asserts on what the
/// restore did. `events` is public so a test that reads the bus subscribes
/// before the restore runs, which is the only way its own `Start` events are
/// still ahead of the reader.
pub(crate) struct TestFlock {
    pub(crate) paths: ShepPaths,
    pub(crate) handle: SupervisorHandle,
    pub(crate) registry: FlockRegistry,
    pub(crate) events: Bus,
    // Kept alive only: dropping it removes the directory `paths` names.
    _dir: tempfile::TempDir,
    // Kept alive only: with no receiver left every send is a silent no-op,
    // and a restore that publishes nothing has no events to assert on.
    _rx: broadcast::Receiver<SharedEvent>,
}

impl TestFlock {
    /// A flock whose shepherd replays `runner`, over a fresh `$SHEP_HOME`
    /// with the roll's parent directory already in place.
    ///
    /// # Panics
    ///
    /// If the tempdir or the roll's parent directory cannot be created, which
    /// is a fixture fault rather than a condition under test.
    #[track_caller]
    pub(crate) fn new(runner: ScriptedRunner) -> Self {
        let dir = tempfile::tempdir().expect("a test must be able to make a tempdir");
        let paths = test_paths(&dir);
        std::fs::create_dir_all(
            paths
                .snapshot
                .parent()
                .expect("a roll path always has a parent"),
        )
        .expect("the test's $SHEP_HOME layout must be creatable");
        let (events, rx) = crate::bus::test_bus(64);
        let handle = spawn_supervisor(runner, paths.clone(), events.clone());
        Self {
            paths,
            handle,
            registry: FlockRegistry::new(),
            events,
            _dir: dir,
            _rx: rx,
        }
    }

    /// Writes `roll` to this flock's roll path, so [`muster`] has a roll to
    /// read.
    ///
    /// # Panics
    ///
    /// If the roll cannot be written, which is a fixture fault rather than a
    /// condition under test.
    #[track_caller]
    pub(crate) fn seed(&self, roll: &FlockSnapshot) {
        write_atomic(&self.paths.snapshot, roll).expect("the fixture roll must be writable");
    }

    /// Restores this flock's own roll, naming no dogs for the restore's
    /// graph. What the flock itself already holds is the test's business.
    ///
    /// # Errors
    ///
    /// Whatever [`muster`] returns: a missing roll is `Ok` of nothing, and a
    /// roll that exists but cannot be read or parsed is an `Err`.
    pub(crate) async fn muster(&self) -> Result<Vec<String>, SnapshotError> {
        muster(
            &self.paths.snapshot,
            &self.registry,
            &self.handle,
            &self.events,
            &[],
            &[],
            None,
        )
        .await
    }

    /// Stops the shepherd and waits for it.
    pub(crate) async fn shutdown(self) {
        self.handle.shutdown().await;
    }
}
