use std::time::Duration;

use shep_core::dogs::manifest::Manifest;

use super::super::{AdoptRefusal, DogSchema, VettedBinary, spawn_probe, vet_binary_within};
use super::{ManifestRead, read_manifest};

#[test]
fn a_probe_retries_a_busy_executable_until_it_can_run() {
    let mut results = [
        Err(std::io::ErrorKind::ExecutableFileBusy.into()),
        Err(std::io::ErrorKind::ExecutableFileBusy.into()),
        Ok("spawned"),
    ]
    .into_iter();
    let mut delays = Vec::new();

    let result = spawn_probe(|| results.next().unwrap(), |delay| delays.push(delay));

    assert_eq!(result, Ok("spawned"));
    assert_eq!(
        delays,
        [Duration::from_millis(10), Duration::from_millis(20)]
    );
    assert!(results.next().is_none());
}

#[test]
fn a_probe_refuses_an_executable_still_busy_after_four_attempts() {
    let mut attempts = 0;
    let mut delays = Vec::new();

    let result = spawn_probe(
        || {
            attempts += 1;
            assert!(attempts <= 4, "the retry bound was exceeded");
            Err::<(), _>(std::io::Error::new(
                std::io::ErrorKind::ExecutableFileBusy,
                format!("busy on attempt {attempts}"),
            ))
        },
        |delay| delays.push(delay),
    );

    assert_eq!(
        result,
        Err(AdoptRefusal::WillNotExec {
            reason: "busy on attempt 4".to_string(),
        })
    );
    assert_eq!(attempts, 4);
    assert_eq!(
        delays,
        [
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(40),
        ]
    );
}

#[test]
fn a_probe_refuses_other_spawn_errors_without_retrying() {
    for kind in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::InvalidInput,
    ] {
        let mut attempts = 0;

        let result = spawn_probe(
            || {
                attempts += 1;
                Err::<(), _>(std::io::Error::new(kind, "spawn refused"))
            },
            |_| panic!("a {kind:?} spawn error must not sleep"),
        );

        assert_eq!(
            result,
            Err(AdoptRefusal::WillNotExec {
                reason: "spawn refused".to_string(),
            })
        );
        assert_eq!(attempts, 1);
    }
}

#[test]
fn a_probe_stops_retrying_when_a_busy_error_becomes_another_spawn_error() {
    let mut results = [
        Err(std::io::ErrorKind::ExecutableFileBusy.into()),
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "binary removed",
        )),
        Ok("must not run"),
    ]
    .into_iter();
    let mut delays = Vec::new();

    let result = spawn_probe(|| results.next().unwrap(), |delay| delays.push(delay));

    assert_eq!(
        result,
        Err(AdoptRefusal::WillNotExec {
            reason: "binary removed".to_string(),
        })
    );
    assert_eq!(delays, [Duration::from_millis(10)]);
    assert_eq!(results.next().unwrap().unwrap(), "must not run");
}

/// Generous: the question is the answer, not the clock.
const BUDGET: Duration = Duration::from_secs(10);

/// Shorter than the fixture dog sleeps, for the case where the budget is the
/// answer: a probe still running when it runs out.
const BUDGET_SHORT: Duration = Duration::from_millis(300);

fn write_dog(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let binary = dir.join("shep-otel");
    std::fs::write(&binary, format!("#!/bin/sh\n{body}\nexit 0\n")).unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();
    binary
}

#[test]
fn a_manifests_settings_are_the_schema_and_the_manifest_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"settings":{"type":"object"}}'
fi"#;
    let binary = write_dog(dir.path(), body);

    let vetted = vet_binary_within(&binary, dir.path(), "otel", BUDGET).unwrap();

    assert_eq!(
        vetted.schema,
        DogSchema::Published(serde_json::json!({"type": "object"}))
    );
    assert_eq!(
        vetted.manifest.map(|manifest| manifest.name),
        Some("otel".to_string())
    );
}

/// Exact string, not a "contains no secret" probe: the manifest's `defaults`
/// are TOML text for a section that may hold a credential.
#[test]
fn vetted_binary_debug_never_prints_the_manifest() {
    let mut manifest = Manifest::new("otel", "0.2.0", 12);
    manifest.defaults = Some("token = \"hunter2\"\n".to_string());
    let vetted = VettedBinary {
        path: "/opt/shep-otel".into(),
        group_writable: Vec::new(),
        answer: None,
        schema: DogSchema::Silent,
        manifest: Some(manifest),
    };

    assert_eq!(
        format!("{vetted:?}"),
        "VettedBinary { path: \"/opt/shep-otel\", group_writable: [], answer: None, \
         schema: Silent, manifest: Some(\"..\") }"
    );
}

const MANIFEST_BODY: &str = r#"if [ "$1" = --shep-manifest ]; then
  echo '{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"defaults":"token = 1"}'
fi"#;

fn cached_files(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(home.join("manifests"))
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default()
}

/// The `defaults` of a manifest can hold a credential, so the cache is
/// readable by its owner alone.
#[test]
fn the_cache_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let binary = write_dog(dir.path(), MANIFEST_BODY);

    read_manifest(&binary, dir.path(), "otel", BUDGET).unwrap();

    let files = cached_files(dir.path());
    assert_eq!(files.len(), 1, "{files:?}");
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&files[0]), 0o600);
    assert_eq!(mode(&dir.path().join("manifests")), 0o700);
}

/// A binary replaced while it answers may have answered as the new build, so
/// its answer is not filed under the digest of the old one.
#[test]
fn a_binary_that_changes_while_probed_is_not_cached() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "{MANIFEST_BODY}\nif [ \"$1\" = --shep-manifest ]; then\n  \
         printf '#!/bin/sh\\n# rebuilt\\n' > \"$0.new\" && mv \"$0.new\" \"$0\"\nfi"
    );
    let binary = write_dog(dir.path(), &body);

    let read = read_manifest(&binary, dir.path(), "otel", BUDGET).unwrap();

    assert!(
        matches!(read, ManifestRead::Read(_)),
        "the answer is still used"
    );
    assert_eq!(cached_files(dir.path()), Vec::<std::path::PathBuf>::new());
}

/// A dog that gave no manifest gave no declaration, in every way a dog gives
/// none: nothing printed, something that is not a manifest printed, a run that
/// failed, a run still going when the budget ran out. Each is the shape of a
/// dog built before `--shep-manifest` or one that ignores an unknown flag, and a
/// caller deciding whether to ask such a dog anything has no declaration to
/// weigh. A cache file is the other thing: a record of a manifest this exact
/// binary once printed.
#[test]
fn a_probe_that_gives_nothing_is_read_as_a_dog_with_no_manifest() {
    let dir = tempfile::tempdir().unwrap();

    let silent = write_dog(dir.path(), "true");
    assert!(matches!(
        read_manifest(&silent, dir.path(), "otel", BUDGET).unwrap(),
        ManifestRead::Absent
    ));

    let broken = write_dog(
        dir.path(),
        r#"if [ "$1" = --shep-manifest ]; then
  echo 'not a manifest'
fi"#,
    );
    assert!(
        matches!(
            read_manifest(&broken, dir.path(), "otel", BUDGET).unwrap(),
            ManifestRead::Absent
        ),
        "a dog that prints a usage banner for a flag it does not know"
    );

    let failed = dir.path().join("shep-otel-failed");
    std::fs::write(
        &failed,
        "#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then\n  echo 'unknown flag'\n  exit 1\nfi\n",
    )
    .unwrap();
    let mut mode = std::fs::metadata(&failed).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&failed, mode).unwrap();
    assert!(
        matches!(
            read_manifest(&failed, dir.path(), "otel", BUDGET).unwrap(),
            ManifestRead::Absent
        ),
        "a dog whose argument parser rejects the flag"
    );

    let slow = write_dog(
        dir.path(),
        r#"if [ "$1" = --shep-manifest ]; then
  sleep 5
fi"#,
    );
    assert!(
        matches!(
            read_manifest(&slow, dir.path(), "otel", BUDGET_SHORT).unwrap(),
            ManifestRead::Absent,
        ),
        "a probe that outlasted its budget said nothing about what it declares"
    );
}

/// A cache file filed under this binary's own digest is a manifest some earlier
/// read of it printed. Neither that file nor a fresh probe says what it said, so
/// this read did not find a dog with no manifest: it lost a record of one.
#[test]
fn a_cache_file_that_will_not_parse_and_a_binary_that_cannot_restate_it_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let binary = write_dog(dir.path(), "true");
    let digest = digest_of(&binary);
    let cache = dir.path().join("manifests");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join(format!("{digest}.json")), "{ truncated").unwrap();

    assert!(matches!(
        read_manifest(&binary, dir.path(), "otel", BUDGET).unwrap(),
        ManifestRead::Unreadable
    ));
}

/// The same record, and a binary that can still answer for itself: the answer
/// is used, and the record is written over, so one bad file is not permanent.
#[test]
fn a_cache_file_that_will_not_parse_is_healed_by_the_answer_it_replaces() {
    let dir = tempfile::tempdir().unwrap();
    let binary = write_dog(dir.path(), MANIFEST_BODY);
    let digest = digest_of(&binary);
    let cache = dir.path().join("manifests");
    std::fs::create_dir_all(&cache).unwrap();
    let file = cache.join(format!("{digest}.json"));
    std::fs::write(&file, "{ truncated").unwrap();

    let read = read_manifest(&binary, dir.path(), "otel", BUDGET).unwrap();

    assert!(
        matches!(read, ManifestRead::Read(manifest) if manifest.name == "otel"),
        "the probe answered, and the answer is the manifest"
    );
    assert_eq!(
        cached_files(dir.path()),
        vec![file],
        "the record is written back over the broken one"
    );
}

/// The sha256 the cache keys a binary on.
fn digest_of(path: &std::path::Path) -> String {
    use std::fmt::Write as _;
    use std::io::Read as _;

    let mut file = std::fs::File::open(path).unwrap();
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        context.update(&chunk[..read]);
    }
    context
        .finish()
        .as_ref()
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}
