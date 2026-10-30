//! `shep set` into a `dogs.toml` section that spells a table inline, the form a
//! hand-written file often uses: the parent stays inline and a new key lands
//! inside it.

use serde_json::json;

use super::*;
use crate::commands::copy::plan::ChangeKind;

/// A section that spells a table inline, which is the form a hand-written
/// `dogs.toml` often uses and the one `set_toml` used to refuse walking.
const INLINE_SECTION: &str = "\
repo = \"/srv/project\"
worker = { timeout = 30, allowed_domains = [\"github.com\"] }
fetch = { git = { merging = \"ff\" } }
";

/// The plan for one key inside [`INLINE_SECTION`].
fn inline_plan(keys: &[&str], value: Json) -> Result<Plan, WriteRefusal> {
    let doc = INLINE_SECTION.parse::<toml_edit::DocumentMut>().unwrap();
    plan_section(
        doc,
        "kelpie",
        &keys.iter().map(|key| key.to_string()).collect::<Vec<_>>(),
        &value,
    )
}

/// A key under an inline table is written, and the parent stays an inline
/// table: the form the section spells a table in is the form it keeps.
#[test]
fn a_key_under_an_inline_table_is_written_inline() {
    let plan = inline_plan(&["worker", "timeout"], json!(45)).unwrap();
    let Write::DogSection { toml, .. } = &plan.write else {
        unreachable!()
    };
    assert!(toml.contains("worker = { timeout = 45"), "{toml}");
    assert!(
        !toml.contains("[worker]"),
        "{toml}: an inline table does not become a header"
    );
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].key, "timeout");
    assert_eq!(plan.changes[0].old, Some(json!(30)));
    assert_eq!(plan.changes[0].new, Some(json!(45)));

    // A key the inline table does not hold yet lands inside it rather than
    // beside it.
    let plan = inline_plan(&["worker", "retries"], json!(3)).unwrap();
    let Write::DogSection { toml, .. } = &plan.write else {
        unreachable!()
    };
    assert!(toml.contains("worker = {"), "{toml}");
    assert!(toml.contains("retries = 3"), "{toml}");
    assert_eq!(plan.changes[0].kind, ChangeKind::Added);
}

/// Two levels down, where the parent inline table holds an inline table.
#[test]
fn a_key_under_a_nested_inline_table_is_written() {
    let plan = inline_plan(&["fetch", "git", "merging"], json!("rebase")).unwrap();
    let Write::DogSection { toml, .. } = &plan.write else {
        unreachable!()
    };
    assert!(
        toml.contains("fetch = { git = { merging = \"rebase\" } }"),
        "{toml}"
    );
    assert_eq!(plan.changes[0].old, Some(json!("ff")));

    // The scalar in the way is still a refusal, and it names the scalar.
    let error =
        inline_plan(&["worker", "timeout", "deep"], json!(1)).expect_err("an integer holds no key");
    assert!(
        error
            .to_string()
            .contains("`kelpie.worker.timeout` holds a value that is not a table"),
        "{error}"
    );
}
