//! `Describe`'s `vcs`: filled from a sheep's own `cwd`, on `Describe` alone,
//! and only on a build with the feature.

use super::*;
use std::fs;
use std::path::Path;

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

/// A directory that is a checkout on a detached head at [`REVISION`].
fn checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir(dir.path().join(".git")).expect("git dir");
    fs::write(dir.path().join(".git/HEAD"), format!("{REVISION}\n")).expect("HEAD");
    dir
}

/// Starts `web` with `instances` instances in `cwd`, asserting the start landed.
async fn start_web_in(h: &Harness, cwd: Option<&Path>, instances: u32) {
    let mut config = AppConfig::minimal("web", "./srv");
    config.cwd = cwd.map(|dir| dir.display().to_string());
    config.instances = instances;
    let started =
        reply_of(dispatch(envelope(1, Request::Start { apps: vec![config] }), &h.ctx).await);
    assert!(started.result.is_ok(), "{:?}", started.result);
}

#[cfg(feature = "vcs")]
mod enabled {
    use super::*;
    use crate::rpc::enrichment::{VcsReads, fill_vcs};
    use crate::vcs::Absent;
    use shep_core::protocol::Vcs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    // The clock is real in the cases that read files: a paused clock would
    // jump past the read timeout while the blocking pool is still working.
    #[tokio::test]
    async fn describe_fills_vcs_from_a_sheeps_cwd() {
        let dir = checkout();
        let h = harness(vec![ProcScript::never_exits()]);
        start_web_in(&h, Some(dir.path()), 1).await;

        let described = describe(&h.ctx, 2, SelectorSpec::Name("web".to_string())).await;

        assert_eq!(described[0].vcs, Some(Vcs::new(REVISION)));
    }

    // The test process runs inside the repository, normally a checkout, so a
    // reader that fell back to the shepherd's own directory would report one.
    #[tokio::test]
    async fn describe_leaves_vcs_none_for_a_sheep_with_no_cwd() {
        let h = harness(vec![ProcScript::never_exits()]);
        start_web_in(&h, None, 1).await;

        let described = describe(&h.ctx, 2, SelectorSpec::Name("web".to_string())).await;

        assert_eq!(described[0].vcs, None);
    }

    // The same, against a reader that calls every directory a checkout, so
    // the case does not depend on where the tests run.
    #[tokio::test]
    async fn a_row_with_no_cwd_is_never_handed_to_the_reader() {
        let h = harness(vec![ProcScript::never_exits()]);
        start_web_in(&h, None, 1).await;
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);

        let rows = fill_vcs(
            h.ctx.supervisor.list().await,
            &h.ctx.supervisor.working_dirs().await.expect("dirs"),
            Duration::from_secs(5),
            &VcsReads::default(),
            Arc::new(move |_: &Path| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(Vcs::new(REVISION))
            }),
        )
        .await;

        assert_eq!(rows[0].vcs, None);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn two_instances_of_one_cwd_are_read_once() {
        let h = harness(vec![ProcScript::never_exits(), ProcScript::never_exits()]);
        start_web_in(&h, Some(&PathBuf::from("/srv/web")), 2).await;
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);

        let rows = fill_vcs(
            h.ctx.supervisor.list().await,
            &h.ctx.supervisor.working_dirs().await.expect("dirs"),
            Duration::from_secs(5),
            &VcsReads::default(),
            Arc::new(move |_: &Path| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(Vcs::new(REVISION))
            }),
        )
        .await;

        assert_eq!(rows.len(), 2, "both instances must be listed: {rows:?}");
        assert!(rows.iter().all(|row| row.vcs == Some(Vcs::new(REVISION))));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn list_flock_never_reads_a_checkout() {
        let dir = checkout();
        let h = harness(vec![ProcScript::never_exits()]);
        start_web_in(&h, Some(dir.path()), 1).await;
        // The fixture is readable: a `Describe` finds it.
        let described = describe(&h.ctx, 2, SelectorSpec::Name("web".to_string())).await;
        assert!(described[0].vcs.is_some(), "{described:?}");

        let rows = list_flock(&h.ctx, 3).await;

        assert!(
            rows.iter().all(|row| row.vcs.is_none()),
            "ListFlock must not read a checkout: {rows:?}"
        );
    }

    // Real clock, because tokio never auto-advances a paused one while a
    // blocking task is running. The reader cannot finish before `release`
    // drops, so the short timeout is the only way this returns. Dropping
    // `release` then frees the parked blocking thread.
    #[tokio::test]
    async fn a_read_that_outlasts_the_timeout_leaves_vcs_none() {
        let h = harness(vec![ProcScript::never_exits()]);
        start_web_in(&h, Some(&PathBuf::from("/srv/web")), 1).await;
        let (release, parked) = std::sync::mpsc::channel::<()>();
        let parked = std::sync::Mutex::new(parked);

        let rows = fill_vcs(
            h.ctx.supervisor.list().await,
            &h.ctx.supervisor.working_dirs().await.expect("dirs"),
            Duration::from_millis(50),
            &VcsReads::default(),
            Arc::new(move |_: &Path| {
                let _ = parked.lock().expect("parked").recv();
                Err::<Vcs, _>(Absent::Unreadable)
            }),
        )
        .await;

        assert_eq!(rows[0].vcs, None);
        drop(release);
    }

    // A timed-out read still holds its blocking thread, so asking again must
    // not start a second one. The first reader parks until `release` drops.
    #[tokio::test]
    async fn a_directory_whose_read_is_stuck_is_not_read_again() {
        let h = harness(vec![ProcScript::never_exits()]);
        start_web_in(&h, Some(&PathBuf::from("/srv/web")), 1).await;
        let dirs = h.ctx.supervisor.working_dirs().await.expect("dirs");
        let reads = VcsReads::default();
        let (release, parked) = std::sync::mpsc::channel::<()>();
        let parked = std::sync::Mutex::new(parked);
        let started = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&started);

        let reader = Arc::new(move |_: &Path| {
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = parked.lock().expect("parked").recv();
            Err::<Vcs, _>(Absent::Unreadable)
        });
        for _ in 0..3 {
            let rows = fill_vcs(
                h.ctx.supervisor.list().await,
                &dirs,
                Duration::from_millis(50),
                &reads,
                Arc::clone(&reader),
            )
            .await;
            assert_eq!(rows[0].vcs, None);
        }

        assert_eq!(started.load(Ordering::SeqCst), 1);
        drop(release);
    }
}

#[cfg(not(feature = "vcs"))]
#[tokio::test]
async fn without_the_feature_describe_never_sets_vcs() {
    let dir = checkout();
    let h = harness(vec![ProcScript::never_exits()]);
    start_web_in(&h, Some(dir.path()), 1).await;

    let described = describe(&h.ctx, 2, SelectorSpec::Name("web".to_string())).await;

    assert_eq!(described[0].vcs, None);
}
