//! Reading and writing a dog's own `[<name>]` section of `dogs.toml`.
//!
//! A dog asks for this over the socket rather than getting it in its
//! environment: `SHEP_HOME` and `SHEP_DOG_NAME` are the only two variables
//! [`super::dog_app`] sets, because the environment is readable from the
//! process table and inherited by every child.

use std::path::Path;

use shep_core::config::DogsConfig;

use super::spec::DogError;

/// The `[<name>]` section of `path`, a `dogs.toml`, as the operator wrote
/// it, as a document in its own right: headers rebased off `name`, and no
/// header of its own.
///
/// Reads the file on every call, so one reader can never be stale and
/// `shep disable X && shep enable X` re-reads an edited section. A missing
/// file, or one with no such section, is `Ok(String::new())`.
///
/// The operator's own bytes, not a re-render: rendering a parsed table drops
/// the comments inside a section and sorts its keys. [`set_dog_section`]
/// takes the same shape back, parsing what it is handed as a document and
/// rebasing it under `name` again.
///
/// # Errors
/// - [`DogError::Config`] if the file exists and is not valid `dogs.toml`, or
///   its section will not render back to TOML.
/// - [`DogError::Io`] if the file exists and could not be read.
pub fn dog_section(path: &Path, name: &str) -> Result<String, DogError> {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(err) => return Err(DogError::Io(err)),
    };
    // Through shep-core's own type, so a broken `dogs.toml` is one named error
    // and not a second parser's opinion of the same file.
    let config =
        DogsConfig::load(Some(&source)).map_err(|err| DogError::Config(err.to_string()))?;
    let Some(table) = config.dog.get(name) else {
        return Ok(String::new());
    };
    // Cannot fail: the stricter parse above already read the same bytes.
    let doc: toml_edit::DocumentMut = source
        .parse()
        .map_err(|err: toml_edit::TomlError| DogError::Config(err.to_string()))?;
    match doc.get(name) {
        Some(toml_edit::Item::Table(spanned)) => {
            // Re-rooted, not `to_string`ed in place. A table renders its own
            // key-values and leaves its child tables to the document, which
            // writes their headers by their path from the root: a section
            // read out of `[bark]` would arrive at the dog either missing
            // `[bark.sinks.local]` entirely or naming it `bark.sinks`, and
            // the dog parses what it gets as its own root. Cloning the table
            // in as a document's root keeps every byte the operator wrote,
            // comments between keys included, and rebases the headers.
            let mut section = spanned.clone();
            // The root of a document takes no header, and the decor here is
            // the `[bark]` line's own: a comment above it belongs to the
            // table rather than to the body, and `set_dog_section` carries
            // it across on the write instead.
            section.set_implicit(true);
            *section.decor_mut() = toml_edit::Decor::default();
            let mut out = toml_edit::DocumentMut::new();
            *out.as_table_mut() = section;
            Ok(out.to_string())
        }
        // An inline table, `bark = { poll = "60s" }`, is a valid entry
        // whose span is `{ ... }`, which is not a section body and is not
        // something the pane could write back. Rendered, as every section
        // was before: there is no comment to lose inside one line.
        _ => toml::to_string(table).map_err(|err| DogError::Config(err.to_string())),
    }
}

/// Replaces `name`'s table in `path`, a `dogs.toml`, with `section` and
/// writes the file back owner-only, under the same lock the CLI's two
/// writers of that file hold.
///
/// `dogs.toml` is hand-editable on purpose ([`DogsConfig`]'s own doc calls
/// it deliberately not a locked shep-owned store), so this reads, modifies
/// and writes the document it read rather than rendering a parsed map back
/// out: every table other than `name`'s comes through byte for byte, and so
/// does a comment outside it. A comment inside the replaced table is the
/// caller's to carry, because the caller is what decided the section's new
/// text, and [`dog_section`] hands it the span rather than a re-render
/// precisely so that it can. The header's own decor, a comment line above
/// `[name]` and anything trailing the header, is carried across here
/// instead: it sits neither inside the section nor outside the table, so
/// neither half would otherwise keep it.
///
/// The rendered result is handed to [`DogsConfig::load`] before anything
/// reaches disk, so a section this daemon could not serve back never lands.
/// That gate is the same one `shep rehome`'s writer takes, and it is the
/// stricter of the two parses: a stray top-level scalar is a valid document
/// and not a valid `DogsConfig`.
///
/// The write itself is the three steps `shep-cli`'s `write_dogs_config`
/// takes, for its reasons: staged in a sibling file created at
/// [`shep_core::atomic_file::OWNER_ONLY_FILE_MODE`] (this is where an
/// operator is told to paste a webhook URL, which is a bearer token in a
/// path), `fsync`ed, then `rename`d over `path` so a crash leaves the whole
/// file or none of it.
///
/// Whether `name` is a dog at all is the caller's question, not this
/// function's: the answer lives in the supervisor, and `rpc::dispatch` asks
/// it before calling here.
///
/// # Errors
/// - [`DogError::Io`]: the lock, the read, the staging file, the `fsync` or
///   the rename.
/// - [`DogError::Config`]: `section` is not valid TOML, `path` is not valid
///   `dogs.toml`, or the spliced result would not load. Nothing has been
///   written in any of the three.
pub fn set_dog_section(path: &Path, name: &str, section: &str) -> Result<(), DogError> {
    use std::io::Write as _;

    use toml_edit::{DocumentMut, Item};

    // Held across the read, the splice and the rename, and dropped on the
    // way out. Two writers that read before either wrote would lose one of
    // the two sections whichever way the renames raced; the boot migration
    // takes the same lock on the same path for the same reason. This
    // function takes no other lock, so it can never be the half of a
    // deadlock that holds `dogs.toml` and waits on `shep.toml`.
    let _lock = shep_core::config_lock::ConfigLock::acquire(path).map_err(DogError::Io)?;

    // A missing file is the ordinary first write: a home that has never had
    // a dog configured has no `dogs.toml` at all.
    let existing = match std::fs::read_to_string(path) {
        Ok(existing) => existing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(DogError::Io(err)),
    };
    let mut doc: DocumentMut = existing.parse().map_err(|err: toml_edit::TomlError| {
        DogError::Config(format!("dogs.toml does not parse: {err}"))
    })?;
    let incoming: DocumentMut = section.parse().map_err(|err: toml_edit::TomlError| {
        DogError::Config(format!("[{name}] does not parse: {err}"))
    })?;

    // `set_implicit(false)`, or a section emptied down to nothing takes the
    // dog's table with it. A document's root table is implicit, and an
    // implicit table with no keys of its own renders no header at all. A
    // section that still holds a key renders `[name]` either way, so this
    // line shows up only in the emptied case, which is where an operator
    // arrives by deleting the last key in the pane.
    let mut table = incoming.as_table().clone();
    table.set_implicit(false);
    // A comment above `[name]`, and anything trailing the header itself,
    // are decor on the table rather than text inside it: `dog_section`
    // hands the caller the body and cannot carry them, and replacing the
    // item wholesale would drop both. Copied across so an operator's note
    // about what a dog is for survives a pane write, as the notes between
    // its keys already do.
    if let Some(Item::Table(existing)) = doc.get(name) {
        *table.decor_mut() = existing.decor().clone();
    }
    doc[name] = Item::Table(table);

    let rendered = doc.to_string();
    DogsConfig::load(Some(&rendered))
        .map_err(|err| DogError::Config(format!("[{name}] would not load: {err}")))?;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = shep_core::config_lock::create_config_file(parent).map_err(DogError::Io)?;
    tmp.write_all(rendered.as_bytes()).map_err(DogError::Io)?;
    shep_core::atomic_file::publish(tmp, path).map_err(DogError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_written_with_sub_tables_reaches_the_dog_as_its_own_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        let source = "# above the header\n[bark]\n# above poll\npoll = \"5s\"\n\n[bark.sinks.local]\nkind = \"json\"\nurl = \"https://example.invalid/h\"\n\n[[bark.rules]]\non = \"event\"\nkinds = [\"exit\"]\nsinks = [\"local\"]\n\n[otel]\nendpoint = \"https://collector.invalid\"\n";
        std::fs::write(&path, source).unwrap();

        let section = dog_section(&path, "bark").unwrap();
        let parsed: toml::Table = section.parse().unwrap();

        // The dog parses the section as its own root, so every sub-table has
        // to arrive rebased off the section name.
        assert!(parsed.contains_key("sinks"), "{section}");
        assert!(parsed.contains_key("rules"), "{section}");
        assert_eq!(
            parsed["sinks"]["local"]["kind"].as_str(),
            Some("json"),
            "{section}"
        );
        assert_eq!(
            parsed["rules"].as_array().map(Vec::len),
            Some(1),
            "{section}"
        );
        // The operator's own comment between keys survives the re-rooting,
        // which is the reason this reads a span rather than re-rendering.
        assert!(section.contains("# above poll"), "{section}");
        // The header's own decor is `set_dog_section`'s to carry, not this.
        assert!(!section.contains("# above the header"), "{section}");
        // A second dog is nobody else's business.
        assert!(!section.contains("otel"), "{section}");
    }

    #[test]
    fn a_sub_table_section_survives_a_read_and_a_write_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        std::fs::write(
            &path,
            "# keep me\n[bark]\npoll = \"5s\"\n\n[bark.sinks.local]\nkind = \"json\"\nurl = \"https://example.invalid/h\"\n",
        )
        .unwrap();

        let section = dog_section(&path, "bark").unwrap();
        set_dog_section(&path, "bark", &section).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        let reloaded: toml::Table = after.parse().unwrap();
        assert_eq!(
            reloaded["bark"]["sinks"]["local"]["url"].as_str(),
            Some("https://example.invalid/h"),
            "{after}"
        );
        assert!(after.contains("# keep me"), "{after}");
        assert_eq!(dog_section(&path, "bark").unwrap(), section, "{after}");
    }

    #[test]
    fn a_dogs_section_comes_back_as_its_own_table_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        std::fs::write(
            &path,
            "[bark]\ndebounce = \"30s\"\n\n[metrics]\nport = 9615\n",
        )
        .unwrap();

        let bark = dog_section(&path, "bark").unwrap();
        assert!(bark.contains("debounce"));
        assert!(
            !bark.contains("9615"),
            "one dog never sees another's config"
        );
        // Round-trips as TOML, the contract the dog parses under.
        let parsed: toml::Table = toml::from_str(&bark).unwrap();
        assert_eq!(parsed["debounce"].as_str(), Some("30s"));

        assert_eq!(dog_section(&path, "absent").unwrap(), "");
        assert_eq!(
            dog_section(&dir.path().join("gone.toml"), "bark").unwrap(),
            ""
        );
    }

    #[test]
    fn a_section_reaches_the_wire_exactly_as_it_did_from_shep_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dogs.toml");
        std::fs::write(&path, "[bark]\ndebounce = \"30s\"\n").expect("write");

        // Pinned as a string: the dog-facing contract is the exact text.
        assert_eq!(
            dog_section(&path, "bark").expect("section"),
            "debounce = \"30s\"\n"
        );
    }

    #[test]
    fn a_dog_with_no_section_still_gets_an_empty_string() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dogs.toml");
        std::fs::write(&path, "[bark]\ndebounce = \"30s\"\n").expect("write");

        assert_eq!(dog_section(&path, "metrics").expect("section"), "");
    }

    /// A comment above another dog's table, or that dog's own keys, would
    /// not survive a regenerate-from-map write. `dogs.toml` is
    /// hand-editable on purpose, so an operator's file coming back
    /// rewritten would be the bug.
    #[test]
    fn set_dog_section_replaces_one_table_and_leaves_the_rest_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        std::fs::write(
            &path,
            "# top comment\n[metrics]\nbind = \"127.0.0.1:9100\"\n\n[bark]\npoll = \"60s\"\n",
        )
        .unwrap();

        set_dog_section(&path, "bark", "poll = \"30s\"\nhistory_bytes = 4096\n").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# top comment"), "{text}");
        assert!(text.contains("bind = \"127.0.0.1:9100\""), "{text}");
        assert!(text.contains("poll = \"30s\""), "{text}");
        assert!(!text.contains("poll = \"60s\""), "{text}");
        let parsed = DogsConfig::load(Some(&text)).unwrap();
        assert_eq!(parsed.dog["bark"]["history_bytes"].as_integer(), Some(4096));
    }

    /// The sibling test above pins a comment outside the edited table,
    /// which the write side preserves on its own; a comment inside it, and
    /// the order of the keys around it, survive only if the section the
    /// pane was handed was the raw span. `toml::map::Map` is a `BTreeMap`
    /// without `preserve_order`, so a re-render alphabetises as well as
    /// stripping. A comment above the header is the third case, decor on
    /// the table itself, which only the write side can carry.
    #[test]
    fn a_pane_round_trip_keeps_the_comments_and_the_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        std::fs::write(
            &path,
            "# what bark is for\n[bark]\n# why bark polls slowly\npoll = \"60s\"\nzz_last = 1\naa_first = 2\n",
        )
        .unwrap();

        let section = dog_section(&path, "bark").unwrap();
        assert!(section.contains("# why bark polls slowly"), "{section}");
        assert!(
            section.find("zz_last") < section.find("aa_first"),
            "the keys come back in the operator\'s order: {section}"
        );

        // What the pane does: write back what it was handed, one value
        // changed.
        set_dog_section(&path, "bark", &section.replace("60s", "30s")).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# why bark polls slowly"), "{text}");
        assert!(
            text.contains("# what bark is for"),
            "the header's own comment lives on the table, not inside it: {text}"
        );
        assert!(text.contains("poll = \"30s\""), "{text}");
        assert!(
            text.find("zz_last") < text.find("aa_first"),
            "the write keeps the order the read handed it: {text}"
        );
    }

    /// A home that has never had a dog configured has no `dogs.toml` at
    /// all, the ordinary case for the first section anyone writes.
    #[test]
    fn set_dog_section_creates_the_file_when_there_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");

        set_dog_section(&path, "bark", "poll = \"30s\"\n").unwrap();

        let parsed = DogsConfig::load(Some(&std::fs::read_to_string(&path).unwrap())).unwrap();
        assert!(parsed.dog.contains_key("bark"));
    }

    /// A section this daemon cannot read back must never reach disk: the
    /// file it lands in is the one every dog is served from, so one bad
    /// section would take the rest of the kennel down with it.
    #[test]
    fn set_dog_section_refuses_text_that_is_not_a_table_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        std::fs::write(&path, "[bark]\npoll = \"60s\"\n").unwrap();

        let err = set_dog_section(&path, "bark", "this is = = not toml").unwrap_err();

        assert!(err.to_string().contains("bark"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[bark]\npoll = \"60s\"\n"
        );
    }

    /// This section parses fine on its own and the spliced document is
    /// valid TOML; what it is not is a valid `DogsConfig`, since the file
    /// it lands in has a top-level scalar. The daemon serves every dog
    /// from one `DogsConfig::load` of this file, so a write that leaves it
    /// unloadable takes the whole kennel down, not just this dog.
    #[test]
    fn set_dog_section_refuses_a_result_the_daemon_could_not_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        std::fs::write(&path, "port = 9100\n[bark]\npoll = \"60s\"\n").unwrap();

        let err = set_dog_section(&path, "bark", "poll = \"30s\"\n").unwrap_err();

        assert!(err.to_string().contains("bark"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "port = 9100\n[bark]\npoll = \"60s\"\n"
        );
    }

    /// A root table is implicit, and an implicit table with no keys of its
    /// own renders no header at all, so an operator who cleared a section
    /// in the pane would find `[bark]` gone from a file they are invited
    /// to hand-edit, and `shep describe` with it.
    #[test]
    fn set_dog_section_leaves_the_table_behind_when_the_section_is_emptied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");
        std::fs::write(&path, "[bark]\npoll = \"60s\"\n").unwrap();

        set_dog_section(&path, "bark", "").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[bark]"), "{text}");
        let parsed = DogsConfig::load(Some(&text)).unwrap();
        assert!(parsed.dog["bark"].is_empty(), "{text}");
    }

    /// `docs/dogs.md` tells an operator to paste a webhook URL here, a
    /// bearer token in a path. The CLI's own writer creates the file
    /// `0600`; a second writer at `0644` would be the downgrade.
    #[cfg(unix)]
    #[test]
    fn set_dog_section_writes_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dogs.toml");

        set_dog_section(&path, "bark", "poll = \"30s\"\n").unwrap();

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
