//! Reading the index's bytes: the version check, the per-entry
//! validation and the field helper, behind [`parse_index`].

use serde_json::{Map, Value};

use super::*;

/// The length of a sha256 digest in hex.
const SHA256_HEX_LEN: usize = 64;

/// Whether `version` spells [`SUPPORTED_INDEX_VERSION`], as either a JSON
/// integer or a JSON float.
///
/// JSON draws no line between `1` and `1.0`, and `Value::as_u64` alone
/// returns `None` for a number the parser represented as a float.
fn version_is_supported(version: &Value) -> bool {
    version.as_u64() == Some(SUPPORTED_INDEX_VERSION)
        || version.as_f64() == Some(SUPPORTED_INDEX_VERSION as f64)
}

/// Parses `bytes` as a community dog index, validating and sanitising every
/// entry.
///
/// Untyped JSON validated by hand: a `serde` derive would make one entry
/// with a wrong field type fail the whole document. A bad entry is counted
/// in [`Index::skipped`] and dropped, never an error.
///
/// # Errors
/// - [`IndexError::Malformed`] if `bytes` are not JSON, or not UTF-8.
/// - [`IndexError::NotAnObject`] if the top level is not an object.
/// - [`IndexError::UnsupportedVersion`] if `version` is missing or unknown.
/// - [`IndexError::MissingDogsArray`] if `dogs` is missing or not an array.
pub fn parse_index(bytes: &[u8]) -> Result<Index, IndexError> {
    let document: Value =
        serde_json::from_slice(bytes).map_err(|err| IndexError::Malformed(err.to_string()))?;
    let Value::Object(document) = document else {
        return Err(IndexError::NotAnObject);
    };
    if !document.get("version").is_some_and(version_is_supported) {
        return Err(IndexError::UnsupportedVersion {
            found: document.get("version").cloned(),
        });
    }
    let Some(entries) = document.get("dogs").and_then(Value::as_array) else {
        return Err(IndexError::MissingDogsArray);
    };

    let mut dogs = Vec::with_capacity(entries.len());
    let mut skipped = 0;
    let mut sanitised = 0;
    for entry in entries {
        // Per entry, not per field: three hostile strings are one row to
        // go and look at.
        let mut entry_sanitised = false;
        match validate_entry(entry, &mut entry_sanitised) {
            Some(dog) => {
                if entry_sanitised {
                    sanitised += 1;
                }
                dogs.push(dog);
            }
            // A skipped entry is not listed, so it is never also counted
            // as sanitised.
            None => skipped += 1,
        }
    }
    Ok(Index {
        dogs,
        skipped,
        sanitised,
    })
}

/// One entry, validated and sanitised, or `None` for the caller to count as
/// skipped.
///
/// Sanitises before validating: the cleaned string is the one that gets
/// printed, so it is the one that has to pass.
fn validate_entry(entry: &Value, sanitised: &mut bool) -> Option<AvailableDog> {
    let entry = entry.as_object()?;
    let name = field(entry, "name", sanitised)?;
    let package = field(entry, "package", sanitised)?;
    let adopt_as = field(entry, "adopt_as", sanitised)?;
    let description = field(entry, "description", sanitised)?;
    let repo = field(entry, "repo", sanitised)?;
    let license = field(entry, "license", sanitised)?;
    let category = field(entry, "category", sanitised)?;
    // Normalised, not refused: an unknown heading costs the entry its shelf
    // and nothing else, unlike the `repo` check below.
    let category = if CATEGORIES.contains(&category.as_str()) {
        category
    } else {
        UNKNOWN_CATEGORY_FALLBACK.to_owned()
    };
    if !is_https(&repo) {
        return None;
    }
    let (source, permissions) = validate_source(entry.get("source")?, sanitised)?;
    let needs = match entry.get("needs") {
        None => Vec::new(),
        Some(needs) => validate_needs(needs)?,
    };
    Some(AvailableDog {
        name,
        package,
        adopt_as,
        description,
        repo,
        license,
        category,
        source,
        permissions,
        needs,
    })
}

/// A `needs` list as the needs shep knows, each once, in the order listed, or
/// `None` when it is not an array of strings.
///
/// A name that is no [`Need`] is dropped rather than refused.
fn validate_needs(needs: &Value) -> Option<Vec<Need>> {
    let mut known = Vec::new();
    for name in needs.as_array()? {
        let need = Need::from_name(name.as_str()?);
        if let Some(need) = need
            && !known.contains(&need)
        {
            known.push(need);
        }
    }
    Some(known)
}

/// One entry's `source`, and the permissions beside it, or `None` for an
/// unknown `kind` or a missing payload.
///
/// `kind` is matched raw and never sanitised: it is a tag rather than prose,
/// so a `kind` carrying an escape matches nothing and takes the entry with it.
///
/// Only a `cargo` row's half of the pair can hold permissions.
fn validate_source(
    source: &Value,
    sanitised: &mut bool,
) -> Option<(DogSourceKind, Option<Vec<IndexedPermission>>)> {
    let source = source.as_object()?;
    match source.get("kind")?.as_str()? {
        "cargo" => {
            // Absent is a normal release; present-and-unusable takes the
            // entry with it.
            let version = match source.get("version") {
                None => None,
                Some(_) => Some(field(source, "version", sanitised)?),
            };
            let sha256 = match source.get("sha256") {
                None => BTreeMap::new(),
                Some(hashes) => validate_hashes(hashes)?,
            };
            let permissions = match source.get("permissions") {
                None => None,
                Some(permissions) => Some(validate_permissions(permissions, sanitised)?),
            };
            Some((DogSourceKind::Cargo { version, sha256 }, permissions))
        }
        "cargo-git" => {
            let url = field(source, "url", sanitised)?;
            if !is_https(&url) {
                return None;
            }
            Some((DogSourceKind::CargoGit { url }, None))
        }
        "go-install" => Some((
            DogSourceKind::GoInstall {
                module: field(source, "module", sanitised)?,
            },
            None,
        )),
        "manual" => Some((
            DogSourceKind::Manual {
                instructions: field(source, "instructions", sanitised)?,
            },
            None,
        )),
        _ => None,
    }
}

/// A `sha256` map, or `None` when it is not an object or any pair breaks
/// the grammar: a triple of lowercase letters, digits, `.`, `_` and `-`
/// mapped to exactly 64 lowercase hex digits.
///
/// Strict because a hash is compared byte for byte later. Nothing here is
/// printed raw, so nothing is sanitised.
fn validate_hashes(hashes: &Value) -> Option<BTreeMap<String, String>> {
    hashes
        .as_object()?
        .iter()
        .map(|(target, hash)| {
            let hash = hash.as_str()?;
            let target_ok = !target.is_empty()
                && target
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
            let hash_ok = hash.len() == SHA256_HEX_LEN
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            (target_ok && hash_ok).then(|| (target.clone(), hash.to_owned()))
        })
        .collect()
}

/// A `permissions` list, or `None` when it is not an array or any item
/// breaks the grammar: an object carrying a non-empty `name` string and a
/// `required` boolean.
///
/// Strict about the two keys it reads and silent about any other, so a key a
/// later index adds costs the row nothing. A name is sanitised like the
/// entry's other strings.
fn validate_permissions(
    permissions: &Value,
    sanitised: &mut bool,
) -> Option<Vec<IndexedPermission>> {
    permissions
        .as_array()?
        .iter()
        .map(|permission| {
            let permission = permission.as_object()?;
            let name = field(permission, "name", sanitised)?;
            Some(IndexedPermission {
                name,
                required: permission.get("required")?.as_bool()?,
            })
        })
        .collect()
}

/// `object[name]` as a sanitised, non-empty string, or `None` when the field
/// is absent, is not a string, or is empty once cleaned.
///
/// A field that is nothing but control characters cleans to the empty
/// string and takes its entry with it. `sanitised` is OR-ed into, so one
/// call cannot clear what an earlier one recorded.
fn field(object: &Map<String, Value>, name: &str, sanitised: &mut bool) -> Option<String> {
    let raw = object.get(name)?.as_str()?;
    let (clean, changed) = sanitise(raw);
    *sanitised |= changed;
    if clean.is_empty() {
        return None;
    }
    Some(clean)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// The live index's own single entry, verbatim from
    /// `web/public/dogs.json`.
    fn valid_entry() -> serde_json::Value {
        serde_json::json!({
            "name": "Spot",
            "package": "shep-log-rotate",
            "adopt_as": "log-rotate",
            "description": "Rotates grown log files and asks the shepherd to reopen them.",
            "repo": "https://github.com/shep-pm/shep-log-rotate",
            "license": "MIT OR Apache-2.0",
            "category": "logs",
            "source": {
                "kind": "cargo-git",
                "url": "https://github.com/shep-pm/shep-log-rotate"
            }
        })
    }

    /// Wraps `entries` in the real document shape: `{"$schema": ...,
    /// "version": 1, "dogs": [...]}`. Every fixture builds through this,
    /// never a bare `serde_json::Value::Array`, which
    /// [`the_old_bare_array_format_is_refused`] pins as refused.
    fn wrap_index(entries: Vec<Value>) -> String {
        serde_json::json!({
            "$schema": "https://shep-pm.com/dogs.schema.json",
            "version": SUPPORTED_INDEX_VERSION,
            "dogs": entries,
        })
        .to_string()
    }

    fn one_entry_with(field: &str, value: &str) -> String {
        let mut entry = valid_entry();
        entry[field] = serde_json::Value::String(value.to_string());
        wrap_index(vec![entry])
    }

    pub(crate) fn one_entry_with_description(description: &str) -> String {
        one_entry_with("description", description)
    }

    fn one_entry_with_category(category: &str) -> String {
        one_entry_with("category", category)
    }

    fn one_entry_with_repo(repo: &str) -> String {
        one_entry_with("repo", repo)
    }

    /// Three entries, the middle one missing `adopt_as`.
    const THREE_ENTRIES_MIDDLE_BROKEN: &[u8] = br#"{
      "$schema": "https://shep-pm.com/dogs.schema.json",
      "version": 1,
      "dogs": [
      {
        "name": "Spot",
        "package": "shep-log-rotate",
        "adopt_as": "log-rotate",
        "description": "Rotates grown log files.",
        "repo": "https://github.com/shep-pm/shep-log-rotate",
        "license": "MIT OR Apache-2.0",
        "category": "logs",
        "source": { "kind": "cargo-git", "url": "https://github.com/shep-pm/shep-log-rotate" }
      },
      {
        "name": "Nameless",
        "package": "shep-nameless",
        "description": "Has no adopt_as, so nobody could adopt it correctly.",
        "repo": "https://github.com/example/shep-nameless",
        "license": "MIT",
        "category": "other",
        "source": { "kind": "manual", "instructions": "Build it yourself." }
      },
      {
        "name": "Rex",
        "package": "shep-watchdog",
        "adopt_as": "watchdog",
        "description": "Barks when a sheep stops answering.",
        "repo": "https://github.com/example/shep-watchdog",
        "license": "Apache-2.0",
        "category": "health",
        "source": { "kind": "go-install", "module": "github.com/example/shep-watchdog" }
      }
    ]}"#;

    #[test]
    fn a_sanitised_entry_still_lists_and_is_counted() {
        let index = parse_index(one_entry_with_description("clean\u{1b}[2Jhere").as_bytes())
            .expect("parses");
        assert_eq!(
            index.dogs.len(),
            1,
            "a hostile description does not remove the dog"
        );
        assert_eq!(index.sanitised, 1);
        assert!(!index.dogs[0].description.contains('\u{1b}'));
    }

    #[test]
    fn a_malformed_entry_is_skipped_and_counted_while_its_neighbours_list() {
        let index = parse_index(THREE_ENTRIES_MIDDLE_BROKEN).expect("parses");
        assert_eq!(index.dogs.len(), 2);
        assert_eq!(index.skipped, 1);
    }

    #[test]
    fn an_unknown_category_is_filed_under_the_fallback_rather_than_dropped() {
        let index = parse_index(one_entry_with_category("logz").as_bytes()).expect("parses");
        assert_eq!(
            index.dogs.len(),
            1,
            "an unknown category must not drop the entry"
        );
        assert_eq!(index.dogs[0].category, UNKNOWN_CATEGORY_FALLBACK);
        assert_eq!(
            index.skipped, 0,
            "a listed entry is never also counted skipped"
        );
    }

    #[test]
    fn a_known_category_survives_unchanged() {
        let index = parse_index(one_entry_with_category("logs").as_bytes()).expect("parses");
        assert_eq!(index.dogs[0].category, "logs");
        assert_eq!(index.skipped, 0);
    }

    #[test]
    fn a_non_https_repo_is_skipped() {
        let index =
            parse_index(one_entry_with_repo("http://example.com/x").as_bytes()).expect("parses");
        assert_eq!(index.skipped, 1);
    }

    #[test]
    fn the_old_bare_array_format_is_refused() {
        let bare = serde_json::Value::Array(vec![valid_entry()]).to_string();
        assert!(
            matches!(parse_index(bare.as_bytes()), Err(IndexError::NotAnObject)),
            "a bare array must be refused now, not silently accepted"
        );
    }

    /// The two refusals ask for different operator actions.
    #[test]
    fn an_object_with_no_version_is_unsupported_not_malformed() {
        let err = parse_index(b"{}").expect_err("no version field");
        assert!(
            matches!(err, IndexError::UnsupportedVersion { found: None }),
            "{err:?}"
        );
        assert!(
            err.to_string().contains("unspecified"),
            "the message must say no version was given: {err}"
        );
    }

    #[test]
    fn a_version_this_build_does_not_understand_is_refused_with_an_upgrade_message() {
        let document = serde_json::json!({ "version": 99, "dogs": [] }).to_string();
        let err = parse_index(document.as_bytes()).expect_err("unsupported version");
        let IndexError::UnsupportedVersion { found } = &err else {
            panic!("wrong variant: {err:?}");
        };
        assert_eq!(found.as_ref().and_then(serde_json::Value::as_u64), Some(99));
        let message = err.to_string();
        assert!(message.contains("99"), "{message}");
        assert!(message.contains("upgrade"), "{message}");
    }

    #[test]
    fn a_version_spelled_with_a_decimal_point_is_still_supported() {
        let as_decimal = SUPPORTED_INDEX_VERSION as f64;
        let document = serde_json::json!({ "version": as_decimal, "dogs": [] }).to_string();
        let index = parse_index(document.as_bytes())
            .expect("a decimal-point spelling is the same number as SUPPORTED_INDEX_VERSION");
        assert!(index.dogs.is_empty());
    }

    #[test]
    fn a_supported_version_with_no_dogs_field_is_refused() {
        let document = serde_json::json!({ "version": SUPPORTED_INDEX_VERSION }).to_string();
        assert!(matches!(
            parse_index(document.as_bytes()),
            Err(IndexError::MissingDogsArray)
        ));
    }

    #[test]
    fn an_empty_dogs_array_is_a_valid_empty_index() {
        let index = parse_index(wrap_index(vec![]).as_bytes()).expect("parses");
        assert!(index.dogs.is_empty());
        assert_eq!(index.skipped, 0);
    }

    // --- Extra hostile cases ---

    /// Stripping per character rather than per sequence is what makes this
    /// a non-event, and it is a property of the sanitiser rather than the
    /// renderer.
    #[test]
    fn an_escape_split_across_a_field_boundary_cannot_reassemble() {
        let mut entry = valid_entry();
        entry["name"] = serde_json::Value::String("Spot\u{1b}".to_string());
        entry["description"] = serde_json::Value::String("[2J and the screen is gone".to_string());
        let document = wrap_index(vec![entry]);

        let index = parse_index(document.as_bytes()).expect("parses");
        assert_eq!(index.dogs.len(), 1);
        let dog = &index.dogs[0];
        let joined = format!("{}{}", dog.name, dog.description);
        assert!(!joined.contains('\u{1b}'), "reassembled in {joined:?}");
        assert_eq!(
            index.sanitised, 1,
            "counted once for the entry, not per field"
        );
    }

    /// Ten thousand escapes is well inside the 1 MiB fetch cap, so the
    /// guard here is the sanitiser rather than the size limit.
    #[test]
    fn a_long_run_of_escapes_is_stripped_without_losing_the_entry() {
        let hostile = format!("{}real text", "\u{1b}".repeat(10_000));
        let index = parse_index(one_entry_with_description(&hostile).as_bytes()).expect("parses");
        assert_eq!(index.dogs.len(), 1);
        assert_eq!(index.dogs[0].description, "real text");
        assert_eq!(index.sanitised, 1);
    }

    #[test]
    fn a_field_that_is_nothing_but_control_characters_skips_the_entry() {
        let index = parse_index(one_entry_with_description("\u{1b}\u{7}\r\n\t").as_bytes())
            .expect("parses");
        assert_eq!(index.dogs.len(), 0);
        assert_eq!(index.skipped, 1);
    }

    #[test]
    fn a_skipped_entry_is_not_also_counted_as_sanitised() {
        let mut entry = valid_entry();
        entry["description"] = serde_json::Value::String("hostile\u{1b}[2J".to_string());
        entry["repo"] = serde_json::Value::String("http://example.com/x".to_string());
        let document = wrap_index(vec![entry]);

        let index = parse_index(document.as_bytes()).expect("parses");
        assert_eq!(index.skipped, 1);
        assert_eq!(index.sanitised, 0);
    }

    /// The reason the parse goes through untyped JSON: a `Deserialize`
    /// derive would make `"name": 42` a whole-document error.
    #[test]
    fn a_field_of_the_wrong_json_type_skips_only_its_own_entry() {
        let mut broken = valid_entry();
        broken["name"] = serde_json::json!(42);
        let mut other = valid_entry();
        other["package"] = serde_json::Value::String("shep-watchdog".to_string());
        let document = wrap_index(vec![broken, other]);

        let index = parse_index(document.as_bytes()).expect("parses");
        assert_eq!(index.dogs.len(), 1);
        assert_eq!(index.skipped, 1);
        assert_eq!(index.dogs[0].package, "shep-watchdog");
    }

    #[test]
    fn a_cargo_source_parses_and_carries_no_fields_of_its_own() {
        let mut entry = valid_entry();
        entry["source"] = serde_json::json!({ "kind": "cargo" });
        let document = wrap_index(vec![entry]);

        let index = parse_index(document.as_bytes()).expect("parses");
        assert_eq!(index.skipped, 0);
        assert_eq!(index.dogs.len(), 1);
        assert_eq!(
            index.dogs[0].source,
            DogSourceKind::Cargo {
                version: None,
                sha256: BTreeMap::new()
            }
        );
    }

    /// `repo` is a link; a `cargo-git` `url` is pasted into a shell, so it
    /// gets the same https check and not a weaker one.
    #[test]
    fn a_non_https_cargo_git_source_url_is_skipped() {
        let mut entry = valid_entry();
        entry["source"] = serde_json::json!({ "kind": "cargo-git", "url": "http://example.com/x" });
        let document = wrap_index(vec![entry]);

        let index = parse_index(document.as_bytes()).expect("parses");
        assert_eq!(index.dogs.len(), 0);
        assert_eq!(index.skipped, 1);
    }

    #[test]
    fn an_unknown_source_kind_is_skipped() {
        let mut entry = valid_entry();
        entry["source"] =
            serde_json::json!({ "kind": "curl-bash", "url": "https://example.com/x" });
        let document = wrap_index(vec![entry]);

        assert_eq!(parse_index(document.as_bytes()).expect("parses").skipped, 1);
    }

    /// Every `source.kind` this file accepts, with a minimal source that
    /// parses as it. Tests only: `validate_source` matches string literals,
    /// since a match on a const is not a match.
    pub(crate) const SOURCE_KINDS: [(&str, &str); 4] = [
        ("cargo", r#"{"kind":"cargo"}"#),
        (
            "cargo-git",
            r#"{"kind":"cargo-git","url":"https://example.com/x"}"#,
        ),
        (
            "go-install",
            r#"{"kind":"go-install","module":"example.com/x"}"#,
        ),
        ("manual", r#"{"kind":"manual","instructions":"build it"}"#),
    ];

    #[test]
    fn a_cargo_source_keeps_the_version_it_names() {
        let mut entry = valid_entry();
        entry["source"] = serde_json::json!({ "kind": "cargo", "version": "0.1.0-alpha.1" });
        let document = wrap_index(vec![entry]);

        let index = parse_index(document.as_bytes()).expect("parses");
        assert_eq!(
            index.dogs[0].source,
            DogSourceKind::Cargo {
                version: Some("0.1.0-alpha.1".to_string()),
                sha256: BTreeMap::new()
            }
        );
    }

    #[test]
    fn a_cargo_source_with_an_empty_version_is_skipped() {
        let mut entry = valid_entry();
        entry["source"] = serde_json::json!({ "kind": "cargo", "version": "" });
        let document = wrap_index(vec![entry]);

        let index = parse_index(document.as_bytes()).expect("parses");
        assert_eq!(index.dogs.len(), 0);
        assert_eq!(index.skipped, 1);
    }

    /// The dangerous direction: a kind the docs site accepts and
    /// `validate_source` does not drops a published entry from every
    /// listing, counted only as an anonymous `1 entry skipped`.
    #[test]
    fn every_listed_source_kind_actually_parses() {
        for (kind, source) in SOURCE_KINDS {
            let mut entry = valid_entry();
            entry["source"] = serde_json::from_str(source).expect("fixture is JSON");
            let document = wrap_index(vec![entry]);

            let index = parse_index(document.as_bytes()).expect("parses");
            assert_eq!(
                index.dogs.len(),
                1,
                "source.kind {kind:?} is listed as supported but its entry was skipped"
            );
        }
    }

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn cargo_entry(source: serde_json::Value) -> String {
        let mut entry = valid_entry();
        entry["source"] = source;
        wrap_index(vec![entry])
    }

    /// The listing's whole point for an unversioned row: it is there, and
    /// it is not installable.
    #[test]
    fn a_cargo_row_without_a_version_parses_lists_and_is_not_installable() {
        let index = parse_index(cargo_entry(serde_json::json!({ "kind": "cargo" })).as_bytes())
            .expect("parses");

        assert_eq!(index.skipped, 0);
        assert_eq!(index.dogs.len(), 1);
        assert!(index.dogs[0].is_unversioned_cargo());
    }

    #[test]
    fn a_cargo_row_with_a_version_is_installable() {
        let source = serde_json::json!({ "kind": "cargo", "version": "0.2.4" });
        let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");

        assert!(!index.dogs[0].is_unversioned_cargo());
    }

    #[test]
    fn a_row_with_a_version_and_a_hash_for_a_target_yields_the_url_and_the_hash() {
        let source = serde_json::json!({
            "kind": "cargo",
            "version": "0.2.4",
            "sha256": { "aarch64-apple-darwin": HASH }
        });
        let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");

        let prebuilt = index.dogs[0]
            .prebuilt_for("aarch64-apple-darwin")
            .expect("hashed");
        assert_eq!(
            prebuilt.url,
            "https://github.com/shep-pm/shep-log-rotate/releases/download/v0.2.4/\
             shep-log-rotate-0.2.4-aarch64-apple-darwin"
        );
        assert_eq!(prebuilt.sha256, HASH);
        assert_eq!(index.dogs[0].prebuilt_for("x86_64-unknown-linux-gnu"), None);
    }

    #[test]
    fn a_hash_that_is_not_sixty_four_lowercase_hex_digits_skips_the_entry() {
        for bad in [
            "abc",
            &HASH.to_uppercase(),
            &format!("{HASH}0"),
            &HASH.replace('0', "g"),
        ] {
            let source = serde_json::json!({
                "kind": "cargo",
                "version": "0.2.4",
                "sha256": { "aarch64-apple-darwin": bad }
            });
            let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");
            assert_eq!(index.skipped, 1, "{bad:?} was accepted");
        }
    }

    #[test]
    fn a_sha256_that_is_not_an_object_skips_the_entry() {
        let source = serde_json::json!({ "kind": "cargo", "version": "0.2.4", "sha256": [HASH] });
        let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");

        assert_eq!(index.skipped, 1);
    }

    /// The keys [`validate_source`] reads off a `cargo` source. Tests only,
    /// like [`SOURCE_KINDS`]: `the_schema_declares_the_cargo_source_keys`
    /// holds this equal to the published schema's `cargo` variant.
    pub(crate) const CARGO_SOURCE_KEYS: [&str; 4] = ["kind", "permissions", "sha256", "version"];

    #[test]
    fn a_cargo_source_lists_each_permission_and_whether_it_is_required() {
        let source = serde_json::json!({
            "kind": "cargo",
            "version": "0.2.4",
            "permissions": [
                { "name": "flock.read", "required": true },
                { "name": "logs.read", "required": false }
            ]
        });
        let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");

        assert_eq!(index.skipped, 0);
        assert_eq!(
            index.dogs[0].permissions,
            Some(vec![
                IndexedPermission {
                    name: "flock.read".to_owned(),
                    required: true,
                },
                IndexedPermission {
                    name: "logs.read".to_owned(),
                    required: false,
                },
            ])
        );
    }

    #[test]
    fn a_cargo_source_that_lists_no_permissions_is_none() {
        let source = serde_json::json!({ "kind": "cargo", "version": "0.2.4" });
        let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");

        assert_eq!(index.dogs[0].permissions, None);
    }

    /// The two are different claims: a row that lists nothing is not a row
    /// that says this version asks for nothing.
    #[test]
    fn an_empty_permissions_list_is_a_version_declaring_none() {
        let source = serde_json::json!({
            "kind": "cargo",
            "version": "0.2.4",
            "permissions": []
        });
        let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");

        assert_eq!(index.skipped, 0);
        assert_eq!(index.dogs[0].permissions, Some(Vec::new()));
    }

    #[test]
    fn a_permissions_value_that_is_not_an_array_skips_the_entry() {
        for bad in [
            serde_json::json!("flock.read"),
            serde_json::json!({"flock.read": true}),
            serde_json::json!(null),
        ] {
            let source =
                serde_json::json!({ "kind": "cargo", "version": "0.2.4", "permissions": bad });
            let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");
            assert_eq!(index.skipped, 1, "{bad} was accepted");
        }
    }

    /// Each item has to name the permission and say whether the dog can run
    /// without it. A name that cleans to nothing is unusable in a prompt.
    #[test]
    fn a_permission_item_that_is_not_a_name_and_a_required_skips_the_entry() {
        for bad in [
            serde_json::json!("flock.read"),
            serde_json::json!({ "name": "flock.read" }),
            serde_json::json!({ "required": true }),
            serde_json::json!({ "name": "flock.read", "required": "yes" }),
            serde_json::json!({ "name": "", "required": true }),
        ] {
            let source = serde_json::json!({
                "kind": "cargo",
                "version": "0.2.4",
                "permissions": [bad]
            });
            let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");
            assert_eq!(index.skipped, 1, "{bad} was accepted");
        }
    }

    #[test]
    fn a_permission_name_carrying_an_escape_is_cleaned_like_every_other_string() {
        let source = serde_json::json!({
            "kind": "cargo",
            "version": "0.2.4",
            "permissions": [{ "name": "flock\u{1b}.read", "required": true }]
        });
        let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");

        let listed = index.dogs[0].permissions.as_ref().expect("listed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "flock.read");
        assert!(listed[0].required);
        assert_eq!(index.sanitised, 1);
    }

    /// Only a `cargo` row carries permissions, so a `permissions` key beside
    /// another kind is one more unknown key.
    #[test]
    fn a_non_cargo_source_lists_no_permissions() {
        let source = serde_json::json!({
            "kind": "cargo-git",
            "url": "https://example.com/x",
            "permissions": [{ "name": "flock.read", "required": true }]
        });
        let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");

        assert_eq!(index.skipped, 0);
        assert_eq!(index.dogs[0].permissions, None);
    }

    fn entry_with_needs(needs: serde_json::Value) -> String {
        let mut entry = valid_entry();
        entry["needs"] = needs;
        wrap_index(vec![entry])
    }

    #[test]
    fn a_row_naming_known_needs_keeps_them_in_order() {
        let index = parse_index(entry_with_needs(serde_json::json!(["alerts", "logs"])).as_bytes())
            .expect("parses");

        assert_eq!(index.skipped, 0);
        assert_eq!(index.dogs[0].needs, [Need::Alerts, Need::Logs]);
    }

    /// The index can name a need before this build shows it.
    #[test]
    fn a_need_shep_does_not_know_is_ignored_and_the_row_still_lists() {
        let needs = serde_json::json!(["metrics", "logs", "Logs"]);
        let index = parse_index(entry_with_needs(needs).as_bytes()).expect("parses");

        assert_eq!(index.skipped, 0);
        assert_eq!(index.dogs.len(), 1);
        assert_eq!(index.dogs[0].needs, [Need::Logs]);
    }

    #[test]
    fn a_row_with_no_needs_field_has_no_needs() {
        let index = parse_index(wrap_index(vec![valid_entry()]).as_bytes()).expect("parses");

        assert_eq!(index.skipped, 0);
        assert!(index.dogs[0].needs.is_empty());
    }

    #[test]
    fn a_repeated_need_is_kept_once() {
        let needs = serde_json::json!(["logs", "alerts", "logs", "alerts"]);
        let index = parse_index(entry_with_needs(needs).as_bytes()).expect("parses");

        assert_eq!(index.dogs[0].needs, [Need::Logs, Need::Alerts]);
    }

    #[test]
    fn a_needs_field_that_is_not_a_list_of_strings_skips_only_its_entry() {
        for bad in [
            serde_json::json!("logs"),
            serde_json::json!({ "logs": true }),
            serde_json::json!(null),
            serde_json::json!(["logs", 7]),
            serde_json::json!([["logs"]]),
        ] {
            let mut broken = valid_entry();
            broken["needs"] = bad.clone();
            let mut other = valid_entry();
            other["package"] = serde_json::Value::String("shep-watchdog".to_string());
            let index = parse_index(wrap_index(vec![broken, other]).as_bytes()).expect("parses");

            assert_eq!(index.skipped, 1, "{bad} was accepted");
            assert_eq!(index.dogs.len(), 1, "{bad} took its neighbour with it");
            assert_eq!(index.dogs[0].package, "shep-watchdog");
        }
    }

    /// A shep from before the hashes read the new document by ignoring
    /// them: the parse is by hand, so an unknown key is not an error.
    #[test]
    fn an_unknown_key_on_a_cargo_source_is_ignored() {
        let source = serde_json::json!({ "kind": "cargo", "version": "0.2.4", "future": 1 });
        let index = parse_index(cargo_entry(source).as_bytes()).expect("parses");

        assert_eq!(index.dogs.len(), 1);
        assert_eq!(index.skipped, 0);
    }
}
