//! The one key/value editor the env rows and the secrets pane share.
//!
//! Both panes list a KEY and a VALUE, so both open the same editor over
//! them: two [`TextBox`]es side by side, one under the cursor, `Tab` and
//! `Shift-Tab` moving between them, `Enter` on VALUE submitting both, and
//! `Esc` dropping the whole thing. What each pane does with what is
//! submitted is its own: an env row files a [`PaneEdit::SetEnv`], a secrets
//! row raises a [`SecretEdit`](crate::lookout::app::SecretEdit).
//!
//! The editor never holds a stored value. Neither pane is ever told one:
//! [`SheepConfigView::new`](shep_core::protocol::SheepConfigView::new) strips
//! `env`, and a secret reaches the screen only through a reveal. So VALUE
//! starts empty on an existing row too, and the only text this type has ever
//! held is what the operator typed.

use shep_core::secrets::MAX_KEY_BYTES;

use super::app::KeyPress;
use super::textbox::TextBox;

/// The sentence a rename with nothing typed in VALUE raises, in both panes.
///
/// Said out loud rather than written silently: no pane is ever told the value
/// a key already holds, so a rename that carried none would set an empty key
/// and remove one that had a value.
pub const EMPTY_RENAME: &str = "a rename needs a value typed: this pane never reads \
     the one the key already holds";

/// A name a shell could expand, which is what an env key has to be: a letter
/// or `_`, then letters, digits or `_`.
///
/// The same rule `shep start web KEY=value` splits its leading assignments on
/// (`commands::lifecycle::resolve`), so a key typed here is one that verb
/// would accept on the command line.
#[must_use]
pub fn is_env_key(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else { return false };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Which key grammar the editor checks its KEY field against.
///
/// Two, because the two panes write two different stores and each one's own
/// module already states its rule: [`shep_core::secrets::is_name`], and
/// [`is_env_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grammar {
    /// A secrets key: [`shep_core::secrets::is_name`].
    Secret,
    /// An env key: [`is_env_key`].
    Env,
}

impl Grammar {
    /// Whether `name` is a key this grammar accepts.
    ///
    /// Not `const`: each half is the store's own rule, and neither of those
    /// functions is `const` either.
    #[must_use]
    pub fn accepts(self, name: &str) -> bool {
        match self {
            Self::Secret => shep_core::secrets::is_name(name),
            Self::Env => is_env_key(name),
        }
    }

    /// The rule a refused KEY names, so an operator is told which half of the
    /// grammar the key broke rather than that it failed. The secrets half is
    /// `shep secret`'s own `--help` wording, kept here rather than copied
    /// beside it.
    #[must_use]
    pub const fn rule(self) -> &'static str {
        match self {
            Self::Secret => {
                "letters, digits, `.`, `_` and `-`, up to 128 bytes, not starting with a dot"
            }
            Self::Env => "a letter or `_`, then letters, digits or `_`",
        }
    }

    /// The longest KEY this grammar accepts, in bytes.
    #[must_use]
    pub const fn key_limit(self) -> usize {
        MAX_KEY_BYTES
    }
}

/// Which of the editor's two fields holds the cursor.
///
/// `Debug` is derived (IR-41): a bare variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The KEY field.
    Key,
    /// The VALUE field.
    Value,
}

/// What the editor was opened on.
///
/// `Debug` is derived (IR-41): a key name is not a value, the same fact
/// [`EditKey`](crate::lookout::edits::EditKey) pins for the pane's change set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The add row: the key does not exist yet.
    Add,
    /// An existing row, named by the key it holds today. A KEY field that ends
    /// up different from this is a rename.
    Under(String),
}

/// Why the editor refused to move or submit, so each pane raises its own
/// sentence over a fact the editor already knows.
///
/// `Debug` is derived (IR-41): a bare variant name, and at most a grammar's own
/// rule sentence, which names no key and no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// KEY holds a name the grammar refuses. Carries the rule it broke.
    Key(&'static str),
    /// A rename with nothing typed in VALUE. See [`EMPTY_RENAME`].
    EmptyValue,
}

impl Refusal {
    /// The sentence a pane raises over this refusal, worded the same in both.
    ///
    /// `key` is the KEY field's own text, which is a name and not a value:
    /// the same thing the secrets pane has always quoted back in a refusal.
    #[must_use]
    pub fn sentence(self, key: &str) -> String {
        match self {
            Self::Key(rule) => format!("{key:?} is not a valid key: {rule}"),
            Self::EmptyValue => EMPTY_RENAME.to_owned(),
        }
    }
}

/// What a keystroke on the editor did, so each pane's write path reads one
/// answer rather than re-deriving which field was open.
///
/// `Debug` is derived (IR-41): no variant carries a key or a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The editor stayed open with the cursor kept where it was.
    Refused(Refusal),
    /// The cursor moved to the other field.
    Moved,
    /// Both fields were submitted.
    Submitted,
}

/// The editor both panes open: a KEY field, a VALUE field, and which one the
/// cursor is on.
///
/// `Debug` is manual and redacted (IR-41), exact-string-tested below. Both
/// buffers print as a character count: the VALUE buffer is the secret being
/// typed, and a secrets row's KEY buffer is a key name no other pane's `Debug`
/// prints either.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyValues {
    grammar: Grammar,
    row: Row,
    key: TextBox,
    value: TextBox,
    focus: Field,
}

/// Prints two character counts, the row kind as a bool, and the focused
/// field's name. See the type doc for why. Exact-string-tested below
/// (`the_editors_debug_prints_two_lengths_and_neither_field`).
impl core::fmt::Debug for KeyValues {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "KeyValues {{ add: {}, key: <{} chars>, value: <{} chars>, focus: {:?} }}",
            matches!(self.row, Row::Add),
            self.key.char_count(),
            self.value.char_count(),
            self.focus,
        )
    }
}

impl KeyValues {
    /// Opens on an add row: KEY first, both fields empty.
    #[must_use]
    pub fn add(grammar: Grammar) -> Self {
        Self {
            grammar,
            row: Row::Add,
            key: TextBox::new(),
            value: TextBox::new(),
            focus: Field::Key,
        }
    }

    /// Opens on an existing row: the cursor at the end of VALUE, which is
    /// empty because no pane is ever told a stored value, and KEY holding
    /// `key` so `Shift-Tab` reaches the key a row is under.
    #[must_use]
    pub fn under(grammar: Grammar, key: &str) -> Self {
        Self {
            grammar,
            row: Row::Under(key.to_owned()),
            key: TextBox::from(key),
            value: TextBox::new(),
            focus: Field::Value,
        }
    }

    /// Which field holds the cursor.
    #[must_use]
    pub const fn focus(&self) -> Field {
        self.focus
    }

    /// The KEY field.
    #[must_use]
    pub fn key(&self) -> &TextBox {
        &self.key
    }

    /// The VALUE field.
    #[must_use]
    pub fn value(&self) -> &TextBox {
        &self.value
    }

    /// The key the editor was opened under, or [`None`] on an add row.
    #[must_use]
    pub fn opened_under(&self) -> Option<&str> {
        match &self.row {
            Row::Add => None,
            Row::Under(key) => Some(key.as_str()),
        }
    }

    /// Whether the KEY field now names a different key from the one the row
    /// was opened under.
    #[must_use]
    pub fn is_rename(&self) -> bool {
        match &self.row {
            Row::Add => false,
            Row::Under(key) => self.key.as_str() != key.as_str(),
        }
    }

    /// A typed character, a backspace or a cursor move, into whichever field
    /// holds the cursor. Any other key is not the box's and is ignored.
    pub fn handle(&mut self, key: KeyPress) {
        match self.focus {
            Field::Key => self.key.handle(key),
            Field::Value => self.value.handle(key),
        }
    }

    /// `Enter`: submit from VALUE, or check KEY and move off it.
    ///
    /// Enter on KEY is the move `Tab` makes: an operator who has just typed a
    /// key means "this one", and the next field is where its value goes.
    pub fn submit(&mut self) -> Outcome {
        match self.focus {
            Field::Key => self.move_focus(Field::Value),
            Field::Value => {
                if self.is_rename() && self.value.is_empty() {
                    return Outcome::Refused(Refusal::EmptyValue);
                }
                Outcome::Submitted
            }
        }
    }

    /// `Tab`, and the `Shift-Tab` that reverses it.
    ///
    /// Two fields, so a step off the end stays where it is: `Tab` on VALUE and
    /// `Shift-Tab` on KEY are the same keystroke an operator makes by reflex,
    /// and neither has anywhere further to go.
    pub fn step(&mut self, back: bool) -> Outcome {
        let to = match (self.focus, back) {
            (Field::Key, false) => Field::Value,
            (Field::Value, true) => Field::Key,
            (Field::Key, true) | (Field::Value, false) => self.focus,
        };
        self.move_focus(to)
    }

    /// Moves the cursor, checking KEY on the way out of it.
    ///
    /// A KEY the grammar refuses keeps the cursor on KEY: the operator is one
    /// backspace away from fixing it, and a cursor that had moved would have
    /// them typing a value for a key that cannot be written.
    fn move_focus(&mut self, to: Field) -> Outcome {
        if self.focus == Field::Key && to == Field::Value {
            if !self.grammar.accepts(self.key.as_str()) {
                return Outcome::Refused(Refusal::Key(self.grammar.rule()));
            }
        }
        self.focus = to;
        Outcome::Moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookout::app::{App, Body, Effect, Msg};
    use crate::lookout::textbox::TextEdit;
    use crate::lookout::view::fixtures;

    fn typed(editor: &mut KeyValues, text: &str) {
        for c in text.chars() {
            editor.handle(KeyPress::TextChar(c));
        }
    }

    #[test]
    fn an_add_row_starts_on_key_with_both_fields_empty() {
        let editor = KeyValues::add(Grammar::Env);
        assert_eq!(editor.focus(), Field::Key);
        assert_eq!(editor.opened_under(), None);
        assert!(editor.key().is_empty());
        assert!(editor.value().is_empty());
    }

    /// The cursor is at the end of the VALUE field an `Enter` opened, which
    /// for a field no pane can seed is the same place as its start.
    #[test]
    fn an_existing_row_starts_on_value_with_the_cursor_at_its_end() {
        let editor = KeyValues::under(Grammar::Env, "DB_HOST");
        assert_eq!(editor.focus(), Field::Value);
        assert_eq!(editor.opened_under(), Some("DB_HOST"));
        assert_eq!(editor.key().as_str(), "DB_HOST");
        assert_eq!(editor.value().cursor(), editor.value().char_count());
    }

    #[test]
    fn tab_and_shift_tab_move_between_the_two_fields() {
        let mut editor = KeyValues::add(Grammar::Env);
        typed(&mut editor, "WEB");

        assert_eq!(editor.step(false), Outcome::Moved);
        assert_eq!(editor.focus(), Field::Value);
        assert_eq!(editor.step(true), Outcome::Moved);
        assert_eq!(editor.focus(), Field::Key);
    }

    /// Two fields, so a step off either end stays put rather than wrapping:
    /// `Tab` on VALUE is a reflex, not a third field.
    #[test]
    fn a_step_off_either_end_stays_on_the_field_it_was_on() {
        let mut editor = KeyValues::add(Grammar::Env);
        assert_eq!(editor.step(true), Outcome::Moved);
        assert_eq!(editor.focus(), Field::Key);

        typed(&mut editor, "WEB");
        editor.step(false);
        assert_eq!(editor.step(false), Outcome::Moved);
        assert_eq!(editor.focus(), Field::Value);
    }

    #[test]
    fn enter_on_value_submits_and_enter_on_key_moves_off_it() {
        let mut editor = KeyValues::under(Grammar::Env, "DB_HOST");
        assert_eq!(editor.submit(), Outcome::Submitted);

        let mut adding = KeyValues::add(Grammar::Env);
        typed(&mut adding, "WEB");
        assert_eq!(adding.submit(), Outcome::Moved);
        assert_eq!(adding.focus(), Field::Value);
        assert_eq!(adding.submit(), Outcome::Submitted);
    }

    /// A typed character lands in the focused field and nowhere else, which
    /// is what keeps the two panes' keymaps identical.
    #[test]
    fn a_typed_character_lands_in_the_focused_field_only() {
        let mut editor = KeyValues::add(Grammar::Env);
        typed(&mut editor, "WEB");
        editor.handle(KeyPress::TextBackspace);
        assert_eq!(editor.key().as_str(), "WE");
        assert!(editor.value().is_empty());

        editor.step(false);
        typed(&mut editor, "value");
        assert_eq!(editor.key().as_str(), "WE");
        assert_eq!(editor.value().as_str(), "value");

        editor.handle(KeyPress::TextEdit(TextEdit::DeleteToStart));
        assert!(editor.value().is_empty());
        assert_eq!(editor.key().as_str(), "WE", "the other field is untouched");
    }

    /// The refusal is raised with the cursor still on KEY, which is what every
    /// caller reads as "the editor stayed open".
    #[test]
    fn a_key_the_grammar_refuses_keeps_the_cursor_on_key() {
        for (grammar, text) in [(Grammar::Env, "9BAD"), (Grammar::Secret, ".bad")] {
            let mut editor = KeyValues::add(grammar);
            typed(&mut editor, text);
            let refusal = Refusal::Key(grammar.rule());

            assert_eq!(editor.submit(), Outcome::Refused(refusal), "{grammar:?} {text}");
            assert_eq!(editor.focus(), Field::Key, "{grammar:?} {text}");
            assert_eq!(editor.step(false), Outcome::Refused(refusal), "{grammar:?} {text}");
            assert_eq!(editor.focus(), Field::Key, "{grammar:?} {text}");
            assert_eq!(editor.key().as_str(), text, "and nothing was thrown away");
        }
    }

    #[test]
    fn each_grammar_accepts_its_own_keys_and_refuses_the_others() {
        for key in ["DB_HOST", "_PRIVATE", "a1"] {
            assert!(Grammar::Env.accepts(key), "{key}");
        }
        for key in ["DB-HOST", "a.b", "9BAD"] {
            assert!(Grammar::Secret.accepts(key), "{key}");
        }
        for key in ["DB-HOST", "a.b", "9BAD", "", ".leading"] {
            assert!(!Grammar::Env.accepts(key), "{key}");
        }
        for key in ["DB HOST", "", ".leading", &"x".repeat(Grammar::Secret.key_limit() + 1)] {
            assert!(!Grammar::Secret.accepts(key), "{key}");
        }
    }

    #[test]
    fn a_rename_is_a_key_field_that_no_longer_names_the_row() {
        let mut editor = KeyValues::under(Grammar::Env, "DB_HOST");
        assert!(!editor.is_rename());
        editor.step(true);
        editor.handle(KeyPress::TextEdit(TextEdit::DeleteToStart));
        typed(&mut editor, "DB_URL");
        assert!(editor.is_rename());
        assert_eq!(editor.opened_under(), Some("DB_HOST"));
    }

    /// A rename that carried no value is refused rather than written: the pane
    /// cannot know the value the old key had, so writing it would empty the new
    /// key and drop the old one.
    #[test]
    fn a_rename_with_nothing_typed_in_value_is_refused() {
        let mut editor = KeyValues::under(Grammar::Env, "DB_HOST");
        editor.step(true);
        editor.handle(KeyPress::TextEdit(TextEdit::DeleteToStart));
        typed(&mut editor, "DB_URL");
        editor.step(false);

        assert_eq!(editor.submit(), Outcome::Refused(Refusal::EmptyValue));
        assert_eq!(editor.focus(), Field::Value);

        typed(&mut editor, "db.internal");
        assert_eq!(editor.submit(), Outcome::Submitted);
    }

    /// An unchanged key with an empty VALUE is not a rename, so it is not
    /// refused: for env that keystroke is the removal, as it has always been.
    #[test]
    fn an_empty_value_on_an_unchanged_key_still_submits() {
        let mut editor = KeyValues::under(Grammar::Env, "DB_HOST");
        assert_eq!(editor.submit(), Outcome::Submitted);
    }

    #[test]
    fn each_refusal_says_the_same_words_in_either_pane() {
        assert_eq!(
            Refusal::Key(Grammar::Secret.rule()).sentence(".bad"),
            "\".bad\" is not a valid key: letters, digits, `.`, `_` and `-`, \
             up to 128 bytes, not starting with a dot"
        );
        assert_eq!(
            Refusal::Key(Grammar::Env.rule()).sentence("9BAD"),
            "\"9BAD\" is not a valid key: a letter or `_`, then letters, digits or `_`"
        );
        assert_eq!(Refusal::EmptyValue.sentence("DB_HOST"), EMPTY_RENAME);
    }

    /// IR-41. Exact-string, so restoring a derived `Debug` fails here rather
    /// than silently printing both buffers.
    #[test]
    fn the_editors_debug_prints_two_lengths_and_neither_field() {
        let mut editor = KeyValues::under(Grammar::Secret, "DB_PASSWORD");
        typed(&mut editor, "hunter2");

        assert_eq!(
            format!("{editor:?}"),
            "KeyValues { add: false, key: <11 chars>, value: <7 chars>, focus: Value }"
        );
        assert!(!format!("{editor:?}").contains("hunter2"));
        assert!(!format!("{editor:?}").contains("DB_PASSWORD"));
    }

    /// The add row's own exact string, the other half of the same redaction:
    /// `add` is a bool, never the half-typed key.
    #[test]
    fn the_add_rows_debug_names_no_half_typed_key() {
        let mut editor = KeyValues::add(Grammar::Secret);
        typed(&mut editor, "STRIPE_KEY");

        assert_eq!(
            format!("{editor:?}"),
            "KeyValues { add: true, key: <10 chars>, value: <0 chars>, focus: Key }"
        );
        assert!(!format!("{editor:?}").contains("STRIPE_KEY"));
    }

    // The same table run against both panes, which is the claim the editor
    // exists to make: one set of keystrokes, the same write in each. Every
    // case is driven through `Msg::Key` rather than by calling pane methods,
    // so a key that never reaches a pane's keymap fails here.

    /// The key each fixture's pane already has, standing in for "the row this
    /// pane is under" in the table below.
    const ENV_KEY: &str = "DB_HOST";
    const SECRET_KEY: &str = "DB_PASSWORD";

    /// What a pane wrote, in the one shape both panes can be read in: a key,
    /// and the value it carries. `None` is a removal.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct Written(String, Option<String>);

    /// One keystroke of a case: a bound key, or a run of characters typed into
    /// whichever field holds the cursor.
    enum Step {
        Press(KeyPress),
        Type(&'static str),
    }

    /// A case: the row it starts on (`"@"` is the pane's own existing row,
    /// `None` is its add row), the keys an operator presses, and what either
    /// pane must write.
    struct Case {
        name: &'static str,
        row: Option<&'static str>,
        steps: Vec<Step>,
        writes: Vec<(&'static str, Option<&'static str>)>,
    }

    fn press(key: KeyPress) -> Step {
        Step::Press(key)
    }

    fn types(text: &'static str) -> Step {
        Step::Type(text)
    }

    fn cases() -> Vec<Case> {
        vec![
            Case {
                name: "add a key and a value",
                row: None,
                steps: vec![
                    press(KeyPress::Confirm),
                    types("NEW_KEY"),
                    press(KeyPress::NextField),
                    types("value"),
                    press(KeyPress::TextApply),
                ],
                writes: vec![("NEW_KEY", Some("value"))],
            },
            Case {
                name: "edit a value",
                row: Some("@"),
                steps: vec![
                    press(KeyPress::Confirm),
                    types("hunter2"),
                    press(KeyPress::TextApply),
                ],
                writes: vec![("@", Some("hunter2"))],
            },
            Case {
                name: "rename a key",
                row: Some("@"),
                steps: vec![
                    press(KeyPress::Confirm),
                    press(KeyPress::PrevField),
                    press(KeyPress::TextEdit(TextEdit::DeleteToStart)),
                    types("RENAMED"),
                    press(KeyPress::NextField),
                    types("hunter2"),
                    press(KeyPress::TextApply),
                ],
                writes: vec![("@", None), ("RENAMED", Some("hunter2"))],
            },
            Case {
                name: "cancel",
                row: Some("@"),
                steps: vec![
                    press(KeyPress::Confirm),
                    types("hunter2"),
                    press(KeyPress::TextAbandon),
                ],
                writes: vec![],
            },
            Case {
                name: "delete",
                row: Some("@"),
                steps: vec![press(KeyPress::ArmDelete), press(KeyPress::Confirm)],
                writes: vec![("@", None)],
            },
        ]
    }

    /// `"@"` is the pane's own key, so one table covers both.
    fn named(key: &str, existing: &str) -> String {
        if key == "@" { existing.to_owned() } else { key.to_owned() }
    }

    fn expected(case: &Case, existing: &str) -> Vec<Written> {
        let mut out: Vec<Written> = case
            .writes
            .iter()
            .map(|(key, value)| {
                Written(
                    named(key, existing),
                    value.map(|value| value.to_owned()),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// Env files rather than sends, so its writes are read off the pane's own
    /// change set: the same `PaneEdit::SetEnv` entries the close dialog turns
    /// into `Request::SetSheepEnv`.
    fn env_writes(app: &App) -> Vec<Written> {
        let mut out: Vec<Written> = app
            .config_pane()
            .expect("the pane is open")
            .edits()
            .iter()
            .filter_map(|(key, entry)| {
                let crate::lookout::edits::EditKey::Env(key) = key else {
                    return None;
                };
                let crate::lookout::pane::PaneEdit::SetEnv { value, .. } = entry.edit() else {
                    return None;
                };
                Some(Written(
                    key.clone(),
                    value.as_ref().map(shep_core::protocol::EnvValue::as_str).map(str::to_owned),
                ))
            })
            .collect();
        out.sort();
        out
    }

    /// Secrets write at once, so its writes are the effects the case raised.
    fn secret_writes(effects: &[Effect]) -> Vec<Written> {
        let mut out: Vec<Written> = Vec::new();
        for effect in effects {
            match effect {
                Effect::WriteSecret(edit, _) => {
                    out.push(Written(edit.key.clone(), edit.value.clone()));
                }
                Effect::WriteSecrets(edits, _) => {
                    out.extend(edits.iter().map(|edit| Written(edit.key.clone(), edit.value.clone())));
                }
                _ => {}
            }
        }
        out.sort();
        out
    }

    fn run_env(case: &Case) -> Vec<Written> {
        let mut app = fixtures::app_in_sheep_pane_with_env(&[(ENV_KEY, "db.internal")]);
        match case.row {
            None => {
                app.update(Msg::Key(KeyPress::SelectLast));
            }
            Some(_) => fixtures::select_env_key(&mut app, ENV_KEY),
        }
        for step in &case.steps {
            apply(&mut app, step);
        }
        env_writes(&app)
    }

    fn run_secrets(case: &Case) -> (Vec<Written>, Vec<Effect>) {
        let mut app = fixtures::app_with_secrets_and_control();
        match case.row {
            None => {
                app.update(Msg::Key(KeyPress::SelectLast));
            }
            Some(_) => {}
        }
        let mut effects: Vec<Effect> = Vec::new();
        for step in &case.steps {
            let effect = apply(&mut app, step);
            if matches!(effect, Effect::WriteSecret(..) | Effect::WriteSecrets(..)) {
                effects.push(effect);
            }
        }
        (secret_writes(&effects), effects)
    }

    fn apply(app: &mut App, step: &Step) -> Effect {
        match step {
            Step::Press(key) => app.update(Msg::Key(*key)),
            Step::Type(text) => {
                let mut effect = Effect::None;
                for character in text.chars() {
                    let next = app.update(Msg::Key(KeyPress::TextChar(character)));
                    if effect == Effect::None {
                        effect = next;
                    }
                }
                effect
            }
        }
    }

    /// The acceptance criterion, in one table: five cases, both panes, the same
    /// writes out of each.
    #[test]
    fn one_table_of_cases_writes_the_same_thing_in_both_panes() {
        for case in cases() {
            assert_eq!(
                run_env(&case),
                expected(&case, ENV_KEY),
                "env: {}",
                case.name
            );
            let (writes, _) = run_secrets(&case);
            assert_eq!(writes, expected(&case, SECRET_KEY), "secrets: {}", case.name);
        }
    }

    /// A rename is one write in each pane: one `Enter`, not a delete and an
    /// add. Env files both halves from the one submit; secrets raises one
    /// effect carrying both.
    #[test]
    fn a_rename_is_one_write_in_either_pane() {
        let case = cases()
            .into_iter()
            .find(|case| case.name == "rename a key")
            .expect("the table has a rename case");

        let mut app = fixtures::app_in_sheep_pane_with_env(&[(ENV_KEY, "db.internal")]);
        fixtures::select_env_key(&mut app, ENV_KEY);
        for step in &case.steps {
            apply(&mut app, step);
        }
        let pane = app.config_pane().expect("the pane is open");
        assert_eq!(
            pane.edits().len(),
            2,
            "the new key and the removal of the old one, from one Enter"
        );

        let (_, effects) = run_secrets(&case);
        assert_eq!(effects.len(), 1, "one effect, both halves: {effects:?}");
        let Effect::WriteSecrets(edits, _) = &effects[0] else {
            panic!("a rename is not a single write: {effects:?}");
        };
        assert_eq!(edits.len(), 2);
    }

    /// The refusal an invalid KEY raises is the same sentence in both panes,
    /// with the cursor kept on KEY.
    ///
    /// The key is `A BAD`, with a space: outside both grammars. A digit-led key
    /// would not do, because the secrets grammar accepts one and the env
    /// grammar does not.
    #[test]
    fn an_invalid_key_is_refused_the_same_way_in_either_pane() {
        let mut env = fixtures::app_in_sheep_pane_with_env(&[(ENV_KEY, "db.internal")]);
        env.update(Msg::Key(KeyPress::SelectLast));
        env.update(Msg::Key(KeyPress::Confirm));
        for character in "A BAD".chars() {
            env.update(Msg::Key(KeyPress::TextChar(character)));
        }
        env.update(Msg::Key(KeyPress::NextField));
        let env_notice = env.notice().expect("a refusal says so").to_string();
        assert_eq!(
            env.config_pane().expect("open").env_typing().expect("open").focus(),
            Field::Key
        );

        let mut secrets = fixtures::app_with_secrets_and_control();
        secrets.update(Msg::Key(KeyPress::SelectLast));
        secrets.update(Msg::Key(KeyPress::Confirm));
        for character in "A BAD".chars() {
            secrets.update(Msg::Key(KeyPress::TextChar(character)));
        }
        secrets.update(Msg::Key(KeyPress::NextField));
        let secret_notice = secrets.notice().expect("a refusal says so").to_string();
        let Body::Secrets(pane) = secrets.body() else {
            panic!("the secrets pane is not open");
        };
        assert_eq!(
            pane.typing.as_ref().expect("open").focus(),
            Field::Key
        );

        // Both name their own grammar, and both say it in the same shape.
        for notice in [&env_notice, &secret_notice] {
            assert!(notice.contains("\"A BAD\" is not a valid key: "), "{notice}");
        }
        assert!(env_notice.contains("a letter or `_`"), "{env_notice}");
        assert!(secret_notice.contains("not starting with a dot"), "{secret_notice}");
    }
}
