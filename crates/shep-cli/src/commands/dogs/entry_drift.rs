//! Which running adopted dogs hold an entry `shep.toml` no longer has.
//!
//! An adopted dog's `path` and `env` reach it only when `shep enable` spawns
//! it. A reload restarts no dog and a restart respawns from the spec the dog
//! already holds, so an edited entry waits for a `disable` then an `enable`.
//!
//! The comparison is the shepherd's own [`Request::ConfigDrift`]: the config
//! [`dog_app`] builds from `shep.toml` against the one the dog was spawned
//! from. It answers field names, so no value crosses the wire.

use std::collections::BTreeSet;

use shep_client::Client;
use shep_core::config::{DaemonConfig, ResolvedApp};
use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, ProcessInfo, Request, Response};
use shep_core::status::ProcStatus;
use shep_daemon::dogs::dog_app;

use crate::commands::daemon::dog_spec;

/// [`emit_notice`](crate::output::emit_notice) code for the sentence
/// [`kept_entry_report`] writes when a `restart` names the dog. A reload
/// writes it under its own `reload` code.
pub(crate) const DOG_ENTRY_KEPT_NOTICE: &str = "dog_entry_kept";

/// One running adopted dog and what in its entry changed since it spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptEntry {
    /// The dog's name: its `[daemon.adopted_dogs]` key.
    pub(crate) name: String,
    /// The binary path in the entry differs.
    pub(crate) path: bool,
    /// The `env` in the entry differs.
    pub(crate) env: bool,
}

/// Every running adopted dog whose entry in `shep.toml` differs from the one
/// it was spawned with.
///
/// Silent on every failure, an unreadable `shep.toml` or a shepherd that does
/// not answer included: this is a courtesy beside a verb that has its own
/// way to fail. Nothing is asked of the shepherd until `shep.toml` names an
/// adopted dog worth asking about.
pub(crate) async fn kept_entries(client: &Client, paths: &ShepPaths) -> Vec<KeptEntry> {
    if current_entries(paths).is_empty() {
        return Vec::new();
    }
    let Ok(flock) = client.list_flock().await else {
        return Vec::new();
    };
    kept_entries_among(client, paths, &flock).await
}

/// [`kept_entries`] over rows the caller already holds, such as the ones a
/// `restart` was answered with, so no flock is listed.
pub(crate) async fn kept_entries_among(
    client: &Client,
    paths: &ShepPaths,
    rows: &[ProcessInfo],
) -> Vec<KeptEntry> {
    // Running only: a dog at rest spawns from the entry `shep enable` reads,
    // so the entry it holds is not one anyone is waiting on.
    let running: BTreeSet<&str> = rows
        .iter()
        .filter(|info| matches!(info.dog, Some(DogSource::Adopted { .. })))
        .filter(|info| !matches!(info.status, ProcStatus::Stopped | ProcStatus::Errored))
        .map(|info| info.name.as_str())
        .collect();
    let apps: Vec<_> = current_entries(paths)
        .into_iter()
        .filter(|app| running.contains(app.config().name.as_str()))
        .map(ResolvedApp::into_config)
        .collect();
    if apps.is_empty() {
        return Vec::new();
    }
    let Ok(Response::Drifted(drifted)) = client.request(Request::ConfigDrift { apps }).await else {
        return Vec::new();
    };
    drifted
        .into_iter()
        .map(|drift| KeptEntry {
            path: drift.fields.iter().any(|field| field == "script"),
            env: drift.fields.iter().any(|field| field == "env"),
            name: drift.name,
        })
        // The other fields a dog's config holds are not the entry's: `channel`
        // follows `channel_dogs`, and the rest are fixed by `dog_app`.
        .filter(|kept| kept.path || kept.env)
        .collect()
}

/// The config each adopted dog in `shep.toml` would spawn with today.
fn current_entries(paths: &ShepPaths) -> Vec<ResolvedApp> {
    let Ok(source) = std::fs::read_to_string(&paths.daemon_config) else {
        return Vec::new();
    };
    let Ok(config) = DaemonConfig::load(Some(&source), &|_| None) else {
        return Vec::new();
    };
    config
        .daemon
        .adopted_dogs
        .keys()
        .filter_map(|name| dog_app(&dog_spec(&config, name), paths).ok())
        .collect()
}

/// The sentence for one dog that keeps the entry it was spawned with.
///
/// Names the entry's fields and never their keys or values (IR-41).
pub(crate) fn kept_entry_report(kept: &KeptEntry) -> String {
    let name = &kept.name;
    let fields = match (kept.path, kept.env) {
        (true, true) => "`path` and `env`",
        (true, false) => "`path`",
        _ => "`env`",
    };
    format!(
        "the `{name}` dog is running with the {fields} it was spawned with, and `shep.toml` now \
         holds a different entry; it keeps the old one until `shep disable {name}` then \
         `shep enable {name}`"
    )
}

/// A shepherd that spawned `kelpie` from a known entry, and the home whose
/// `shep.toml` holds another, for the tests of every verb that reports it.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use shep_core::config::AppConfig;
    use shep_core::protocol::SheepDrift;

    /// What a shepherd that spawned `kelpie` from `spawned` answers: the flock
    /// with that dog running, and `ConfigDrift` as `Actor::config_drift`
    /// computes it, by comparing the config sent to the one it holds.
    pub(crate) fn shepherd_running(
        spawned: AppConfig,
        status: ProcStatus,
    ) -> impl Fn(&Request) -> Response {
        move |req| match req {
            Request::ListFlock => Response::Flock(vec![
                ProcessInfo::builder(1, "kelpie", status)
                    .dog(Some(DogSource::Adopted {
                        path: spawned.script.clone(),
                        channel: false,
                        grants: None,
                    }))
                    .build(),
            ]),
            Request::ConfigDrift { apps } => Response::Drifted(
                apps.iter()
                    .filter(|app| app.name == spawned.name)
                    .filter_map(|app| {
                        let fields = spawned.drifted_fields(app);
                        (!fields.is_empty()).then(|| SheepDrift::new(&app.name, fields))
                    })
                    .collect(),
            ),
            _ => Response::Flock(Vec::new()),
        }
    }

    /// A home whose `shep.toml` adopts `kelpie` with `env`, and the config a
    /// dog spawned from `spawned_env` holds.
    pub(crate) fn home_and_spawned(
        dir: &std::path::Path,
        entry_env: &str,
        spawned_env: &[(&str, &str)],
    ) -> (ShepPaths, AppConfig) {
        let paths = ShepPaths::resolve(&|_| None, dir);
        std::fs::create_dir_all(paths.daemon_config.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.daemon_config,
            format!(
                "[daemon]\nenabled_dogs = [\"kelpie\"]\n\n[daemon.adopted_dogs.kelpie]\n\
                 path = \"/usr/local/bin/shep-kelpie\"\nenv = {{ {entry_env} }}\n"
            ),
        )
        .unwrap();
        let spawned = dog_app(
            &shep_daemon::dogs::DogSpec {
                name: "kelpie".to_string(),
                source: DogSource::Adopted {
                    path: "/usr/local/bin/shep-kelpie".to_string(),
                    channel: false,
                    grants: None,
                },
                env: spawned_env
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            },
            &paths,
        )
        .unwrap()
        .into_config();
        (paths, spawned)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{home_and_spawned, shepherd_running};
    use super::*;
    use shep_core::config::AppConfig;

    async fn ask(
        paths: &ShepPaths,
        dir: &std::path::Path,
        spawned: AppConfig,
        status: ProcStatus,
    ) -> Vec<KeptEntry> {
        let addr = shep_client::testing::control_address(dir);
        let (client, _envelopes) =
            shep_client::testing::fake_client_answering(&addr, shepherd_running(spawned, status))
                .await;
        kept_entries(&client, paths).await
    }

    #[tokio::test]
    async fn an_env_that_gained_a_key_since_the_spawn_is_kept_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, spawned) =
            home_and_spawned(dir.path(), "PADDOCK_KEY = \"{{secret:paddock-key}}\"", &[]);

        let kept = ask(&paths, dir.path(), spawned, ProcStatus::Online).await;

        assert_eq!(
            kept,
            vec![KeptEntry {
                name: "kelpie".to_string(),
                path: false,
                env: true,
            }]
        );
    }

    #[tokio::test]
    async fn an_entry_unchanged_since_the_spawn_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, spawned) = home_and_spawned(
            dir.path(),
            "PADDOCK_KEY = \"{{secret:paddock-key}}\"",
            &[("PADDOCK_KEY", "{{secret:paddock-key}}")],
        );

        let kept = ask(&paths, dir.path(), spawned, ProcStatus::Online).await;

        assert_eq!(kept, Vec::new());
    }

    #[tokio::test]
    async fn a_dog_at_rest_holds_no_entry_anyone_is_waiting_on() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, spawned) = home_and_spawned(dir.path(), "K = \"v\"", &[]);

        let kept = ask(&paths, dir.path(), spawned, ProcStatus::Stopped).await;

        assert_eq!(kept, Vec::new());
    }

    #[tokio::test]
    async fn rows_in_hand_are_judged_without_listing_the_flock() {
        let dir = tempfile::tempdir().unwrap();
        let (paths, spawned) = home_and_spawned(dir.path(), "K = \"v\"", &[]);
        let addr = shep_client::testing::control_address(dir.path());
        let rows = [ProcessInfo::builder(1, "kelpie", ProcStatus::Online)
            .dog(Some(DogSource::Adopted {
                path: spawned.script.clone(),
                channel: false,
                grants: None,
            }))
            .build()];
        let (client, mut envelopes) = shep_client::testing::fake_client_answering(
            &addr,
            shepherd_running(spawned, ProcStatus::Online),
        )
        .await;

        let kept = kept_entries_among(&client, &paths, &rows).await;

        assert_eq!(kept.len(), 1);
        assert!(matches!(
            envelopes.recv().await.unwrap().body,
            Request::ConfigDrift { .. }
        ));
        assert!(envelopes.try_recv().is_err(), "no flock was listed");
    }

    #[tokio::test]
    async fn a_home_with_no_adopted_dog_asks_the_shepherd_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let addr = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) =
            shep_client::testing::fake_client_answering(&addr, |_| Response::Pong).await;

        let kept = kept_entries(&client, &paths).await;

        assert_eq!(kept, Vec::new());
        assert!(
            envelopes.try_recv().is_err(),
            "nothing to compare, nothing to ask"
        );
    }

    /// Pinned as an exact string in the three shapes, and with a value that
    /// must not appear.
    #[test]
    fn the_report_names_the_dog_and_the_remedy_and_never_a_value() {
        let kept = |path, env| KeptEntry {
            name: "kelpie".to_string(),
            path,
            env,
        };
        assert_eq!(
            kept_entry_report(&kept(false, true)),
            "the `kelpie` dog is running with the `env` it was spawned with, and `shep.toml` \
             now holds a different entry; it keeps the old one until `shep disable kelpie` \
             then `shep enable kelpie`"
        );
        assert!(kept_entry_report(&kept(true, false)).contains("with the `path` it was"));
        assert!(kept_entry_report(&kept(true, true)).contains("with the `path` and `env` it was"));
    }
}
