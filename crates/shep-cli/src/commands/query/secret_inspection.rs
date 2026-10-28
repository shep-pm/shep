use crate::cli::SelectorArgs;
use crate::commands::query::read_roll;
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::secret::host_environment;
use crate::commands::selector::parse_selector_spec;
use crate::exit::ExitCode;
use crate::output::{DescribedSecret, SecretStatus, Streams, emit_described, write_outcome};
use crate::remote::Link;
use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{ProcessInfo, Request, Response, SelectorSpec};
#[cfg(test)]
use shep_core::secrets::Resolution;
use shep_core::secrets::{self, SecretRef, SecretView};
use std::collections::BTreeMap;

/// `describe` and `flock_display::fold`'s shared body: one `Request::Describe`
/// against `selector`, rendered as the sheep table and each lamb tree.
///
/// `secrets` alone gates [`gather_secrets`]: `flock_display::fold` passes `None`.
///
/// Not routed through [`request_and_render`](crate::commands::rpc::request_and_render):
/// it renders one `Vec<ProcessInfo>` into two tables.
pub(super) async fn describe_selector(
    client: &Client,
    streams: &mut Streams<'_>,
    command: &str,
    secrets: Option<(&ShepPaths, &str)>,
    selector: SelectorSpec,
) -> ExitCode {
    match client.request(Request::Describe { selector }).await {
        Ok(Response::Described(procs)) => {
            let secrets = if let Some((paths, host_environment)) = secrets {
                let (rows, unreadable) = gather_secrets(paths, host_environment, &procs);
                if let Some(error) = unreadable {
                    let message = format!(
                        "the secret store at {} could not be read ({error}), so every bare \
                         reference below reads as missing whether or not the store holds a value \
                         for it; namespaced references still resolve from the provider cache",
                        paths.secrets.display()
                    );
                    streams.aside(SECRET_STORE_UNREADABLE_NOTICE, &message);
                }
                rows
            } else {
                Vec::new()
            };
            let result = emit_described(
                &mut *streams.out,
                streams.fmt,
                command,
                procs,
                streams.style,
                &secrets,
            );
            write_outcome(result)
        }
        Ok(_unrecognised) => unexpected_response(streams),
        Err(err) => client_error(streams, &err),
    }
}

/// Renders one sheep's secret references as `describe`'s table form would:
/// one line per reference, its environment, and a verdict. Never a value.
///
/// The verdict word comes from
/// [`SecretStatus::from_resolution`](crate::output::SecretStatus::from_resolution), the same classifier [`gather_secrets`] uses.
///
/// Test-only: it exercises those three verdicts without a fake daemon.
#[cfg(test)]
pub(super) fn render_describe_secrets(entries: &[(&str, &str, Resolution<'_>)]) -> String {
    let mut rendered = String::new();
    for (reference, environment, resolution) in entries {
        let verdict = SecretStatus::from_resolution(resolution).as_table_word();
        rendered.push_str(&format!("  {reference} ({environment}): {verdict}\n"));
    }
    rendered
}

/// `describe`'s secrets section for `procs`, once per distinct sheep name:
/// the JSON-safe rows [`emit_described`] both serializes beside `data` and
/// renders as the table's own "Secrets for `<name>`:" block.
///
/// Reads three local files rather than asking the shepherd: the muster roll,
/// the operator's own secret store, and the provider cache. A store that
/// would not read is reported unreadable, not folded into a `missing` verdict.
pub(super) fn gather_secrets(
    paths: &ShepPaths,
    host_environment: &str,
    procs: &[ProcessInfo],
) -> (Vec<DescribedSecret>, Option<secrets::SecretError>) {
    let (store, unreadable) = match secrets::all(&paths.secrets) {
        Ok(store) => (store, None),
        Err(error) => (BTreeMap::new(), Some(error)),
    };
    let providers = secrets::provider_cache_on_disk(&paths.secrets_cache);

    let namers = crate::secret_readers::namers(host_environment, read_roll(paths).as_ref(), procs);
    let mut views: BTreeMap<&str, SecretView> = BTreeMap::new();
    let mut json = Vec::new();
    for namer in &namers {
        let view = views.entry(&namer.environment).or_insert_with(|| {
            SecretView::new(namer.environment.clone(), store.clone(), providers.clone())
        });
        for reference in &namer.references {
            let Some(parsed) = SecretRef::parse(reference) else {
                continue;
            };
            json.push(DescribedSecret {
                name: namer.name.clone(),
                reference: reference.clone(),
                environment: namer.environment.clone(),
                status: SecretStatus::from_resolution(&view.resolve(&parsed)),
            });
        }
    }
    (json, unreadable)
}

/// [`emit_notice`](crate::output::emit_notice) code for the warning
/// `describe` prints when the operator's secret store will not read. Not a
/// failure: the sheep still describes.
const SECRET_STORE_UNREADABLE_NOTICE: &str = "secret_store_unreadable";

/// [`emit_notice`](crate::output::emit_notice) code for the line `describe`
/// prints over `--host`, where it draws no secrets section.
const SECRETS_NOT_SHOWN_NOTICE: &str = "secrets_not_shown";

/// Describes the sheep matching `args.selector` in detail.
///
/// Over a [`Link::Host`] it draws no secrets section. [`host_environment`]
/// and [`gather_secrets`] read this machine's `shep.toml`, secret store,
/// provider cache and muster roll, and resolving those against another
/// machine's sheep would report what the wrong machine holds. One aside says
/// so, once per run.
pub async fn describe(
    client: &Client,
    streams: &mut Streams<'_>,
    link: &Link<'_>,
    args: &SelectorArgs,
) -> ExitCode {
    // One pass per target, each its own detail view: `describe` answers with
    // a tree per sheep, so merging them would lose that shape.
    let mut failure: Option<ExitCode> = None;
    let mut host: Option<String> = None;
    // Said before any selector is parsed: a malformed one ends the run, and
    // the operator still learns nothing was resolved against a local store.
    if matches!(link, Link::Host { .. }) {
        streams.aside(
            SECRETS_NOT_SHOWN_NOTICE,
            "secrets are not shown over --host",
        );
    }
    for raw in &args.selectors {
        let selector = match parse_selector_spec(streams, raw) {
            Ok(selector) => selector,
            Err(code) => return code,
        };
        let secrets = match link {
            Link::Local(paths) => {
                let host = host.get_or_insert_with(|| host_environment(streams, paths));
                Some((*paths, host.as_str()))
            }
            Link::Host { .. } => None,
        };
        let code = describe_selector(client, streams, "describe", secrets, selector).await;
        if code != ExitCode::Success {
            failure = failure.or(Some(code));
        }
    }
    failure.unwrap_or(ExitCode::Success)
}

#[cfg(test)]
mod tests {

    use shep_core::paths::ShepPaths;
    use shep_core::protocol::{ProcessInfo, Request, SelectorSpec};
    #[cfg(test)]
    use shep_core::secrets::Resolution;

    use crate::cli::{Format, SelectorArgs};
    use shep_core::status::ProcStatus;
    use shep_daemon::snapshot::FlockSnapshot;

    use crate::exit::ExitCode;

    use crate::output::Streams;

    use shep_client::testing::{
        fake_client_capturing_envelopes, fake_client_with_ack, sample_ack, sample_info,
    };

    use super::super::testing::*;
    use super::*;

    #[tokio::test]
    async fn describe_sends_the_parsed_selector_in_its_compiled_form() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        for (input, expected) in [
            ("all", SelectorSpec::All),
            ("7", SelectorSpec::Id(7)),
            ("web", SelectorSpec::Name("web".into())),
            ("/^web-/", SelectorSpec::Regex("^web-".into())),
            ("fold:api", SelectorSpec::Fold("api".into())),
        ] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            let args = SelectorArgs {
                selectors: vec![input.into()],
            };
            let _ = describe(&client, &mut streams, &Link::Local(&paths), &args).await;
            let sent = tokio::time::timeout(RECV_TIMEOUT, envelopes.recv())
                    .await
                    .unwrap_or_else(|_| {
                        panic!("describe({input}) must reach the wire; it hung instead of sending a request")
                    })
                    .unwrap();
            assert_eq!(
                sent.body,
                Request::Describe { selector: expected },
                "{input}"
            );
        }
    }

    /// `"/[/"` is one of the only three inputs the selector grammar rejects:
    /// an unterminated regex character class.
    #[tokio::test]
    async fn a_malformed_selector_exits_usage_without_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            describe(
                &client,
                &mut streams,
                &Link::Local(&paths),
                &SelectorArgs {
                    selectors: vec!["/[/".into()],
                },
            )
            .await
        };
        assert_eq!(code, ExitCode::Usage);
        assert!(
            envelopes.try_recv().is_err(),
            "a malformed selector must fail locally"
        );
    }

    #[tokio::test]
    async fn describe_response_round_trips_into_rendered_flock_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_with_ack(&path, sample_ack()).await;
        daemon.reply_to_describe(vec![sample_info()]);
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Json);
            describe(
                &client,
                &mut streams,
                &Link::Local(&paths),
                &SelectorArgs {
                    selectors: vec!["all".into()],
                },
            )
            .await
        };

        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(json["command"], "describe");
        assert_eq!(json["data"][0]["name"], "web");
    }

    #[test]
    fn describe_lists_secret_references_with_a_verdict_and_no_values() {
        let rendered = render_describe_secrets(&[
            ("DB_PASSWORD", "production", Resolution::Found("hunter2")),
            ("vercel/API_KEY", "production", Resolution::MissingNamespace),
            ("ABSENT", "production", Resolution::MissingKey),
        ]);
        assert!(rendered.contains("DB_PASSWORD"));
        assert!(rendered.contains("vercel/API_KEY"));
        assert!(!rendered.contains("hunter2"), "never a value");
    }

    #[tokio::test]
    async fn describe_prints_real_secret_verdicts_in_the_table() {
        let (code, _paths, out) = describe_with_a_seeded_web(Format::Table).await;
        assert_eq!(code, ExitCode::Success);
        let rendered = String::from_utf8(out).unwrap();
        // `emit_described` places this block once, right after Overridden.
        // A second renderer writing the same block again is the exact bug
        // this pins: `.contains()` alone is true whether it printed once
        // or twice.
        assert_eq!(rendered.matches("Secrets for web").count(), 1, "{rendered}");
        assert!(
            rendered.contains("DB_PASSWORD (production): resolved"),
            "{rendered}"
        );
        assert!(
            rendered
                .contains("vercel/API_KEY (production): not cached; a provider may still have it"),
            "{rendered}"
        );
        assert!(!rendered.contains("hunter2"), "never a value: {rendered}");
    }

    #[tokio::test]
    async fn describe_json_carries_the_same_verdicts_as_an_additive_field() {
        let (code, _paths, out) = describe_with_a_seeded_web(Format::Json).await;
        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(json["command"], "describe");
        assert_eq!(json["schema_version"], 1, "SCHEMA_VERSION must not move");
        // `data` stays exactly what it always was: an array of ProcessInfo.
        assert_eq!(json["data"][0]["name"], "web");
        let entries = json["secrets"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|e| e["reference"] == "DB_PASSWORD" && e["status"] == "resolved"),
            "{entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e["reference"] == "vercel/API_KEY" && e["status"] == "uncached"),
            "{entries:?}"
        );
        assert!(!out_contains(&json, "hunter2"), "never a value");
    }

    /// The CLI reads the same pairs the shepherd resolves against, so a
    /// namespace pushed for `production` reads as uncached for a `staging`
    /// sheep rather than as a key the provider does not have. Reporting
    /// `missing` here would tell an operator to go and find a value that a
    /// dog mid-poll is about to supply.
    #[tokio::test]
    async fn describe_reads_a_namespace_pushed_for_another_environment_as_uncached() {
        let mut staging = app_with_env("web", &[("B", "{{secret:vercel/API_KEY}}")]);
        staging.environment = Some("staging".into());
        let Described { code, out, .. } = describe_against_a_local_roll(
            vec![staging],
            |paths| {
                // The pair the dog has pushed is production; staging is still to come.
                std::fs::write(
                    &paths.secrets_cache,
                    r#"{"version":2,"namespaces":{"vercel":{"API_KEY":{"production":"sk_live"}}},"pushed":{"vercel":["production"]}}"#,
                )
                .unwrap();
            },
            Format::Json,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let entries = json["secrets"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["environment"], "staging");
        assert_eq!(entries[0]["status"], "uncached", "{entries:?}");
        assert!(!out_contains(&json, "sk_live"), "never a value");
    }

    /// `gather_secrets` builds one view per environment and shares it
    /// between the apps naming that environment. Two apps naming two
    /// environments must still each read their own: `K` has a value for
    /// `staging` alone, so one view across both would resolve `floating`
    /// against staging's value and call a production key it has no slot
    /// for `resolved`.
    #[tokio::test]
    async fn two_apps_in_two_environments_each_resolve_against_their_own() {
        let mut pinned = app_with_env("pinned", &[("A", "{{secret:K}}")]);
        pinned.environment = Some("staging".into());
        let floating = app_with_env("floating", &[("A", "{{secret:K}}")]);
        let Described { code, out, .. } = describe_against_a_local_roll(
            vec![pinned, floating],
            |paths| {
                // Staging and no `all` slot, so the daemon's own `production` has
                // nothing to fall back to.
                secrets::set(&paths.secrets, "K", "staging", "sk_staging").unwrap();
            },
            Format::Json,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let entries = json["secrets"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "{entries:?}");
        let row = |name: &str| {
            entries
                .iter()
                .find(|entry| entry["name"] == name)
                .unwrap_or_else(|| panic!("{name} has a row: {entries:?}"))
        };
        assert_eq!(row("pinned")["environment"], "staging");
        assert_eq!(row("pinned")["status"], "resolved", "{entries:?}");
        assert_eq!(row("floating")["environment"], "production");
        assert_eq!(row("floating")["status"], "missing", "{entries:?}");
        assert!(!out_contains(&json, "sk_staging"), "never a value");
    }

    /// A store that will not parse is not a store with nothing in it. The
    /// verdicts still say `missing`, since nothing local can say otherwise,
    /// but the operator is told why before being sent to `shep secret set`
    /// for a value the store may already hold.
    #[tokio::test]
    async fn describe_says_the_store_would_not_read_rather_than_calling_keys_missing() {
        let Described {
            code,
            paths,
            out,
            err,
        } = describe_against_a_local_roll(
            vec![app_with_env("web", &[("A", "{{secret:DB_PASSWORD}}")])],
            |paths| std::fs::write(&paths.secrets, b"{not json").unwrap(),
            Format::Table,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        let warning = String::from_utf8(err).unwrap();
        assert!(
            warning.contains("could not be read"),
            "the notice has to say the store failed: {warning}"
        );
        assert!(
            warning.contains(&paths.secrets.display().to_string()),
            "and name the file: {warning}"
        );
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("DB_PASSWORD"), "{rendered}");
    }

    /// The roll, the secret store and the provider cache are this machine's.
    /// Resolving them against a host's sheep would call a remote reference
    /// missing or resolved by what a different machine holds.
    #[tokio::test]
    async fn describe_over_a_host_reads_no_local_secret_store_and_draws_no_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_with_ack(&path, sample_ack()).await;
        daemon.reply_to_describe(vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
        ]);
        let home = dir.path().display().to_string();
        let paths = ShepPaths::resolve(
            &move |key| (key == "SHEP_HOME").then(|| home.clone()),
            dir.path(),
        );
        let mut config = shep_core::config::AppConfig::minimal("web", "./srv");
        config
            .env
            .insert("A".into(), "{{secret:DB_PASSWORD}}".into());
        let roll = FlockSnapshot::with_apps(vec![shep_daemon::snapshot::SavedApp {
            app: config,
            instances_running: 1,
            disabled: false,
        }]);
        std::fs::write(&paths.snapshot, serde_json::to_vec(&roll).unwrap()).unwrap();
        std::fs::write(&paths.secrets, b"{not json").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Json);
            describe(
                &client,
                &mut streams,
                &Link::to_host("amp2"),
                &SelectorArgs {
                    selectors: vec!["all".into()],
                },
            )
            .await
        };

        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(json["data"][0]["name"], "web");
        // The field is omitted when empty, which reads as an empty array.
        assert_eq!(json["secrets"].as_array().map_or(0, Vec::len), 0, "{json}");
        let notices = String::from_utf8(err).unwrap();
        assert!(
            !notices.contains(SECRET_STORE_UNREADABLE_NOTICE),
            "the local store was read: {notices}"
        );
        assert!(
            notices.contains("secrets are not shown over --host"),
            "{notices}"
        );
        assert_eq!(notices.matches("not shown over --host").count(), 1);
    }

    /// The aside is the operator's only sign that nothing was resolved, so a
    /// selector that fails to parse must not skip it.
    #[tokio::test]
    async fn describe_over_a_host_says_secrets_are_not_shown_even_for_a_bad_selector() {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, _daemon) = fake_client_with_ack(&path, sample_ack()).await;

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
            describe(
                &client,
                &mut streams,
                &Link::to_host("amp2"),
                &SelectorArgs {
                    selectors: vec!["/(/".into()],
                },
            )
            .await
        };

        assert_ne!(code, ExitCode::Success);
        let notices = String::from_utf8(err).unwrap();
        assert_eq!(
            notices.matches("not shown over --host").count(),
            1,
            "{notices}"
        );
    }

    /// The whole notice, byte for byte. The old wording said "every
    /// reference below reads as missing", which is true only of a bare one:
    /// a namespaced reference resolves against the provider cache, a
    /// separate file `gather_secrets` reads regardless. A store newer than
    /// this build is the unreadable case whose failure text is this repo's
    /// own, so the notice can be pinned as one literal rather than matched
    /// around a `serde_json` message that a dependency bump may reword.
    #[tokio::test]
    async fn the_unreadable_store_notice_says_bare_references_read_as_missing_and_namespaced_ones_still_resolve()
     {
        let Described {
            code, paths, err, ..
        } = describe_against_a_local_roll(
            vec![app_with_env("web", &[("A", "{{secret:DB_PASSWORD}}")])],
            |paths| std::fs::write(&paths.secrets, r#"{"version":99,"entries":{}}"#).unwrap(),
            Format::Table,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            format!(
                "notice[{SECRET_STORE_UNREADABLE_NOTICE}]: the secret store at {} could not be \
                 read (secret store is version 99, newer than this build understands), so every \
                 bare reference below reads as missing whether or not the store holds a value for \
                 it; namespaced references still resolve from the provider cache\n",
                paths.secrets.display()
            ),
            "the notice must name the file, say bare references read as missing, and say \
             namespaced ones still resolve"
        );
    }

    /// The case the old wording denied: `vercel/KEY` resolves against the
    /// provider cache, a separate file `gather_secrets` reads whether or not
    /// the store opened. So one run can print the notice and still call a
    /// namespaced reference `resolved`.
    #[tokio::test]
    async fn a_namespaced_reference_still_resolves_while_the_store_is_unreadable() {
        let Described { code, out, err, .. } = describe_against_a_local_roll(
            vec![app_with_env(
                "web",
                &[("A", "{{secret:DB_PASSWORD}}"), ("B", "{{secret:vercel/KEY}}")],
            )],
            |paths| {
                std::fs::write(&paths.secrets, b"{not json").unwrap();
                std::fs::write(
                    &paths.secrets_cache,
                    r#"{"version":2,"namespaces":{"vercel":{"KEY":{"production":"sk_live"}}},"pushed":{"vercel":["production"]}}"#,
                )
                .unwrap();
            },
            Format::Table,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        let warning = String::from_utf8(err).unwrap();
        assert!(
            warning.contains("could not be read"),
            "the store still failed: {warning}"
        );
        let rendered = String::from_utf8(out).unwrap();
        assert!(
            rendered.contains("vercel/KEY (production): resolved"),
            "the provider cache is read regardless of the store: {rendered}"
        );
        assert!(
            rendered.contains("DB_PASSWORD (production): missing"),
            "the bare reference is the one that reads as missing: {rendered}"
        );
        assert!(!rendered.contains("sk_live"), "never a value: {rendered}");
    }
}
