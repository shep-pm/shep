//! What `adopt` and `enable` tell the operator about a dog's grants.

use shep_core::permissions::ENFORCED_SINCE;
use shep_core::protocol::DogSource;

use crate::output::Streams;

/// [`emit_notice`](crate::output::emit_notice) code for grants an older
/// shepherd records and does not enforce; not a failure.
const DOG_GRANTS_UNENFORCED_NOTICE: &str = "dog_grants_unenforced";

/// Says so when `source` carries grants and the shepherd that just took the
/// dog speaks a protocol older than [`ENFORCED_SINCE`].
///
/// A dog with no grants is undeclared and has nothing to enforce, so it
/// stays silent.
pub(super) fn warn_if_unenforced(
    streams: &mut Streams<'_>,
    name: &str,
    source: &DogSource,
    shepherd_protocol: u32,
) {
    if source.grants().is_none() || shepherd_protocol >= ENFORCED_SINCE {
        return;
    }
    let message = format!(
        "{name}'s grants are recorded in shep.toml but not enforced: this shepherd speaks \
         protocol {shepherd_protocol} and enforces grants from protocol {ENFORCED_SINCE}. \
         Upgrade and restart the shepherd to enforce them"
    );
    streams.aside(DOG_GRANTS_UNENFORCED_NOTICE, &message);
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeMap;

    use shep_client::Client;
    use shep_client::testing::{fake_daemon_answering_with_ack, sample_ack, sample_info};
    use shep_core::permissions::{Grants, Permission};
    use shep_core::protocol::{HelloAck, Response};

    use super::super::DogEntry;
    use super::super::enable::{EnableRefusal, enable_after_config, enable_in_config};
    use super::*;
    use crate::commands::dogs::test_support::streams;
    use crate::exit::ExitCode;

    fn granted() -> DogSource {
        DogSource::BuiltIn {
            grants: Some(Grants::new([Permission::FlockRead])),
        }
    }

    fn said(source: &DogSource, protocol: u32) -> String {
        let mut out = Vec::new();
        let mut err = Vec::new();
        warn_if_unenforced(&mut streams(&mut out, &mut err), "otel", source, protocol);
        String::from_utf8(err).unwrap()
    }

    #[test]
    fn an_older_shepherd_is_told_the_grants_are_recorded_but_not_enforced() {
        let text = said(&granted(), ENFORCED_SINCE - 1);

        assert!(
            text.contains("otel's grants are recorded in shep.toml but not enforced"),
            "{text}"
        );
        assert!(text.contains("protocol 11"), "{text}");
    }

    #[test]
    fn a_shepherd_that_enforces_has_nothing_to_warn_about() {
        assert_eq!(said(&granted(), ENFORCED_SINCE), "");
    }

    #[test]
    fn a_dog_with_no_grants_has_nothing_to_enforce_on_any_shepherd() {
        let undeclared = DogSource::BuiltIn { grants: None };

        assert_eq!(said(&undeclared, ENFORCED_SINCE - 1), "");
    }

    /// The typo is caught by `enable`, where the operator is looking, and not
    /// recorded as a grant that matches nothing until the next boot refuses it.
    #[test]
    fn enable_refuses_a_misspelt_grant_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shep.toml");
        let src = "[daemon.dog_grants.metrics]\ngranted = [\"flock.reed\"]\n";
        std::fs::write(&path, src).unwrap();

        let refusal = enable_in_config(&path, "metrics");

        let Err(EnableRefusal::Config(err)) = refusal else {
            panic!("expected a config refusal, got {refusal:?}");
        };
        assert!(
            err.to_string()
                .contains("`flock.reed` is not a permission shep knows"),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), src);
    }

    /// Through `enable`'s daemon half, so the check runs against the
    /// protocol the handshake really reported.
    #[tokio::test]
    async fn enable_warns_when_the_shepherd_it_reached_predates_enforcement() {
        for (protocol, warned) in [(ENFORCED_SINCE - 1, true), (ENFORCED_SINCE, false)] {
            let dir = tempfile::tempdir().unwrap();
            let path = shep_client::testing::control_address(dir.path());
            let ack = HelloAck {
                protocol,
                ..sample_ack()
            };
            let _envelopes =
                fake_daemon_answering_with_ack(&path, ack, |_| Response::DogStarted(sample_info()))
                    .await;
            let client = Client::connect(&path).await.unwrap();
            let mut out = Vec::new();
            let mut err = Vec::new();

            let entry = DogEntry {
                source: granted(),
                env: BTreeMap::default(),
            };
            let code = enable_after_config(
                &mut streams(&mut out, &mut err),
                "otel",
                &entry,
                Some(&client),
            )
            .await;

            assert_eq!(code, ExitCode::Success);
            let text = String::from_utf8(err).unwrap();
            assert_eq!(text.contains("not enforced"), warned, "{text}");
        }
    }
}
