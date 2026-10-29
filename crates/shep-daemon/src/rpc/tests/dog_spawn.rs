//! Retrying a dog's refused spawn through `EnableDog`.

use super::*;

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
