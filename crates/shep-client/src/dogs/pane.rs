//! Answering `shep.pane`: the one call a dog makes to turn the pane it built
//! into the reply body it writes back on its channel.
//!
//! A dog that declared [`pane`](shep_core::dogs::manifest::Manifest::pane) in
//! its manifest gets this action from lookout, on the same channel it answers
//! `shep trigger` and its stop message on. The action name is
//! [`PANE_ACTION`](shep_core::dogs::pane::PANE_ACTION), and the reply body is
//! whatever this returns.

use shep_core::dogs::pane::Pane;

/// The reply body a dog writes back for a `shep.pane` action.
///
/// `build` makes the pane afresh on each ask, so it shows what is true now.
/// `params` is ignored: the pull takes none. A pane that cannot be written,
/// such as a row with a cell short of its columns, answers the line that names
/// the fault, which lookout shows the operator as an unreadable answer.
///
/// # Examples
///
/// ```
/// use shep_client::dogs::{Destructive, PANE_ACTION, Pane, Row, pane_answer};
///
/// fn build() -> Pane {
///     Pane::new()
///         .fields("host", [("VRAM", "20 GiB")])
///         .table("models", ["MODEL", "STATE"], [Row::new("qwen3-32b", ["qwen3-32b", "loaded"])])
///         .row_action("unload", "Unload", Destructive::Yes)
/// }
///
/// // In the dog: `shep_channel::serve().on_action(PANE_ACTION, |params, _| pane_answer(params, build));`
/// assert_eq!(PANE_ACTION, "shep.pane");
/// let body = pane_answer(None, build);
/// assert_eq!(Pane::read(&body), Ok(build()));
/// ```
#[must_use = "the body is what the dog writes back on its channel"]
pub fn pane_answer(params: Option<&str>, build: impl FnOnce() -> Pane) -> String {
    let _ = params;
    build().body().unwrap_or_else(|fault| fault)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use shep_core::dogs::pane::{Destructive, Row, Section};

    use super::*;

    fn built() -> Pane {
        Pane::new()
            .fields("host", [("VRAM", "20 GiB")])
            .table(
                "models",
                ["MODEL", "STATE"],
                [Row::new("qwen3-32b", ["qwen3-32b", "loaded"])],
            )
            .row_action("unload", "Unload", Destructive::Yes)
            .action("pause", "Pause the board", Destructive::No)
    }

    #[test]
    fn the_answer_is_the_json_of_the_built_pane() {
        let body = pane_answer(None, built);

        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["sections"][0]["id"], json!("host"));
        assert_eq!(json["sections"][1]["rows"][0]["key"], json!("qwen3-32b"));
        assert_eq!(json["actions"][0]["action"], json!("pause"));
    }

    #[test]
    fn the_answer_reads_back_to_the_same_sections() {
        let body = pane_answer(None, built);

        let read = Pane::read(&body).expect("shep reads what the dog wrote");
        assert_eq!(read.sections, built().sections);
        assert_eq!(read.actions, built().actions);
        let Section::Table(table) = &read.sections[1] else {
            panic!("the second section is the table");
        };
        assert_eq!(table.row_actions[0].action, "unload");
    }

    /// The pull takes no params, so whatever arrives changes nothing.
    #[test]
    fn params_do_not_change_the_answer() {
        assert_eq!(
            pane_answer(Some(r#"{"row":"x"}"#), built),
            pane_answer(None, built)
        );
    }

    /// A pane the dog's own reader would refuse must not leave the dog as a
    /// body shep cannot tell from a good one.
    #[test]
    fn a_pane_that_cannot_be_written_answers_the_fault_and_never_a_pane() {
        let broken = || Pane::new().table("models", ["MODEL"], [Row::new("a", ["a", "extra"])]);

        let body = pane_answer(None, broken);

        let refused = Pane::read(&body).expect_err("the fault is not a pane");
        assert!(
            refused.contains("section `models` has 1 column"),
            "{refused}"
        );
    }
}
