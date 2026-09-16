//! Push and persist: grammar and size caps, the `persist` key, a
//! `describe` sweep past a dog, and enabling a dog over a sheep's name.

use super::*;

/// Sends `entries` for `namespace` in `production` and returns the reply.
async fn push(ctx: &RpcContext, id: u64, namespace: &str, entries: &[(&str, &str)]) -> Reply {
    reply_of(
        dispatch(
            envelope(
                id,
                Request::PutSecrets {
                    namespace: namespace.to_string(),
                    environment: "production".to_string(),
                    entries: entries
                        .iter()
                        .map(|(key, value)| ((*key).to_string(), (*value).to_string().into()))
                        .collect(),
                },
            ),
            ctx,
        )
        .await,
    )
}

/// The registry the arm writes is the one the supervisor resolves
/// against, so this asserts on the shared handle and not on the count
/// alone: an arm that answered `accepted` off its own request without
/// storing anything would pass on the reply and fail here.
#[tokio::test(start_paused = true)]
async fn a_push_lands_in_the_registry_the_supervisor_reads() {
    let h = harness(vec![]);
    let reply = push(&h.ctx, 1, "vercel", &[("API_KEY", "sk_live"), ("B", "2")]).await;
    assert_eq!(reply.result.unwrap(), Response::SecretsPut { accepted: 2 });

    let snapshot = h.ctx.provider_secrets.snapshot();
    assert_eq!(
        snapshot.values["vercel"]["API_KEY"]["production"],
        "sk_live"
    );
    assert!(snapshot.pushed["vercel"].contains("production"));
}

/// A namespace with a `/` in it is the exact shape `SecretRef::parse`
/// splits on, so storing under it would answer `accepted` for values
/// no `{{secret:...}}` reference could ever name.
#[tokio::test(start_paused = true)]
async fn a_namespace_outside_the_grammar_is_refused_and_stores_nothing() {
    let h = harness(vec![]);
    let reply = push(&h.ctx, 1, "ver/cel", &[("A", "1")]).await;

    let Err(err) = reply.result else {
        panic!("a namespace carrying a separator must be refused")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig, "{err:?}");
    assert!(h.ctx.provider_secrets.pushed().is_empty());
}

/// A key with a `/` in it is as unreachable as a namespace with one:
/// `SecretRef::parse` splits on the separator, so `{{secret:ns/A/B}}`
/// names a namespace `ns` and a key `A/B` that `parse` refuses. The two
/// names were checked and the keys were not.
#[tokio::test(start_paused = true)]
async fn an_entry_key_outside_the_grammar_refuses_the_whole_push() {
    let h = harness(vec![]);
    let reply = push(&h.ctx, 1, "vercel", &[("GOOD", "1"), ("A/B", "2")]).await;

    let Err(err) = reply.result else {
        panic!("a key carrying a separator must be refused")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig, "{err:?}");
    assert!(err.message.contains("A/B"), "names the key: {err:?}");
    assert!(
        h.ctx.provider_secrets.pushed().is_empty(),
        "the whole push is refused, not the one key"
    );
}

/// `MAX_VALUE_BYTES` guards `secrets::set`, so an operator cannot put a
/// blob in the store. Without the same cap here a socket peer sets the
/// shepherd's memory, and with `persist` on, what it sent is
/// re-serialized and fsynced whole on every later push.
#[tokio::test(start_paused = true)]
async fn an_oversized_value_refuses_the_whole_push_without_quoting_it() {
    let h = harness(vec![]);
    let huge = "x".repeat(shep_core::secrets::MAX_VALUE_BYTES + 1);
    let reply = push(&h.ctx, 1, "vercel", &[("BIG", &huge)]).await;

    let Err(err) = reply.result else {
        panic!("a value over the cap must be refused")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig, "{err:?}");
    assert!(err.message.contains("BIG"), "names the key: {err:?}");
    assert!(
        !err.message.contains("xxxx"),
        "never the value itself: {err:?}"
    );
    assert!(h.ctx.provider_secrets.pushed().is_empty());
}

/// The cap is a ceiling, not a refusal of anything long: a 4096-bit RSA
/// key in PEM is 3272 bytes, which is what the limit was sized for.
#[tokio::test(start_paused = true)]
async fn a_value_at_the_cap_is_accepted() {
    let h = harness(vec![]);
    let big = "x".repeat(shep_core::secrets::MAX_VALUE_BYTES);
    assert!(
        push(&h.ctx, 1, "vercel", &[("BIG", &big)])
            .await
            .result
            .is_ok()
    );
}

/// The same refusal for the other name, since an environment outside
/// the grammar is just as unreachable as a namespace.
#[tokio::test(start_paused = true)]
async fn an_environment_outside_the_grammar_is_refused() {
    let h = harness(vec![]);
    let reply = reply_of(
        dispatch(
            envelope(
                1,
                Request::PutSecrets {
                    namespace: "vercel".to_string(),
                    environment: String::new(),
                    entries: BTreeMap::new(),
                },
            ),
            &h.ctx,
        )
        .await,
    );

    let Err(err) = reply.result else {
        panic!("an empty environment must be refused")
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig, "{err:?}");
}

/// The arm reads `persist` from the file on every push, so a dog whose
/// operator turned it off never has its values written to disk. Asserts
/// on the file rather than on the reply: the push succeeds either way,
/// and the whole point of the setting is what is NOT there.
#[tokio::test(start_paused = true)]
async fn persist_false_in_dogs_toml_keeps_the_values_off_disk() {
    let h = harness(vec![]);
    std::fs::write(&h.ctx.dogs_config, "[vercel]\npersist = false\n").unwrap();

    assert!(
        push(&h.ctx, 1, "vercel", &[("A", "1")])
            .await
            .result
            .is_ok()
    );

    assert!(!h.ctx.paths.secrets_cache.exists());
    assert!(h.ctx.provider_secrets.pushed().contains_key("vercel"));
}

/// The default, and the half the test above cannot prove: a dog whose
/// section says nothing gets a cache.
#[tokio::test(start_paused = true)]
async fn a_dog_that_says_nothing_about_persist_gets_a_cache() {
    let h = harness(vec![]);
    assert!(
        push(&h.ctx, 1, "vercel", &[("A", "1")])
            .await
            .result
            .is_ok()
    );
    assert!(h.ctx.paths.secrets_cache.exists());
}

/// Both halves: a filter that excluded dogs outright would leave `shep
/// describe bark` unable to answer at all, and a listing that includes
/// them puts a row in the flock table with nowhere to go.
#[tokio::test(start_paused = true)]
async fn describe_sweeps_past_a_dog_and_still_answers_when_one_is_named() {
    let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal("web", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(started.result.is_ok(), "{:?}", started.result);
    let dog = enable_dog(&h.ctx, 2, "bark").await;

    let swept = reply_of(
        dispatch(
            envelope(
                3,
                Request::Describe {
                    selector: SelectorSpec::All,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::Described(hits)) = swept.result else {
        panic!("expected Described, got {:?}", swept.result)
    };
    assert_eq!(
        hits.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["web"],
        "`all` is the flock, not the kennel"
    );

    let named = reply_of(
        dispatch(
            envelope(
                4,
                Request::Describe {
                    selector: SelectorSpec::Name("bark".to_string()),
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Ok(Response::Described(hits)) = named.result else {
        panic!("expected Described, got {:?}", named.result)
    };
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, dog.id);
}

/// `start_dog` is idempotent by name, so the squatter comes back as an
/// `Ok`, and a caller that trusted it would print "bark enabled", write
/// `enabled_dogs = ["bark"]`, and never have a dog.
#[tokio::test(start_paused = true)]
async fn enabling_a_dog_over_a_sheeps_name_is_refused_rather_than_faked() {
    let h = harness(vec![ProcScript::never_exits()]);
    let started = reply_of(
        dispatch(
            envelope(
                1,
                Request::Start {
                    apps: vec![AppConfig::minimal("bark", "./srv")],
                },
            ),
            &h.ctx,
        )
        .await,
    );
    assert!(started.result.is_ok(), "{:?}", started.result);

    let reply = reply_of(
        dispatch(
            envelope(
                2,
                Request::EnableDog {
                    name: "bark".to_string(),
                    source: DogSource::BuiltIn,
                },
            ),
            &h.ctx,
        )
        .await,
    );
    let Err(err) = reply.result else {
        panic!("expected a refusal, got {:?}", reply.result)
    };
    assert_eq!(err.code, RpcErrorCode::InvalidConfig);
    assert!(
        err.message.contains("bark"),
        "the refusal names the collision: {}",
        err.message
    );
    let listed = h.ctx.supervisor.list().await;
    assert_eq!(listed.len(), 1, "nothing was started: {listed:?}");
    assert_eq!(listed[0].dog, None);
}
