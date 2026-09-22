//! The macro's refusals, pinned against what rustc actually prints.
//!
//! Every message in `dog_config`'s "Compile errors" section has a case here.
//! A refusal with no test is a message free to become unhelpful, and this is
//! the one crate whose entire output is diagnostics.
//!
//! `.stderr` files hold rustc's rendering, so regenerate them with
//! `TRYBUILD=overwrite cargo test -p shep-macros` and read the diff rather
//! than trusting it.

#[test]
fn every_documented_refusal_is_refused_with_its_own_message() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/refusals/*.rs");
}

#[test]
fn the_shapes_the_docs_promise_are_accepted() {
    let t = trybuild::TestCases::new();
    t.pass("tests/accepted/*.rs");
}
