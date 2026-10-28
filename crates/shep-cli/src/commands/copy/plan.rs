//! What a copy would write, and every key it changes on the target.
//!
//! The plan is built entirely from the two configs this command already read,
//! so the operator sees the whole change list before a byte is written, and
//! the write is one request the shepherd already answers.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use shep_core::config::DogTable;

use super::{CopyRefusal, address};

/// The request a plan turns into.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Write {
    /// One `AppConfig` field, through `Request::SetSheepField`.
    Field {
        /// The field name.
        name: String,
        /// The value the source holds for it.
        value: Value,
    },
    /// One dog's whole table, through `Request::SetSheepDogSettings`.
    ///
    /// The whole table and not the copied slice: that request replaces a
    /// dog's table, so the merged result is what carries the keys the copy
    /// left alone.
    DogTable {
        /// The dog's name.
        dog: String,
        /// The table the target holds afterwards.
        table: DogTable,
    },
    /// The source's whole env, through `Request::CopySheepEnv`.
    ///
    /// The shepherd reads both envs and merges them, so the write carries the
    /// source's name, the flags and the change list the operator reviewed,
    /// and never a value.
    Env {
        /// The sheep the shepherd reads the env from.
        from: String,
        /// Whether the target ends up with exactly the source's keys.
        replace: bool,
        /// Keys the copy leaves as the target had them.
        except: Vec<String>,
        /// The change list the operator is shown, sent back so the shepherd
        /// refuses a write whose own list differs.
        expect: Vec<shep_core::protocol::EnvCopyChange>,
    },
}

/// What one key ends up as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    /// The target had no such key.
    Added,
    /// The target had one, holding something else.
    Changed,
    /// The target had one and the copy leaves it out.
    Removed,
}

impl ChangeKind {
    /// The word the table column and the JSON field carry.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Changed => "changed",
            Self::Removed => "removed",
        }
    }
}

/// One key the copy touches.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Change {
    /// The key, dotted, under the path the copy names.
    pub(crate) key: String,
    /// What happens to it.
    pub(crate) kind: ChangeKind,
    /// What the target held, `None` for an added key.
    pub(crate) old: Option<Value>,
    /// What the target holds afterwards, `None` for a removed key.
    pub(crate) new: Option<Value>,
}

/// The whole copy: what changes, and what is sent for it.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Plan {
    /// One entry per key the copy touches, in dotted-key order.
    pub(crate) changes: Vec<Change>,
    /// The write, `None` when nothing differs and there is nothing to send.
    pub(crate) write: Option<Write>,
}

/// The plan for one `AppConfig` field.
///
/// `source` and `target` are the two sheep's configs as JSON objects.
#[must_use]
pub(crate) fn plan_field(field: &str, source: &Value, target: &Value) -> Plan {
    let Some(value) = source.get(field) else {
        return Plan::default();
    };
    let current = target.get(field);
    if current == Some(value) {
        return Plan::default();
    }
    Plan {
        changes: vec![Change {
            key: field.to_string(),
            kind: ChangeKind::Changed,
            old: current.cloned(),
            new: Some(value.clone()),
        }],
        write: Some(Write::Field {
            name: field.to_string(),
            value: value.clone(),
        }),
    }
}

/// The two sheep one copy runs between, for the messages that name a path.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sides<'a> {
    /// The sheep the values come from.
    pub(crate) source: &'a str,
    /// The sheep the values land on.
    pub(crate) target: &'a str,
}

/// The plan for one dog's table, or one key inside it.
///
/// `keys` is the path under the dog's table, empty for the whole table.
/// `except` names keys under that same path.
///
/// # Errors
/// [`CopyRefusal::MissingSource`] when the source sheep has no table for the
/// dog, or no key at `keys`. [`CopyRefusal::Refused`] when a side holds
/// something that is not a table where the copy has to write through one.
/// [`CopyRefusal::Usage`] for an `--except` that descends into a value the
/// source does not hold as a table.
pub(crate) fn plan_dog(
    sides: Sides<'_>,
    dog: &str,
    keys: &[String],
    except: &[Vec<String>],
    replace: bool,
    source: &Value,
    target: &Value,
) -> Result<Plan, CopyRefusal> {
    let Sides {
        source: from,
        target: to,
    } = sides;
    let source_table = address::dog_table(source, dog);
    let source_here = if keys.is_empty() {
        source_table
    } else {
        source_table.and_then(|table| walk(table, keys))
    };
    let Some(source_here) = source_here else {
        return Err(if source_table.is_none() {
            CopyRefusal::MissingSource(format!(
                "no `{}.dogs.{dog}`: that sheep has no table for the dog `{dog}`",
                from
            ))
        } else {
            CopyRefusal::MissingSource(format!(
                "no `{}` under `{}.dogs.{dog}`",
                keys.join("."),
                from
            ))
        });
    };
    let before = Value::Object(
        address::dog_table(target, dog)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    );
    let target_here: Option<&Value> = if keys.is_empty() {
        Some(&before)
    } else {
        walk(&before, keys)
    };
    // An `--except` is read the way the address was: a key holding a dot is
    // one key when the table it names holds it, so `--except api.token` leaves
    // out the key `api.token` rather than the key `token` under `api`.
    let except: Vec<Vec<String>> = except
        .iter()
        .map(|spec| address::resolve_keys(spec, Some(source_here), target_here))
        .collect();
    check_except(source_here, &format!("{from}.dogs.{dog}"), &except)?;
    let mut after = before.clone();
    if keys.is_empty() {
        after = merge(Some(&after), source_here, replace, &except)?;
    } else {
        let merged = merge(walk(&after, keys), source_here, replace, &except)?;
        set_path(&mut after, keys, merged).map_err(|held| {
            let at = if held.is_empty() {
                format!("{to}.dogs.{dog}")
            } else {
                format!("{to}.dogs.{dog}.{held}")
            };
            CopyRefusal::Refused(format!(
                "`{at}` holds a value that is not a table, and a copy onto `{}` would replace it",
                keys.join(".")
            ))
        })?;
    }

    // A dog table is a table by construction, so this is the shape check that
    // keeps a plan from naming changes it cannot write. `Write::DogTable` is
    // the whole table, and a whole table that is not one is refused here
    // rather than turned into a write of nothing later.
    let Some(table) = after.as_object().cloned() else {
        return Err(CopyRefusal::Refused(format!(
            "`{from}.dogs.{dog}` holds a value that is not a table, so there is no table to copy \
             onto `{to}`"
        )));
    };

    let before_here: Option<&Value> = if keys.is_empty() {
        Some(&before)
    } else {
        walk(&before, keys)
    };
    let after_here: Option<&Value> = if keys.is_empty() {
        Some(&after)
    } else {
        walk(&after, keys)
    };

    let mut changes = Vec::new();
    collect_changes(before_here, after_here, "", &mut changes);
    if let Some(last) = keys.last() {
        // The copied path itself is not a table on both sides, so the diff
        // reports it as one change. It is named by the path the operator
        // typed, not by the blank its children are named under.
        for change in &mut changes {
            if change.key.is_empty() {
                change.key = last.clone();
            }
        }
    }

    let write = (!changes.is_empty()).then(|| Write::DogTable {
        dog: dog.to_string(),
        table: DogTable::from(table),
    });

    Ok(Plan { changes, write })
}

/// The `--except` arguments, as key paths under the copied path.
///
/// # Errors
/// [`CopyRefusal::Usage`] for a spec with an empty part, which cannot name a
/// key.
pub(crate) fn except_paths(raw: &[String]) -> Result<Vec<Vec<String>>, CopyRefusal> {
    raw.iter()
        .map(|spec| {
            let parts: Vec<&str> = spec.split('.').collect();
            if parts.iter().any(|part| part.is_empty()) {
                return Err(CopyRefusal::Usage(format!(
                    "`--except {spec}` has an empty part: a key is named as `git.checkout`, not \
                     as a pattern"
                )));
            }
            Ok(parts.into_iter().map(str::to_string).collect())
        })
        .collect()
}

/// The value at `keys` under `root`, `None` when any step is missing or is
/// not a table.
///
/// `pub(crate)`: `shep set` reads the old value at the same path `copy` walks
/// (issue #1096), and one walk is what keeps the two verbs naming one key.
pub(crate) fn walk<'a>(root: &'a Value, keys: &[String]) -> Option<&'a Value> {
    let mut node = root;
    for key in keys {
        node = node.as_object()?.get(key)?;
    }
    Some(node)
}

/// Writes `value` at `keys` under `root`, creating the tables on the way.
///
/// `Err(held)` names the dotted path, under `root`, of the value this write
/// would have to turn into a table: a change to that value, which the caller
/// reports rather than making on the way to the key it was asked to write.
///
/// `pub(crate)`: `shep set` writes through the same rule.
pub(crate) fn set_path(root: &mut Value, keys: &[String], value: Value) -> Result<(), String> {
    write_at(root, keys, "", value)
}

/// [`set_path`] with the path it has walked so far, for the error's name.
fn write_at(root: &mut Value, keys: &[String], held: &str, value: Value) -> Result<(), String> {
    let Some((head, rest)) = keys.split_first() else {
        *root = value;
        return Ok(());
    };
    let Some(map) = root.as_object_mut() else {
        return Err(held.to_string());
    };
    if !map.contains_key(head) {
        map.insert(head.clone(), Value::Object(Map::new()));
    }
    let Some(child) = map.get_mut(head) else {
        return Err(held.to_string());
    };
    let walked = if held.is_empty() {
        head.clone()
    } else {
        format!("{held}.{head}")
    };
    write_at(child, rest, &walked, value)
}

/// Whether every step an `--except` descends through is a table on the source
/// side.
///
/// An `--except` prunes the value it names out of what the copy brings over.
/// A scalar or an array at one of its steps holds no key to prune, so the
/// copy would overwrite that value whole while reporting only the excepted
/// key, which is the opposite of what the flag says.
///
/// `path` is the copied path, for the message.
///
/// # Errors
/// [`CopyRefusal::Usage`] naming the spec and the value it stops at.
fn check_except(
    source_here: &Value,
    path: &str,
    except: &[Vec<String>],
) -> Result<(), CopyRefusal> {
    for spec in except {
        let mut node = source_here;
        let mut walked: Vec<&str> = Vec::new();
        for key in &spec[..spec.len() - 1] {
            let next = node.as_object().and_then(|map| map.get(key));
            match next {
                Some(child) => {
                    node = child;
                    walked.push(key);
                }
                // The source says nothing at this step, so it has nothing to
                // prune here either, and the target's own value is what
                // reaches the copy.
                None => break,
            }
        }
        if !node.is_object() {
            let at = if walked.is_empty() {
                path.to_string()
            } else {
                format!("{path}.{}", walked.join("."))
            };
            return Err(CopyRefusal::Usage(format!(
                "`--except {}` cannot be honored: `{at}` holds a value that is not a table",
                spec.join(".")
            )));
        }
    }
    Ok(())
}

/// Removes `keys` from `root`, if it is there.
fn remove_path(root: &mut Value, keys: &[String]) {
    let Some((head, rest)) = keys.split_first() else {
        return;
    };
    let Some(map) = root.as_object_mut() else {
        return;
    };
    if rest.is_empty() {
        map.remove(head);
        return;
    }
    if let Some(child) = map.get_mut(head) {
        remove_path(child, rest);
    }
}

/// The table the target ends up with at the copied path.
///
/// Merged by default: a table keeps the target's own keys, and a table on
/// both sides merges key by key so a `--except` deep inside one is honoured.
/// A scalar or an array in the source replaces the target's entry whole.
/// `replace` starts from the source instead of the target.
///
/// `except` are paths under this node. One that names this node's own key is
/// handled by the caller that recursed here.
///
/// # Errors
/// Whatever [`set_path`] refuses for a `--except` restoration under `replace`.
fn merge(
    target: Option<&Value>,
    source: &Value,
    replace: bool,
    except: &[Vec<String>],
) -> Result<Value, CopyRefusal> {
    if replace {
        let mut out = source.clone();
        for path in except {
            remove_path(&mut out, path);
            if let Some(value) = target.and_then(|held| walk(held, path)) {
                set_path(&mut out, path, value.clone()).map_err(|held| {
                    CopyRefusal::Refused(format!(
                        "`{held}` under the copied path holds a value that is not a table, so \
                         `--except {}` has nowhere to put the target's own value back",
                        path.join(".")
                    ))
                })?;
            }
        }
        return Ok(out);
    }
    let Some(source_map) = source.as_object() else {
        // The source's value here is one value rather than a table, and it
        // replaces whatever the target holds. An `--except` descending into it
        // was refused by `check_except`, so nothing is being dropped here.
        return Ok(source.clone());
    };
    // A target holding a scalar here contributes no keys to keep, and a
    // target holding nothing at all still has its keys pruned out of the
    // source's table rather than copied in beside them.
    let empty = Map::new();
    let target_map = target.and_then(Value::as_object).unwrap_or(&empty);
    let mut out = target_map.clone();
    for (key, value) in source_map {
        let nested = nested_excepts(except, key);
        if nested.iter().any(Vec::is_empty) {
            continue;
        }
        out.insert(
            key.clone(),
            merge(target_map.get(key), value, false, &nested)?,
        );
    }
    Ok(Value::Object(out))
}

/// The `--except` paths that live under `key`, with `key` off the front.
fn nested_excepts(except: &[Vec<String>], key: &str) -> Vec<Vec<String>> {
    except
        .iter()
        .filter(|path| path.first().map(String::as_str) == Some(key))
        .map(|path| path[1..].to_vec())
        .collect()
}

/// Every key that differs between `before` and `after`, as dotted paths under
/// `key`. A key holding the same value on both sides is no change.
///
/// `pub(crate)`: `shep set` prints the same old-and-new list (issue #1096).
pub(crate) fn collect_changes(
    before: Option<&Value>,
    after: Option<&Value>,
    key: &str,
    out: &mut Vec<Change>,
) {
    match (before, after) {
        (Some(Value::Object(before)), Some(Value::Object(after))) => {
            let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
            for name in keys {
                let child = child_key(key, name);
                collect_changes(before.get(name), after.get(name), &child, out);
            }
        }
        (Some(Value::Object(before)), None) => {
            for name in before.keys() {
                let child = child_key(key, name);
                collect_changes(Some(&before[name]), None, &child, out);
            }
        }
        (None, Some(Value::Object(after))) => {
            for name in after.keys() {
                let child = child_key(key, name);
                collect_changes(None, Some(&after[name]), &child, out);
            }
        }
        (Some(before), Some(after)) => {
            if before != after {
                out.push(Change {
                    key: key.to_string(),
                    kind: ChangeKind::Changed,
                    old: Some(before.clone()),
                    new: Some(after.clone()),
                });
            }
        }
        (Some(before), None) => out.push(Change {
            key: key.to_string(),
            kind: ChangeKind::Removed,
            old: Some(before.clone()),
            new: None,
        }),
        (None, Some(after)) => out.push(Change {
            key: key.to_string(),
            kind: ChangeKind::Added,
            old: None,
            new: Some(after.clone()),
        }),
        (None, None) => {}
    }
}

/// A child key under `key`, which is bare at the copied path itself.
fn child_key(key: &str, name: &str) -> String {
    if key.is_empty() {
        name.to_string()
    } else {
        format!("{key}.{name}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn table(pairs: &[(&str, Value)]) -> Value {
        let mut map = Map::new();
        for (key, value) in pairs {
            map.insert((*key).to_string(), value.clone());
        }
        json!({ "dogs": { "kelpie": map } })
    }

    /// The two sheep every plan test names.
    fn sides() -> Sides<'static> {
        Sides {
            source: "shep",
            target: "api",
        }
    }

    /// The keys a plan reports are the keys under the path the operator named.
    #[test]
    fn a_deep_path_reports_its_keys_under_itself() {
        let source = json!({ "dogs": { "kelpie": { "rulings": { "panel": "panel" } } } });
        let target = json!({ "dogs": { "kelpie": { "rulings": { "panel": "issue" } } } });
        let plan = plan_dog(
            sides(),
            "kelpie",
            &["rulings".to_string()],
            &[],
            false,
            &source,
            &target,
        )
        .unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].key, "panel");
    }

    /// A copied path that is a table on one side and not on the other is one
    /// change, and it carries the name the operator typed.
    #[test]
    fn a_path_that_stops_being_a_table_is_named_by_that_path() {
        let source = json!({ "dogs": { "kelpie": { "rulings": { "decide": "panel" } } } });
        let target = json!({ "dogs": { "kelpie": { "rulings": "yes" } } });
        let plan = plan_dog(
            sides(),
            "kelpie",
            &["rulings".to_string()],
            &[],
            false,
            &source,
            &target,
        )
        .unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(
            plan.changes[0].key, "rulings",
            "the change list names the path it replaces rather than a blank"
        );
        assert_eq!(plan.changes[0].old, Some(json!("yes")));
        assert_eq!(plan.changes[0].new, Some(json!({ "decide": "panel" })));

        let other = plan_dog(
            sides(),
            "kelpie",
            &["rulings".to_string()],
            &[],
            false,
            &target,
            &source,
        )
        .unwrap();
        assert_eq!(other.changes[0].key, "rulings");
        assert_eq!(other.changes[0].kind, ChangeKind::Changed);
    }

    /// A copy that would have to write through a value that is not a table is
    /// refused, not made.
    #[test]
    fn a_copy_into_a_value_that_is_not_a_table_is_refused() {
        let source = json!({ "dogs": { "kelpie": { "rulings": { "panel": "panel" } } } });
        let target = json!({ "dogs": { "kelpie": { "rulings": "yes" } } });
        let err = plan_dog(
            sides(),
            "kelpie",
            &["rulings".to_string(), "panel".to_string()],
            &[],
            false,
            &source,
            &target,
        )
        .expect_err("`rulings` is a scalar on the target");
        assert_eq!(
            err,
            CopyRefusal::Refused(
                "`api.dogs.kelpie.rulings` holds a value that is not a table, and a copy onto \
                 `rulings.panel` would replace it"
                    .to_string()
            ),
            "{err}"
        );
    }

    /// An `--except` that descends into a value the source does not hold as a
    /// table cannot be honored, and saying so beats overwriting it quietly.
    #[test]
    fn an_except_that_descends_into_a_non_table_is_refused() {
        let source = json!({ "dogs": { "kelpie": { "git": "origin" } } });
        let target = json!({ "dogs": { "kelpie": { "git": { "checkout": "./mine" } } } });
        let except = [vec!["git".to_string(), "checkout".to_string()]];
        let err = plan_dog(sides(), "kelpie", &[], &except, false, &source, &target)
            .expect_err("the source's `git` is a string");
        assert_eq!(
            err,
            CopyRefusal::Usage(
                "`--except git.checkout` cannot be honored: `shep.dogs.kelpie.git` holds a value \
                 that is not a table"
                    .to_string()
            ),
            "{err}"
        );
    }

    /// An `--except` whose parent the target lacks still prunes the key out of
    /// what the source brings over.
    #[test]
    fn an_except_prunes_a_key_from_a_table_the_target_does_not_have() {
        let source = table(&[("git", json!({ "checkout": "./src", "remote": "origin" }))]);
        let target = table(&[("watch", json!(true))]);
        let except = [vec!["git".to_string(), "checkout".to_string()]];
        let plan = plan_dog(sides(), "kelpie", &[], &except, false, &source, &target).unwrap();
        let Write::DogTable { table, .. } = plan.write.unwrap() else {
            panic!("a dog copy writes a dog table");
        };
        assert_eq!(
            table.as_map()["git"],
            json!({ "remote": "origin" }),
            "`git.checkout` is not copied in just because the target has no `git`"
        );
        assert_eq!(table.as_map()["watch"], json!(true));
    }

    #[test]
    fn a_nested_table_merges_and_an_array_overwrites_whole() {
        let source =
            json!({ "dogs": { "kelpie": { "limits": { "a": 1, "b": 2 }, "tags": ["x", "y"] } } });
        let target =
            json!({ "dogs": { "kelpie": { "limits": { "b": 9, "c": 3 }, "tags": ["z"] } } });
        let plan = plan_dog(sides(), "kelpie", &[], &[], false, &source, &target).unwrap();
        let keys: Vec<&str> = plan.changes.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["limits.a", "limits.b", "tags"]);
        let Write::DogTable { table, .. } = plan.write.unwrap() else {
            panic!("a dog copy writes a dog table");
        };
        assert_eq!(
            table.as_map()["limits"],
            json!({ "a": 1, "b": 2, "c": 3 }),
            "a table on both sides merges key by key, so `c` survives"
        );
        assert_eq!(
            table.as_map()["tags"],
            json!(["x", "y"]),
            "an array is one value, not a table"
        );
    }

    #[test]
    fn a_table_merges_key_by_key_and_keeps_what_only_the_target_has() {
        let source = table(&[("git", json!({ "checkout": "./src", "remote": "origin" }))]);
        let target = table(&[("git", json!({ "checkout": "./other", "own": "keep" }))]);
        let plan = plan_dog(sides(), "kelpie", &[], &[], false, &source, &target).unwrap();
        let keys: Vec<&str> = plan.changes.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["git.checkout", "git.remote"]);
        let Write::DogTable { table, .. } = plan.write.unwrap() else {
            panic!("a dog copy writes a dog table");
        };
        assert_eq!(
            table.as_map()["git"],
            json!({ "checkout": "./src", "own": "keep", "remote": "origin" })
        );
    }

    #[test]
    fn an_excepted_key_is_pruned_from_the_source_and_restored_under_replace() {
        let source = table(&[("git", json!({ "checkout": "./src", "remote": "origin" }))]);
        let target = table(&[("git", json!({ "checkout": "./other" }))]);
        let except = [vec!["git".to_string(), "checkout".to_string()]];

        let merged = plan_dog(sides(), "kelpie", &[], &except, false, &source, &target).unwrap();
        let Write::DogTable { table, .. } = merged.write.unwrap() else {
            panic!("a dog copy writes a dog table");
        };
        assert_eq!(
            table.as_map()["git"],
            json!({ "checkout": "./other", "remote": "origin" })
        );

        let replaced = plan_dog(sides(), "kelpie", &[], &except, true, &source, &target).unwrap();
        let Write::DogTable { table, .. } = replaced.write.unwrap() else {
            panic!("a dog copy writes a dog table");
        };
        assert_eq!(
            table.as_map()["git"],
            json!({ "checkout": "./other", "remote": "origin" })
        );
    }

    /// `--except` reads a dotted key the way the address does, so it leaves
    /// out the key the operator meant and not the one a segment split lands
    /// on.
    #[test]
    fn an_except_leaves_out_the_key_the_table_holds() {
        let source = table(&[("api.token", json!("src")), ("api", json!({ "limit": 5 }))]);
        let target = table(&[("api.token", json!("mine")), ("api", json!({ "limit": 9 }))]);
        let except = [vec!["api".to_string(), "token".to_string()]];
        let plan = plan_dog(sides(), "kelpie", &[], &except, false, &source, &target).unwrap();
        let Write::DogTable { table, .. } = plan.write.unwrap() else {
            panic!("a dog copy writes a dog table");
        };
        assert_eq!(
            table.as_map()["api.token"],
            json!("mine"),
            "the table holds `api.token` as one key, so that is the key left out"
        );
        assert_eq!(
            table.as_map()["api"],
            json!({ "limit": 5 }),
            "and the key under `api` is copied, which is what the operator did not except"
        );
        let keys: Vec<&str> = plan.changes.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["api.limit"]);
    }

    #[test]
    fn a_missing_except_part_is_a_usage_error() {
        let err = except_paths(&["git..remote".to_string()]).unwrap_err();
        assert!(err.message().contains("--except git..remote"), "{err}");
    }

    #[test]
    fn a_field_that_already_matches_needs_no_write() {
        let plan = plan_field(
            "restart_delay",
            &json!({ "restart_delay": "3s" }),
            &json!({ "restart_delay": "3s" }),
        );
        assert!(plan.changes.is_empty());
        assert_eq!(plan.write, None);
    }
}
