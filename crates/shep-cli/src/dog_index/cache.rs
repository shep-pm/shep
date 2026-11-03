//! The index through a one-day cache at `$SHEP_HOME/cache/dogs.json`.
//!
//! [`cached_index`] is the one way to read the index. A caller that wants
//! the network says so with `refresh`; nothing else fetches.

use std::future::Future;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

use ring::digest;
use serde_json::{Value, json};
use shep_core::paths::ShepPaths;

use super::{Index, IndexError, fetch_index_document, parse_index};

/// How long a cached copy is read without a fetch.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// Where [`cached_index`]'s answer came from.
#[derive(Debug)]
pub enum Origin {
    /// Fetched just now, and written to the cache.
    Fetched,
    /// A cached copy younger than [`MAX_AGE`]; nothing was fetched.
    Cache,
    /// The fetch failed, so this is the cached copy, however old.
    Stale {
        /// How long ago the copy was cached.
        age: Duration,
        /// Why the fetch failed.
        cause: IndexError,
    },
}

/// An index and where it came from.
#[derive(Debug)]
pub struct LoadedIndex {
    /// The parsed index.
    pub index: Index,
    /// Whether it was fetched, cached or fell back to a stale copy.
    pub origin: Origin,
}

/// Reads the index at `url` through the cache under `paths`.
///
/// A cached copy for this same `url` younger than [`MAX_AGE`] is read with
/// no fetch, unless `refresh` is set. Otherwise the index is fetched and
/// cached. A failed fetch falls back to the cached copy of any age.
///
/// # Errors
/// The fetch's own [`IndexError`] when it fails and there is no usable
/// cached copy for `url`.
pub async fn cached_index(
    paths: &ShepPaths,
    url: &str,
    refresh: bool,
) -> Result<LoadedIndex, IndexError> {
    load(
        &paths.dog_index_cache,
        url,
        refresh,
        SystemTime::now(),
        || fetch_index_document(url),
    )
    .await
}

async fn load<F, Fut>(
    cache: &Path,
    url: &str,
    refresh: bool,
    now: SystemTime,
    fetch: F,
) -> Result<LoadedIndex, IndexError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<u8>, IndexError>>,
{
    let cached = read_cache(cache, url, now);
    if !refresh
        && let Some((index, age)) = &cached
        && *age < MAX_AGE
    {
        return Ok(LoadedIndex {
            index: index.clone(),
            origin: Origin::Cache,
        });
    }

    let fetched = match fetch().await {
        Ok(bytes) => parse_index(&bytes).map(|index| (index, bytes)),
        Err(err) => Err(err),
    };
    match (fetched, cached) {
        (Ok((index, bytes)), _) => {
            // The cache is an optimisation: a read-only home still lists.
            let _ = write_cache(cache, url, &bytes);
            Ok(LoadedIndex {
                index,
                origin: Origin::Fetched,
            })
        }
        (Err(cause), Some((index, age))) => Ok(LoadedIndex {
            index,
            origin: Origin::Stale { age, cause },
        }),
        (Err(err), None) => Err(err),
    }
}

/// The cached index for `url` and its age, or `None` when the file is
/// absent, unreadable, written for another url, or no longer parses.
///
/// A modification time ahead of `now` counts as [`MAX_AGE`], so a skewed
/// clock fetches instead of trusting the copy forever.
fn read_cache(cache: &Path, url: &str, now: SystemTime) -> Option<(Index, Duration)> {
    let modified = std::fs::metadata(cache).ok()?.modified().ok()?;
    let envelope: Value = serde_json::from_slice(&std::fs::read(cache).ok()?).ok()?;
    if envelope.get("source")?.as_str()? != url_key(url) {
        return None;
    }
    let document = serde_json::to_vec(envelope.get("document")?).ok()?;
    let index = parse_index(&document).ok()?;
    let age = now.duration_since(modified).unwrap_or(MAX_AGE);
    Some((index, age))
}

/// Writes `bytes`, a document that already parsed, as the cache for `url`.
///
/// Staged in a sibling file and renamed, so a reader never sees half a
/// file.
fn write_cache(cache: &Path, url: &str, bytes: &[u8]) -> std::io::Result<()> {
    let document: Value = serde_json::from_slice(bytes)?;
    let envelope = json!({ "source": url_key(url), "document": document });
    let dir = cache.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    staged.write_all(&serde_json::to_vec(&envelope)?)?;
    staged.persist(cache).map_err(|err| err.error)?;
    Ok(())
}

/// The sha256 of `url` in hex.
///
/// What the cache file records instead of the url, which may carry
/// credentials in a query string.
fn url_key(url: &str) -> String {
    digest::digest(&digest::SHA256, url.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `age` as a phrase for a sentence: `3 hours`, `2 days`. Hours up to two
/// days, so a day-old copy reads `24 hours`.
pub fn describe_age(age: Duration) -> String {
    let secs = age.as_secs();
    let (count, unit) = if secs < HOUR {
        (secs / MINUTE, "minute")
    } else if secs < 2 * DAY {
        (secs / HOUR, "hour")
    } else {
        (secs / DAY, "day")
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tempfile::TempDir;

    use super::*;
    use crate::fetch::FetchError;

    const URL: &str = "https://example.test/dogs.json";

    fn document(name: &str) -> Vec<u8> {
        json!({
            "version": 1,
            "dogs": [{
                "name": name,
                "package": "shep-example",
                "adopt_as": "example",
                "description": "Does an example thing.",
                "repo": "https://github.com/someone/shep-example",
                "license": "MIT",
                "category": "other",
                "source": { "kind": "cargo", "version": "1.0.0" }
            }]
        })
        .to_string()
        .into_bytes()
    }

    fn names(loaded: &LoadedIndex) -> Vec<&str> {
        loaded.index.dogs.iter().map(|d| d.name.as_str()).collect()
    }

    fn hours(n: u64) -> Duration {
        Duration::from_secs(n * HOUR)
    }

    /// A fetch that answers with `body` and counts its calls.
    fn answering<'a>(
        body: Vec<u8>,
        calls: &'a Cell<u32>,
    ) -> impl FnOnce() -> core::future::Ready<Result<Vec<u8>, IndexError>> + 'a {
        move || {
            calls.set(calls.get() + 1);
            core::future::ready(Ok(body))
        }
    }

    /// A fetch that fails and counts its calls.
    fn failing(
        calls: &Cell<u32>,
    ) -> impl FnOnce() -> core::future::Ready<Result<Vec<u8>, IndexError>> + '_ {
        move || {
            calls.set(calls.get() + 1);
            core::future::ready(Err(IndexError::Fetch(FetchError::Timeout)))
        }
    }

    /// Caches `body` for `URL` in a fresh home, as of `cached_at`.
    async fn home_cached_at(body: Vec<u8>, cached_at: SystemTime) -> (TempDir, std::path::PathBuf) {
        let home = TempDir::new().unwrap();
        let cache = home.path().join("cache").join("dogs.json");
        let calls = Cell::new(0);
        load(&cache, URL, false, cached_at, answering(body, &calls))
            .await
            .expect("seeds the cache");
        std::fs::File::options()
            .write(true)
            .open(&cache)
            .unwrap()
            .set_modified(cached_at)
            .unwrap();
        (home, cache)
    }

    #[tokio::test]
    async fn a_cache_younger_than_a_day_is_read_without_a_fetch() {
        let now = SystemTime::now();
        let (_home, cache) = home_cached_at(document("Cached"), now - hours(23)).await;
        let calls = Cell::new(0);

        let loaded = load(
            &cache,
            URL,
            false,
            now,
            answering(document("Fetched"), &calls),
        )
        .await
        .expect("loads");

        assert_eq!(calls.get(), 0, "a young cache must not fetch");
        assert_eq!(names(&loaded), ["Cached"]);
        assert!(
            matches!(loaded.origin, Origin::Cache),
            "{:?}",
            loaded.origin
        );
    }

    #[tokio::test]
    async fn a_cache_older_than_a_day_is_fetched_and_replaced() {
        let now = SystemTime::now();
        let (_home, cache) = home_cached_at(document("Cached"), now - hours(25)).await;
        let calls = Cell::new(0);

        let loaded = load(
            &cache,
            URL,
            false,
            now,
            answering(document("Fetched"), &calls),
        )
        .await
        .expect("loads");

        assert_eq!(calls.get(), 1);
        assert_eq!(names(&loaded), ["Fetched"]);
        assert!(
            matches!(loaded.origin, Origin::Fetched),
            "{:?}",
            loaded.origin
        );
        let again = load(&cache, URL, false, now, failing(&calls))
            .await
            .expect("loads");
        assert_eq!(names(&again), ["Fetched"], "the fetch replaced the cache");
    }

    #[tokio::test]
    async fn refresh_fetches_whatever_the_age_of_the_cache() {
        let now = SystemTime::now();
        let (_home, cache) = home_cached_at(document("Cached"), now - hours(1)).await;
        let calls = Cell::new(0);

        let loaded = load(
            &cache,
            URL,
            true,
            now,
            answering(document("Fetched"), &calls),
        )
        .await
        .expect("loads");

        assert_eq!(calls.get(), 1);
        assert_eq!(names(&loaded), ["Fetched"]);
    }

    #[tokio::test]
    async fn a_failed_fetch_falls_back_to_the_cache_and_reports_its_age() {
        let now = SystemTime::now();
        let (_home, cache) = home_cached_at(document("Cached"), now - hours(30)).await;
        let calls = Cell::new(0);

        let loaded = load(&cache, URL, false, now, failing(&calls))
            .await
            .expect("falls back");

        assert_eq!(names(&loaded), ["Cached"]);
        let Origin::Stale { age, .. } = loaded.origin else {
            panic!("wrong origin: {:?}", loaded.origin);
        };
        assert_eq!(age, hours(30));
    }

    #[tokio::test]
    async fn a_failed_fetch_with_no_cache_refuses_with_the_fetch_error() {
        let home = TempDir::new().unwrap();
        let cache = home.path().join("cache").join("dogs.json");
        let calls = Cell::new(0);

        let err = load(&cache, URL, false, SystemTime::now(), failing(&calls))
            .await
            .expect_err("nothing to fall back to");

        assert!(
            matches!(err, IndexError::Fetch(FetchError::Timeout)),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_fetched_document_that_does_not_parse_is_not_cached() {
        let home = TempDir::new().unwrap();
        let cache = home.path().join("cache").join("dogs.json");
        let calls = Cell::new(0);

        let err = load(
            &cache,
            URL,
            false,
            SystemTime::now(),
            answering(b"[]".to_vec(), &calls),
        )
        .await
        .expect_err("a bare array is refused");

        assert!(matches!(err, IndexError::NotAnObject), "{err:?}");
        assert!(!cache.exists(), "a bad document must not become the cache");
    }

    #[tokio::test]
    async fn a_cache_written_for_another_url_is_not_read() {
        let now = SystemTime::now();
        let (_home, cache) = home_cached_at(document("Cached"), now - hours(1)).await;
        let calls = Cell::new(0);

        let loaded = load(
            &cache,
            "https://other.test/dogs.json",
            false,
            now,
            answering(document("Fetched"), &calls),
        )
        .await
        .expect("loads");

        assert_eq!(calls.get(), 1);
        assert_eq!(names(&loaded), ["Fetched"]);
    }

    #[tokio::test]
    async fn a_cache_that_is_not_json_is_fetched_over() {
        let home = TempDir::new().unwrap();
        let cache = home.path().join("cache").join("dogs.json");
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, b"not json").unwrap();
        let calls = Cell::new(0);

        let loaded = load(
            &cache,
            URL,
            false,
            SystemTime::now(),
            answering(document("Fetched"), &calls),
        )
        .await
        .expect("loads");

        assert_eq!(names(&loaded), ["Fetched"]);
    }

    #[tokio::test]
    async fn the_cache_file_never_records_the_url() {
        let url = "https://example.test/dogs.json?token=sentinel-secret";
        let home = TempDir::new().unwrap();
        let cache = home.path().join("cache").join("dogs.json");
        let calls = Cell::new(0);

        load(
            &cache,
            url,
            false,
            SystemTime::now(),
            answering(document("X"), &calls),
        )
        .await
        .expect("loads");

        let written = std::fs::read_to_string(&cache).unwrap();
        assert!(!written.contains("sentinel-secret"), "{written}");
        assert!(!written.contains("example.test"), "{written}");
    }

    #[test]
    fn an_age_reads_in_the_largest_whole_unit() {
        for (secs, expected) in [
            (0, "0 minutes"),
            (60, "1 minute"),
            (59 * 60, "59 minutes"),
            (HOUR, "1 hour"),
            (30 * HOUR - 1, "29 hours"),
            (DAY, "24 hours"),
            (2 * DAY, "2 days"),
            (3 * DAY + HOUR, "3 days"),
        ] {
            assert_eq!(describe_age(Duration::from_secs(secs)), expected);
        }
    }
}
