use core::fmt;
use shep_core::config::template::RenderError;

/// Why a spawn spec could not be built: a `{{secret:...}}` that would not
/// resolve, or a dog the sheep needs and the shepherd lacks.
///
/// Redacted by construction (IR-41): `field` is an env key or a field's own
/// name, [`RenderError`] quotes only the reference, the namespace and the
/// environment, and `dog` is a dog's name. None can hold a secret's value.
#[non_exhaustive]
#[derive(Debug)]
pub enum AssembleError {
    /// A template in `field` could not be rendered.
    Template {
        /// The env key it was in, or the field's own name (`args`,
        /// `out_file`, `err_file`).
        field: String,
        /// Why.
        source: RenderError,
    },
    /// The sheep holds a table for dog `dog` that must launch it, and the
    /// shepherd has no enabled, adopted binary under that name.
    DogMissing {
        /// The dog's name.
        dog: String,
    },
}

impl AssembleError {
    /// Whether waiting could make this spec assemble.
    ///
    /// `true` only for a namespace no provider dog has pushed to yet; see
    /// [`RenderError::is_retriable`]. A caller that calls every refusal
    /// retriable turns a key nobody has set into a crash loop. A missing dog
    /// waits on an operator, never on time.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::Template { source, .. } => source.is_retriable(),
            Self::DogMissing { .. } => false,
        }
    }
}

impl fmt::Display for AssembleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Template { field, source } => write!(f, "`{field}`: {source}"),
            Self::DogMissing { dog } => write!(
                f,
                "the `{dog}` dog is not adopted or is disabled, and this sheep does not run without it"
            ),
        }
    }
}

impl core::error::Error for AssembleError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Template { source, .. } => Some(source),
            Self::DogMissing { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::spawn_spec::assemble;
    use super::AssembleError;

    use shep_core::secrets::SecretView;

    use super::super::testing::*;
    use shep_core::config::{AppConfig, normalize};

    #[test]
    fn a_missing_key_refuses_the_spawn_and_names_the_field() {
        let mut config = AppConfig::minimal("web", "./srv");
        config.env.insert("PW".into(), "{{secret:ABSENT}}".into());
        let app = normalize(config).unwrap();
        let err = assemble(
            &app,
            0,
            &test_paths(),
            None,
            &SecretView::empty("production".into()),
        )
        .unwrap_err();
        assert!(!err.is_retriable());
        // Exact strings for both renderings (IR-41): the type is meant to
        // carry a field name and a reference and never a value, and a
        // substring check cannot see a field it was never told about.
        assert_eq!(
            err.to_string(),
            "`PW`: `{{secret:ABSENT}}` has no value in the `production` environment"
        );
        assert_eq!(
            format!("{err:?}"),
            "Template { field: \"PW\", source: Unresolved { reference: \"{{secret:ABSENT}}\", \
                 environment: \"production\" } }"
        );
        let rendered = err.to_string();
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );
    }

    #[test]
    fn a_missing_dog_is_named_and_never_worth_waiting_for() {
        let err = AssembleError::DogMissing {
            dog: "sandbox".into(),
        };
        assert!(!err.is_retriable());
        assert_eq!(
            err.to_string(),
            "the `sandbox` dog is not adopted or is disabled, and this sheep does not run without it"
        );
        assert_eq!(format!("{err:?}"), "DogMissing { dog: \"sandbox\" }");
    }
}
