use super::rules::{Rule, Rules};
use super::sinks::Sink;
use serde::Deserialize;
use shep_client::dogs::dog_config;
use shep_core::barks;
use shep_core::values::UpDuration;
use std::collections::BTreeMap;

/// `[dog.bark]`.
///
/// `deny_unknown_fields`: a misspelled key must be a startup error naming
/// it, the same reasoning [`super::super::metrics::MetricsConfig`] gives for its
/// own section.
#[dog_config]
#[derive(Debug, Clone, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct BarkConfig {
    /// Named sinks, `[dog.bark.sinks]`.
    ///
    /// Marked whole AS WELL AS per URL, which is belt and braces rather
    /// than redundancy: [`Sink`]'s own marks redact each URL inside the
    /// sub-screen the map opens, and this one redacts the collapsed map a
    /// pane shows before anyone opens it. Over-redacting is the safe
    /// direction.
    #[shep(secret)]
    pub sinks: BTreeMap<String, Sink>,
    /// Named rules, `[[dog.bark.rules]]`. Empty means
    /// [`Rules::default_rules`].
    pub rules: Vec<Rule>,
    /// How often the reconciliation poll runs when nothing has gone wrong.
    pub poll: UpDuration,
    /// Cap on `barks.jsonl`.
    pub history_bytes: u64,
    /// Per-delivery timeout.
    pub sink_timeout: UpDuration,
}

/// The rule set a `[bark]` section means: its own rules, or
/// [`super::rules::Rules::default_rules`] when it configured none.
///
/// Asked at startup and again on every `config.dog.bark` frame, so a
/// reloading bark cannot get different defaults from a starting one.
///
/// # Errors
/// - [`super::rules::RulesError`] as [`super::rules::Rules::new`]: a rule routing to a
///   sink the section does not define, an unknown event kind, or a sink
///   url that cannot work (an insecure webhook scheme, or credentials
///   before the host).
pub fn rules_for(config: &BarkConfig) -> Result<Rules, super::rules::RulesError> {
    let rule_list = if config.rules.is_empty() {
        Rules::default_rules(&config.sinks)
    } else {
        config.rules.clone()
    };
    Rules::new(rule_list, &config.sinks)
}

/// Hand-written: `#[serde(default)]` on the struct needs a `Default`, and
/// a derived one would give `poll`, `history_bytes` and `sink_timeout`
/// their types' zero values.
impl Default for BarkConfig {
    fn default() -> Self {
        Self {
            sinks: BTreeMap::new(),
            rules: Vec::new(),
            // 30s: the fallback cadence for when nothing has gone wrong.
            // A drop already triggers an immediate poll, so this bounds
            // steady-state cost, not responsiveness.
            poll: UpDuration::from_millis(30_000),
            // The cap `shep-daemon`'s own writer uses: one shared number
            // for the one file both append to.
            history_bytes: barks::DEFAULT_MAX_BYTES,
            // 10s: well past how fast Discord and Slack answer, well short
            // of the poll cadence above, so one stuck sink cannot absorb a
            // whole interval's deliveries.
            sink_timeout: UpDuration::from_millis(10_000),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sinks map carries the credential marker, every sink's own URL
    /// carries it under `$defs`, and the rules beside them do not.
    ///
    /// Both marks, because they cover different views: `Sink`'s redacts each
    /// URL inside the sub-screen the map opens, and the map's redacts the
    /// collapsed map a pane shows before anyone opens it. The `$defs/Sink`
    /// half is the one that covers bark if the map mark is ever judged to be
    /// over-redaction. `rules::Rule` is checked under `$defs` too, the one
    /// place a marker could wrongly land on `Rule::sinks`.
    #[test]
    fn the_bark_schema_marks_every_sink_url_and_leaves_the_rules_plain() {
        let schema = shep_client::dogs::config_schema::<BarkConfig>();
        let schema = schema.as_value();

        assert_eq!(
            schema.pointer("/properties/sinks/x-shep-secret"),
            Some(&serde_json::Value::Bool(true)),
            "every sink holds a webhook URL, which is a bearer credential"
        );
        assert_eq!(
            schema.pointer("/properties/rules/x-shep-secret"),
            None,
            "a rule names sinks and holds no credential of its own"
        );
        assert!(
            schema.pointer("/$defs/Rule/properties/sinks").is_some(),
            "the pointer below is only a check while `Rule` still has this \
                 shape, so a rename that moved it must fail here first"
        );
        assert_eq!(
            schema.pointer("/$defs/Rule/properties/sinks/x-shep-secret"),
            None,
            "a rule's sinks are names, and a name is not a credential"
        );

        // Every variant, not the first: `Sink` is internally tagged, so its
        // schema is a `oneOf` and a mark missing from one arm is the defect
        // a single pointer would read straight past.
        let variants = schema
            .pointer("/$defs/Sink/oneOf")
            .and_then(|it| it.as_array())
            .expect("an internally tagged enum is a oneOf");
        assert_eq!(variants.len(), 3);
        for variant in variants {
            assert_eq!(
                variant.pointer("/properties/url/x-shep-secret"),
                Some(&serde_json::Value::Bool(true)),
                "the mark travels with the field, so bark's own schema \
                 carries it too"
            );
        }
    }

    /// Fails if an unconfigured `[dog.bark]` polls in a hot loop, keeps no
    /// history, or times every delivery out instantly: what
    /// `#[derive(Default)]` would give this struct.
    #[test]
    fn an_empty_section_gets_sane_defaults_not_zeros() {
        let parsed: BarkConfig = toml::from_str("").unwrap();
        assert_eq!(parsed, BarkConfig::default());
        assert_eq!(BarkConfig::default().poll.as_millis(), 30_000);
        assert_eq!(
            BarkConfig::default().history_bytes,
            barks::DEFAULT_MAX_BYTES
        );
        assert_eq!(BarkConfig::default().sink_timeout.as_millis(), 10_000);
    }

    /// Fails if `[dog.bark]` cannot parse the fragment `docs/dogs.md` and
    /// `web/src/pages/docs/dogs.astro` publish, copy-pasted here relative
    /// to `[dog.bark]` the way `runtime.config::<BarkConfig>()` sees it,
    /// so `[sinks]`/`[[rules]]` rather than the full paths.
    ///
    /// The only other `toml::from_str::<BarkConfig>` in this module parses
    /// an empty document, which never deserializes a [`super::super::rules::Rule`].
    #[test]
    fn the_documented_bark_config_parses_from_toml() {
        let toml_str = r#"
    [sinks]
    oncall = { kind = "discord", url = "https://discord.com/api/webhooks/..." }
    audit = { kind = "json", url = "https://example.internal/hook" }

    [[rules]]
    on = "gave_up"
    sinks = ["oncall", "audit"]

    [[rules]]
    on = "restart_rate"
    restarts = 5
    within = "2m"
    sinks = ["oncall"]
    "#;
        let config: BarkConfig =
            toml::from_str(toml_str).expect("the documented [dog.bark] example must parse");
        assert_eq!(config.sinks.len(), 2);
        assert_eq!(config.rules.len(), 2);
        assert_eq!(
            config.rules[0].when,
            super::super::rules::Trigger::GaveUp {}
        );
        assert_eq!(config.rules[0].sinks, vec!["oncall", "audit"]);
        assert_eq!(
            config.rules[1].when,
            super::super::rules::Trigger::RestartRate {
                restarts: 5,
                within: UpDuration::from_millis(2 * 60_000),
            }
        );
        assert_eq!(config.rules[1].sinks, vec!["oncall"]);
        // Parsing is necessary but not sufficient: the dog starts only if
        // `Rules::new` accepts both rules against the sinks beside them.
        Rules::new(config.rules, &config.sinks).expect("both documented rules route to real sinks");
    }
}
