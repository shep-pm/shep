//! A copy that would put a dog-wide key into a sheep's table is refused.

use shep_core::paths::ShepPaths;

use super::address::{self, ConfigPath};
use super::{Address, CopyRefusal};
use crate::commands::settings_write;

/// The refusal for a copy that would put a dog-wide key into `target`'s table
/// for a dog that follows the settings standard: the key is the dog's section's,
/// so no sheep's table holds its own.
///
/// Only the keys the copy takes are looked at, so a stale dog-wide key the
/// target already holds does not stop a copy of another one.
pub(super) fn refusal(
    paths: &ShepPaths,
    from: &Address,
    target: &str,
    except: &[Vec<String>],
    source: &serde_json::Value,
) -> Option<CopyRefusal> {
    let ConfigPath::Dog { dog, keys } = &from.path else {
        return None;
    };
    let (_, standard) = settings_write::dog_standard(dog, paths)?;
    let copied = match (keys.is_empty(), address::dog_table(source, dog)) {
        (true, Some(serde_json::Value::Object(table))) => serde_json::Value::Object(
            table
                .iter()
                .filter(|(key, _)| {
                    !except
                        .iter()
                        .any(|path| path.as_slice() == [(*key).clone()])
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        _ => serde_json::Value::Null,
    };
    settings_write::dog_wide::dog_wide_refusal(&standard, target, dog, keys, &copied)
        .map(|refusal| CopyRefusal::Refused(refusal.to_string()))
}
