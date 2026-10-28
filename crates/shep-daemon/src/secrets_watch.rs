//! Noticing that `secrets.json` moved a value a running dog names.
//!
//! `shep secret set` and a lookout write edit the store without speaking to
//! the shepherd, so the shepherd polls it. A dog whose `dogs.toml` section
//! names a changed key hears `config.dog.<name>` and re-reads its section.

use core::fmt;
use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use shep_core::secrets::{self, ProviderCache, Resolution, SecretRef, SecretView};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::bus::{Bus, publish_dog_config_changed};
use crate::dogs::{StoreFault, dogs_referencing};

/// Gap between two looks at the store.
///
/// Polled rather than watched with `notify`, so it behaves the same on
/// Windows and costs one `stat` per look while nothing moves.
pub(crate) const STORE_POLL: Duration = Duration::from_secs(5);

/// What the last look resolved each referenced key to: `None` for a key the
/// host environment cannot resolve.
///
/// Redacted (IR-41): the values are secrets.
#[derive(Default)]
struct Resolved(BTreeMap<String, Option<String>>);

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("keys", &self.0.len())
            .finish()
    }
}

/// What tells a rewrite of a file from the file as last seen.
///
/// Modification time and length alone miss a same-length rewrite inside one
/// clock granule, and every store write is a rename of a new file over the
/// old. On unix the inode and the change time are stamped too, which that
/// rename always moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: SystemTime,
    len: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed: (i64, i64),
}

/// `None` for a file that is absent or cannot be statted.
type Stamp = Option<FileStamp>;

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;
    Some(FileStamp {
        modified: meta.modified().ok()?,
        len: meta.len(),
        #[cfg(unix)]
        inode: meta.ino(),
        #[cfg(unix)]
        changed: (meta.ctime(), meta.ctime_nsec()),
    })
}

/// The poll state: how each file looked at the last look, and what the
/// referenced keys resolved to.
///
/// `dogs.toml` is stamped as well as the store: a dog that starts naming a
/// key has to enter the baseline before that key's next rotation, or the
/// rotation would read as a first sighting.
pub(crate) struct Watcher {
    secrets: PathBuf,
    dogs_config: PathBuf,
    environment: String,
    seen: Option<(Stamp, Stamp)>,
    /// Whether the last read failed, so a store that stays broken is warned
    /// about once.
    broken: bool,
    /// `None` until a read succeeds, so the first one fires nothing.
    baseline: Option<Resolved>,
}

/// Redacted (IR-41): `baseline` holds secret values.
impl fmt::Debug for Watcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watcher")
            .field("environment", &self.environment)
            .field("keys", &self.baseline.as_ref().map(|known| known.0.len()))
            .finish()
    }
}

impl Watcher {
    pub(crate) fn new(secrets: PathBuf, dogs_config: PathBuf, environment: String) -> Self {
        Self {
            secrets,
            dogs_config,
            environment,
            seen: None,
            broken: false,
            baseline: None,
        }
    }

    /// One look: the dogs that name a key whose resolved value in the host
    /// environment differs from the last good read, sorted.
    ///
    /// A store that will not read logs one `warn!` and changes nothing, so
    /// the next good read is compared against the baseline from before it
    /// broke. A key a dog names for the first time has no earlier value and
    /// announces nothing.
    pub(crate) fn tick(&mut self) -> Vec<String> {
        let now = (stamp(&self.secrets), stamp(&self.dogs_config));
        if self.seen.as_ref() == Some(&now) {
            return Vec::new();
        }
        let store = match secrets::all(&self.secrets) {
            Ok(store) => store,
            Err(err) => {
                // The class of failure, not the error's text: a decode error
                // can quote the value that failed to parse (IR-41).
                if !self.broken {
                    tracing::warn!(
                        path = %self.secrets.display(),
                        fault = %StoreFault::from(&err),
                        "the secret store could not be read; dogs are not told of changes until it reads again"
                    );
                }
                // `seen` stays as it was, so the next look reads again.
                self.broken = true;
                return Vec::new();
            }
        };
        self.broken = false;
        self.seen = Some(now);
        let named = dogs_referencing(&self.dogs_config, None);
        let view = SecretView::new(self.environment.clone(), store, ProviderCache::default());
        let resolved: BTreeMap<String, Option<String>> = named
            .values()
            .flatten()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|key| {
                let value = match view.resolve(&SecretRef {
                    namespace: None,
                    key,
                }) {
                    Resolution::Found(value) => Some(value.to_owned()),
                    _ => None,
                };
                (key.clone(), value)
            })
            .collect();

        let moved: BTreeSet<&String> =
            self.baseline.as_ref().map_or_else(BTreeSet::new, |before| {
                resolved
                    .iter()
                    .filter(|(key, value)| before.0.get(*key).is_some_and(|prev| prev != *value))
                    .map(|(key, _)| key)
                    .collect()
            });
        let affected = named
            .iter()
            .filter(|(_, keys)| keys.iter().any(|key| moved.contains(key)))
            .map(|(dog, _)| dog.clone())
            .collect();
        self.baseline = Some(Resolved(resolved));
        affected
    }
}

/// Runs [`Watcher::tick`] every [`STORE_POLL`] and publishes
/// `config.dog.<name>` for each dog it names, until `shutdown` flips.
///
/// The first look is immediate, which is what sets the baseline at boot.
pub(crate) fn spawn_secrets_watch(
    mut watcher: Watcher,
    events: Bus,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(STORE_POLL);
        // A look missed under load is not owed: the next one sees the file
        // as it is.
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticks.tick() => {
                    publish_dog_config_changed(&events, &watcher.tick());
                }
                // Flipped, or its sender gone: either way nothing is left to
                // tell.
                _ = shutdown.wait_for(|down| *down) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testing::capture_logs;
    use shep_core::secrets;

    const HOST: &str = "production";

    /// A `$SHEP_HOME` with a store and a `dogs.toml`, and a [`Watcher`] over
    /// them.
    ///
    /// Every rewrite in these tests changes the store's length: a tick tells
    /// a rewrite by mtime and length, and a filesystem with coarse mtimes
    /// would hide two same-length writes made microseconds apart.
    struct Fixture {
        dir: tempfile::TempDir,
        watcher: Watcher,
    }

    impl Fixture {
        fn new(dogs_toml: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("dogs.toml"), dogs_toml).unwrap();
            let watcher = Watcher::new(
                dir.path().join("secrets.json"),
                dir.path().join("dogs.toml"),
                HOST.to_string(),
            );
            Self { dir, watcher }
        }

        fn store(&self) -> PathBuf {
            self.dir.path().join("secrets.json")
        }

        fn set(&self, key: &str, environment: &str, value: &str) {
            secrets::set(&self.store(), key, environment, value).unwrap();
        }

        fn unset(&self, key: &str, environment: &str) {
            assert!(secrets::unset(&self.store(), key, environment).unwrap());
        }

        fn tick(&mut self) -> Vec<String> {
            self.watcher.tick()
        }
    }

    const DISCORD: &str = "[discord]\ntoken = \"{{secret:DISCORD_TOKEN}}\"\n";

    fn names(dogs: &[&str]) -> Vec<String> {
        dogs.iter().map(|dog| (*dog).to_string()).collect()
    }

    #[test]
    fn the_first_tick_sets_the_baseline_and_fires_nothing() {
        let mut fixture = Fixture::new(DISCORD);
        fixture.set("DISCORD_TOKEN", HOST, "first");

        assert!(fixture.tick().is_empty(), "a daemon start wakes no dog");
        assert!(fixture.tick().is_empty());
    }

    #[test]
    fn rotating_a_referenced_key_names_the_dogs_that_name_it() {
        let mut fixture = Fixture::new(&format!(
            "{DISCORD}[metrics.auth]\nkey = \"{{{{secret:DISCORD_TOKEN}}}}\"\n[bark]\nurl = \"{{{{secret:HOOK}}}}\"\n"
        ));
        fixture.set("DISCORD_TOKEN", HOST, "first");
        fixture.set("HOOK", HOST, "https://hook");
        fixture.tick();

        fixture.set("DISCORD_TOKEN", HOST, "the-rotated-token");

        assert_eq!(fixture.tick(), names(&["discord", "metrics"]));
        assert!(fixture.tick().is_empty(), "announced once");
    }

    #[test]
    fn rotating_a_key_no_dog_names_fires_nothing() {
        let mut fixture = Fixture::new(DISCORD);
        fixture.set("DISCORD_TOKEN", HOST, "first");
        fixture.set("UNRELATED", HOST, "a");
        fixture.tick();

        fixture.set("UNRELATED", HOST, "a-much-longer-value");

        assert!(fixture.tick().is_empty());
    }

    #[test]
    fn rewriting_the_same_value_fires_nothing() {
        let mut fixture = Fixture::new(DISCORD);
        fixture.set("DISCORD_TOKEN", HOST, "first");
        fixture.tick();

        // A different key moves the file, so the store really is re-read.
        fixture.set("DISCORD_TOKEN", HOST, "first");
        fixture.set("UNRELATED", HOST, "x");

        assert!(fixture.tick().is_empty());
    }

    #[test]
    fn setting_a_missing_key_and_unsetting_a_present_one_both_fire() {
        let mut fixture = Fixture::new(DISCORD);
        fixture.set("UNRELATED", HOST, "x");
        fixture.tick();

        fixture.set("DISCORD_TOKEN", HOST, "now-there");
        assert_eq!(fixture.tick(), names(&["discord"]), "missing to found");

        fixture.unset("DISCORD_TOKEN", HOST);
        assert_eq!(fixture.tick(), names(&["discord"]), "found to missing");
    }

    #[test]
    fn a_change_to_another_environment_fires_nothing_and_an_all_slot_change_fires_only_when_no_exact_slot_shadows_it()
     {
        let mut fixture =
            Fixture::new(&format!("{DISCORD}[bark]\nurl = \"{{{{secret:HOOK}}}}\"\n"));
        fixture.set("DISCORD_TOKEN", HOST, "exact");
        fixture.tick();

        fixture.set("DISCORD_TOKEN", "staging", "a-staging-token");
        assert!(
            fixture.tick().is_empty(),
            "another environment is invisible"
        );

        fixture.set("DISCORD_TOKEN", "all", "an-all-token");
        assert!(fixture.tick().is_empty(), "the exact slot shadows `all`");

        fixture.set("HOOK", "all", "https://fallback");
        assert_eq!(
            fixture.tick(),
            names(&["bark"]),
            "no exact slot, so `all` is what resolves"
        );
    }

    #[test]
    fn a_corrupt_store_fires_nothing_and_a_later_good_read_still_announces_the_change_made_meanwhile()
     {
        let mut fixture = Fixture::new(DISCORD);
        fixture.set("DISCORD_TOKEN", HOST, "first");
        fixture.tick();

        std::fs::write(fixture.store(), "{ not json").unwrap();
        assert!(fixture.tick().is_empty());

        std::fs::remove_file(fixture.store()).unwrap();
        fixture.set("DISCORD_TOKEN", HOST, "rotated-while-it-was-broken");
        assert_eq!(fixture.tick(), names(&["discord"]));
    }

    #[test]
    fn the_warning_names_the_path_and_never_a_value() {
        let mut fixture = Fixture::new(DISCORD);
        fixture.set("DISCORD_TOKEN", HOST, "first");
        fixture.tick();
        // Truncated mid-value, so a decode error that quoted its input would
        // quote the sentinel.
        std::fs::write(
            fixture.store(),
            "{\"version\":1,\"entries\":{\"DISCORD_TOKEN\":{\"production\":\"s3ntinel-hunter2\"",
        )
        .unwrap();

        let logged = capture_logs(|| {
            assert!(fixture.tick().is_empty());
            assert!(
                fixture.tick().is_empty(),
                "a store that stays broken warns once"
            );
        });

        assert!(
            logged.contains(&fixture.store().display().to_string()),
            "{logged}"
        );
        assert_eq!(logged.matches("WARN").count(), 1, "{logged}");
        assert!(!logged.contains("s3ntinel"), "{logged}");
    }

    /// A rotation to a value of the same length, landing in the same
    /// modification-time granule as the file the last look read. The mtime
    /// is put back by hand to stand in for a coarse clock.
    #[cfg(unix)]
    #[test]
    fn a_same_length_rotation_in_the_same_clock_granule_still_fires() {
        let mut fixture = Fixture::new(DISCORD);
        fixture.set("DISCORD_TOKEN", HOST, "aaaaaaaa");
        fixture.tick();
        let granule = std::fs::metadata(fixture.store())
            .unwrap()
            .modified()
            .unwrap();

        fixture.set("DISCORD_TOKEN", HOST, "bbbbbbbb");
        std::fs::File::options()
            .write(true)
            .open(fixture.store())
            .unwrap()
            .set_modified(granule)
            .unwrap();

        assert_eq!(fixture.tick(), names(&["discord"]));
    }

    /// A read that fails leaves the file looking unchanged, so the next look
    /// has to read it again: the failure may have been a passing one.
    #[test]
    fn a_store_that_failed_to_read_is_read_again_on_the_next_look() {
        let mut fixture = Fixture::new(DISCORD);
        fixture.set("DISCORD_TOKEN", HOST, "first");
        fixture.tick();
        std::fs::write(fixture.store(), "{ not json").unwrap();
        assert!(fixture.tick().is_empty());

        assert!(
            fixture
                .watcher
                .seen
                .is_none_or(|(secrets, _)| secrets != stamp(&fixture.store())),
            "the broken file's stamp must not be remembered as read"
        );
    }

    #[test]
    fn the_watcher_debug_prints_a_count_and_no_value() {
        let mut fixture = Fixture::new(DISCORD);
        fixture.set("DISCORD_TOKEN", HOST, "s3ntinel-hunter2");
        assert_eq!(
            format!("{:?}", fixture.watcher),
            "Watcher { environment: \"production\", keys: None }"
        );

        fixture.tick();

        let rendered = format!("{:?}", fixture.watcher);
        assert_eq!(
            rendered,
            "Watcher { environment: \"production\", keys: Some(1) }"
        );
        assert_eq!(
            format!("{:?}", fixture.watcher.baseline.as_ref().unwrap()),
            "Resolved { keys: 1 }"
        );
    }

    /// Under a paused clock: the first look sets the baseline, a rewrite
    /// between looks is announced on the next one, and the task ends when
    /// the shutdown watch flips.
    #[tokio::test(start_paused = true)]
    async fn the_task_announces_a_rotation_on_the_bus_and_ends_on_shutdown() {
        let fixture = Fixture::new(DISCORD);
        fixture.set("DISCORD_TOKEN", HOST, "first");
        let events = crate::bus::new_bus();
        let mut subscriber = events.subscribe();
        let (shutdown, down) = watch::channel(false);
        let Fixture { dir, watcher } = fixture;
        let task = spawn_secrets_watch(watcher, events, down);

        // The immediate first look, which is the baseline.
        tokio::task::yield_now().await;
        assert!(subscriber.try_recv().is_err(), "the baseline fires nothing");

        secrets::set(
            &dir.path().join("secrets.json"),
            "DISCORD_TOKEN",
            HOST,
            "the-rotated-token",
        )
        .unwrap();
        tokio::time::advance(STORE_POLL + Duration::from_millis(1)).await;

        let event = subscriber.recv().await.unwrap();
        assert_eq!(event.topic(), "config.dog.discord");

        shutdown.send(true).unwrap();
        task.await.unwrap();
    }
}
