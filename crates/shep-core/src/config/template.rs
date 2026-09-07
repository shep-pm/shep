//! The `{{...}}` grammar for Flockfile values.
//!
//! Three tokens in env values, args, and the two log-path fields:
//! `{{instance}}` and `{{name}}` substitute from the sheep's identity, and
//! `{{secret:KEY}}` (or `{{secret:namespace/KEY}}`) reads
//! [`crate::secrets`]. An unknown token between doubled braces is refused
//! at config time rather than reaching a child process as literal text.
//!
//! Doubled braces avoid collision with single-brace content already in these
//! values: JSON blobs, regex quantifiers, Go or Helm templates passed
//! through as args.
//!
//! `{{{{` and `}}}}` escape to literal `{{` and `}}`. A lone `}}`, as in
//! `{"a":{"b":1}}`, is ordinary text and passes through unchanged.

use core::convert::Infallible;
use core::fmt;

use crate::secrets::{Resolution, SecretRef, SecretView};

/// The positional tokens this grammar knows, in the order an error lists
/// them.
const TOKENS: &[&str] = &["instance", "name"];

/// The prefix marking a store lookup, as it appears inside the braces.
const SECRET_PREFIX: &str = "secret:";

/// The store reference `token` names, or `None` when it is not a well-formed
/// `{{secret:...}}` body.
///
/// [`SecretRef::parse`] is the only grammar for a reference, so a token
/// [`validate`] accepts is one [`render`] can parse.
///
/// `pub(crate)`: [`crate::secrets::references`] shares this rather than
/// re-deriving what a `secret:` body is.
pub(crate) fn secret_reference(token: &str) -> Option<SecretRef<'_>> {
    token.strip_prefix(SECRET_PREFIX).and_then(SecretRef::parse)
}

/// A value that is not a valid template.
///
/// `pub(crate)`: `normalize` is the only caller, and wraps this in its own
/// [`NormalizeError::BadTemplate`](super::normalize::NormalizeError::BadTemplate).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TemplateError {
    /// A `{{...}}` naming something this grammar does not define
    UnknownToken {
        /// The token as the user wrote it, without the braces
        token: String,
    },
    /// A `{{` with no closing `}}`
    Unclosed,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownToken { token } if token.starts_with(SECRET_PREFIX) => write!(
                f,
                "`{{{{{token}}}}}` is not a valid secret reference: write \
                 `{{{{secret:KEY}}}}` or `{{{{secret:namespace/KEY}}}}`, where each part \
                 holds only letters, digits, `.`, `_` or `-` and does not start with `.`"
            ),
            Self::UnknownToken { token } => write!(
                f,
                "`{{{{{token}}}}}` is not a template token: valid tokens are {}",
                TOKENS
                    .iter()
                    .map(|t| format!("`{{{{{t}}}}}`"))
                    .chain(core::iter::once(format!("`{{{{{SECRET_PREFIX}...}}}}`")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Unclosed => f.write_str("a `{{` in this value is never closed by a `}}`"),
        }
    }
}

impl core::error::Error for TemplateError {}

/// A value whose grammar is valid but whose `{{secret:...}}` cannot be
/// resolved.
///
/// Redacted by construction (IR-41): a variant carries the reference as the
/// operator wrote it, the namespace and the environment, and no field can
/// hold a value.
///
/// `#[non_exhaustive]`: shep-core is published, so a new way for a
/// reference to fail must not break an out-of-tree `match`. It costs
/// in-tree callers nothing, since [`Self::is_retriable`] already gives them
/// the one classification they act on.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The store holds no value for this reference in this environment
    Unresolved {
        /// The reference as it appears in the value, braces and all
        reference: String,
        /// The environment the lookup ran against
        environment: String,
    },
    /// No provider dog has pushed the namespace this reference reads for
    /// the environment it was resolved in
    NamespaceUnready {
        /// The namespace the reference names
        namespace: String,
        /// The reference as it appears in the value, braces and all
        reference: String,
        /// The environment the lookup ran against
        environment: String,
    },
}

impl RenderError {
    /// Whether waiting could make this reference resolve.
    ///
    /// `true` for [`Self::NamespaceUnready`] alone: a provider dog that has
    /// not pushed this environment yet is the one failure a later attempt
    /// can clear. An [`Self::Unresolved`] waits on a person instead.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        matches!(self, Self::NamespaceUnready { .. })
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unresolved {
                reference,
                environment,
            } => write!(
                f,
                "`{reference}` has no value in the `{environment}` environment"
            ),
            Self::NamespaceUnready {
                namespace,
                reference,
                environment,
            } => write!(
                f,
                "`{reference}` reads the `{namespace}` namespace, which no provider dog \
                 has pushed to for the `{environment}` environment yet"
            ),
        }
    }
}

impl core::error::Error for RenderError {}

/// One piece of `value` as [`walk`] sees it: ordinary text, or a token name
/// with the braces stripped.
///
/// `pub(crate)`: [`crate::secrets::references`] matches on this directly
/// rather than [`walk`] growing a second, narrower traversal.
pub(crate) enum Segment<'a> {
    /// A run of ordinary text, copied through unchanged.
    Literal(&'a str),
    /// The name between a `{{` and its `}}`, braces stripped.
    Token(&'a str),
}

/// How far [`walk`] got through a value.
pub(crate) enum Completion {
    /// Every `{{` was closed by a `}}`.
    Complete,
    /// A `{{` was never closed; the segments before it were still emitted.
    Unclosed,
}

/// Walks `value`, calling `on_segment` for each literal run and each token.
///
/// One walker, one closure, so [`validate`], [`render`], [`render_positional`]
/// and [`crate::secrets::references`] can never disagree about what a token
/// is.
///
/// Generic over the closure's error so each caller keeps its own, with an
/// unclosed `{{` reported through [`Completion`] rather than as an error
/// every caller would have to be able to spell.
///
/// `pub(crate)`: [`crate::secrets::references`] walks a config's own values
/// for `{{secret:...}}` tokens rather than parsing them a second way.
///
/// # Errors
///
/// Whatever `on_segment` returns, at the first segment it refuses.
pub(crate) fn walk<E>(
    value: &str,
    mut on_segment: impl FnMut(Segment<'_>) -> Result<(), E>,
) -> Result<Completion, E> {
    let bytes = value.as_bytes();
    let mut at = 0;
    let mut literal_from = 0;
    while at < bytes.len() {
        if bytes[at..].starts_with(b"{{{{") {
            on_segment(Segment::Literal(&value[literal_from..at]))?;
            on_segment(Segment::Literal("{{"))?;
            at += 4;
            literal_from = at;
        } else if bytes[at..].starts_with(b"}}}}") {
            on_segment(Segment::Literal(&value[literal_from..at]))?;
            on_segment(Segment::Literal("}}"))?;
            at += 4;
            literal_from = at;
        } else if bytes[at..].starts_with(b"{{") {
            on_segment(Segment::Literal(&value[literal_from..at]))?;
            let rest = &value[at + 2..];
            let Some(end) = rest.find("}}") else {
                return Ok(Completion::Unclosed);
            };
            on_segment(Segment::Token(&rest[..end]))?;
            at += 2 + end + 2;
            literal_from = at;
        } else {
            at += 1;
        }
    }
    on_segment(Segment::Literal(&value[literal_from..]))?;
    Ok(Completion::Complete)
}

/// Writes `token` back with its braces, for a token the caller leaves alone.
fn push_token(out: &mut String, token: &str) {
    out.push_str("{{");
    out.push_str(token);
    out.push_str("}}");
}

/// The value `reference` names in `secrets`.
///
/// # Errors
///
/// - [`RenderError::NamespaceUnready`]: the reference names a namespace no
///   provider has pushed for this view's environment.
/// - [`RenderError::Unresolved`]: every other miss.
fn resolve_secret<'a>(
    reference: &SecretRef<'_>,
    secrets: &'a SecretView,
) -> Result<&'a str, RenderError> {
    match (secrets.resolve(reference), reference.namespace) {
        (Resolution::Found(value), _) => Ok(value),
        (Resolution::MissingNamespace, Some(namespace)) => Err(RenderError::NamespaceUnready {
            namespace: namespace.to_string(),
            reference: reference.to_string(),
            environment: secrets.environment().to_string(),
        }),
        (Resolution::MissingKey | Resolution::MissingNamespace, _) => {
            Err(RenderError::Unresolved {
                reference: reference.to_string(),
                environment: secrets.environment().to_string(),
            })
        }
    }
}

/// Whether `value` carries a `{{secret:...}}` this grammar would resolve.
///
/// `pub(crate)`: `normalize` asks it of the two log-path fields, which may
/// not hold a secret. Walks the same tokenizer [`render`] resolves against,
/// so a reference this misses is one `render` would not have substituted
/// either.
pub(crate) fn holds_secret(value: &str) -> bool {
    let mut found = false;
    let _ = walk::<Infallible>(value, |segment| {
        if let Segment::Token(token) = segment
            && secret_reference(token).is_some()
        {
            found = true;
        }
        Ok(())
    });
    found
}

/// Checks that every `{{...}}` in `value` names a token this grammar defines.
///
/// `pub(crate)`: only `normalize` asks this, at config time. [`render`] stays
/// public since shep-daemon's `assemble` runs it on already-validated values.
///
/// # Errors
///
/// - [`TemplateError::UnknownToken`]: a token this grammar does not define.
/// - [`TemplateError::Unclosed`]: a `{{` with no closing `}}`.
pub(crate) fn validate(value: &str) -> Result<(), TemplateError> {
    let completion = walk(value, |segment| match segment {
        Segment::Literal(_) => Ok(()),
        Segment::Token(token) if TOKENS.contains(&token) || secret_reference(token).is_some() => {
            Ok(())
        }
        Segment::Token(token) => Err(TemplateError::UnknownToken {
            token: token.to_string(),
        }),
    })?;
    match completion {
        Completion::Complete => Ok(()),
        Completion::Unclosed => Err(TemplateError::Unclosed),
    }
}

/// Substitutes `{{instance}}` and `{{name}}` only, leaving every other
/// token, `{{secret:...}}` included, exactly as written.
///
/// For callers that have no store to consult. `normalize` uses it to compare
/// two instances' log paths, where a secret resolves to the same value for
/// both instances and so cannot tell them apart anyway.
///
/// Call `validate` first: an unclosed `{{` renders truncated at that
/// point.
#[must_use]
pub fn render_positional(value: &str, name: &str, instance: u32) -> String {
    let mut out = String::with_capacity(value.len());
    let slot = instance.to_string();
    let _: Result<Completion, Infallible> = walk(value, |segment| {
        match segment {
            Segment::Literal(literal) => out.push_str(literal),
            Segment::Token("instance") => out.push_str(&slot),
            Segment::Token("name") => out.push_str(name),
            Segment::Token(token) => push_token(&mut out, token),
        }
        Ok(())
    });
    out
}

/// Substitutes every token in `value`, resolving `{{secret:...}}` against
/// `secrets`.
///
/// Call `validate` first: this assumes the grammar already passed, so a
/// token this grammar does not define is written back as it was, and an
/// unclosed `{{` renders truncated at that point.
///
/// # Errors
///
/// - [`RenderError::Unresolved`]: a reference the store has no value for in
///   this view's environment. Nothing but a person will supply it.
/// - [`RenderError::NamespaceUnready`]: a namespace no provider dog has
///   pushed to for this view's environment yet.
///   [`RenderError::is_retriable`] is `true` for this one alone.
pub fn render(
    value: &str,
    name: &str,
    instance: u32,
    secrets: &SecretView,
) -> Result<String, RenderError> {
    let mut out = String::with_capacity(value.len());
    let slot = instance.to_string();
    walk(value, |segment| {
        match segment {
            Segment::Literal(literal) => out.push_str(literal),
            Segment::Token("instance") => out.push_str(&slot),
            Segment::Token("name") => out.push_str(name),
            Segment::Token(token) => match secret_reference(token) {
                Some(reference) => out.push_str(resolve_secret(&reference, secrets)?),
                None => push_token(&mut out, token),
            },
        }
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_tokens_render() {
        assert_eq!(render_positional("z-{{instance}}", "worker", 3), "z-3");
        assert_eq!(
            render_positional("{{name}}-{{instance}}d", "worker", 3),
            "worker-3d"
        );
        assert_eq!(render_positional("91{{instance}}", "worker", 7), "917");
    }

    #[test]
    fn a_value_with_no_token_is_returned_unchanged() {
        // The collision case the doubled braces exist for: single braces are
        // ordinary content and must survive untouched. Both renderers, since
        // a JSON blob reaches a child through the fallible one.
        let empty = SecretView::empty("production".to_string());
        for value in [
            r#"{"ts":"%t","level":"%l"}"#,
            r#"{"a":{"b":1}}"#,
            "^[a-z]{2,3}$",
            "plain",
        ] {
            assert_eq!(
                render_positional(value, "worker", 1),
                value,
                "unchanged: {value}"
            );
            assert_eq!(
                render(value, "worker", 1, &empty).unwrap(),
                value,
                "unchanged: {value}"
            );
            assert!(validate(value).is_ok(), "and accepted: {value}");
        }
    }

    #[test]
    fn an_unknown_token_is_refused_by_name() {
        let err = validate("z-{{instnace}}").unwrap_err();
        assert!(matches!(&err, TemplateError::UnknownToken { token } if token == "instnace"));
        let rendered = err.to_string();
        assert!(rendered.contains("instnace"), "names the typo: {rendered}");
        assert!(
            rendered.contains("instance"),
            "and what is valid: {rendered}"
        );
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );
    }

    #[test]
    fn doubling_escapes_a_literal_token() {
        assert_eq!(
            render_positional("{{{{instance}}}}", "worker", 3),
            "{{instance}}"
        );
        assert!(validate("{{{{ .Values.port }}}}").is_ok());
        assert_eq!(
            render_positional("{{{{ .Values.port }}}}", "worker", 3),
            "{{ .Values.port }}",
            "a Helm template passes through for the tool that consumes it"
        );
    }

    #[test]
    fn an_unclosed_token_is_refused() {
        assert!(validate("z-{{instance").is_err());
    }

    fn view(environment: &str) -> SecretView {
        use crate::secrets::ProviderCache;
        use std::collections::{BTreeMap, BTreeSet};
        let store = BTreeMap::from([(
            "DB_PASSWORD".to_string(),
            BTreeMap::from([("production".to_string(), "hunter2".to_string())]),
        )]);
        let providers = ProviderCache {
            values: BTreeMap::from([(
                "vercel".to_string(),
                BTreeMap::from([(
                    "API_KEY".to_string(),
                    BTreeMap::from([("production".to_string(), "sk_live".to_string())]),
                )]),
            )]),
            pushed: BTreeMap::from([(
                "vercel".to_string(),
                BTreeSet::from(["production".to_string()]),
            )]),
        };
        SecretView::new(environment.to_string(), store, providers)
    }

    #[test]
    fn a_secret_token_validates_with_and_without_a_namespace() {
        assert!(validate("{{secret:DB_PASSWORD}}").is_ok());
        assert!(validate("{{secret:vercel/API_KEY}}").is_ok());
        assert!(validate("postgres://u:{{secret:DB_PASSWORD}}@db/app").is_ok());
    }

    #[test]
    fn a_malformed_reference_is_refused_at_config_time() {
        for bad in [
            "{{secret:}}",
            "{{secret:/KEY}}",
            "{{secret:ns/}}",
            "{{secret:a/b/c}}",
            "{{secret:has space}}",
        ] {
            let err = validate(bad).unwrap_err();
            let rendered = err.to_string();
            assert!(rendered.contains("secret"), "{bad}: {rendered}");
        }
    }

    #[test]
    fn an_unknown_prefix_is_still_refused_by_name() {
        // The closed token set is the whole reason the prefix exists.
        let err = validate("{{sekret:K}}").unwrap_err();
        assert!(matches!(&err, TemplateError::UnknownToken { token } if token == "sekret:K"));
    }

    #[test]
    fn render_substitutes_a_resolved_secret() {
        assert_eq!(
            render("pw={{secret:DB_PASSWORD}}", "web", 0, &view("production")).unwrap(),
            "pw=hunter2"
        );
        assert_eq!(
            render("{{secret:vercel/API_KEY}}", "web", 0, &view("production")).unwrap(),
            "sk_live"
        );
    }

    #[test]
    fn positional_tokens_still_render_beside_a_secret() {
        assert_eq!(
            render(
                "{{name}}-{{instance}}-{{secret:DB_PASSWORD}}",
                "web",
                3,
                &view("production")
            )
            .unwrap(),
            "web-3-hunter2"
        );
    }

    #[test]
    fn an_unresolvable_key_errors_naming_the_reference_and_the_environment() {
        let err = render("{{secret:ABSENT}}", "web", 0, &view("production")).unwrap_err();
        assert!(!err.is_retriable(), "a missing key is nobody's to retry");
        let rendered = err.to_string();
        assert!(rendered.contains("{{secret:ABSENT}}"), "{rendered}");
        assert!(rendered.contains("production"), "{rendered}");
    }

    #[test]
    fn a_secret_missing_only_in_this_environment_errors_rather_than_borrowing_another() {
        let err = render("{{secret:DB_PASSWORD}}", "web", 0, &view("staging")).unwrap_err();
        assert!(err.to_string().contains("staging"));
    }

    #[test]
    fn an_unready_namespace_is_retriable_and_says_which_one() {
        let err = render("{{secret:vault/ANY}}", "web", 0, &view("production")).unwrap_err();
        assert!(err.is_retriable(), "no dog has pushed under this name yet");
        let rendered = err.to_string();
        assert!(rendered.contains("vault"), "{rendered}");
    }

    #[test]
    fn a_namespace_that_is_up_and_lacks_the_key_is_not_retriable() {
        let err = render("{{secret:vercel/ABSENT}}", "web", 0, &view("production")).unwrap_err();
        assert!(!err.is_retriable());
    }

    #[test]
    fn no_render_error_ever_prints_a_value() {
        // Every variant, checked against the one value the fixture holds.
        // Debug as well as Display: a field added later that captured a
        // resolved value would leak through the derive.
        for value in ["{{secret:ABSENT}}", "{{secret:vault/ANY}}"] {
            let err = render(value, "web", 0, &view("production")).unwrap_err();
            for rendered in [err.to_string(), format!("{err:?}")] {
                assert!(!rendered.contains("hunter2"), "{rendered}");
                assert!(!rendered.contains("sk_live"), "{rendered}");
                assert!(
                    !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
                    "no em or en dash in copy a user reads: {rendered}"
                );
            }
        }
    }

    #[test]
    fn render_positional_leaves_a_secret_token_alone() {
        // normalize's log-path collision check runs at config time with no
        // store, and two instances share a secret's value anyway.
        assert_eq!(
            render_positional("{{secret:DB_PASSWORD}}-{{instance}}", "web", 2),
            "{{secret:DB_PASSWORD}}-2"
        );
    }

    #[test]
    fn doubling_still_escapes_a_secret_token() {
        assert_eq!(
            render("{{{{secret:DB_PASSWORD}}}}", "web", 0, &view("production")).unwrap(),
            "{{secret:DB_PASSWORD}}"
        );
    }
}
