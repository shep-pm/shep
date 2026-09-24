//! Which dog this process runs as, as the shepherd named it.

use shep_core::dogs::DOG_NAME_VAR;

/// The two names a dog needs: the one it announces at the handshake, and
/// the `[<name>]` section of `dogs.toml` it reads.
///
/// Two because only the section may fall back to a default. A handshake
/// name the shepherd never handed out gets that other dog restarted for
/// this process's fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DogIdentity {
    handshake: Option<String>,
    section: String,
}

impl DogIdentity {
    /// Reads [`DOG_NAME_VAR`] through `env`, falling back to
    /// `default_section` for a process nothing named.
    ///
    /// A blank value names no dog. Anything else is taken verbatim, since
    /// the shepherd matches it against its own registry.
    #[must_use]
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>, default_section: &str) -> Self {
        match env(DOG_NAME_VAR).filter(|name| !name.trim().is_empty()) {
            Some(name) => Self::named(name),
            None => Self {
                handshake: None,
                section: default_section.to_owned(),
            },
        }
    }

    /// A dog the shepherd named some other way, such as a built-in dog's
    /// argv, which reads the section of the same name.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            handshake: Some(name.clone()),
            section: name,
        }
    }

    /// The name to announce at the handshake, or `None` for a process no
    /// shepherd started.
    #[must_use]
    pub fn handshake(&self) -> Option<&str> {
        self.handshake.as_deref()
    }

    /// The `[<name>]` section of `dogs.toml` this dog reads.
    #[must_use]
    pub fn section(&self) -> &str {
        &self.section
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(value: &str) -> impl Fn(&str) -> Option<String> {
        let value = value.to_owned();
        move |key| (key == DOG_NAME_VAR).then(|| value.clone())
    }

    #[test]
    fn the_shepherds_name_is_both_the_handshake_and_the_section() {
        let identity = DogIdentity::from_env(&named("rotate"), "log-rotate");
        assert_eq!(identity.handshake(), Some("rotate"));
        assert_eq!(identity.section(), "rotate");
    }

    #[test]
    fn an_unnamed_process_announces_nothing_and_reads_the_default() {
        let identity = DogIdentity::from_env(&|_| None, "log-rotate");
        assert_eq!(identity.handshake(), None);
        assert_eq!(identity.section(), "log-rotate");
    }

    /// shep-deploy's answer, and now every dog's: `[  ]` is not a section
    /// anybody writes, and no registry entry answers to it.
    #[test]
    fn a_blank_name_is_no_name() {
        for blank in ["", " ", "\t\n"] {
            let identity = DogIdentity::from_env(&named(blank), "discord");
            assert_eq!(identity, DogIdentity::from_env(&|_| None, "discord"));
        }
    }

    #[test]
    fn a_name_is_passed_through_verbatim() {
        let identity = DogIdentity::from_env(&named(" deploy "), "deploy");
        assert_eq!(identity.handshake(), Some(" deploy "));
    }
}
