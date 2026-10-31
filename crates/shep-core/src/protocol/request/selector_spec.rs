//! [`SelectorSpec`], the wire form of a verb's sheep selector.

use serde::{Deserialize, Serialize};

/// Serializable selector (mirror of [`crate::selector::ProcessSelector`];
/// regex travels as its source string)
// wire format: changing this is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SelectorSpec {
    /// Every sheep
    All,
    /// By id
    Id(u32),
    /// By exact name
    Name(String),
    /// By regex source
    Regex(String),
    /// By fold name
    Fold(String),
    // Both field names are wire contract, pinned by `request_wire_v14`.
    /// By app name and instance slot
    ///
    /// On the wire: `{"kind":"instance","value":{"name":"web","slot":2}}`.
    Instance {
        /// The app name
        name: String,
        /// The instance slot, counting from 0
        slot: u32,
    },
}
