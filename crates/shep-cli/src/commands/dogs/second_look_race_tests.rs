//! `shep enable` while another invocation changes the same dog's grants: the
//! answer must not write over what that invocation did.

use std::io::{BufRead, Cursor, Read};

use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;

use super::consent::Input;
use super::enable::enable_asking;
use super::test_support::streams;
use crate::commands::shep_toml::{GrantRecord, ShepToml};
use crate::exit::ExitCode;

/// An operator's answer that, as it is read, lets another invocation edit
/// `shep.toml` first.
struct AnswerAfter<F: FnMut() + Send> {
    meanwhile: Option<F>,
    typed: Cursor<&'static [u8]>,
}

impl<F: FnMut() + Send> Read for AnswerAfter<F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.typed.read(buf)
    }
}

impl<F: FnMut() + Send> BufRead for AnswerAfter<F> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if let Some(mut meanwhile) = self.meanwhile.take() {
            meanwhile();
        }
        self.typed.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.typed.consume(amt);
    }
}

fn declared() -> Vec<String> {
    crate::dog::builtin_manifest("bark")
        .expect("bark declares itself")
        .permissions
        .into_iter()
        .map(|permission| permission.name)
        .collect()
}

fn record(paths: &ShepPaths) -> Option<GrantRecord> {
    ShepToml::read_only(&paths.daemon_config)
        .unwrap()
        .grant_record("bark")
        .unwrap()
}

#[tokio::test]
async fn a_revoke_made_while_the_question_is_open_is_not_undone_by_the_yes() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(
        &paths.daemon_config,
        "[daemon]\nenabled_dogs = [\"bark\"]\n",
    )
    .unwrap();
    let (taken, kept) = declared()
        .split_first()
        .map(|(first, rest)| (first.clone(), rest.to_vec()))
        .unwrap();
    ShepToml::try_edit(&paths.daemon_config, |cfg| {
        cfg.set_grant_record(
            "bark",
            &GrantRecord::at_upgrade(Grants::from_names(declared())),
        )
    })
    .unwrap();
    let revoked = GrantRecord {
        revoked: Grants::from_names([taken]),
        ..GrantRecord::reviewed(Grants::from_names(kept))
    };
    let elsewhere = paths.clone();
    let after = revoked.clone();
    let mut input = Input::reading(
        true,
        AnswerAfter {
            meanwhile: Some(move || {
                ShepToml::try_edit(&elsewhere.daemon_config, |cfg| {
                    cfg.set_grant_record("bark", &after)
                })
                .unwrap();
            }),
            typed: Cursor::new(b"y\n"),
        },
    );
    let mut out = Vec::new();
    let mut err = Vec::new();

    let code = enable_asking(
        &mut streams(&mut out, &mut err),
        &paths,
        "bark",
        false,
        &mut input,
    )
    .await;

    let said = String::from_utf8(err).unwrap();
    assert_eq!(code, ExitCode::Failure, "{said}");
    assert!(
        said.contains("changed while the question was open"),
        "{said}"
    );
    assert_eq!(record(&paths), Some(revoked));
}
