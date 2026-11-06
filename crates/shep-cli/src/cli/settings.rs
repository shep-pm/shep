//! Arguments for the settings verbs: `shep get <name>[.<path>]`,
//! `shep set <name>[.<path>] <value>` and `shep unset <name>[.<path>]`.
//!
//! One address, spelled [`CopyArgs`](crate::cli::CopyArgs)'s way: a sheep's
//! or a dog's name, then a path inside the settings that name holds. Nothing
//! here parses that address or its value: the verb module that owns the verb
//! does, through the one address parser `shep copy` already has.

/// Arguments to `shep get`.
///
/// Not [`SelectorArgs`](crate::cli::SelectorArgs): a read names one settings
/// holder, and `all`, a glob, `fold:<name>` or an id have no path to read a
/// path out of. The positional is required: bare `shep get` would have to
/// guess whether it means the whole flock or the key/value store, and the
/// store has its own verb.
#[derive(Debug, clap::Args)]
pub struct GetArgs {
    /// A sheep or a dog, and the path inside its settings, such as
    /// `api.max_memory`, `api.dogs.kelpie.git`, or `kelpie.worker`
    pub address: String,
}

/// Arguments to `shep set`.
///
/// [`GetArgs`]'s address and [`CopyArgs`](crate::cli::CopyArgs)'s two flags:
/// a write is a read followed by one value, and the question `copy` asks
/// before it writes is the question `set` asks too. There is no `--replace`
/// and no `--except`: `set` writes the one path it was given, and a path that
/// names a whole table takes a value that is one.
#[derive(Debug, clap::Args)]
pub struct SetArgs {
    /// A sheep or a dog, and the path inside its settings, such as
    /// `api.max_memory`, `api.dogs.kelpie.git`, or `kelpie.worker`
    pub address: String,
    /// The new value, as TOML: `5`, `true`, `["a", "b"]`, `{ keep = 1 }`. Text
    /// that is not TOML is the string as typed, such as `3s`
    pub value: String,
    /// Print what would change and write nothing
    #[arg(short = 'd', long)]
    pub dry_run: bool,
    /// Write without asking
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// Arguments to `shep unset`.
///
/// [`SetArgs`] without the value: the address names what to drop, and the
/// question `set` asks before it writes is the question `unset` asks too. A
/// sheep's field has no value to put back by hand, so dropping its override
/// is what returns the Flockfile's; a dog's key is removed so the dog's own
/// default applies.
#[derive(Debug, clap::Args)]
pub struct UnsetArgs {
    /// A sheep's field, or a key inside a dog's settings, such as
    /// `api.max_memory`, `api.dogs.kelpie.git`, or `kelpie.worker.timeout`
    pub address: String,
    /// Print what would change and write nothing
    #[arg(short = 'd', long)]
    pub dry_run: bool,
    /// Write without asking
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[cfg(test)]
mod tests {
    use crate::cli::{Cli, Commands};

    /// The address `shep get <argv>` parsed, or a panic naming the argv.
    fn address(argv: &[&str]) -> String {
        use clap::Parser as _;

        let full: Vec<&str> = ["shep", "get"]
            .into_iter()
            .chain(argv.iter().copied())
            .collect();
        let Commands::Get(args) = Cli::try_parse_from(&full)
            .unwrap_or_else(|err| panic!("{full:?} must parse: {err}"))
            .command
        else {
            panic!("{full:?} is not `shep get`")
        };
        args.address
    }

    /// fails if `get` stops taking one address, or stops accepting the four
    /// shapes an address has: a name alone, a sheep's field, a dog table key
    /// inside a sheep, and a table inside a dog's own section.
    #[test]
    fn get_takes_one_address_in_any_of_its_shapes() {
        assert_eq!(address(&["api"]), "api");
        assert_eq!(address(&["api.max_memory"]), "api.max_memory");
        assert_eq!(address(&["api.dogs.kelpie.git"]), "api.dogs.kelpie.git");
        assert_eq!(address(&["kelpie.worker"]), "kelpie.worker");
    }

    /// fails if bare `shep get` becomes accepted. It used to list the
    /// key/value store, and an operator who means that types `shep kv get`.
    #[test]
    fn bare_get_is_a_usage_error() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "get"]).is_err());
    }

    /// fails if `get` grows a second positional. One address per read is what
    /// keeps `shep get api.max_memory` saying one thing.
    #[test]
    fn get_takes_exactly_one_address() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "get", "api.max_memory", "web.max_memory"]).is_err());
    }

    /// fails if `set` stops taking one address and one value, or stops
    /// accepting a value that is TOML rather than a bare word.
    #[test]
    fn set_takes_an_address_a_value_and_two_flags() {
        use clap::Parser;
        let parse = |argv: &[&str]| match Cli::try_parse_from(argv).unwrap().command {
            Commands::Set(args) => args,
            other => panic!("expected set, got {other:?}"),
        };

        let plain = parse(&["shep", "set", "api.max_memory", "512M"]);
        assert_eq!(plain.address, "api.max_memory");
        assert_eq!(plain.value, "512M");
        assert!(!plain.dry_run);
        assert!(!plain.yes);

        let table = parse(&["shep", "set", "api.dogs.kelpie.git", "{ keep = 1 }"]);
        assert_eq!(table.value, "{ keep = 1 }");

        let flagged = parse(&[
            "shep",
            "set",
            "kelpie.worker.timeout",
            "30",
            "--dry-run",
            "--yes",
        ]);
        assert!(flagged.dry_run);
        assert!(flagged.yes);
    }

    /// fails if bare `shep set` becomes accepted. It used to be the
    /// key/value store's verb, and a write with no address named has nothing
    /// to write into.
    #[test]
    fn set_needs_both_positionals() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "set"]).is_err());
        assert!(Cli::try_parse_from(["shep", "set", "api.max_memory"]).is_err());
        assert!(
            Cli::try_parse_from(["shep", "set", "api.max_memory", "1", "2"]).is_err(),
            "one value per write is what keeps `shep set api.name a` saying one thing"
        );
    }
}
