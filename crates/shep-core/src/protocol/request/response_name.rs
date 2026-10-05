//! [`Response::name`]: which answer arrived, for an error that must not
//! print its body.

use super::Response;

impl Response {
    /// The variant's name, never its body, for an error saying which answer
    /// arrived.
    ///
    /// A listing would flood the message, and [`Self::DogSection`] carries
    /// credentials. Exhaustive here, so a new variant names itself.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Pong => "Pong",
            Self::Flock(_) => "Flock",
            Self::HostUsage(_) => "HostUsage",
            Self::Described(_) => "Described",
            Self::Started(_) => "Started",
            Self::Added(_) => "Added",
            Self::Drifted(_) => "Drifted",
            Self::Applied(_) => "Applied",
            Self::SheepConfig(_) => "SheepConfig",
            Self::SheepEnvSet { .. } => "SheepEnvSet",
            Self::SheepEnvBatch { .. } => "SheepEnvBatch",
            Self::SheepFieldSet { .. } => "SheepFieldSet",
            Self::DogSheepSettings { .. } => "DogSheepSettings",
            Self::SheepDogSettingsSet { .. } => "SheepDogSettingsSet",
            Self::DogConfigSet { .. } => "DogConfigSet",
            Self::SecretsPut { .. } => "SecretsPut",
            Self::Stopped(_) => "Stopped",
            Self::Restarted { .. } => "Restarted",
            Self::Reloading { .. } => "Reloading",
            Self::Scaled(_) => "Scaled",
            Self::SmitPainted(_) => "SmitPainted",
            Self::Deleted(_) => "Deleted",
            Self::Reopened(_) => "Reopened",
            Self::Flushed(_) => "Flushed",
            Self::Triggered(_) => "Triggered",
            Self::Signalled(_) => "Signalled",
            Self::SentLine(_) => "SentLine",
            Self::Answered { .. } => "Answered",
            Self::RollSaved { .. } => "RollSaved",
            Self::Mustered(_) => "Mustered",
            Self::DogSection { .. } => "DogSection",
            Self::DogStarted(_) => "DogStarted",
            Self::DogStaleness { .. } => "DogStaleness",
            Self::HandoverFitness { .. } => "HandoverFitness",
            Self::Subscribed => "Subscribed",
            Self::ShuttingDown => "ShuttingDown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::process::sample_info;
    use super::*;

    /// An error names the variant and a captured frame names the tag, so the
    /// two have to agree. A sample of shapes: unit, listing, struct, secret.
    #[test]
    fn a_responses_name_is_its_wire_tag_in_the_variants_spelling() {
        let responses = [
            Response::Pong,
            Response::Flock(vec![sample_info()]),
            Response::HostUsage(None),
            Response::RollSaved {
                path: "/home/ada/.shep/flock.json".to_string(),
                apps: 2,
            },
            Response::DogSection {
                toml: "webhook = \"https://example.invalid/hook\"\n"
                    .to_string()
                    .into(),
            },
            Response::HandoverFitness { refusal: None },
            Response::ShuttingDown,
        ];
        for response in responses {
            let wire = serde_json::to_value(&response).unwrap();
            let snake: String = response
                .name()
                .chars()
                .enumerate()
                .flat_map(|(at, c)| {
                    let gap = (at > 0 && c.is_ascii_uppercase()).then_some('_');
                    gap.into_iter().chain(c.to_lowercase())
                })
                .collect();
            assert_eq!(wire["kind"], snake.as_str(), "{}", response.name());
        }
    }
}
