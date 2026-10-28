//! The `{{...}}` grammar for Flockfile values.
//!
//! Four tokens, in env values, args, and the two log-path fields.
//! `{{instance}}` and `{{name}}` read the sheep's own identity.
//! `{{SHEP_HOME}}` reads the directory the shepherd keeps its state in.
//! `{{secret:KEY}}`, or `{{secret:namespace/KEY}}`, reads [`crate::secrets`].
//! An unknown token is refused at config time, not left to a child as text.
//!
//! `{{{{` and `}}}}` escape to literal `{{` and `}}`. A lone `}}`, as in
//! `{"a":{"b":1}}`, is ordinary text.

use core::convert::Infallible;
use core::fmt;
use std::borrow::Cow;
use std::path::Path;

use crate::secrets::{Resolution, SecretRef, SecretView};

/// The positional tokens this grammar knows, in the order an error lists
/// them.
const TOKENS: &[&str] = &["instance", "name", SHEP_HOME];

/// The prefix marking a store lookup, as it appears inside the braces.
const SECRET_PREFIX: &str = "secret:";

/// The token naming the shepherd's own home directory.
///
/// Uppercase because it names an environment variable; see the casing rule
/// in this module's own documentation.
const SHEP_HOME: &str = "SHEP_HOME";

/// The store reference `token` names, or `None` when it is not a well-formed
/// `{{secret:...}}` body.
///
/// [`SecretRef::parse`] is the only grammar for a reference, so a token
/// [`validate`] accepts is one [`render`] can parse.
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
/// Redacted by construction (IR-41): no field can hold a value. A variant
/// carries the reference as written, the namespace and the environment.
/// `#[non_exhaustive]` because shep-core is published: a new failure mode
/// must not break an out-of-tree `match`.
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
    /// A `{{secret:...}}` whose body is outside the reference grammar
    ///
    /// Only [`render_secrets`] reports this: `validate` has already refused
    /// the spelling by the time [`render`] sees a value.
    Malformed {
        /// The token as it appears in the value, braces and all
        reference: String,
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
            Self::Malformed { reference } => write!(
                f,
                "`{reference}` is not a valid secret reference: write `{{{{secret:KEY}}}}` \
                 or `{{{{secret:namespace/KEY}}}}`"
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
/// One walker, so [`validate`], [`render`], [`render_positional`] and
/// [`crate::secrets::references`] cannot disagree about what a token is.
/// Generic over the closure's error, so each caller keeps its own. An
/// unclosed `{{` comes back through [`Completion`], not as a shared error.
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

/// Whether any token in `value` satisfies `wanted`.
///
/// Walks the same tokenizer the renderers resolve against, so a token this
/// misses is one they would not have substituted either. An escaped
/// `{{{{name}}}}` is a literal here, as it is there.
fn holds_token(value: &str, wanted: impl Fn(&str) -> bool) -> bool {
    let mut found = false;
    let _ = walk::<Infallible>(value, |segment| {
        if let Segment::Token(token) = segment
            && wanted(token)
        {
            found = true;
        }
        Ok(())
    });
    found
}

/// Whether `value` carries a `{{secret:...}}` this grammar would resolve.
///
/// `pub(crate)`: `normalize` asks it of the two log-path fields, which may
/// not hold a secret.
pub(crate) fn holds_secret(value: &str) -> bool {
    holds_token(value, |token| secret_reference(token).is_some())
}

/// Whether `value` carries a `{{SHEP_HOME}}`.
///
/// `pub(crate)`: `normalize` asks it of every templated field, to refuse a
/// value nothing could expand rather than hand a child a path with braces
/// in it.
pub(crate) fn holds_shep_home(value: &str) -> bool {
    holds_token(value, |token| token == SHEP_HOME)
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

/// The text a token expands to, or `None` for a token neither renderer
/// knows positionally.
///
/// The positional tokens live here, not in each renderer's match. No token
/// expands one way under `render` and another under `render_positional`.
/// `{{SHEP_HOME}}` with no `shep_home` answers `None` and is written back
/// with its braces, as an unknown token is.
fn positional<'a>(
    token: &str,
    name: &'a str,
    slot: &'a str,
    shep_home: Option<&'a Path>,
) -> Option<Cow<'a, str>> {
    match token {
        "instance" => Some(Cow::Borrowed(slot)),
        "name" => Some(Cow::Borrowed(name)),
        SHEP_HOME => shep_home.map(|home| home.to_string_lossy()),
        _ => None,
    }
}

/// Substitutes `{{instance}}`, `{{name}}` and `{{SHEP_HOME}}` only, leaving
/// every other token, `{{secret:...}}` included, exactly as written.
///
/// For callers with no store to consult. `shep_home` is the directory
/// [`crate::paths::shep_home`] names; `None` leaves `{{SHEP_HOME}}` written
/// back with its braces. Call `validate` first: an unclosed `{{` renders
/// truncated at that point.
#[must_use]
pub fn render_positional(
    value: &str,
    name: &str,
    instance: u32,
    shep_home: Option<&Path>,
) -> String {
    let mut out = String::with_capacity(value.len());
    let slot = instance.to_string();
    let _: Result<Completion, Infallible> = walk(value, |segment| {
        match segment {
            Segment::Literal(literal) => out.push_str(literal),
            Segment::Token(token) => match positional(token, name, &slot, shep_home) {
                Some(text) => out.push_str(&text),
                None => push_token(&mut out, token),
            },
        }
        Ok(())
    });
    out
}

/// Substitutes every token in `value`, resolving `{{secret:...}}` against
/// `secrets`. `shep_home` means what it does in [`render_positional`]: the
/// two share one resolver.
///
/// Call `validate` first, which this assumes already passed. An unknown
/// token is written back as it was, and an unclosed `{{` renders truncated.
///
/// # Errors
///
/// - [`RenderError::Unresolved`]: a reference the store has no value for in
///   this view's environment.
/// - [`RenderError::NamespaceUnready`]: a namespace no provider dog has
///   pushed to yet. [`RenderError::is_retriable`] is `true` for this alone.
pub fn render(
    value: &str,
    name: &str,
    instance: u32,
    shep_home: Option<&Path>,
    secrets: &SecretView,
) -> Result<String, RenderError> {
    let mut out = String::with_capacity(value.len());
    let slot = instance.to_string();
    walk(value, |segment| {
        match segment {
            Segment::Literal(literal) => out.push_str(literal),
            Segment::Token(token) => match positional(token, name, &slot, shep_home) {
                Some(text) => out.push_str(&text),
                None => match secret_reference(token) {
                    Some(reference) => out.push_str(resolve_secret(&reference, secrets)?),
                    None => push_token(&mut out, token),
                },
            },
        }
        Ok(())
    })?;
    Ok(out)
}

/// Substitutes only the `{{secret:...}}` tokens in `value`, resolving them
/// against `secrets`. Every other byte is copied through as written.
///
/// Another reader parses this text for its own tokens. Unescaping or
/// expanding them here would change what it sees.
///
/// # Errors
///
/// - [`RenderError::Malformed`]: a `{{secret:` token outside
///   [`SecretRef::parse`]'s grammar, refused rather than passed through.
/// - [`RenderError::Unresolved`]: the store has no value for it in this view's environment.
/// - [`RenderError::NamespaceUnready`]: no provider dog has pushed to that namespace yet.
pub fn render_secrets(value: &str, secrets: &SecretView) -> Result<String, RenderError> {
    let mut out = String::with_capacity(value.len());
    // How much of `value` the segments so far account for. `walk` hands an
    // escape over as the text it stands for, so the doubled form is read
    // back off the source rather than rebuilt from the segment.
    let mut at = 0;
    let completion = walk(value, |segment| {
        match segment {
            Segment::Literal(literal @ ("{{" | "}}"))
                if value[at..].starts_with(&literal.repeat(2)) =>
            {
                out.push_str(&literal.repeat(2));
                at += 4;
            }
            Segment::Literal(literal) => {
                out.push_str(literal);
                at += literal.len();
            }
            Segment::Token(token) => {
                match secret_reference(token) {
                    Some(reference) => out.push_str(resolve_secret(&reference, secrets)?),
                    None if token.starts_with(SECRET_PREFIX) => {
                        let mut reference = String::new();
                        push_token(&mut reference, token);
                        return Err(RenderError::Malformed { reference });
                    }
                    None => push_token(&mut out, token),
                }
                at += token.len() + 4;
            }
        }
        Ok(())
    })?;
    if matches!(completion, Completion::Unclosed) {
        out.push_str(&value[at..]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_tokens_render() {
        assert_eq!(
            render_positional("z-{{instance}}", "worker", 3, None),
            "z-3"
        );
        assert_eq!(
            render_positional("{{name}}-{{instance}}d", "worker", 3, None),
            "worker-3d"
        );
        assert_eq!(
            render_positional("91{{instance}}", "worker", 7, None),
            "917"
        );
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
                render_positional(value, "worker", 1, None),
                value,
                "unchanged: {value}"
            );
            assert_eq!(
                render(value, "worker", 1, None, &empty).unwrap(),
                value,
                "unchanged: {value}"
            );
            assert!(validate(value).is_ok(), "and accepted: {value}");
        }
    }

    #[test]
    fn shep_home_renders_from_the_directory_it_is_handed() {
        let home = Path::new("/home/ada/.shep");
        let empty = SecretView::empty("production".to_string());
        for value in ["{{SHEP_HOME}}/logs/out.log", "{{SHEP_HOME}}"] {
            let expected = value.replace("{{SHEP_HOME}}", "/home/ada/.shep");
            assert_eq!(
                render_positional(value, "worker", 0, Some(home)),
                expected,
                "positionally: {value}"
            );
            assert_eq!(
                render(value, "worker", 0, Some(home), &empty).unwrap(),
                expected,
                "and through the store: {value}"
            );
        }
    }

    /// The invariant `positional` exists for: a token both renderers reach
    /// through one resolver cannot expand one way here and another there.
    /// This is the check that fails if a later token is wired into only one.
    #[test]
    fn every_positional_token_renders_alike_in_both_renderers() {
        let home = Path::new("/home/ada/.shep");
        let empty = SecretView::empty("production".to_string());
        for token in TOKENS {
            let value = format!("<{{{{{token}}}}}>");
            assert_eq!(
                render_positional(&value, "worker", 4, Some(home)),
                render(&value, "worker", 4, Some(home), &empty).unwrap(),
                "both renderers agree on {value}"
            );
        }
    }

    /// Instance-independent on purpose: it is the one positional token that
    /// cannot tell two slots apart, which is what normalize's log-path
    /// collision check reads it for.
    #[test]
    fn shep_home_renders_the_same_for_every_instance() {
        let home = Path::new("/home/ada/.shep");
        let value = "{{SHEP_HOME}}/logs/{{name}}.log";
        assert_eq!(
            render_positional(value, "worker", 0, Some(home)),
            render_positional(value, "worker", 1, Some(home))
        );
    }

    #[test]
    fn shep_home_with_no_home_is_written_back_with_its_braces() {
        // normalize refuses this value rather than letting it reach a child,
        // so the arm is a degradation; what it must not do is drop the token.
        assert_eq!(
            render_positional("{{SHEP_HOME}}/logs/out.log", "worker", 0, None),
            "{{SHEP_HOME}}/logs/out.log"
        );
    }

    #[test]
    fn doubling_escapes_shep_home_too() {
        assert_eq!(
            render_positional(
                "{{{{SHEP_HOME}}}}",
                "worker",
                0,
                Some(Path::new("/home/ada/.shep"))
            ),
            "{{SHEP_HOME}}"
        );
    }

    /// Casing is the rule, not a spelling shep is relaxed about: a lowercase
    /// name would read as a config field, and this grammar has none by that
    /// name.
    #[test]
    fn a_lowercase_shep_home_is_not_the_token() {
        let err = validate("{{shep_home}}/logs").unwrap_err();
        assert!(matches!(&err, TemplateError::UnknownToken { token } if token == "shep_home"));
        assert!(
            err.to_string().contains("{{SHEP_HOME}}"),
            "and the error shows the casing: {err}"
        );
    }

    /// The sibling of the test below, and the one with teeth: `holds_secret`
    /// is what `normalize` refuses a log path on, so a `secret_reference`
    /// that started accepting an empty key would widen that refusal with
    /// nothing to say so.
    #[test]
    fn holds_secret_sees_a_reference_and_not_its_escape() {
        assert!(holds_secret("{{secret:DB_PASSWORD}}"));
        assert!(holds_secret("postgres://u:{{secret:PW}}@db/app"));
        assert!(holds_secret("{{secret:vercel/API_KEY}}"));
        assert!(!holds_secret("{{{{secret:DB_PASSWORD}}}}"));
        assert!(!holds_secret("{{secret:}}"), "not a well-formed reference");
        assert!(!holds_secret("{{sekret:K}}"), "not this grammar's prefix");
        assert!(!holds_secret("{{name}}-out.log"));
        assert!(!holds_secret("/var/log/out.log"));
    }

    #[test]
    fn holds_shep_home_sees_the_token_and_not_its_escape() {
        assert!(holds_shep_home("{{SHEP_HOME}}/logs/out.log"));
        assert!(!holds_shep_home("{{{{SHEP_HOME}}}}/logs/out.log"));
        assert!(!holds_shep_home("/var/log/out.log"));
        assert!(!holds_shep_home("{{name}}-out.log"));
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
            render_positional("{{{{instance}}}}", "worker", 3, None),
            "{{instance}}"
        );
        assert!(validate("{{{{ .Values.port }}}}").is_ok());
        assert_eq!(
            render_positional("{{{{ .Values.port }}}}", "worker", 3, None),
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
        let providers = ProviderCache::new(
            BTreeMap::from([(
                "vercel".to_string(),
                BTreeMap::from([(
                    "API_KEY".to_string(),
                    BTreeMap::from([("production".to_string(), "sk_live".to_string())]),
                )]),
            )]),
            BTreeMap::from([(
                "vercel".to_string(),
                BTreeSet::from(["production".to_string()]),
            )]),
        );
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
            render(
                "pw={{secret:DB_PASSWORD}}",
                "web",
                0,
                None,
                &view("production")
            )
            .unwrap(),
            "pw=hunter2"
        );
        assert_eq!(
            render(
                "{{secret:vercel/API_KEY}}",
                "web",
                0,
                None,
                &view("production")
            )
            .unwrap(),
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
                None,
                &view("production")
            )
            .unwrap(),
            "web-3-hunter2"
        );
    }

    #[test]
    fn an_unresolvable_key_errors_naming_the_reference_and_the_environment() {
        let err = render("{{secret:ABSENT}}", "web", 0, None, &view("production")).unwrap_err();
        assert!(!err.is_retriable(), "a missing key is nobody's to retry");
        let rendered = err.to_string();
        assert!(rendered.contains("{{secret:ABSENT}}"), "{rendered}");
        assert!(rendered.contains("production"), "{rendered}");
    }

    #[test]
    fn a_secret_missing_only_in_this_environment_errors_rather_than_borrowing_another() {
        let err = render("{{secret:DB_PASSWORD}}", "web", 0, None, &view("staging")).unwrap_err();
        assert!(err.to_string().contains("staging"));
    }

    #[test]
    fn an_unready_namespace_is_retriable_and_says_which_one() {
        let err = render("{{secret:vault/ANY}}", "web", 0, None, &view("production")).unwrap_err();
        assert!(err.is_retriable(), "no dog has pushed under this name yet");
        let rendered = err.to_string();
        assert!(rendered.contains("vault"), "{rendered}");
    }

    #[test]
    fn a_namespace_that_is_up_and_lacks_the_key_is_not_retriable() {
        let err = render(
            "{{secret:vercel/ABSENT}}",
            "web",
            0,
            None,
            &view("production"),
        )
        .unwrap_err();
        assert!(!err.is_retriable());
    }

    /// Every variant, both renderings, as exact strings (IR-41): a field
    /// added later that captured a resolved value would leak through the
    /// derived `Debug`, and a `contains` check cannot see a field it was
    /// never told to look for.
    #[test]
    fn no_render_error_ever_prints_a_value() {
        let unresolved =
            render("{{secret:ABSENT}}", "web", 0, None, &view("production")).unwrap_err();
        assert_eq!(
            unresolved.to_string(),
            "`{{secret:ABSENT}}` has no value in the `production` environment"
        );
        assert_eq!(
            format!("{unresolved:?}"),
            "Unresolved { reference: \"{{secret:ABSENT}}\", environment: \"production\" }"
        );

        let unready =
            render("{{secret:vault/ANY}}", "web", 0, None, &view("production")).unwrap_err();
        assert_eq!(
            unready.to_string(),
            "`{{secret:vault/ANY}}` reads the `vault` namespace, which no provider dog \
             has pushed to for the `production` environment yet"
        );
        assert_eq!(
            format!("{unready:?}"),
            "NamespaceUnready { namespace: \"vault\", reference: \"{{secret:vault/ANY}}\", \
             environment: \"production\" }"
        );

        for rendered in [unresolved.to_string(), unready.to_string()] {
            assert!(
                !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
                "no em or en dash in copy a user reads: {rendered}"
            );
        }
    }

    #[test]
    fn render_positional_leaves_a_secret_token_alone() {
        // normalize's log-path collision check runs at config time with no
        // store, and two instances share a secret's value anyway.
        assert_eq!(
            render_positional("{{secret:DB_PASSWORD}}-{{instance}}", "web", 2, None),
            "{{secret:DB_PASSWORD}}-2"
        );
    }

    #[test]
    fn render_secrets_leaves_every_other_token_and_escape_alone() {
        let value = "{{name}}|{{instance}}|{{SHEP_HOME}}|{{unknown}}|{{{{secret:DB_PASSWORD}}}}|\
                     }}}}|a}}b|{\"a\":{\"b\":1}}|{{secret:DB_PASSWORD}}|{{secret:vercel/API_KEY}}";
        assert_eq!(
            render_secrets(value, &view("production")).unwrap(),
            "{{name}}|{{instance}}|{{SHEP_HOME}}|{{unknown}}|{{{{secret:DB_PASSWORD}}}}|\
             }}}}|a}}b|{\"a\":{\"b\":1}}|hunter2|sk_live"
        );
    }

    #[test]
    fn render_secrets_leaves_an_unclosed_token_as_literal_text() {
        for value in ["token {{secret:DB_PASSWORD", "{{secret:", "x {{ y"] {
            assert_eq!(render_secrets(value, &view("production")).unwrap(), value);
        }
    }

    #[test]
    fn render_secrets_refuses_a_malformed_secret_token_naming_it() {
        for bad in ["{{secret:}}", "{{secret:bad key}}", "{{secret:a/b/c}}"] {
            let err = render_secrets(&format!("ok {bad}"), &view("production")).unwrap_err();
            assert_eq!(
                err,
                RenderError::Malformed {
                    reference: bad.to_string()
                }
            );
            assert!(err.to_string().contains(bad), "names it: {err}");
            assert!(!err.is_retriable());
        }
    }

    #[test]
    fn render_secrets_error_names_the_reference_and_never_a_value() {
        let unresolved = render_secrets("{{secret:ABSENT}}", &view("production")).unwrap_err();
        assert_eq!(
            unresolved.to_string(),
            "`{{secret:ABSENT}}` has no value in the `production` environment"
        );
        let unready = render_secrets("{{secret:vault/ANY}}", &view("production")).unwrap_err();
        assert!(unready.is_retriable());
        let malformed = render_secrets("{{secret:bad key}}", &view("production")).unwrap_err();
        assert_eq!(
            format!("{malformed:?}"),
            "Malformed { reference: \"{{secret:bad key}}\" }"
        );
        for err in [unresolved, unready, malformed] {
            for rendered in [err.to_string(), format!("{err:?}")] {
                assert!(
                    !rendered.contains("hunter2") && !rendered.contains("sk_live"),
                    "no value: {rendered}"
                );
            }
        }
    }

    #[test]
    fn doubling_still_escapes_a_secret_token() {
        assert_eq!(
            render(
                "{{{{secret:DB_PASSWORD}}}}",
                "web",
                0,
                None,
                &view("production")
            )
            .unwrap(),
            "{{secret:DB_PASSWORD}}"
        );
    }
}
