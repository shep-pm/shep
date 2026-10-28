//! The contract between shep and a dog: the flag names, the grammar of the
//! `shep-` key lines, the schema's secret marker key, and the [`validate`]
//! trigger a dog answers before shep writes its settings.
//!
//! Shared by `shep-cli`'s `adopt` (the asker) and `shep_client::dogs::probe`
//! (the answerer), so the two agree by construction rather than by copying
//! a doc snippet.

pub mod manifest;
pub mod validate;

/// The flag a candidate is spawned with when shep asks for its version; the
/// contract `docs/dogs.md` publishes. Read by `shep-cli`'s `adopt`, answered
/// by `shep_client::dogs::probe`.
pub const VERSION_FLAG: &str = "--version";

/// The flag a candidate is spawned with when shep asks for its config
/// schema. Asked by `shep-cli`'s `adopt` on the same terms as
/// [`VERSION_FLAG`]: a dog that answers nothing is refused nothing.
pub const SCHEMA_FLAG: &str = "--schema";

/// The flag a candidate is spawned with when shep asks it to describe itself
/// whole: one JSON [`manifest::Manifest`] on stdout. A dog that exits
/// non-zero, prints nothing or prints something that is not a manifest has
/// none, and is asked [`VERSION_FLAG`] and [`SCHEMA_FLAG`] instead.
pub const MANIFEST_FLAG: &str = "--shep-manifest";

/// The key a `--version` answer states its protocol under.
///
/// Every `shep-` key besides this one and [`SHEP_CHANNEL_KEY`] is reserved,
/// and ignored rather than refused, so a dog written against a later
/// contract stays adoptable by this one.
pub const SHEP_PROTOCOL_KEY: &str = "shep-protocol";

/// The key a `--version` answer asks for the shepherd channel under.
///
/// Only the value `true` asks. A dog that asks is started with `channel`
/// and `shutdown_with_message` set, so it can answer `shep trigger` and be
/// stopped by a message rather than a signal.
pub const SHEP_CHANNEL_KEY: &str = "shep-channel";

/// The variable the shepherd names a dog in: the `[<name>]` section it
/// reads, and the name it announces at the handshake.
///
/// Set on every run shep makes of an adopted dog: supervised, `shep <name>`
/// and the probes. A built-in dog reads its name from argv instead.
pub const DOG_NAME_VAR: &str = "SHEP_DOG_NAME";

/// The variable that names the descriptor a supervised dog was spawned with
/// an open connection to its shepherd on.
///
/// Unix only, and set on every supervised dog. A process nothing supervised
/// has no such variable and connects on the control socket, where it acts as
/// the operator. See [`DOG_LINK_FD`]. Windows has no inherited descriptor and
/// uses [`DOG_LINK_PIPE_VAR`] instead.
pub const DOG_LINK_FD_VAR: &str = "SHEP_DOG_FD";

/// The Windows name of the pipe a supervised dog opens to its shepherd.
///
/// `$SHEP_CHANNEL_PIPE` names the channel's. This is set on every
/// supervised dog in place of [`DOG_LINK_FD_VAR`]. The shepherd creates
/// the pipe before the dog starts, under a descriptor admitting its own
/// user. An unsupervised process has no such variable and uses the
/// control pipe.
pub const DOG_LINK_PIPE_VAR: &str = "SHEP_DOG_PIPE";

/// The descriptor number a dog's connection arrives on, beside the shepherd
/// channel's fd 3.
pub const DOG_LINK_FD: i32 = 4;

/// The schemars extension key that marks a config field as a credential.
/// Written by the `dog_config` attribute. A typo fails silently: the schema
/// still validates, the field is simply not marked, and a credential can
/// render unredacted.
pub const SECRET_KEY: &str = "x-shep-secret";

/// The schemars extension key that holds the schema of a dog's per-sheep
/// `[app.dogs.<name>]` table, inside its `--schema` answer.
///
/// A dog that acts per sheep publishes this alongside its own top-level
/// properties, sharing the same `$defs`, so a `$ref` under this key
/// resolves exactly as any other one in the document does. A dog that
/// predates it simply has no such key.
pub const SHEEP_SCHEMA_KEY: &str = "x-shep-sheep";

/// What a dog answered [`VERSION_FLAG`] with, parsed by
/// [`parse_version_answer`] from the format `docs/dogs.md` publishes.
///
/// `protocol` decides whether the dog can handshake at all; `version` only
/// names the build. `protocol` is optional: an absent one reads as unknown,
/// not a fault.
#[derive(Debug, PartialEq, Eq)]
pub struct DogVersion {
    /// The last whitespace-separated field of line 1, the version. The
    /// name before it is ignored, so a crate whose name differs from the
    /// dog's registered name answers correctly without knowing it.
    pub version: String,
    /// The `shep-protocol` line's value, and `None` when the answer carried
    /// no such line or carried one that is not a decimal number. Answering
    /// is optional, so `None` is an unknown protocol rather than a fault.
    pub protocol: Option<u32>,
    /// Whether the answer carried [`SHEP_CHANNEL_KEY`] set to `true`. Any
    /// other value, or no such line, is a dog that did not ask.
    pub channel: bool,
}

/// Parses the format `docs/dogs.md` publishes: `<name> <version>` on line
/// 1, then `<key>: <value>` lines.
///
/// `None` when there is no line 1. Unknown keys, blank lines, key order and
/// a non-numeric `shep-protocol` are all tolerated rather than refused;
/// only an exact [`SHEP_PROTOCOL_KEY`] carrying a decimal is believed, and
/// only an exact [`SHEP_CHANNEL_KEY`] carrying `true` asks.
#[must_use]
pub fn parse_version_answer(text: &str) -> Option<DogVersion> {
    let mut lines = text.lines();
    let version = lines.next()?.split_whitespace().next_back()?.to_string();
    let mut protocol = None;
    let mut channel = false;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            SHEP_PROTOCOL_KEY => protocol = value.trim().parse().ok(),
            SHEP_CHANNEL_KEY => channel = value.trim() == "true",
            _ => {}
        }
    }
    Some(DogVersion {
        version,
        protocol,
        channel,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_output_is_no_answer() {
        assert_eq!(parse_version_answer(""), None);
    }

    #[test]
    fn a_bad_protocol_number_reads_as_unknown_not_a_fault() {
        assert_eq!(
            parse_version_answer("shep-otel 0.1.3\nshep-protocol: two\n"),
            Some(DogVersion {
                version: "0.1.3".to_string(),
                protocol: None,
                channel: false,
            })
        );
    }

    #[test]
    fn a_dog_asks_for_the_channel_with_true_and_nothing_else() {
        let asks = |line: &str| {
            parse_version_answer(&format!("shep-otel 0.1.3\n{line}\n"))
                .unwrap()
                .channel
        };
        assert!(asks("shep-channel: true"));
        assert!(
            asks("shep-channel:true"),
            "the space is optional, as it is for the protocol"
        );
        assert!(!asks("shep-channel: false"));
        assert!(!asks("shep-channel: yes"), "only `true` asks");
        assert!(!asks("shep-channel: TRUE"), "the value is case-sensitive");
        assert!(!asks("x-shep-channel: true"), "the key is exact");
        assert!(!asks("shep-protocol: 11"), "no line is no ask");
    }

    #[test]
    fn a_channel_ask_does_not_need_a_stated_protocol() {
        assert_eq!(
            parse_version_answer("shep-otel 0.1.3\nshep-channel: true\n"),
            Some(DogVersion {
                version: "0.1.3".to_string(),
                protocol: None,
                channel: true,
            })
        );
    }
}
