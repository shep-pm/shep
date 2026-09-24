//! What a dog does between [`probe`](super::probe) returning and its own
//! loop starting: read its arguments, then find its shep home.
//!
//! The arguments half is optional. A dog with a command mode of its own
//! parses them itself, and still resolves its home here.

use core::fmt;
use std::ffi::OsString;

use shep_core::dogs::{SCHEMA_FLAG, VERSION_FLAG};
use shep_core::paths::{self, HOME_DIR_VAR, SHEP_HOME_VAR, ShepPaths};

/// The one flag a dog answers itself: print a commented block for its
/// section of `dogs.toml`, then exit.
pub const PRINT_CONFIG_FLAG: &str = "--print-config";

/// What a dog's arguments asked for, once `probe` has answered shep's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DogAction {
    /// Run the dog until something stops it. No arguments at all.
    Run,
    /// Print the dog's commented section and exit: [`PRINT_CONFIG_FLAG`].
    PrintConfig,
}

/// An argument a dog does not accept.
///
/// Its `Display` is one sentence naming the dog, for the dog to follow
/// with its own usage text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError {
    dog: String,
    argument: String,
}

impl UsageError {
    /// The argument refused.
    #[must_use]
    pub fn argument(&self) -> &str {
        &self.argument
    }

    /// [`shep_core::exit::USAGE`], the code shep itself exits on bad
    /// arguments with.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        shep_core::exit::USAGE
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { dog, argument } = self;
        match argument.as_str() {
            "--help" | "-h" => write!(f, "{dog} takes no options."),
            VERSION_FLAG | SCHEMA_FLAG => write!(
                f,
                "{dog} answers {argument} as its first argument only, which is where the \
                 shepherd asks it."
            ),
            _ => write!(f, "{dog} does not understand {argument}."),
        }
    }
}

impl core::error::Error for UsageError {}

/// Reads a dog's arguments, program name excluded, as `dog` names itself.
///
/// A repeated [`PRINT_CONFIG_FLAG`] is still one request. A probe flag
/// reaching this was not first, which is the only place `probe` reads it.
///
/// # Errors
///
/// [`UsageError`] for any other argument. Refused rather than ignored: a
/// flag the dog silently skipped would look like a setting that took.
pub fn parse_args<'a>(
    dog: &str,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<DogAction, UsageError> {
    let mut action = DogAction::Run;
    for argument in args {
        if argument != PRINT_CONFIG_FLAG {
            return Err(UsageError {
                dog: dog.to_owned(),
                argument: argument.to_owned(),
            });
        }
        action = DogAction::PrintConfig;
    }
    Ok(action)
}

/// Why no shep home could be found to reach the shepherd through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HomeError {
    /// Neither `SHEP_HOME` nor a home directory is set.
    Unset,
    /// `SHEP_HOME` is set to the empty string.
    Empty,
    /// `SHEP_HOME` is not valid UTF-8, which shep refuses as a home.
    NotUtf8,
}

impl HomeError {
    /// [`shep_core::exit::USAGE`], the code shep itself refuses a home
    /// with.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        shep_core::exit::USAGE
    }
}

impl fmt::Display for HomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unset => write!(
                f,
                "neither {SHEP_HOME_VAR} nor {HOME_DIR_VAR} is set, so there is no shep home \
                 to find the shepherd in"
            ),
            Self::Empty => write!(f, "{SHEP_HOME_VAR} is set but empty"),
            Self::NotUtf8 => write!(f, "{SHEP_HOME_VAR} is not valid UTF-8"),
        }
    }
}

impl core::error::Error for HomeError {}

/// The `$SHEP_HOME` layout, read through `var` the way `shep` reads it.
///
/// `SHEP_HOME` wins when set. Otherwise the default sits under the user's
/// home, which on Windows is `USERPROFILE` when `HOME` is unset.
///
/// # Errors
///
/// [`HomeError`] when neither names a directory, or `SHEP_HOME` is set to
/// something that cannot.
pub fn resolve_paths(var: &dyn Fn(&str) -> Option<OsString>) -> Result<ShepPaths, HomeError> {
    let named = match var("SHEP_HOME") {
        None => None,
        Some(value) if value.is_empty() => return Err(HomeError::Empty),
        Some(value) => Some(value.into_string().map_err(|_| HomeError::NotUtf8)?),
    };
    let env = |key: &str| (key == "SHEP_HOME").then(|| named.clone()).flatten();
    let home = paths::shep_home(&env, paths::user_home(var).as_deref()).ok_or(HomeError::Unset)?;
    Ok(ShepPaths::at(home))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn no_arguments_runs_and_print_config_prints() {
        assert_eq!(parse_args("shep-discord", []), Ok(DogAction::Run));
        assert_eq!(
            parse_args("shep-discord", ["--print-config", "--print-config"]),
            Ok(DogAction::PrintConfig)
        );
    }

    #[test]
    fn every_other_argument_is_refused_by_name() {
        let refused = |argument| parse_args("shep-log-rotate", [argument]).unwrap_err();
        assert_eq!(
            refused("-h").to_string(),
            "shep-log-rotate takes no options."
        );
        assert_eq!(
            refused("--dry-run").to_string(),
            "shep-log-rotate does not understand --dry-run."
        );
        assert_eq!(refused("--dry-run").exit_code(), shep_core::exit::USAGE);
    }

    /// `probe` reads the first argument only, so a probe flag reaching the
    /// parser is out of place rather than unknown.
    #[test]
    fn a_probe_flag_after_the_first_argument_says_where_it_belongs() {
        let err = parse_args("shep-discord", ["--print-config", VERSION_FLAG]).unwrap_err();
        assert_eq!(err.argument(), VERSION_FLAG);
        assert!(
            err.to_string().contains("as its first argument only"),
            "{err}"
        );
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
            .collect();
        move |key| {
            pairs
                .iter()
                .find(|(named, _)| named == key)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn shep_home_wins_and_needs_no_home_directory() {
        let paths = resolve_paths(&vars(&[("SHEP_HOME", "/srv/shep")])).unwrap();
        assert_eq!(paths, ShepPaths::at(PathBuf::from("/srv/shep")));
    }

    #[cfg(unix)]
    #[test]
    fn the_default_sits_under_the_users_home() {
        let paths = resolve_paths(&vars(&[("HOME", "/home/ada")])).unwrap();
        assert_eq!(paths.home, PathBuf::from("/home/ada/.shep"));
    }

    #[test]
    fn no_home_at_all_is_refused_rather_than_the_working_directory() {
        assert_eq!(resolve_paths(&vars(&[])), Err(HomeError::Unset));
        assert_eq!(
            resolve_paths(&vars(&[("SHEP_HOME", ""), ("HOME", "/home/ada")])),
            Err(HomeError::Empty)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_shep_home_that_is_not_utf8_is_refused() {
        use std::os::unix::ffi::OsStringExt as _;
        let lossy = OsString::from_vec(vec![b'/', 0xff]);
        let var = move |key: &str| (key == "SHEP_HOME").then(|| lossy.clone());
        assert_eq!(resolve_paths(&var), Err(HomeError::NotUtf8));
    }
}
