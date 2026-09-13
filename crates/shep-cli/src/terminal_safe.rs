//! Two sanitisers for text on its way to a terminal: [`sanitise`] for a
//! string an untrusted host worded, [`sanitise_multiline`] for prose shep
//! lays out in lines itself.
//!
//! Lives here, at a leaf both `crate::fetch` and `dog_index` can reach,
//! rather than in either: a module cycle is the wrong way to share it.
//!
//! Strips rather than escapes: a stripped sequence's printable tail
//! survives as inert text (`\u{1b}[2J` prints as `[2J`), which is
//! simpler to get right than rendering it literally.
//!
//! Not [`crate::output::width::sanitize_cell`], which keeps a
//! well-formed CSI sequence since shep's own colouring is made of them.

/// Strips everything that could drive a terminal out of `field`,
/// returning the cleaned text and whether anything was removed. See
/// [`is_unprintable`] for the exact set.
///
/// A string with nothing to strip is returned byte for byte, and reports
/// `false`: the flag must mean "carried control characters", not "had
/// two spaces in a row".
pub fn sanitise(field: &str) -> (String, bool) {
    strip_unprintable(field, false)
}

/// [`sanitise`] with `\n` kept and whitespace left as written, so a line
/// break and the two spaces indenting the line after it both survive.
///
/// The weaker of the two, and deliberately: a `\n` can forge a line of
/// output. For text shep wrote and laid out, where a collapsed remedy is
/// one the operator cannot copy; a string an untrusted host worded goes
/// through [`sanitise`] at the seam that captures it. Everything that
/// rewrites a row already drawn or drives the terminal still goes, `\r`,
/// `\t`, `\u{1b}` and `\u{9b}` among them.
///
/// Reports `true` on the same terms [`sanitise`] does, counting a kept
/// `\n` as nothing removed.
pub fn sanitise_multiline(field: &str) -> (String, bool) {
    strip_unprintable(field, true)
}

/// The body of both sanitisers. `keep_line_breaks` spares `\n` from the
/// strip, and with it the runs of whitespace that would otherwise collapse.
fn strip_unprintable(field: &str, keep_line_breaks: bool) -> (String, bool) {
    let doomed = |ch: char| is_unprintable(ch) && !(keep_line_breaks && ch == '\n');
    if !field.chars().any(doomed) {
        return (field.to_owned(), false);
    }
    let stripped: String = field
        .chars()
        .filter_map(|ch| {
            if !doomed(ch) {
                Some(ch)
            } else if is_line_or_space_like(ch) {
                // A line break was separating two words; a plain deletion
                // would join them.
                Some(' ')
            } else {
                None
            }
        })
        .collect();
    if keep_line_breaks {
        return (stripped, true);
    }
    let cleaned = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    (cleaned, true)
}

/// Whether `ch` must never reach a terminal.
///
/// Two groups: `char::is_control` (Unicode `Cc`, including `\u{1b}` and
/// `\u{9b}`, the single-character CSI introducer), and a fixed list of
/// invisible or reordering format characters that are not control
/// characters: zero-width spaces and joiners, bidi overrides (`U+202E`
/// can make `exe.gnp` read as `png.exe`), variation selectors, and tags
/// (`U+E0000..=U+E007F` maps onto ASCII, so it can carry a hidden string).
///
/// Everything else survives: accented Latin, kana, Han, emoji and
/// combining marks are ordinary prose. Stripping `U+FE0F` costs an
/// emoji's presentation form, so `❤️` prints as `❤`.
fn is_unprintable(ch: char) -> bool {
    ch.is_control()
        || matches!(ch,
            '\u{ad}'                  // soft hyphen
            | '\u{34f}'               // combining grapheme joiner
            | '\u{61c}'               // arabic letter mark (bidi, as U+200E/F)
            | '\u{115f}' | '\u{1160}' // hangul choseong/jungseong fillers
            | '\u{180b}'..='\u{180f}' // mongolian variation selectors, vowel separator
            | '\u{200b}'..='\u{200f}' // zero-width space/non-joiner/joiner, LRM, RLM
            | '\u{2028}' | '\u{2029}' // line and paragraph separators
            | '\u{202a}'..='\u{202e}' // bidi embeddings and overrides
            | '\u{2060}'..='\u{206f}' // word joiner, invisible operators, isolates, deprecated
            | '\u{3164}'              // hangul filler
            | '\u{fe00}'..='\u{fe0f}' // variation selectors
            | '\u{feff}'              // byte order mark / zero-width no-break space
            | '\u{ffa0}'             // halfwidth hangul filler
            | '\u{fff9}'..='\u{fffb}' // interlinear annotation
            | '\u{1d173}'..='\u{1d17a}' // musical beam, slur, phrase marks
            | '\u{e0000}'..='\u{e007f}' // tags: an invisible ascii alphabet
            | '\u{e0100}'..='\u{e01ef}' // variation selectors supplement
        )
}

/// Whether a character [`is_unprintable`] rejected was separating words, and
/// so should leave a space behind rather than vanish.
fn is_line_or_space_like(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_escape_class_is_stripped() {
        // Each of these reaches a terminal if it survives. shep emits colour
        // itself, so a reader cannot tell an entry's bytes from shep's own.
        let hostile = [
            ("\u{1b}[2J", "clears the screen"),
            ("\u{1b}]0;pwned\u{7}", "rewrites the window title"),
            ("\u{1b}[31mred", "forges shep's own colour"),
            ("before\rafter", "overwrites the line with a bare CR"),
            ("a\u{0}b", "a nul byte"),
            ("tab\there", "a raw tab"),
            ("line\nbreak", "escapes the row"),
        ];
        for (input, why) in hostile {
            let (clean, changed) = sanitise(input);
            assert!(changed, "{why}: should have been reported as sanitised");
            assert!(
                !clean.contains('\u{1b}'),
                "{why}: escape survived in {clean:?}"
            );
            for ch in clean.chars() {
                assert!(
                    !ch.is_control(),
                    "{why}: control char survived in {clean:?}"
                );
            }
        }
    }

    #[test]
    fn ordinary_text_is_left_exactly_alone() {
        let (clean, changed) = sanitise("Rotates grown log files. MIT OR Apache-2.0.");
        assert_eq!(clean, "Rotates grown log files. MIT OR Apache-2.0.");
        assert!(!changed);
    }

    #[test]
    fn non_ascii_prose_survives_because_it_is_not_the_threat() {
        let (clean, changed) = sanitise("rotiert Protokolldateien");
        assert_eq!(clean, "rotiert Protokolldateien");
        assert!(!changed);
    }

    /// fails if a trailing escape survives. A lone `\u{1b}` at the very end
    /// of a field opens a sequence that the NEXT thing printed completes,
    /// which is whatever shep itself writes after the cell. It is the one
    /// escape that does nothing on its own and everything in context.
    #[test]
    fn a_lone_escape_at_the_end_of_a_string_is_stripped() {
        let (clean, changed) = sanitise("tail\u{1b}");
        assert_eq!(clean, "tail");
        assert!(changed);
    }

    /// fails if `\u{9b}` survives. It is the single-character CSI
    /// introducer: `\u{9b}2J` does what `\u{1b}[2J` does, in one character
    /// that is not `\u{1b}` and does not look like an escape. Covered here
    /// because it lives in the C1 block, which `char::is_control` includes
    /// and a hand-rolled `ch == '\u{1b}' || ch < ' '` check would not.
    #[test]
    fn the_single_character_csi_introducer_is_stripped() {
        let (clean, changed) = sanitise("clean\u{9b}2Jhere");
        assert!(changed);
        assert!(!clean.contains('\u{9b}'), "C1 CSI survived in {clean:?}");
    }

    /// fails if any invisible or reordering character survives. None of
    /// these is a control character, so `char::is_control` alone misses
    /// every one. `\u{202e}` is the interesting one: it reverses what
    /// follows, so a package named `shep-exe.gnp` renders as
    /// `shep-png.exe`, and the two entries are indistinguishable in a
    /// table.
    #[test]
    fn invisible_and_reordering_characters_are_stripped() {
        let hostile = [
            ('\u{202e}', "right-to-left override"),
            ('\u{202d}', "left-to-right override"),
            ('\u{2066}', "left-to-right isolate"),
            ('\u{200d}', "zero width joiner"),
            ('\u{200b}', "zero width space"),
            ('\u{2060}', "word joiner"),
            ('\u{feff}', "byte order mark"),
            ('\u{2028}', "line separator"),
        ];
        for (ch, why) in hostile {
            let (clean, changed) = sanitise(&format!("safe{ch}text"));
            assert!(changed, "{why}: should have been reported as sanitised");
            assert!(!clean.contains(ch), "{why}: survived in {clean:?}");
        }
    }

    /// fails if a stripped line break welds two words together. `line` and
    /// `break` are separate words in the source, and `linebreak` is a
    /// different string from either.
    #[test]
    fn a_stripped_line_break_leaves_a_space_behind() {
        let (clean, changed) = sanitise("line\nbreak");
        assert_eq!(clean, "line break");
        assert!(changed);
    }

    /// fails if the multiline sanitiser flattens the layout it exists to
    /// keep. The indent is load-bearing: a remedy line reads as a remedy
    /// because it sits under the lead line, not beside it.
    #[test]
    fn a_multiline_message_keeps_its_breaks_and_its_indent() {
        let written = "no flock at /tmp/x\n  did you mean to drop --home?";
        let (clean, changed) = sanitise_multiline(written);
        assert_eq!(clean, written);
        assert!(!changed, "a kept newline is not something removed");
    }

    /// fails if `\n` bought anything else a passage. Each of these moves the
    /// cursor or rewrites a row already drawn, which is the property the
    /// looser sanitiser still has to hold.
    #[test]
    fn only_the_line_feed_survives_the_multiline_sanitiser() {
        let hostile = [
            ('\r', "a bare CR overwrites the line"),
            ('\t', "a tab jumps the cursor to a column"),
            ('\u{b}', "a vertical tab moves down a row"),
            ('\u{c}', "a form feed clears the page"),
            ('\u{1b}', "the escape introducer"),
            ('\u{9b}', "the single-character CSI introducer"),
            ('\u{85}', "next line"),
            ('\u{2028}', "line separator"),
            ('\u{2029}', "paragraph separator"),
            ('\u{202e}', "right-to-left override"),
        ];
        for (ch, why) in hostile {
            let (clean, changed) = sanitise_multiline(&format!("safe{ch}text"));
            assert!(changed, "{why}: should have been reported as sanitised");
            assert!(!clean.contains(ch), "{why}: survived in {clean:?}");
        }
    }

    /// fails if a hostile escape rides into a table on the back of the
    /// newline exemption. `\u{1b}[2J` is still an escape when it follows
    /// one.
    #[test]
    fn an_escape_after_a_kept_newline_is_still_stripped() {
        let (clean, changed) = sanitise_multiline("lead\n  \u{1b}[2Jremedy");
        assert_eq!(clean, "lead\n  [2Jremedy");
        assert!(changed);
    }

    /// fails if any of these still reaches a terminal. Each is invisible
    /// or reordering, and each is the same class as one [`is_unprintable`]
    /// already names.
    #[test]
    fn the_invisible_classes_are_taken_whole_not_as_a_remembered_subset() {
        let hostile = [
            ('\u{ad}', "soft hyphen"),
            ('\u{34f}', "combining grapheme joiner"),
            ('\u{61c}', "arabic letter mark, a bidi control"),
            ('\u{115f}', "hangul choseong filler, renders blank"),
            ('\u{1160}', "hangul jungseong filler, renders blank"),
            ('\u{180e}', "mongolian vowel separator"),
            ('\u{206b}', "deprecated: activate symmetric swapping"),
            ('\u{3164}', "hangul filler, renders blank"),
            ('\u{fe0f}', "variation selector 16"),
            ('\u{ffa0}', "halfwidth hangul filler, renders blank"),
            ('\u{1d173}', "musical symbol begin beam"),
            ('\u{e0041}', "tag latin capital A: an invisible letter"),
            ('\u{e0101}', "variation selector supplement"),
        ];
        for (ch, why) in hostile {
            let (clean, changed) = sanitise(&format!("safe{ch}text"));
            assert!(changed, "{why}: should have been reported as sanitised");
            assert!(!clean.contains(ch), "{why}: survived in {clean:?}");
        }
    }

    /// fails if the tags block can still smuggle a whole hidden string past
    /// a reader. `U+E0000`..`U+E007F` maps one-to-one onto ASCII, so a name
    /// can carry an invisible second name that no human sees and every
    /// `contains` matches.
    #[test]
    fn a_hidden_ascii_string_written_in_tags_does_not_survive() {
        let hidden: String = "rm -rf"
            .chars()
            .map(|c| char::from_u32(0xe_0000 + c as u32).unwrap())
            .collect();
        let (clean, changed) = sanitise(&format!("Spot{hidden}"));
        assert!(changed);
        assert_eq!(clean, "Spot");
    }

    /// fails if the widened list started eating prose. This is the check
    /// that says the ranges above are ranges of *format* characters and not
    /// of everything unfamiliar: kana, Han, accented Latin, a combining
    /// mark and an emoji base all have to come back byte for byte.
    #[test]
    fn prose_in_other_scripts_still_survives_the_wider_list() {
        for ordinary in [
            "\u{30ed}\u{30b0}\u{3092}\u{30ed}\u{30fc}\u{30c6}\u{30fc}\u{30c8}", // rotates logs, in katakana
            "\u{65e5}\u{8a8c}\u{306e}\u{56de}\u{8ee2}",                         // ditto, in kanji
            "rotiert Protokolldateien",
            "cafe\u{301}", // combining acute: a mark, not a format character
            "\u{2764}",    // an emoji base, unaccompanied by its selector
        ] {
            let (clean, changed) = sanitise(ordinary);
            assert_eq!(clean, ordinary, "prose was altered");
            assert!(!changed, "prose was reported as sanitised");
        }
    }
}
