//! What the operator is shown and what a yes, a no and a missing terminal
//! come to, driven the way `adopt` and `enable` drive it: a manifest in, a
//! reader standing in for the terminal, the words and the verdict out.

use std::io::Cursor;

use shep_core::dogs::manifest::{Manifest, Permission as Declared};
use shep_core::permissions::{Grants, Permission};

use super::consent::{Mode, Verdict, consent, review};
use super::test_support::streams;
use crate::exit::ExitCode;

fn manifest(permissions: &[(&str, bool, &str)]) -> Manifest {
    let mut manifest = Manifest::new("otel", "0.2.0", 12);
    manifest.permissions = permissions
        .iter()
        .map(|(name, required, reason)| Declared::new(*name, *required, *reason))
        .collect();
    manifest
}

/// Runs the whole step, and hands back the verdict with what was said on
/// stderr.
fn run(
    manifest: Option<&Manifest>,
    mode: Mode,
    typed: &str,
) -> (Result<Verdict, ExitCode>, String) {
    let reviewed = match review(manifest) {
        Ok(reviewed) => reviewed,
        Err(refusal) => {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let code = refusal.report(&mut streams(&mut out, &mut err), "otel", "adopted");
            return (Err(code), String::from_utf8(err).unwrap());
        }
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let verdict = consent(
        &mut streams(&mut out, &mut err),
        "otel",
        "adopt",
        &reviewed,
        mode,
        &mut Cursor::new(typed.as_bytes()),
    );
    (verdict, String::from_utf8(err).unwrap())
}

const ASK: Mode = Mode {
    yes: false,
    stdin_is_terminal: true,
};
const YES: Mode = Mode {
    yes: true,
    stdin_is_terminal: false,
};
const PIPED: Mode = Mode {
    yes: false,
    stdin_is_terminal: false,
};

fn two() -> Manifest {
    manifest(&[
        ("flock.read", true, "reads the flock to report on it"),
        ("events:process.", false, "hears when a sheep exits"),
    ])
}

fn both() -> Grants {
    Grants::new([Permission::FlockRead, Permission::Events("process.".into())])
}

#[test]
fn the_prompt_lists_each_permission_with_its_reason_and_says_grants_do_not_confine() {
    let (_, said) = run(Some(&two()), ASK, "y\n");

    assert!(
        said.contains("flock.read (required): reads the flock to report on it"),
        "{said}"
    );
    assert!(
        said.contains("events:process. (optional): hears when a sheep exits"),
        "{said}"
    );
    assert!(
        said.contains("do not confine") && said.contains("runs as you"),
        "{said}"
    );
    assert!(said.contains("[y/N]"), "{said}");
}

#[test]
fn a_yes_grants_everything_declared() {
    let (verdict, _) = run(Some(&two()), ASK, "yes\n");

    assert_eq!(verdict.unwrap(), Verdict::Grant(Some(both())));
}

#[test]
fn a_no_and_an_empty_answer_grant_nothing() {
    for typed in ["n\n", "\n", ""] {
        let (verdict, _) = run(Some(&two()), ASK, typed);

        assert_eq!(verdict.unwrap(), Verdict::Declined, "typed {typed:?}");
    }
}

#[test]
fn dash_dash_yes_grants_the_declared_list_without_asking() {
    let (verdict, said) = run(Some(&two()), YES, "");

    assert_eq!(verdict.unwrap(), Verdict::Grant(Some(both())));
    assert!(!said.contains("[y/N]"), "{said}");
    assert!(
        said.contains("flock.read"),
        "what was granted is still said: {said}"
    );
}

#[test]
fn no_terminal_and_no_yes_refuses_rather_than_granting() {
    let (verdict, said) = run(Some(&two()), PIPED, "y\n");

    assert_eq!(verdict.unwrap_err(), ExitCode::Usage);
    assert!(said.contains("--yes"), "{said}");
    assert!(said.contains("no terminal"), "{said}");
}

#[test]
fn an_unknown_required_permission_refuses_and_names_it() {
    let manifest = manifest(&[
        ("flock.read", true, "r"),
        ("telepathy", true, "reads minds"),
    ]);

    let (verdict, said) = run(Some(&manifest), YES, "");

    assert_eq!(verdict.unwrap_err(), ExitCode::InvalidConfig);
    assert!(said.contains("telepathy"), "{said}");
    assert!(said.contains("requires"), "{said}");
}

#[test]
fn an_unknown_optional_permission_is_dropped_with_a_warning_and_the_rest_granted() {
    let manifest = manifest(&[
        ("flock.read", true, "r"),
        ("telepathy", false, "reads minds"),
    ]);

    let (verdict, said) = run(Some(&manifest), YES, "");

    assert_eq!(
        verdict.unwrap(),
        Verdict::Grant(Some(Grants::new([Permission::FlockRead])))
    );
    assert!(
        said.contains("telepathy") && said.contains("dropped"),
        "{said}"
    );
}

#[test]
fn an_undeclared_dog_prompts_that_it_runs_with_full_access() {
    let (verdict, said) = run(None, ASK, "y\n");

    assert!(
        said.contains("declares nothing, runs with full access"),
        "{said}"
    );
    assert_eq!(verdict.unwrap(), Verdict::Grant(None));
}

#[test]
fn an_undeclared_dog_declined_is_declined_and_refused_without_a_terminal() {
    assert_eq!(run(None, ASK, "n\n").0.unwrap(), Verdict::Declined);
    assert_eq!(run(None, PIPED, "").0.unwrap_err(), ExitCode::Usage);
    assert_eq!(run(None, YES, "").0.unwrap(), Verdict::Grant(None));
}

#[test]
fn a_manifest_declaring_no_permissions_is_granted_nothing_without_a_question() {
    let (verdict, said) = run(Some(&manifest(&[])), PIPED, "");

    assert_eq!(verdict.unwrap(), Verdict::Grant(Some(Grants::default())));
    assert!(!said.contains("[y/N]"), "{said}");
}
