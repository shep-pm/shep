use crate::cli::SelectorArgs;
use crate::commands::query::read_roll;
use crate::commands::rpc::{client_error, unexpected_response};
use crate::commands::selector::parse_selector_spec;
use crate::exit::ExitCode;
use crate::output::{DescribedSecret, SecretStatus, Streams, emit_described, write_outcome};
use shep_client::Client;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{ProcessInfo, Request, Response, SelectorSpec};
#[cfg(test)]
use shep_core::secrets::Resolution;
use shep_core::secrets::{self, SecretRef, SecretView};
use std::collections::BTreeMap;

/// `describe` and `flock_display::fold`'s shared body: one `Request::Describe` against
/// `selector`, rendered through [`emit_described`] as the sheep table and
/// each sheep's lamb tree beneath it. `command` is the verb name the output
/// envelope reports.
///
/// `include_secrets` alone gates [`gather_secrets`]: `flock_display::fold` passes `false`
/// and stays byte-identical to before this section existed, because `flock_display::fold`
/// is a group view across a selector's sheep, not the single-sheep
/// diagnostic this feature was built for.
///
/// Not routed through [`request_and_render`](crate::commands::rpc::request_and_render): `emit_described` renders one
/// `Vec<ProcessInfo>` into two tables, which no single
/// [`crate::output::Render`] impl can express.
pub(super) async fn describe_selector(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    command: &str,
    include_secrets: bool,
    selector: SelectorSpec,
) -> ExitCode {
    match client.request(Request::Describe { selector }).await {
        Ok(Response::Described(procs)) => {
            let secrets = if include_secrets {
                let (rows, unreadable) = gather_secrets(paths, &procs);
                if let Some(error) = unreadable {
                    let message = format!(
                        "the secret store at {} could not be read ({error}), so every \
                         reference below reads as missing whether or not the store holds \
                         a value for it",
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

/// Renders one sheep's secret references the way `describe`'s table form
/// would: one line per reference, the reference as written, the environment
/// it resolved in, and a verdict. Never a value: `Resolution::Found`'s
/// payload is read only to tell it apart from a miss.
///
/// The verdict word comes from [`SecretStatus::from_resolution`](crate::output::SecretStatus::from_resolution), the same
/// classifier [`gather_secrets`] uses for the JSON form, so the two can
/// never name a different verdict for the same reference.
///
/// Test-only: `emit_described` renders the actual table now, so this exists
/// to exercise [`SecretStatus::from_resolution`](crate::output::SecretStatus::from_resolution)'s three verdicts in
/// isolation, without a fake daemon connection.
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
/// Reads three local files rather than asking the shepherd: the muster roll
/// for each name's [`shep_core::config::AppConfig`] (which can trail a
/// config change that has not yet reached disk by
/// `shep_daemon::snapshot`'s debounce window), the operator's own secret
/// store, and the provider cache [`secrets::provider_cache_on_disk`]
/// reads. A namespace whose provider pushed with `persist = false` never
/// reaches that cache, so this can call a namespace uncached when the
/// running shepherd already has it in memory; only the shepherd itself can
/// answer that half.
///
/// A name the roll does not know, or one with no `{{secret:...}}` at all,
/// contributes nothing: this reports references that exist, not a claim
/// that every sheep has one.
///
/// The second half of the answer is why the operator's own store is empty,
/// when it is empty because it would not read. Folding that into an empty
/// store on its own would print `missing` beside every bare reference and
/// send the operator to `shep secret set` for values the store may already
/// hold. Rows are still produced, so a sheep needing nothing still
/// describes; the caller has the [`Streams`] to say so on.
///
/// One [`SecretView`] per distinct environment across `procs`, not one per
/// name: a view owns its copy of both maps, and neither map varies by name,
/// so a flock sharing one environment copies them once. Not one view for
/// the whole flock either, because an app's Flockfile can pin an
/// `environment` of its own and each has to resolve against that one.
pub(super) fn gather_secrets(
    paths: &ShepPaths,
    procs: &[ProcessInfo],
) -> (Vec<DescribedSecret>, Option<secrets::SecretError>) {
    let (store, unreadable) = match secrets::all(&paths.secrets) {
        Ok(store) => (store, None),
        Err(error) => (BTreeMap::new(), Some(error)),
    };
    let providers = secrets::provider_cache_on_disk(&paths.secrets_cache);

    let namers = crate::secret_readers::namers(paths, read_roll(paths).as_ref(), procs);
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

/// Describes the sheep matching `args.selector` in detail.
pub async fn describe(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &SelectorArgs,
) -> ExitCode {
    // One pass per target, each its own detail view: `describe` answers with
    // a tree per sheep, so merging them would lose that shape.
    let mut failure: Option<ExitCode> = None;
    for raw in &args.selectors {
        let selector = match parse_selector_spec(streams, raw) {
            Ok(selector) => selector,
            Err(code) => return code,
        };
        let code = describe_selector(client, streams, paths, "describe", true, selector).await;
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
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let args = SelectorArgs {
                selectors: vec![input.into()],
            };
            let _ = describe(&client, &mut streams, &paths, &args).await;
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
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            describe(
                &client,
                &mut streams,
                &paths,
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
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Json,
            };
            describe(
                &client,
                &mut streams,
                &paths,
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
        config.environment = Some("staging".into());
        config
            .env
            .insert("B".into(), "{{secret:vercel/API_KEY}}".into());
        let roll = FlockSnapshot::with_apps(vec![shep_daemon::snapshot::SavedApp {
            app: config,
            instances_running: 1,
        }]);
        std::fs::write(&paths.snapshot, serde_json::to_vec(&roll).unwrap()).unwrap();
        // The pair the dog has pushed is production; staging is still to come.
        std::fs::write(
                &paths.secrets_cache,
                r#"{"version":2,"namespaces":{"vercel":{"API_KEY":{"production":"sk_live"}}},"pushed":{"vercel":["production"]}}"#,
            )
            .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Json,
            };
            describe(
                &client,
                &mut streams,
                &paths,
                &SelectorArgs {
                    selectors: vec!["all".into()],
                },
            )
            .await
        };

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
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, daemon) = fake_client_with_ack(&path, sample_ack()).await;
        daemon.reply_to_describe(vec![
            ProcessInfo::builder(1, "pinned", ProcStatus::Online).build(),
            ProcessInfo::builder(2, "floating", ProcStatus::Online).build(),
        ]);
        let home = dir.path().display().to_string();
        let paths = ShepPaths::resolve(
            &move |key| (key == "SHEP_HOME").then(|| home.clone()),
            dir.path(),
        );

        let mut pinned = shep_core::config::AppConfig::minimal("pinned", "./srv");
        pinned.environment = Some("staging".into());
        pinned.env.insert("A".into(), "{{secret:K}}".into());
        let mut floating = shep_core::config::AppConfig::minimal("floating", "./srv");
        floating.env.insert("A".into(), "{{secret:K}}".into());
        let roll = FlockSnapshot::with_apps(
            [pinned, floating]
                .into_iter()
                .map(|app| shep_daemon::snapshot::SavedApp {
                    app,
                    instances_running: 1,
                })
                .collect(),
        );
        std::fs::write(&paths.snapshot, serde_json::to_vec(&roll).unwrap()).unwrap();
        // Staging and no `all` slot, so the daemon's own `production` has
        // nothing to fall back to.
        secrets::set(&paths.secrets, "K", "staging", "sk_staging").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Json,
            };
            describe(
                &client,
                &mut streams,
                &paths,
                &SelectorArgs {
                    selectors: vec!["all".into()],
                },
            )
            .await
        };

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
        }]);
        std::fs::write(&paths.snapshot, serde_json::to_vec(&roll).unwrap()).unwrap();
        std::fs::write(&paths.secrets, b"{not json").unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = {
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            describe(
                &client,
                &mut streams,
                &paths,
                &SelectorArgs {
                    selectors: vec!["all".into()],
                },
            )
            .await
        };

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
}
