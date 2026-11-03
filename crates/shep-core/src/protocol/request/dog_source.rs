//! Where a dog came from, as a flock row carries it.

use serde::{Deserialize, Serialize};

use crate::permissions::Grants;

// Named by an intra-doc link and by nothing rustc compiles.
#[cfg(doc)]
use super::ProcessInfo;

/// Where a dog came from: this binary, or one an operator adopted.
///
/// Carried on [`ProcessInfo::dog`], so a listing distinguishes the two
/// populations without a second request.
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DogSource {
    /// An argv branch of the shep binary itself (`shep dog <name>`).
    BuiltIn {
        /// What the operator granted the dog, or `None` when nothing was
        /// recorded: an undeclared dog, which keeps full access. Absent on
        /// the wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grants: Option<Grants>,
        /// The event kinds the dog declared in its manifest, which it may
        /// publish as `dog.<name>.<kind>`. Absent on the wire when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<String>,
    },
    /// A binary an operator adopted, run at the daemon's own trust level.
    Adopted {
        /// The binary's path, exactly as the operator gave it to `adopt`.
        path: String,
        /// Whether the binary asked for the shepherd channel when it was
        /// adopted, with a `shep-channel: true` line in its `--version`
        /// answer. Such a dog is spawned with `channel` and
        /// `shutdown_with_message` set. Absent on the wire when `false`.
        #[serde(default, skip_serializing_if = "core::ops::Not::not")]
        channel: bool,
        /// What the operator granted the dog, or `None` when nothing was
        /// recorded: an undeclared dog, which keeps full access. An empty
        /// set is a dog granted nothing. Absent on the wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grants: Option<Grants>,
        /// The event kinds the dog declared in its manifest, which it may
        /// publish as `dog.<name>.<kind>`. Absent on the wire when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<String>,
    },
    /// A kind this build has not been taught.
    ///
    /// Only ever produced by decoding: an unrecognized `kind` falls through
    /// to this variant via `#[serde(other)]` instead of failing the whole
    /// row. Nothing constructs one to send, which is a call-site invariant
    /// rather than a type-level one: `#[serde(other)]` governs decoding
    /// only, so serializing this would emit `{"kind":"unrecognized"}`.
    #[serde(other)]
    Unrecognized,
}

impl DogSource {
    /// What the operator granted this dog, or `None` for a dog with no grants
    /// recorded, which is undeclared and keeps full access.
    #[must_use]
    pub const fn grants(&self) -> Option<&Grants> {
        match self {
            DogSource::BuiltIn { grants, .. } | DogSource::Adopted { grants, .. } => {
                grants.as_ref()
            }
            DogSource::Unrecognized => None,
        }
    }

    /// The event kinds this dog declared, which it may publish.
    #[must_use]
    pub fn events(&self) -> &[String] {
        match self {
            DogSource::BuiltIn { events, .. } | DogSource::Adopted { events, .. } => events,
            DogSource::Unrecognized => &[],
        }
    }

    /// This source with `events` in place of the kinds it carried. A kind
    /// this build has not been taught comes back as it was.
    #[must_use]
    pub fn with_events(self, events: Vec<String>) -> Self {
        match self {
            DogSource::BuiltIn { grants, .. } => DogSource::BuiltIn { grants, events },
            DogSource::Adopted {
                path,
                channel,
                grants,
                ..
            } => DogSource::Adopted {
                path,
                channel,
                grants,
                events,
            },
            DogSource::Unrecognized => DogSource::Unrecognized,
        }
    }

    /// This source with `grants` in place of the ones it carried. A kind this
    /// build has not been taught carries none and comes back as it was.
    #[must_use]
    pub fn with_grants(self, grants: Option<Grants>) -> Self {
        match self {
            DogSource::BuiltIn { events, .. } => DogSource::BuiltIn { grants, events },
            DogSource::Adopted {
                path,
                channel,
                events,
                ..
            } => DogSource::Adopted {
                path,
                channel,
                grants,
                events,
            },
            DogSource::Unrecognized => DogSource::Unrecognized,
        }
    }

    /// The kind's one-word label, `built-in`, `adopted` or `unrecognized`.
    ///
    /// Never the adopted path: this is what a SOURCE column shows, and the
    /// path is on the variant for a caller that wants it.
    const fn as_str(&self) -> &'static str {
        match self {
            DogSource::BuiltIn { .. } => "built-in",
            DogSource::Adopted { .. } => "adopted",
            DogSource::Unrecognized => "unrecognized",
        }
    }
}

/// The kind's label, for a caller rendering a source into a cell or a
/// metric value. Takes a borrow, since [`DogSource::Adopted`] owns a
/// `String` that the label does not read.
impl From<&DogSource> for &'static str {
    fn from(source: &DogSource) -> Self {
        source.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dog_source_serializes_snake_case_under_its_kind() {
        assert_eq!(
            serde_json::to_string(&DogSource::BuiltIn {
                grants: None,
                events: Vec::new()
            })
            .unwrap(),
            r#"{"kind":"built_in"}"#
        );
        let adopted = DogSource::Adopted {
            path: "/usr/local/bin/shep-otel".to_string(),
            channel: false,
            grants: None,
            events: Vec::new(),
        };
        let wire = r#"{"kind":"adopted","path":"/usr/local/bin/shep-otel"}"#;
        assert_eq!(serde_json::to_string(&adopted).unwrap(), wire);
        assert_eq!(serde_json::from_str::<DogSource>(wire).unwrap(), adopted);
    }

    #[test]
    fn with_grants_swaps_the_grants_and_keeps_the_rest() {
        let granted = Grants::default();
        let adopted = DogSource::Adopted {
            path: "/usr/local/bin/shep-otel".to_string(),
            channel: true,
            grants: None,
            events: Vec::new(),
        };

        assert_eq!(
            adopted.with_grants(Some(granted.clone())),
            DogSource::Adopted {
                path: "/usr/local/bin/shep-otel".to_string(),
                channel: true,
                grants: Some(granted.clone()),
                events: Vec::new(),
            }
        );
        assert_eq!(
            DogSource::BuiltIn {
                grants: None,
                events: Vec::new()
            }
            .with_grants(Some(granted.clone())),
            DogSource::BuiltIn {
                grants: Some(granted),
                events: Vec::new()
            }
        );
        assert_eq!(
            DogSource::Unrecognized.with_grants(None),
            DogSource::Unrecognized
        );
    }

    /// The row above is also the shape every peer before `channel` wrote,
    /// so it pins that an older peer's row still reads as no channel.
    #[test]
    fn an_adopted_dog_that_asked_for_the_channel_says_so_on_the_wire() {
        let adopted = DogSource::Adopted {
            path: "/usr/local/bin/shep-otel".to_string(),
            channel: true,
            grants: None,
            events: Vec::new(),
        };
        let wire = r#"{"kind":"adopted","path":"/usr/local/bin/shep-otel","channel":true}"#;
        assert_eq!(serde_json::to_string(&adopted).unwrap(), wire);
        assert_eq!(serde_json::from_str::<DogSource>(wire).unwrap(), adopted);
    }

    /// A kind this build has never heard of must decode, not fail the row.
    #[test]
    fn an_unknown_dog_source_decodes_as_unrecognized() {
        assert_eq!(
            serde_json::from_str::<DogSource>(r#"{"kind":"invented_next_year"}"#).unwrap(),
            DogSource::Unrecognized
        );
        let wire = r#"{"kind":"adopted","path":"/usr/local/bin/shep-otel"}"#;
        assert_eq!(
            serde_json::from_str::<DogSource>(wire).unwrap(),
            DogSource::Adopted {
                path: "/usr/local/bin/shep-otel".to_string(),
                channel: false,
                grants: None,
                events: Vec::new(),
            }
        );
    }

    /// No grants and no grants recorded are different dogs, so the wire
    /// tells them apart: an absent key is undeclared, and an empty list is a
    /// dog granted nothing.
    #[test]
    fn a_dog_granted_nothing_is_not_a_dog_with_no_grants() {
        let none = DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        };
        let empty = DogSource::BuiltIn {
            grants: Some(Grants::default()),
            events: Vec::new(),
        };
        assert_eq!(
            serde_json::to_string(&none).unwrap(),
            r#"{"kind":"built_in"}"#
        );
        let wire = r#"{"kind":"built_in","grants":[]}"#;
        assert_eq!(serde_json::to_string(&empty).unwrap(), wire);
        assert_eq!(serde_json::from_str::<DogSource>(wire).unwrap(), empty);
        assert_eq!(empty.grants(), Some(&Grants::default()));
        assert_eq!(none.grants(), None);
    }

    #[test]
    fn grants_ride_beside_an_adopted_dogs_path() {
        let adopted = DogSource::Adopted {
            path: "/usr/local/bin/shep-otel".to_string(),
            channel: false,
            grants: Some(Grants::parse(["flock.read"]).unwrap()),
            events: Vec::new(),
        };
        let wire =
            r#"{"kind":"adopted","path":"/usr/local/bin/shep-otel","grants":["flock.read"]}"#;
        assert_eq!(serde_json::to_string(&adopted).unwrap(), wire);
        assert_eq!(serde_json::from_str::<DogSource>(wire).unwrap(), adopted);
    }

    #[test]
    fn declared_events_ride_beside_the_grants_and_are_left_out_when_none() {
        let adopted = DogSource::Adopted {
            path: "/usr/local/bin/shep-otel".to_string(),
            channel: false,
            grants: None,
            events: vec!["exported".to_string()],
        };
        let wire = r#"{"kind":"adopted","path":"/usr/local/bin/shep-otel","events":["exported"]}"#;
        assert_eq!(serde_json::to_string(&adopted).unwrap(), wire);
        assert_eq!(serde_json::from_str::<DogSource>(wire).unwrap(), adopted);
        assert_eq!(adopted.events(), ["exported".to_string()]);
        assert_eq!(adopted.with_events(Vec::new()).events(), &[] as &[String]);
    }
}
