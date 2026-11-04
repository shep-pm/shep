//! What a dog's page reads from its receipt.

use super::*;

/// Write a receipt for `kelpie` naming `package` at `version`.
fn receipt_for_kelpie(paths: &ShepPaths, package: &str, version: &str) {
    let receipt = paths.dogs_dir.join("kelpie").join("receipt.json");
    std::fs::create_dir_all(receipt.parent().unwrap()).unwrap();
    std::fs::write(
        &receipt,
        serde_json::json!({
            "package": package,
            "row": { "repo": "https://github.com/shep-pm/shep-kelpie" },
            "version": version,
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn an_installed_dog_names_its_version_and_the_receipts_source() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("kelpie", KELPIE_BODY)]);
    receipt_for_kelpie(&paths, "shep-kelpie", "0.9.2");

    let page = page(&paths, dir.path(), "kelpie");

    assert!(
        page.ends_with(
            "\nkelpie 0.9.2 from shep-pm/shep-kelpie. Anything after `shep kelpie` goes to the dog.\n"
        ),
        "{page}"
    );
}

#[test]
fn a_binary_that_is_not_the_receipts_version_gets_no_source() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("kelpie", KELPIE_BODY)]);
    receipt_for_kelpie(&paths, "shep-kelpie", "0.9.1");

    let page = page(&paths, dir.path(), "kelpie");

    assert!(
        page.ends_with("\nkelpie 0.9.2. Anything after `shep kelpie` goes to the dog.\n"),
        "{page}"
    );
}

#[test]
fn a_package_the_index_dropped_gets_no_other_rows_description() {
    let dir = tempfile::tempdir().unwrap();
    let paths = a_home_with(dir.path(), [("log-rotate", VERSIONED_BODY)]);
    let receipt = paths.dogs_dir.join("log-rotate").join("receipt.json");
    std::fs::create_dir_all(receipt.parent().unwrap()).unwrap();
    std::fs::write(
        &receipt,
        serde_json::json!({
            "package": "acme-rotate",
            "row": { "repo": "https://github.com/acme/acme-rotate" },
            "version": "0.2.5",
        })
        .to_string(),
    )
    .unwrap();
    crate::dog_index::write_cache(
        &paths.dog_index_cache,
        INDEX,
        &index_row("shep-log-rotate", "log-rotate", "Rotates logs as they grow"),
    )
    .unwrap();

    let page = page(&paths, dir.path(), "log-rotate");

    assert!(!page.contains("Rotates"), "{page}");
    let listed = adopted_verbs_in(&paths, INDEX);
    assert_eq!(listed[0].description, None);
}
