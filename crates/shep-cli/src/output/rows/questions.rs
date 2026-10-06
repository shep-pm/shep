//! The questions the flock has put to the operator: [`QuestionRows`] for
//! `shep answer` with no arguments, and [`DescribedQuestionRows`] for
//! `describe`'s section on one sheep.

use serde::Serialize;
use shep_core::protocol::{OpenQuestion, ProcessInfo, Takes};

use crate::output::{Render, human_duration};
use crate::terminal_safe::sanitise;

use super::replies::preview_body;

/// One open question, with the sheep that asked it.
///
/// `text` is the question in full; the table cuts it, the JSON does not.
#[derive(Debug, Serialize)]
pub struct QuestionRow {
    /// Id of the sheep that asked.
    pub id: u32,
    /// Name of the sheep that asked.
    pub name: String,
    /// The id the sheep gave the question.
    pub question: String,
    /// What the sheep asked.
    pub text: String,
    /// The kind of answer it takes: `yes-no`, `text`, or `unknown` for a
    /// kind this build predates.
    pub takes: String,
    /// When the shepherd received the question, in Unix milliseconds.
    pub asked_at_ms: u64,
    /// How long ago that was, as of building the row.
    #[serde(skip)]
    age_ms: u64,
    /// Whether the app runs more than one instance, so the name alone does
    /// not say which asked.
    #[serde(skip)]
    instanced: bool,
}

impl QuestionRow {
    fn new(sheep: &ProcessInfo, open: &OpenQuestion, instanced: bool, now_ms: u64) -> Self {
        Self {
            id: sheep.id,
            name: sheep.name.clone(),
            question: open.question.to_string(),
            text: open.text.to_string(),
            takes: takes_word(open.takes).to_string(),
            asked_at_ms: open.asked_at_ms,
            age_ms: now_ms.saturating_sub(open.asked_at_ms),
            instanced,
        }
    }

    fn sheep_cell(&self) -> String {
        if self.instanced {
            format!("{}#{}", self.name, self.id)
        } else {
            self.name.clone()
        }
    }

    /// The cells the two tables share, after SHEEP.
    fn shared_cells(&self) -> [String; 4] {
        [
            self.question.clone(),
            self.takes.clone(),
            human_duration(self.age_ms),
            // The app worded it, so a bidi override goes too.
            sanitise(&preview_body(&self.text)).0,
        ]
    }
}

/// The wire word for `takes`; a kind this build does not know reads
/// `unknown`.
fn takes_word(takes: Takes) -> &'static str {
    match takes {
        Takes::YesNo => "yes-no",
        Takes::Text => "text",
        _ => "unknown",
    }
}

/// Every open question in the flock: flock order, then oldest first.
///
/// `transparent`, so `--format json` is an array of [`QuestionRow`].
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct QuestionRows(pub Vec<QuestionRow>);

impl QuestionRows {
    /// The open questions of `flock` as of `now_ms` (Unix milliseconds).
    #[must_use]
    pub fn from_flock(flock: &[ProcessInfo], now_ms: u64) -> Self {
        let mut rows = Vec::new();
        for sheep in flock {
            let Some(questions) = &sheep.questions else {
                continue;
            };
            let instanced = flock
                .iter()
                .filter(|other| other.name == sheep.name)
                .count()
                > 1;
            let mut ordered: Vec<&OpenQuestion> = questions.iter().collect();
            ordered.sort_by_key(|open| open.asked_at_ms);
            rows.extend(
                ordered
                    .into_iter()
                    .map(|open| QuestionRow::new(sheep, open, instanced, now_ms)),
            );
        }
        Self(rows)
    }
}

impl Render for QuestionRows {
    fn headers() -> &'static [&'static str] {
        &["SHEEP", "QUESTION", "TAKES", "ASKED", "TEXT"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|row| {
                let mut cells = vec![row.sheep_cell()];
                cells.extend(row.shared_cells());
                cells
            })
            .collect()
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "SHEEP" => "name",
            "QUESTION" => "question",
            "TAKES" => "takes",
            "ASKED" => "asked_at_ms",
            "TEXT" => "text",
            other => panic!("QuestionRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[
        // The SHEEP cell appends it only for an app with several instances.
        "id",
    ];

    // Parallel to `headers()`. The text is what the operator answers, so it
    // stays; TAKES and ASKED go first.
    const PRIORITIES: &'static [u8] = &[0, 0, 7, 6, 0];
}

/// One sheep's open questions, as `describe`'s section: [`QuestionRows`]
/// without the SHEEP column.
///
/// Not read as JSON: `describe --format json` carries `questions` on each
/// `ProcessInfo`. It exists to reach [`render_table`](crate::output::render_table).
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct DescribedQuestionRows(pub Vec<QuestionRow>);

impl DescribedQuestionRows {
    /// `sheep`'s open questions as of `now_ms`, oldest first.
    #[must_use]
    pub fn of(sheep: &ProcessInfo, now_ms: u64) -> Self {
        let mut rows = QuestionRows::from_flock(std::slice::from_ref(sheep), now_ms).0;
        for row in &mut rows {
            row.instanced = false;
        }
        Self(rows)
    }
}

impl Render for DescribedQuestionRows {
    fn headers() -> &'static [&'static str] {
        &["QUESTION", "TAKES", "ASKED", "TEXT"]
    }

    fn rows(&self) -> Vec<Vec<String>> {
        self.0
            .iter()
            .map(|row| row.shared_cells().to_vec())
            .collect()
    }

    /// # Panics
    /// If `header` is not one of `Self::headers()`'s own values.
    #[track_caller]
    fn json_key_for(header: &str) -> &'static str {
        match header {
            "QUESTION" => "question",
            "TAKES" => "takes",
            "ASKED" => "asked_at_ms",
            "TEXT" => "text",
            other => panic!("DescribedQuestionRows::headers() does not include {other:?}"),
        }
    }

    const JSON_ONLY: &'static [&'static str] = &[
        // The section's heading names the sheep.
        "id", "name",
    ];

    const PRIORITIES: &'static [u8] = &[0, 7, 6, 0];
}

#[cfg(test)]
mod tests {
    use shep_core::protocol::{QuestionId, QuestionText};
    use shep_core::status::ProcStatus;

    use super::super::tests::assert_no_drift;
    use super::*;

    fn open(question: &str, text: &str, takes: Takes, asked_at_ms: u64) -> OpenQuestion {
        OpenQuestion::new(
            QuestionId::new(question).unwrap(),
            QuestionText::new(text).unwrap(),
            takes,
            asked_at_ms,
        )
    }

    fn asking(id: u32, name: &str, questions: Vec<OpenQuestion>) -> ProcessInfo {
        ProcessInfo::builder(id, name, ProcStatus::Online)
            .questions(Some(questions))
            .build()
    }

    fn one(text: &str) -> QuestionRows {
        let flock = [asking(
            3,
            "web",
            vec![open("q1", text, Takes::YesNo, 1_000)],
        )];
        QuestionRows::from_flock(&flock, 61_000)
    }

    #[test]
    fn the_list_names_the_sheep_question_kind_age_and_text() {
        let rows = one("Ship it?").rows();
        assert_eq!(
            rows,
            vec![vec![
                "web".to_string(),
                "q1".to_string(),
                "yes-no".to_string(),
                human_duration(60_000),
                "Ship it?".to_string(),
            ]]
        );
    }

    #[test]
    fn a_sheep_with_several_instances_carries_its_id() {
        let flock = [
            asking(3, "web", vec![open("q1", "a", Takes::Text, 1)]),
            asking(4, "web", vec![open("q1", "b", Takes::Text, 2)]),
        ];
        let rows = QuestionRows::from_flock(&flock, 10).rows();
        assert_eq!(rows[0][0], "web#3");
        assert_eq!(rows[1][0], "web#4");
    }

    #[test]
    fn flock_order_comes_first_then_oldest_first() {
        let flock = [
            asking(
                1,
                "b",
                vec![
                    open("late", "x", Takes::Text, 50),
                    open("early", "x", Takes::Text, 10),
                ],
            ),
            asking(2, "a", vec![open("only", "x", Takes::Text, 5)]),
            ProcessInfo::builder(3, "quiet", ProcStatus::Online).build(),
        ];
        let rows = QuestionRows::from_flock(&flock, 100).rows();
        let order: Vec<&str> = rows.iter().map(|row| row[1].as_str()).collect();
        assert_eq!(order, ["early", "late", "only"]);
    }

    #[test]
    fn a_long_text_is_cut_at_eighty_characters_with_dots() {
        let text = "x".repeat(120);
        let cell = one(&text).rows()[0][4].clone();
        assert_eq!(cell, format!("{}...", "x".repeat(80)));
    }

    #[test]
    fn a_newline_in_the_text_is_escaped() {
        assert_eq!(one("one\ntwo").rows()[0][4], "one\\ntwo");
    }

    #[test]
    fn the_json_carries_the_full_text_and_the_documented_keys() {
        let text = "y".repeat(120);
        let json = serde_json::to_value(one(&text)).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{
                "id": 3,
                "name": "web",
                "question": "q1",
                "text": text,
                "takes": "yes-no",
                "asked_at_ms": 1_000,
            }])
        );
    }

    #[test]
    fn question_rows_do_not_drift() {
        assert_no_drift(&one("Ship it?"), |j| &j[0], &["ASKED"]);
    }

    #[test]
    fn described_question_rows_do_not_drift() {
        let sheep = asking(3, "web", vec![open("q1", "Ship it?", Takes::Text, 1_000)]);
        assert_no_drift(
            &DescribedQuestionRows::of(&sheep, 2_000),
            |j| &j[0],
            &["ASKED"],
        );
    }

    #[test]
    fn the_describe_section_drops_the_sheep_column_even_for_an_instanced_app() {
        let sheep = asking(3, "web", vec![open("q1", "Ship it?", Takes::Text, 1_000)]);
        let rows = DescribedQuestionRows::of(&sheep, 2_000).rows();
        assert_eq!(rows[0].len(), 4);
        assert_eq!(rows[0][0], "q1");
    }
}
