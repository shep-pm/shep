//! `[dog.metrics]`: [`MetricsConfig`], its defaults, and the `Debug` that
//! keeps its headers out of a log.

use core::fmt;
use std::collections::BTreeMap;
use std::net::SocketAddr;

use serde::Deserialize;
use shep_client::dogs::dog_config;
use shep_core::values::UpDuration;

/// `[dog.metrics]`.
///
/// `deny_unknown_fields`: a misspelled key must be a startup error naming
/// it, not a dog silently serving on a port the operator did not choose.
///
/// `dog_config` marks `otlp_headers`, the one key here that carries a
/// credential.
#[dog_config]
#[derive(Clone, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsConfig {
    /// Where to listen. Loopback by default; binding wider is explicit.
    ///
    /// A metrics endpoint carries every sheep's name, and on many hosts a
    /// sheep's name is the name of an internal service. `0.0.0.0:9615` is
    /// available to an operator who wants it and is never the default —
    /// this dog will not widen its own exposure as a side effect of being
    /// enabled.
    pub bind: SocketAddr,
    /// Base URL of an OTLP/HTTP collector, `http://` or `https://`. Absent
    /// means no export at all.
    ///
    /// The collector's `/v1/metrics` path is appended here, so
    /// `"http://127.0.0.1:4318"` and `"http://127.0.0.1:4318/otlp"` both
    /// reach a working URL. Export needs the `otel` cargo feature.
    pub otlp_endpoint: Option<String>,
    /// How long between exports. The first lands one interval after the dog
    /// starts.
    pub otlp_interval: UpDuration,
    /// How long one export may take, connect through response.
    pub otlp_timeout: UpDuration,
    /// Extra headers to send on every export, `[dog.metrics.otlp_headers]`:
    /// the shape an authenticated collector needs,
    /// `Authorization = "Bearer ..."`.
    ///
    /// Marked secret: an OTLP collector's credential is a bearer token in a
    /// header value, and a pane that printed it prints the token.
    #[shep(secret)]
    pub otlp_headers: BTreeMap<String, String>,
}

/// Hand-written: `#[serde(default)]` on the struct needs a `Default`, and a
/// derived one would bind to `0.0.0.0:0` and export on a zero-length
/// interval and timeout.
impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 9615)),
            otlp_endpoint: None,
            // 60s: a collector scraping shep's own exposition defaults to
            // 60s too, so the two paths report the same series at the same
            // cadence unless an operator says otherwise.
            otlp_interval: UpDuration::from_millis(60_000),
            // 10s: bark's `sink_timeout`, the same budget for the same kind
            // of outbound POST. Well past how fast a collector on the same
            // host answers, well short of the interval above.
            otlp_timeout: UpDuration::from_millis(10_000),
            otlp_headers: BTreeMap::new(),
        }
    }
}

/// Manual: a derived `Debug` would print `otlp_headers`, and an OTLP
/// collector's auth header is a bearer token (IR-41).
///
/// `otlp_endpoint` is printed in full. Startup validates it through
/// [`crate::fetch::parse_url`], which refuses a `user@` or `user:pass@`
/// authority, so no endpoint this dog runs with carries a credential in its
/// URL.
impl fmt::Debug for MetricsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MetricsConfig {{ bind: {:?}, otlp_endpoint: {:?}, otlp_interval: {:?}, otlp_timeout: {:?}, otlp_headers: <redacted> }}",
            self.bind, self.otlp_endpoint, self.otlp_interval, self.otlp_timeout,
        )
    }
}

#[cfg(test)]
impl MetricsConfig {
    /// A config bound to `port` on loopback, every OTLP key at its default.
    /// [`Default`] pins `9615`; a test that binds a real socket passes `0`
    /// so the OS assigns one.
    pub(super) fn default_on_port(port: u16) -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], port)),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `deny_unknown_fields` becomes `additionalProperties: false`;
    /// `default` drops `required` and puts the real default beside the key.
    #[test]
    fn the_metrics_schema_offers_the_port_it_would_bind_and_refuses_unknown_keys() {
        let schema = shep_client::dogs::config_schema::<MetricsConfig>();
        let schema = schema.as_value();

        assert_eq!(
            schema.get("additionalProperties"),
            Some(&serde_json::Value::Bool(false)),
            "`deny_unknown_fields` is what catches a misspelled key"
        );
        assert_eq!(
            schema.get("required"),
            None,
            "`default` on the type makes every key optional"
        );
        assert_eq!(
            schema.pointer("/properties/bind/default"),
            Some(&serde_json::Value::String("127.0.0.1:9615".to_owned())),
            "the default is loopback, and a pane shows the port rather than a blank"
        );
        // Every OTLP key is in the schema whatever build this is: the
        // struct and its schema carry them always, so a `dogs.toml` written
        // for an `otel` build parses in a plain one too, and a pane shows
        // the keys rather than refusing the section.
        assert_eq!(
            schema.pointer("/properties/otlp_endpoint/default"),
            Some(&serde_json::Value::Null),
            "no export is the default, and a pane shows an empty key rather than a guess"
        );
        // [`UpDuration`]'s `Display` picks the largest unit that divides the
        // value exactly, so 60_000ms reaches a pane as `1m`.
        assert_eq!(
            schema.pointer("/properties/otlp_interval/default"),
            Some(&serde_json::Value::String("1m".to_owned())),
            "the interval a collector scraping /metrics already assumes"
        );
        assert_eq!(
            schema.pointer("/properties/otlp_timeout/default"),
            Some(&serde_json::Value::String("10s".to_owned())),
            "the same per-delivery budget bark gives a sink"
        );
        assert_eq!(
            schema.pointer("/properties/otlp_headers/default"),
            Some(&serde_json::Value::Object(serde_json::Map::new())),
            "an unconfigured exporter sends no extra header"
        );
        assert_eq!(
            schema.pointer("/properties/otlp_headers/x-shep-secret"),
            Some(&serde_json::Value::Bool(true)),
            "an OTLP collector's credential rides in a header value"
        );
    }

    #[test]
    fn the_default_bind_is_loopback() {
        assert_eq!(
            MetricsConfig::default().bind,
            "127.0.0.1:9615".parse::<SocketAddr>().unwrap()
        );
        // An empty section resolves to that same default, not to
        // `0.0.0.0`, which a `Default` derived on `SocketAddr` would give.
        let parsed: MetricsConfig = toml::from_str("").unwrap();
        assert_eq!(parsed, MetricsConfig::default());
    }

    /// Fails if a `derive(Debug)` replaces the hand-written impl: the value
    /// of `Authorization` would then reach a log line, a panic message or a
    /// `shep dogs` rendering. The exact string is the assertion, with the
    /// header's value named in it as the thing that must be absent.
    #[test]
    fn metrics_config_debug_redacts_the_headers() {
        let config = MetricsConfig {
            otlp_endpoint: Some("https://otlp.internal:4318".to_owned()),
            otlp_headers: BTreeMap::from([(
                "Authorization".to_owned(),
                "Bearer s3cr3t-token".to_owned(),
            )]),
            ..MetricsConfig::default()
        };
        let rendered = format!("{config:?}");
        assert_eq!(
            rendered,
            r#"MetricsConfig { bind: 127.0.0.1:9615, otlp_endpoint: Some("https://otlp.internal:4318"), otlp_interval: UpDuration(60s), otlp_timeout: UpDuration(10s), otlp_headers: <redacted> }"#
        );
        assert!(!rendered.contains("s3cr3t-token"), "{rendered}");
        assert!(!rendered.contains("Authorization"), "{rendered}");
    }

    /// The keys `docs/dogs.md` and `web/src/pages/docs/dogs.astro` publish,
    /// copy-pasted here relative to `[dog.metrics]` the way
    /// `runtime.config::<MetricsConfig>()` sees it.
    #[test]
    fn the_documented_metrics_config_parses_from_toml() {
        let toml_str = r#"
bind = "127.0.0.1:9615"
otlp_endpoint = "http://127.0.0.1:4318"
otlp_interval = "30s"
otlp_timeout = "5s"

[otlp_headers]
Authorization = "Bearer token-from-a-secret-manager"
"#;
        let config: MetricsConfig =
            toml::from_str(toml_str).expect("the documented [dog.metrics] example must parse");
        assert_eq!(
            config.otlp_endpoint.as_deref(),
            Some("http://127.0.0.1:4318")
        );
        assert_eq!(config.otlp_interval.as_millis(), 30_000);
        assert_eq!(config.otlp_timeout.as_millis(), 5_000);
        assert_eq!(config.otlp_headers.len(), 1);
    }
}
