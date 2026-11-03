//! What [`load_settings`] says each dog's source and version are.

use super::*;
use crate::dog_origin::Source;

fn dog<'a>(snapshot: &'a SettingsSnapshot, name: &str) -> &'a DogView {
    snapshot.dogs.iter().find(|dog| dog.name == name).unwrap()
}

#[test]
fn each_dog_reads_the_source_its_receipt_or_its_absence_gives() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shep.toml");
    ShepToml::edit(&path, |doc| {
        doc.adopt_dog("log-rotate", Path::new("/opt/shep-log-rotate"), false)
            .unwrap();
        doc.adopt_dog("otel", Path::new("/opt/shep-otel"), false)
            .unwrap();
    })
    .unwrap();
    let installed = dir.path().join("dogs/log-rotate");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(
        installed.join("receipt.json"),
        serde_json::json!({
            "package": "shep-log-rotate",
            "row": { "repo": "https://github.com/shep-pm/shep-log-rotate" },
            "manifest": { "version": "0.2.4" },
        })
        .to_string(),
    )
    .unwrap();

    let snapshot = load_settings(
        &path,
        Path::new("/var/run/shep.sock"),
        (StyleLevel::Full, StyleSource::Config),
    )
    .unwrap();

    assert_eq!(dog(&snapshot, "bark").origin.source, Source::BuiltIn);
    let log_rotate = &dog(&snapshot, "log-rotate").origin;
    assert_eq!(log_rotate.source, Source::Installed);
    assert_eq!(log_rotate.version.as_deref(), Some("0.2.4"));
    assert_eq!(dog(&snapshot, "otel").origin.source, Source::Adopted);
}
