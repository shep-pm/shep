//! `shep.validate`: the check a dog runs on a settings table before shep writes it.
//!
//! A manifest that declares [`validate`](crate::dogs::manifest::Manifest::validate)
//! gets this trigger on its shepherd channel carrying [`Proposed`], the table
//! the write would put in place, before `shep set`, `shep unset` or a lookout
//! write lands. The dog answers [`Verdict`]; a refusal, an answer this cannot
//! read, and no answer at all each stop the write.
//!
//! Both ends live here so the asker in `shep-cli` and the answerer in
//! `shep_client::dogs` agree by construction, the same way the probe flags do.

use serde_json::{Map, Value};

/// The action name shep sends on the dog's shepherd channel.
///
/// A `shep-` name, reserved the way the `--version` answer's key lines are.
pub const VALIDATE_ACTION: &str = "shep.validate";

/// How much of a dog's unreadable answer is quoted back to the operator.
///
/// A refusal is the sentence the operator acts on, and the body is the only
/// thing the dog said. The cap is what keeps a dog that answers with a whole
/// file out of the error line; `shep trigger`'s own table caps at 80 for the
/// same reason, and this is the wider of the two because a refusal is the
/// whole answer here rather than one column of a table.
const QUOTE_CAP: usize = 200;

/// The table a settings write would put in place, and whose holder it is.
///
/// `Debug` prints the key count and never a value: a dog's own `dogs.toml`
/// section and a sheep's table for a dog both routinely hold credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct Proposed {
    table: Map<String, Value>,
    sheep: Option<String>,
}

impl Proposed {
    /// The table `table`, held by `sheep`, or held by the dog itself when
    /// `sheep` is `None`.
    #[must_use]
    pub fn new(table: Map<String, Value>, sheep: Option<String>) -> Self {
        Self { table, sheep }
    }

    /// The table the dog is asked about, as the write would leave it.
    #[must_use]
    pub fn table(&self) -> &Map<String, Value> {
        &self.table
    }

    /// The sheep whose table this is, and `None` when it is the dog's own
    /// section.
    #[must_use]
    pub fn sheep(&self) -> Option<&str> {
        self.sheep.as_deref()
    }

    /// The `params` text shep puts on the channel.
    ///
    /// Compact rather than pretty: `params` is one opaque string on the wire,
    /// and a dog reading it by hand in a shell gets one line.
    #[must_use]
    pub fn params(&self) -> String {
        let mut params = Map::new();
        params.insert("table".to_string(), Value::Object(self.table.clone()));
        if let Some(sheep) = &self.sheep {
            params.insert("sheep".to_string(), Value::String(sheep.clone()));
        }
        Value::Object(params).to_string()
    }

    /// Reads the `params` text a dog was sent.
    ///
    /// [`None`] for anything that is not what [`Self::params`] writes: no
    /// JSON, no object, no `table` object, or a `sheep` that is not a string.
    /// A dog answers a refusal for those rather than guessing at a table.
    #[must_use]
    pub fn parse(params: &str) -> Option<Self> {
        let Value::Object(params) = serde_json::from_str(params).ok()? else {
            return None;
        };
        let Value::Object(table) = params.get("table")? else {
            return None;
        };
        let sheep = match params.get("sheep") {
            Some(Value::String(sheep)) => Some(sheep.clone()),
            Some(_other) => return None,
            None => None,
        };
        Some(Self {
            table: table.clone(),
            sheep,
        })
    }
}

impl core::fmt::Debug for Proposed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let n = self.table.len();
        f.debug_struct("Proposed")
            .field(
                "table",
                &format_args!("<{n} key{}>", if n == 1 { "" } else { "s" }),
            )
            .field("sheep", &self.sheep)
            .finish()
    }
}

/// A dog's answer to a [`VALIDATE_ACTION`] trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The dog accepts the table as proposed.
    Accept,
    /// The dog refuses it. `reason` is the sentence the operator reads.
    Refuse {
        /// Why the dog will not accept this table.
        reason: String,
    },
}

impl Verdict {
    /// A refusal, with the sentence the operator reads.
    #[must_use]
    pub fn refuse(reason: impl Into<String>) -> Self {
        Self::Refuse {
            reason: reason.into(),
        }
    }

    /// The reply body a dog writes back on its channel.
    #[must_use]
    pub fn body(&self) -> String {
        let mut body = Map::new();
        match self {
            Self::Accept => {
                body.insert("accept".to_string(), Value::Bool(true));
            }
            Self::Refuse { reason } => {
                body.insert("accept".to_string(), Value::Bool(false));
                body.insert("reason".to_string(), Value::String(reason.clone()));
            }
        }
        Value::Object(body).to_string()
    }

    /// What a reply body says.
    ///
    /// Anything this cannot read is a refusal, and the body is quoted as the
    /// reason: the write is gated on the dog answering, and a dog that
    /// answers with something that is not an answer has not accepted it.
    #[must_use]
    pub fn read(body: &str) -> Self {
        let Ok(Value::Object(body)) = serde_json::from_str(body) else {
            return Self::refuse(unreadable(body));
        };
        match body.get("accept") {
            Some(Value::Bool(true)) => Self::Accept,
            Some(Value::Bool(false)) => match body.get("reason") {
                Some(Value::String(reason)) if !reason.trim().is_empty() => {
                    Self::refuse(reason.clone())
                }
                _ => Self::refuse("the dog refused the write without saying why"),
            },
            _ => Self::refuse(unreadable(&Value::Object(body).to_string())),
        }
    }
}

/// The reason given for an answer that is not a verdict.
///
/// Shared with [`pane`](super::pane), which quotes an unreadable pane answer
/// the same way rather than writing its own cap.
pub(super) fn unreadable(body: &str) -> String {
    // Cut on a character boundary rather than a byte index: a dog answering in
    // another script still comes back whole up to the cap, and never as a
    // prefix that is not a string.
    let mut cut = QUOTE_CAP.min(body.len());
    while !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let quoted = if cut < body.len() {
        format!("{}...", &body[..cut])
    } else {
        body.to_string()
    };
    format!("the dog answered with something shep cannot read: {quoted}")
}

#[cfg(test)]
mod tests;
