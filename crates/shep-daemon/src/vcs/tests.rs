//! The reader against checkouts built by hand, one temp dir per case.

use super::*;
use std::fs;
use tempfile::TempDir;

const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A checkout whose `.git` holds `head` and nothing else.
fn checkout_with_head(head: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let git = dir.path().join(".git");
    fs::create_dir_all(git.join("refs/heads")).expect("git dir");
    fs::write(git.join("HEAD"), head).expect("HEAD");
    dir
}

#[test]
fn a_branch_head_resolves_through_a_loose_ref() {
    let dir = checkout_with_head("ref: refs/heads/main\n");
    fs::write(dir.path().join(".git/refs/heads/main"), format!("{SHA1}\n")).expect("ref");

    assert_eq!(
        read_revision(dir.path()),
        Some(Vcs::new(SHA1).with_branch("main"))
    );
}

#[test]
fn a_branch_head_resolves_through_packed_refs() {
    let dir = checkout_with_head("ref: refs/heads/feature/x\n");
    let other = "f".repeat(40);
    fs::write(
        dir.path().join(".git/packed-refs"),
        format!(
            "# pack-refs with: peeled fully-peeled sorted \n{other} refs/heads/main\n{SHA1} refs/heads/feature/x\n^{other}\n"
        ),
    )
    .expect("packed-refs");

    assert_eq!(
        read_revision(dir.path()),
        Some(Vcs::new(SHA1).with_branch("feature/x"))
    );
}

#[test]
fn a_detached_head_is_its_hex_and_has_no_branch() {
    let dir = checkout_with_head(&format!("{SHA1}\n"));

    assert_eq!(read_revision(dir.path()), Some(Vcs::new(SHA1)));
}

#[test]
fn a_dot_git_file_follows_gitdir_and_commondir_for_a_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let main = dir.path().join("main");
    let linked = main.join(".git/worktrees/wt");
    fs::create_dir_all(main.join(".git/refs/heads")).expect("main git dir");
    fs::create_dir_all(&linked).expect("worktree git dir");
    fs::write(linked.join("HEAD"), "ref: refs/heads/topic\n").expect("HEAD");
    fs::write(linked.join("commondir"), "../..\n").expect("commondir");
    fs::write(main.join(".git/refs/heads/topic"), format!("{SHA1}\n")).expect("ref");
    let tree = dir.path().join("wt");
    fs::create_dir_all(&tree).expect("worktree");
    fs::write(tree.join(".git"), format!("gitdir: {}\n", linked.display())).expect(".git file");

    assert_eq!(
        read_revision(&tree),
        Some(Vcs::new(SHA1).with_branch("topic"))
    );
}

#[test]
fn a_sha256_repository_reads_sixty_four_hex() {
    let dir = checkout_with_head(&format!("{SHA256}\n"));

    assert_eq!(read_revision(dir.path()), Some(Vcs::new(SHA256)));
}

/// The layout `git init --ref-format=reftable` writes: `HEAD` is a fixed
/// placeholder, and the refs are in `reftable/`.
#[test]
fn a_reftable_repository_reads_as_none() {
    let dir = checkout_with_head("ref: refs/heads/.invalid\n");
    fs::create_dir(dir.path().join(".git/reftable")).expect("reftable");

    assert_eq!(read_revision(dir.path()), None);
}

#[test]
fn a_ref_that_escapes_the_git_dir_is_none() {
    // A readable revision outside the git dir, so a reader that followed the
    // name would find one.
    let dir = checkout_with_head("ref: refs/../../outside\n");
    fs::write(dir.path().join("outside"), format!("{SHA1}\n")).expect("outside");
    assert_eq!(read_revision(dir.path()), None);

    let absolute = dir.path().join("outside");
    fs::write(
        dir.path().join(".git/HEAD"),
        format!("ref: {}\n", absolute.display()),
    )
    .expect("HEAD");
    assert_eq!(read_revision(dir.path()), None);

    fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/../x\n").expect("HEAD");
    assert_eq!(read_revision(dir.path()), None);
}

#[test]
fn a_head_that_is_neither_hex_nor_a_ref_is_none() {
    for head in [
        "",
        "main\n",
        "ref: heads/main\n",
        &format!("{}\n", SHA1.to_uppercase()),
        &format!("{}\n", &SHA1[..39]),
        &format!("{SHA1}0\n"),
        &format!("{SHA1} \n"),
    ] {
        let dir = checkout_with_head(head);
        assert_eq!(read_revision(dir.path()), None, "HEAD was {head:?}");
    }
}

#[test]
fn a_file_over_the_read_cap_is_none() {
    let dir = checkout_with_head("ref: refs/heads/main\n");
    let padding = "x".repeat(usize::try_from(READ_CAP).expect("cap fits") + 1);
    fs::write(
        dir.path().join(".git/refs/heads/main"),
        format!("{SHA1}\n{padding}"),
    )
    .expect("ref");

    assert_eq!(read_revision(dir.path()), None);
}

#[test]
fn a_directory_below_the_checkout_walks_up_to_it() {
    let dir = checkout_with_head(&format!("{SHA1}\n"));
    let below = dir.path().join("services/web");
    fs::create_dir_all(&below).expect("below");

    assert_eq!(read_revision(&below), Some(Vcs::new(SHA1)));
}

#[test]
fn a_directory_outside_any_checkout_says_so() {
    let dir = tempfile::tempdir().expect("tempdir");

    assert_eq!(revision_of(dir.path()), Err(Absent::NotACheckout));
}

/// A checkout nearer than another is the one that answers, even when it is
/// the one that cannot.
#[test]
fn the_nearest_dot_git_decides_even_when_it_is_unreadable() {
    let outer = checkout_with_head(&format!("{SHA1}\n"));
    let inner = outer.path().join("inner");
    fs::create_dir_all(inner.join(".git")).expect("inner git dir");

    assert_eq!(revision_of(&inner), Err(Absent::Unreadable));
}
