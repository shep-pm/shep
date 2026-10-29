//! Retrying a dog's refused spawn through `EnableDog`.

use super::*;

#[tokio::test(start_paused = true)]
async fn describe_reports_the_refused_spawn_without_env_values() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let h = harness(Vec::new());
        let refused = reply_of(
            dispatch(
                envelope(
                    1,
                    Request::EnableDog {
                        name: "kelpie".to_string(),
                        source: DogSource::Adopted {
                            grants: None,
                            path: "/dogs/kelpie".to_string(),
                            channel: false,
                        },
                        env: BTreeMap::from([
                            ("TOKEN".to_string(), "{{secret:MISSING}}".to_string().into()),
                            (
                                "PRIVATE".to_string(),
                                "never-print-this-value".to_string().into(),
                            ),
                        ]),
                    },
                ),
                &h.ctx,
            )
            .await,
        )
        .result
        .unwrap_err();
        let rows = describe(&h.ctx, 2, SelectorSpec::Name("kelpie".to_string())).await;
        assert_eq!(rows[0].status, ProcStatus::Errored);
        let json = serde_json::to_value(&rows).unwrap();
        assert_eq!(json[0]["spawn_failure"], refused.message);
        assert!(refused.message.contains("MISSING"), "{}", refused.message);
        assert!(!json.to_string().contains("never-print-this-value"));
        assert!(!refused.message.contains("never-print-this-value"));
        h.ctx.supervisor.shutdown().await;
    })
    .await
    .expect("describe must answer after a refused spawn");
}

#[tokio::test(start_paused = true)]
async fn enable_retries_an_errored_dog_from_the_current_entry() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let h = harness(vec![ProcScript::never_exits()]);
        let source = DogSource::Adopted {
            grants: None,
            path: "/dogs/kelpie".to_string(),
            channel: false,
        };
        let refused = reply_of(
            dispatch(
                envelope(
                    1,
                    Request::EnableDog {
                        name: "kelpie".to_string(),
                        source: source.clone(),
                        env: BTreeMap::from([(
                            "TOKEN".to_string(),
                            "{{secret:MISSING}}".to_string().into(),
                        )]),
                    },
                ),
                &h.ctx,
            )
            .await,
        )
        .result
        .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::SpawnFailed);
        let before = list_flock(&h.ctx, 2).await;
        assert_eq!(before[0].status, ProcStatus::Errored);

        let retried = reply_of(
            dispatch(
                envelope(
                    3,
                    Request::EnableDog {
                        name: "kelpie".to_string(),
                        source: source.clone(),
                        env: BTreeMap::new(),
                    },
                ),
                &h.ctx,
            )
            .await,
        )
        .result
        .unwrap();
        let Response::DogStarted(info) = retried else {
            panic!("expected DogStarted")
        };
        assert_eq!(info.id, before[0].id);
        assert_eq!(info.status, ProcStatus::Online);
        assert!(info.pid.is_some());

        let unchanged = reply_of(
            dispatch(
                envelope(
                    4,
                    Request::EnableDog {
                        name: "kelpie".to_string(),
                        source,
                        env: BTreeMap::new(),
                    },
                ),
                &h.ctx,
            )
            .await,
        )
        .result
        .unwrap();
        assert_eq!(unchanged, Response::DogStarted(info));
        h.ctx.supervisor.shutdown().await;
    })
    .await
    .expect("enable must finish the refused spawn and its retry");
}
