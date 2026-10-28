//! The optional half of a metric sample, built rather than configured.

use std::collections::BTreeMap;

use crate::wire::MetricType;

/// The optional half of a metric sample: kind, labels and help text.
///
/// Built, not configured: every method takes and returns `Self`. Names and
/// labels carry no grammar check here. The same reason
/// [`ChildMessage::Metric`](crate::ChildMessage::Metric) carries none: the
/// metrics dog enforces it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct MetricOptions {
    pub(crate) kind: Option<MetricType>,
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) help: Option<String>,
}

// Hand-written: the derive would print label values and help text
// verbatim, the same reason `Shepherd`'s Debug hides metric names (IR-41).
impl core::fmt::Debug for MetricOptions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MetricOptions")
            .field("kind", &self.kind)
            .field("labels", &self.labels.len())
            .field("help", &self.help.is_some())
            .finish()
    }
}

impl MetricOptions {
    /// Options with no kind, no labels and no help text.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the metric kind. Absent on the wire means gauge.
    #[must_use]
    pub fn kind(mut self, kind: MetricType) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Adds one label, replacing any prior value for `key`.
    #[must_use]
    pub fn label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    /// Sets the help text.
    #[must_use]
    pub fn help(mut self, text: impl Into<String>) -> Self {
        self.help = Some(text.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IR-41: pins the exact string, and separately that no label value
    /// or help text appears. Either check alone would catch a regression.
    #[test]
    fn metric_options_debug_hides_labels_and_help() {
        let options = MetricOptions::new()
            .kind(MetricType::Counter)
            .label("model", "SECRET-LABEL-VALUE")
            .help("SECRET HELP TEXT");
        let rendered = format!("{options:?}");
        assert_eq!(
            rendered,
            "MetricOptions { kind: Some(Counter), labels: 1, help: true }"
        );
        assert!(
            !rendered.contains("SECRET-LABEL-VALUE") && !rendered.contains("SECRET HELP TEXT"),
            "a label or help text reached the Debug output: {rendered}"
        );
    }
}
