//! The colour a read log line asked for, as runs over its plain text.
//!
//! The reader hands a view plain text and the styling the sheep asked for
//! beside it. This is where the grammar lives: the SGR subset `shep bleats`
//! already applies to the same bytes, folded into what a line's sequences have
//! asked for so far, and cut into the runs `view::feed_ink` paints.
//!
//! A field of a parameter list remembers the separator that ended it, which is
//! all that tells `38;2;r;g;b` from `38:2:c;r;g;b`. Every parameter also takes
//! its own sub-parameters with it, drawn or not.

use ratatui::style::{Color, Modifier, Style};

use super::InkRun;

/// The eight SGR colours `30`..`37` name for a foreground and `40`..`47` for a
/// background, in the order the numbers give them.
const STANDARD_COLOURS: [Color; 8] = [
    Color::Black,
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Gray,
];

/// The same eight at full intensity, `90`..`97` and `100`..`107`.
const BRIGHT_COLOURS: [Color; 8] = [
    Color::DarkGray,
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
    Color::White,
];

/// The styling a line's own SGR sequences have asked for so far.
///
/// `None` is not ratatui's `Color::Reset`: it means no sequence asked for that
/// attribute, which is what a `39` or a `49` puts back. A model that stored a
/// reset as a colour would record a run for a line that only ever reset one,
/// and a view would then paint an explicit reset over the stream's own colour.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Sgr {
    fg: Option<Color>,
    bg: Option<Color>,
    modifiers: Modifier,
}

impl Sgr {
    /// Whether this says something a default cell does not.
    fn styled(self) -> bool {
        self.fg.is_some() || self.bg.is_some() || !self.modifiers.is_empty()
    }

    /// The same styling, as a cell style.
    fn style(self) -> Style {
        let mut style = Style::default();
        if let Some(fg) = self.fg {
            style = style.fg(fg);
        }
        if let Some(bg) = self.bg {
            style = style.bg(bg);
        }
        if !self.modifiers.is_empty() {
            style = style.add_modifier(self.modifiers);
        }
        style
    }
}

/// Walk a line already through [`crate::output::log_line::sanitize_log_line`], where every
/// remaining escape is a well-formed SGR, into its plain text and the runs its
/// sequences ask for.
///
/// A run is recorded only where the current styling says something a default
/// cell does not, so a line that resets without ever colouring contributes no
/// run for a view to paint.
pub(super) fn split_runs(clean: &str) -> (String, Vec<InkRun>) {
    let mut text = String::with_capacity(clean.len());
    let mut runs: Vec<InkRun> = Vec::new();
    let mut sgr = Sgr::default();
    let mut start = 0usize;
    let mut chars = clean.char_indices().peekable();

    while let Some((_, c)) = chars.next() {
        if c != '\u{1b}' {
            text.push(c);
            continue;
        }
        chars.next();
        let mut params = String::new();
        while let Some(&(_, p)) = chars.peek() {
            if !matches!(p, '0'..='9' | ';' | ':') {
                break;
            }
            params.push(p);
            chars.next();
        }
        // The final `m` is what makes the sequence styling rather than a
        // cursor move; `sanitize_log_line` only ever leaves the styling kind.
        // A terminator that is not one is dropped, and the character after it
        // stays log text.
        match chars.next() {
            Some((_, 'm')) => {}
            Some((_, other)) => {
                text.push(other);
                continue;
            }
            None => break,
        }
        if sgr.styled() && text.len() > start {
            runs.push(InkRun {
                range: start..text.len(),
                style: sgr.style(),
            });
        }
        apply_sgr(&params, &mut sgr);
        start = text.len();
    }

    if sgr.styled() && text.len() > start {
        runs.push(InkRun {
            range: start..text.len(),
            style: sgr.style(),
        });
    }
    (text, runs)
}

/// One field of an SGR parameter list: its value, and the separator that
/// ended it.
///
/// The separator matters for the extended colour forms alone. `38;2;255;0;0`
/// names its components directly, while the ITU T.416 spelling
/// `38:2::255:0:0` carries a colorspace field between the `2` and the
/// components. Flattening both separators reads that field as a component.
#[derive(Clone, Copy)]
struct Field {
    value: Option<u32>,
    colon: bool,
}

/// Split an SGR parameter list into its fields.
///
/// An empty field is `None`: zero for an ordinary code, which the grammar gives
/// as a reset, and the absent colorspace field for an extended colour.
fn fields(params: &str) -> Vec<Field> {
    let mut out: Vec<Field> = Vec::new();
    let mut current = String::new();
    for c in params.chars() {
        if c != ';' && c != ':' {
            current.push(c);
            continue;
        }
        out.push(Field {
            value: current.parse::<u32>().ok(),
            colon: c == ':',
        });
        current = String::new();
    }
    out.push(Field {
        value: current.parse::<u32>().ok(),
        colon: false,
    });
    out
}

/// The field at `at`, with `at` moved past it. `None` when the list has no
/// field there.
fn take(fields: &[Field], at: &mut usize) -> Option<Field> {
    let field = fields.get(*at).copied()?;
    *at += 1;
    Some(field)
}

/// Fold one SGR parameter list into `sgr`.
///
/// `;` separates parameters and `:` separates the sub-parameters of a
/// parameter that has them, and which one ended a field is what tells the two
/// spellings apart. An empty parameter reads as zero, which is a reset. Every
/// parameter keeps its own sub-parameters, drawn or not, so a code this pane
/// does not draw cannot leave one behind as the next code.
fn apply_sgr(params: &str, sgr: &mut Sgr) {
    let fields = fields(params);
    let mut at = 0usize;
    while at < fields.len() {
        let field = fields[at];
        at += 1;
        let code = field.value.unwrap_or(0);
        // A field ended by `:` carries sub-parameters, and they belong to it:
        // an underline style (`4:3`), a strike style (`9:1`), a framed border
        // (`51:1`). Read them off the list rather than let the next pass take
        // one as the SGR code it was written as a style of.
        if field.colon && !matches!(code, 38 | 48 | 58) {
            drop_subparameters(&fields, &mut at);
        }
        match code {
            0 => *sgr = Sgr::default(),
            1 => sgr.modifiers |= Modifier::BOLD,
            2 => sgr.modifiers |= Modifier::DIM,
            3 => sgr.modifiers |= Modifier::ITALIC,
            4 => sgr.modifiers |= Modifier::UNDERLINED,
            7 => sgr.modifiers |= Modifier::REVERSED,
            9 => sgr.modifiers |= Modifier::CROSSED_OUT,
            22 => sgr.modifiers.remove(Modifier::BOLD | Modifier::DIM),
            23 => sgr.modifiers.remove(Modifier::ITALIC),
            24 => sgr.modifiers.remove(Modifier::UNDERLINED),
            27 => sgr.modifiers.remove(Modifier::REVERSED),
            29 => sgr.modifiers.remove(Modifier::CROSSED_OUT),
            30..=37 => sgr.fg = Some(STANDARD_COLOURS[usize::try_from(code - 30).unwrap_or(0)]),
            38 => {
                if let Some(colour) = extended_colour(&fields, &mut at) {
                    sgr.fg = Some(colour);
                }
            }
            39 => sgr.fg = None,
            40..=47 => sgr.bg = Some(STANDARD_COLOURS[usize::try_from(code - 40).unwrap_or(0)]),
            48 => {
                if let Some(colour) = extended_colour(&fields, &mut at) {
                    sgr.bg = Some(colour);
                }
            }
            49 => sgr.bg = None,
            58 => {
                // The underline colour. It spells itself exactly like a `38`, so
                // its components are read and dropped: this pane draws no
                // underline colour, and leaving them on the list is how
                // `58;2;0;0;0` came to reset the line it was styling.
                extended_colour(&fields, &mut at);
            }
            59 => {}
            90..=97 => sgr.fg = Some(BRIGHT_COLOURS[usize::try_from(code - 90).unwrap_or(0)]),
            100..=107 => sgr.bg = Some(BRIGHT_COLOURS[usize::try_from(code - 100).unwrap_or(0)]),
            // Every other parameter this pane does not draw: a font, a framed
            // border, an overline. It changes nothing rather than guessing at
            // what a terminal would do with it.
            _ => {}
        }
    }
}

/// Read a colon-terminated field's sub-parameters off the list. The chain ends
/// at a field that a `;` ended or at the end of the list.
fn drop_subparameters(fields: &[Field], at: &mut usize) {
    let mut parent_colon = true;
    while parent_colon {
        let Some(field) = take(fields, at) else {
            break;
        };
        parent_colon = field.colon;
    }
}

/// The fields a truecolor `2` names its components with, always read off the
/// list whole.
///
/// The semicolon form names three ordinary parameters. The colon form's are
/// sub-parameters, and a colorspace field may lead them, so the whole chain
/// comes back and [`extended_colour`] says which field is which.
fn colour_fields(fields: &[Field], at: &mut usize, colon_form: bool) -> Vec<Field> {
    if !colon_form {
        return (0..3).filter_map(|_| take(fields, at)).collect();
    }
    let mut chain = Vec::new();
    let mut parent_colon = true;
    while parent_colon {
        let Some(field) = take(fields, at) else {
            break;
        };
        parent_colon = field.colon;
        chain.push(field);
    }
    chain
}

/// The colour a `38` or a `48` names, reading its own fields off the same list
/// and always consuming them. A field left behind would be read as the next SGR
/// code, so a malformed spelling would apply a reset it never asked for.
///
/// `None` for a spelling that carries no colour or a component no colour takes,
/// which styles nothing rather than guessing one from the bytes there.
fn extended_colour(fields: &[Field], at: &mut usize) -> Option<Color> {
    let mode = take(fields, at)?;
    match mode.value.unwrap_or(0) {
        5 => {
            let index = take(fields, at);
            u8::try_from(index?.value?).ok().map(Color::Indexed)
        }
        2 => {
            let chain = colour_fields(fields, at, mode.colon);
            // The ITU spelling may lead with a colorspace field, empty for the
            // default one and an id for a named one. xterm and kitty also take
            // the three components with no colorspace field at all, so the
            // count of what the `2` named says which of the two this is.
            let components = if mode.colon && chain.len() >= 4 {
                &chain[1..]
            } else {
                &chain[..]
            };
            let [red, green, blue] = components else {
                return None;
            };
            Some(Color::Rgb(
                u8::try_from(red.value?).ok()?,
                u8::try_from(green.value?).ok()?,
                u8::try_from(blue.value?).ok()?,
            ))
        }
        // A mode this pane does not know, `38:1` for a CMYK colour. Its fields
        // still belong to it, so they are read off the list and nothing is
        // drawn.
        _ => {
            if mode.colon {
                drop_subparameters(fields, at);
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lookout::tail::{Stream, TailLine};

    use super::*;

    /// The line an operator reads and the colour the sheep asked for are two
    /// different fields: the text carries no escapes for ratatui to drop, and
    /// the styling survives beside it.
    #[test]
    fn a_coloured_line_keeps_its_runs_and_a_plain_text() {
        let line = TailLine::new(
            Stream::Err,
            "\u{1b}[31mconnection refused\u{1b}[0m on :8080",
        );
        assert_eq!(
            line.text, "connection refused on :8080",
            "no escape survives"
        );
        assert!(line.own_colour, "the line styled its own text");
        assert_eq!(
            line.runs,
            vec![InkRun {
                range: 0..18,
                style: Style::default().fg(Color::Red),
            }],
            "the run covers the styled stretch and stops at the reset"
        );
    }

    /// A log line is content. A cursor move inside one is a redraw the sheep
    /// has no business asking a pane for, and `TailLine::text` is what every
    /// filter, level rule and wrap count measures.
    #[test]
    fn a_cursor_move_in_a_log_line_is_dropped_from_the_text() {
        let line = TailLine::new(Stream::Out, "over\u{1b}[2Cwritten\u{1b}[0m");
        assert_eq!(line.text, "overwritten");
        assert!(
            !line.own_colour,
            "a cursor move and a bare reset are not colours"
        );
        assert!(line.runs.is_empty(), "and nothing is left to paint");
    }

    /// `39` and `49` ask for the default rather than for a colour. Stored as a
    /// colour they paint an explicit reset over the stream's own, and a logger
    /// that emits one before every line would empty the feed's palette.
    #[test]
    fn a_line_that_only_resets_a_colour_stays_a_plain_line() {
        let line = TailLine::new(Stream::Out, "\u{1b}[39mplain\u{1b}[49m too");
        assert_eq!(line.text, "plain too");
        assert!(line.runs.is_empty(), "a reset contributes no run to paint");
        assert!(!line.own_colour, "and it is not the line's own colour");
    }

    /// A `38` reads its own parameters off the same stream. A number left
    /// behind is then read as the next SGR code, so a malformed extended colour
    /// would apply resets the line never asked for.
    #[test]
    fn a_malformed_extended_colour_consumes_its_own_parameters() {
        let line = TailLine::new(Stream::Out, "\u{1b}[31mred\u{1b}[38;2;999;0;0m still red");
        assert_eq!(line.text, "red still red");
        assert_eq!(
            line.runs,
            vec![
                InkRun {
                    range: 0..3,
                    style: Style::default().fg(Color::Red),
                },
                InkRun {
                    range: 3..13,
                    style: Style::default().fg(Color::Red),
                },
            ],
            "the malformed spelling styles nothing, and cancels nothing either"
        );

        let indexed = TailLine::new(Stream::Out, "\u{1b}[38;5;300mout of range");
        assert_eq!(indexed.text, "out of range");
        assert!(
            indexed.runs.is_empty(),
            "an index no colour takes styles nothing"
        );
        assert!(!indexed.own_colour, "so the line is still a plain one");
    }

    /// The ITU spelling of an extended colour carries a colorspace field
    /// between the `2` and the components, empty for the default one. Read as
    /// a component it shifts the colour one place and leaves a zero behind to
    /// reset the line.
    #[test]
    fn a_colon_form_truecolor_reads_its_own_colorspace_field() {
        let defaulted = TailLine::new(Stream::Out, "\u{1b}[38:2::255:0:0mred\u{1b}[0m");
        assert_eq!(defaulted.text, "red");
        assert_eq!(
            defaulted.runs,
            vec![InkRun {
                range: 0..3,
                style: Style::default().fg(Color::Rgb(255, 0, 0)),
            }],
            "the empty colorspace field is not a component"
        );

        let named = TailLine::new(Stream::Out, "\u{1b}[38:2:1:0:255:0mgreen\u{1b}[0m");
        assert_eq!(
            named.runs[0].style,
            Style::default().fg(Color::Rgb(0, 255, 0)),
            "a named colorspace is skipped rather than painted"
        );

        let legacy = TailLine::new(Stream::Out, "\u{1b}[38;2;0;0;255mblue\u{1b}[0m");
        assert_eq!(
            legacy.runs[0].style,
            Style::default().fg(Color::Rgb(0, 0, 255)),
            "the semicolon form names its components directly"
        );

        let indexed = TailLine::new(Stream::Out, "\u{1b}[38:5:196mindexed\u{1b}[0m");
        assert_eq!(
            indexed.runs[0].style,
            Style::default().fg(Color::Indexed(196)),
            "and the colon form of the indexed one reads the same"
        );
    }

    /// xterm and kitty also take the three components with no colorspace field
    /// at all, and that is what a logger writing `38:2:255:0:0` emits. Reading
    /// the first component as a colorspace loses the colour, and with a
    /// parameter after it it reaches into that one and uses it up as blue.
    #[test]
    fn a_colon_form_truecolor_needs_no_colorspace_field_to_be_a_colour() {
        let bare = TailLine::new(Stream::Out, "\u{1b}[38:2:255:0:0mred\u{1b}[0m");
        assert_eq!(
            bare.runs,
            vec![InkRun {
                range: 0..3,
                style: Style::default().fg(Color::Rgb(255, 0, 0)),
            }],
            "three fields after the 2 are the components"
        );

        let then_bold = TailLine::new(Stream::Out, "\u{1b}[38:2:255:0:0;1mred\u{1b}[0m");
        assert_eq!(
            then_bold.runs[0].style,
            Style::default()
                .fg(Color::Rgb(255, 0, 0))
                .add_modifier(Modifier::BOLD),
            "the bold after the colour is the sheep's own and stays its own"
        );

        let named_space = TailLine::new(Stream::Out, "\u{1b}[38:2:0:255:0:0mred\u{1b}[0m");
        assert_eq!(
            named_space.runs[0].style,
            Style::default().fg(Color::Rgb(255, 0, 0)),
            "four fields after the 2 lead with the colorspace"
        );
    }

    /// A parameter this pane does not draw still owns its sub-parameters. Left
    /// on the list, the next pass takes them as SGR codes: an underline colour
    /// reset the red the sheep asked for, and a curly underline asked for an
    /// italic it never wrote.
    #[test]
    fn a_code_this_pane_does_not_draw_takes_its_own_parameters_with_it() {
        let underline_colour = TailLine::new(Stream::Out, "\u{1b}[31;58;2;0;0;0mtext\u{1b}[0m");
        assert_eq!(
            underline_colour.runs,
            vec![InkRun {
                range: 0..4,
                style: Style::default().fg(Color::Red),
            }],
            "the underline colour is read and dropped, not applied as DIM and a reset"
        );

        let curly = TailLine::new(Stream::Out, "\u{1b}[4:3mtext\u{1b}[0m");
        assert_eq!(
            curly.runs,
            vec![InkRun {
                range: 0..4,
                style: Style::default().add_modifier(Modifier::UNDERLINED),
            }],
            "the underline style is not an italic"
        );

        let unknown_mode = TailLine::new(Stream::Out, "\u{1b}[38:1:100:200:0:0mtext\u{1b}[0m");
        assert!(
            unknown_mode.runs.is_empty(),
            "a CMYK colour this pane does not draw styles nothing and leaves \
             nothing behind to be read as a code"
        );
    }
}
