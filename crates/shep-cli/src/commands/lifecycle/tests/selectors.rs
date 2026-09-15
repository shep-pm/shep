use super::*;

/// The narrow payload is koji alone and the armed listing has four
/// entries, so the two differ by row count as well as by content.
#[tokio::test]
async fn a_lifecycle_verb_renders_the_whole_flock_as_a_table() {
    use shep_client::testing::fake_client_on;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    let dir = tempfile::tempdir().unwrap();
    let address = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_on(&address).await;
    daemon.reply_to_list(a_flock_with_a_dog());

    let mut out = Vec::new();
    let mut err = Vec::new();
    let touched = vec![ProcessInfo::builder(1, "koji", ProcStatus::Stopped).build()];
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        render_outcome(&client, &mut streams, "stop", FlockRows(touched)).await
    };

    assert_eq!(code, ExitCode::Success);
    let printed = String::from_utf8(out).unwrap();
    // Split at the caption rather than filtered: the two tables have
    // different columns, so a name read from the wrong one is a
    // different field.
    let (sheep, dogs) = printed
        .split_once("\nDogs\n")
        .unwrap_or_else(|| panic!("the dogs table needs its own caption: {printed}"));
    let sheep_names: Vec<&str> = sheep
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter(|word| *word != "NAME")
        .collect();
    assert_eq!(
        sheep_names,
        vec!["golbat", "koji", "rotom"],
        "every sheep, not only the one that was stopped, and no dog among \
             them: {printed}"
    );
    // Column 1, not 0: the dogs table leads with ID.
    let dog_names: Vec<&str> = dogs
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter(|word| *word != "NAME")
        .collect();
    assert_eq!(
        dog_names,
        vec!["log-rotate"],
        "the dog renders through the dogs table: {printed}"
    );
    assert!(
        dogs.contains("SOURCE") && dogs.contains("adopted"),
        "with the SOURCE column the sheep table has not: {printed}"
    );
}

/// A script reads `data[0]` to learn what it stopped, so four rows break
/// it silently. `list_flock_count` proves the JSON path fetches no
/// listing rather than fetching one and discarding it.
#[tokio::test]
async fn the_json_surface_keeps_the_rows_the_verb_touched() {
    use shep_client::testing::fake_client_on;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    let dir = tempfile::tempdir().unwrap();
    let address = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_on(&address).await;
    daemon.reply_to_list(a_flock_with_a_dog());

    let mut out = Vec::new();
    let mut err = Vec::new();
    let touched = vec![ProcessInfo::builder(1, "koji", ProcStatus::Stopped).build()];
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Json,
        };
        render_outcome(&client, &mut streams, "stop", FlockRows(touched)).await
    };

    assert_eq!(code, ExitCode::Success);
    let envelope: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let names: Vec<&str> = envelope["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["koji"],
        "only what the verb touched: {envelope}"
    );
    assert_eq!(
        daemon.list_flock_count(),
        0,
        "and no listing was fetched to build it"
    );
}

/// The precedence `StartArgs::targets`' own help states, one case per
/// tier.
#[test]
fn a_start_target_walks_the_precedence() {
    assert_eq!(
        matched_names("koji"),
        vec!["koji"],
        "tier 1: a sheep by name"
    );
    assert_eq!(matched_names("1"), vec!["koji"], "tier 1: a sheep by id");
    assert_eq!(
        matched_names("fold:backed"),
        vec!["golbat", "koji"],
        "tier 2: a fold, named as one"
    );
    assert_eq!(
        matched_names("backed"),
        vec!["golbat", "koji"],
        "tier 2: the same fold, named bare"
    );
    assert!(
        matched_names("nosuchthing").is_empty(),
        "and a token that is none of those falls through to the file tiers"
    );
}

/// The only fixture that can tell the two apart:
/// `a_start_target_walks_the_precedence`'s `backed` is a fold and not a
/// sheep, so it would pass under either order.
#[test]
fn a_sheep_outranks_a_fold_of_the_same_name() {
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    let flock = vec![
        ProcessInfo::builder(0, "backed", ProcStatus::Stopped).build(),
        ProcessInfo::builder(1, "koji", ProcStatus::Stopped)
            .fold(Some("backed".to_string()))
            .build(),
    ];
    let selector = ProcessSelector::parse("backed").unwrap();
    let names: Vec<String> = flock_matches(&selector, &flock)
        .into_iter()
        .map(|info| info.name)
        .collect();
    assert_eq!(names, vec!["backed"], "the sheep, not the fold it names");
}

/// A dog is a process an operator installed, not a member of the flock
/// `all` means.
#[test]
fn a_wildcard_passes_a_dog_by_and_an_exact_name_reaches_it() {
    assert_eq!(
        matched_names("all"),
        vec!["golbat", "koji", "rotom"],
        "no dog in the sweep"
    );
    assert_eq!(
        matched_names("log-rotate"),
        vec!["log-rotate"],
        "but naming it outright reaches it"
    );
}

/// `ProcessSelector::is_exact` counts `Instance` as exact. Enumerating
/// `Name` and `Id` instead would send `metrics:0` through the
/// dog-filtering tier.
#[test]
fn an_instance_selector_reaches_a_dog() {
    use shep_core::protocol::{DogSource, ProcessInfo};
    use shep_core::status::ProcStatus;

    let flock = vec![
        ProcessInfo::builder(0, "metrics", ProcStatus::Online)
            .dog(Some(DogSource::BuiltIn))
            .instance(Some(0))
            .build(),
        ProcessInfo::builder(1, "metrics", ProcStatus::Online)
            .dog(Some(DogSource::BuiltIn))
            .instance(Some(1))
            .build(),
    ];
    let selector = ProcessSelector::parse("metrics:0").unwrap();
    let ids: Vec<u32> = flock_matches(&selector, &flock)
        .into_iter()
        .map(|info| info.id)
        .collect();
    assert_eq!(ids, vec![0], "the named slot, dog or not");
}

/// The escape hatch for a fold that shares a name with a file in the
/// current directory. `/web/` is full of slashes and is a regex, so the
/// rule is on the parsed form rather than on the raw token.
#[test]
fn a_token_with_a_path_separator_is_never_a_name() {
    let path = ProcessSelector::parse("./backed").unwrap();
    assert!(
        !is_reachable_as_a_name(&path),
        "./backed can only be a file"
    );
    let bare = ProcessSelector::parse("backed").unwrap();
    assert!(is_reachable_as_a_name(&bare), "backed may be either");
    let regex = ProcessSelector::parse("/web/").unwrap();
    assert!(
        is_reachable_as_a_name(&regex),
        "a regex is not a name, so the separator rule does not apply to it"
    );
}

/// A bare name or id carries no marker and may have meant a filename, so
/// it keeps the message that names every tier.
#[test]
fn a_selector_that_matched_nothing_is_reported_as_a_selector() {
    let miss = |target: &str, flock: &[shep_core::protocol::ProcessInfo]| {
        selector_miss(target, &ProcessSelector::parse(target).unwrap(), flock)
    };
    let empty: [shep_core::protocol::ProcessInfo; 0] = [];

    assert_eq!(
        miss("fold:typo", &empty).as_deref(),
        Some("no sheep is in a fold called typo")
    );
    assert_eq!(
        miss("zz-*", &empty).as_deref(),
        Some("no sheep matched zz-*")
    );
    assert_eq!(
        miss("all", &empty).as_deref(),
        Some("the flock is empty; there is nothing to start")
    );
    assert_eq!(
        miss("koji", &empty),
        None,
        "a bare name may still be a file, so the unresolvable message stands"
    );
    assert_eq!(miss("11", &empty), None, "and so may a bare id");
}

/// The fixture is `web`, `api`, `web`: sorted, the two `web` rows would
/// be adjacent and comparing against the previous name alone would pass.
/// First-seen order is asserted too, since the notice reads as a list.
#[test]
fn unique_names_drops_a_duplicate_that_is_not_adjacent() {
    use shep_core::status::ProcStatus;

    let rows = [
        ProcessInfo::builder(0, "web", ProcStatus::Stopped).build(),
        ProcessInfo::builder(1, "api", ProcStatus::Stopped).build(),
        ProcessInfo::builder(2, "web", ProcStatus::Stopped).build(),
    ];
    let borrowed: Vec<&ProcessInfo> = rows.iter().collect();
    assert_eq!(
        unique_names(&borrowed),
        vec!["web", "api"],
        "one entry per name, in the order each was first seen"
    );
}

/// Both halves in one case: a build that always says "no sheep" and one
/// that always says "empty" each pass half of it.
#[test]
fn an_all_that_matched_nothing_counts_sheep_and_not_dogs() {
    use shep_core::protocol::{DogSource, ProcessInfo};
    use shep_core::status::ProcStatus;

    let all = ProcessSelector::parse("all").unwrap();
    let dogs_only = [ProcessInfo::builder(0, "log-rotate", ProcStatus::Online)
        .dog(Some(DogSource::BuiltIn))
        .build()];

    let said = selector_miss("all", &all, &dogs_only).expect("a miss is reported");
    assert!(
        said.starts_with("no sheep in the flock"),
        "a flock holding only dogs is not empty: {said}"
    );
    assert!(
        said.contains("`shep dogs`"),
        "and it says where the rows an operator can see came from: {said}"
    );

    let empty: [ProcessInfo; 0] = [];
    assert_eq!(
        selector_miss("all", &all, &empty).as_deref(),
        Some("the flock is empty; there is nothing to start"),
        "with nothing registered at all, empty is the honest word"
    );
}

/// Driven through the verb rather than `selector_miss`: the mapping from
/// "matched nothing" to an exit code lives in `load_one`.
#[tokio::test]
async fn a_start_on_an_empty_fold_exits_not_found_without_a_start_request() {
    use shep_client::testing::fake_client_on;

    let dir = tempfile::tempdir().unwrap();
    let address = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_on(&address).await;
    daemon.reply_to_list(a_foldable_flock());

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        start(
            &client,
            &mut streams,
            &start_args("fold:typo"),
            None,
            &BTreeMap::new(),
        )
        .await
    };

    assert_eq!(code, ExitCode::NotFound);
    let said = String::from_utf8(err).unwrap();
    assert!(
        said.contains("no sheep is in a fold called typo"),
        "the refusal names the fold, not a file: {said}"
    );
    assert!(
        !said.contains("existing path"),
        "and never mentions a path nobody asked about: {said}"
    );
    assert!(out.is_empty(), "stdout stays empty on a failure");
}

/// Someone who typed `start` did not ask for their live service to be
/// replaced.
#[tokio::test]
async fn a_target_naming_a_running_sheep_leaves_it_alone() {
    use shep_client::testing::fake_client_on;
    use shep_core::status::ProcStatus;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_on(&path).await;
    daemon.reply_to_list(vec![
        shep_core::protocol::ProcessInfo::builder(7, "api-auth", ProcStatus::Online).build(),
    ]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        start(
            &client,
            &mut streams,
            &start_args("api-auth"),
            None,
            &BTreeMap::new(),
        )
        .await
    };

    assert_eq!(code, ExitCode::Success);
    let said = String::from_utf8_lossy(&err);
    assert!(said.contains("already"), "the operator is told: {said}");
    assert!(
        said.contains("shep restart api-auth"),
        "and pointed at the verb that would replace it: {said}"
    );
}
