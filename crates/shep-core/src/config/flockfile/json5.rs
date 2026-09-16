//! The nesting bound json5's parser needs to not abort the process.
//!
//! Its recursive descent overflows the stack at around 4500 levels, and a
//! stack overflow is a SIGABRT no caller can catch, so the depth is counted
//! before the parser ever sees the source.

// json5's recursive-descent parser stack-overflows (SIGABRT, uncatchable)
// around ~4500 levels of nesting. 64 is far beyond the deepest legitimate
// Flockfile nesting (4) and comfortably clear of the crash threshold.
pub(super) const MAX_JSON5_NESTING_DEPTH: u32 = 64;

// Scans for the maximum concurrently-open `[`/`{` depth, skipping quoted
// strings and `//`/`/* */` comments so bracket-like characters inside
// them don't distort the count. Fails closed: an unterminated string or
// comment returns `u32::MAX`, which always exceeds the depth cap.
pub(super) fn json5_nesting_depth(source: &str) -> u32 {
    let mut depth: u32 = 0;
    let mut max_depth: u32 = 0;
    let mut in_string: Option<char> = None;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(quote) = in_string {
            match c {
                '\\' => {
                    chars.next(); // skip the escaped character
                }
                q if q == quote => in_string = None,
                _ => {}
            }
            continue;
        }
        match c {
            '/' if chars.peek() == Some(&'/') => {
                chars.next(); // consume the second '/'
                for c2 in chars.by_ref() {
                    if c2 == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next(); // consume the '*'
                let mut prev = '\0';
                let mut closed = false;
                for c2 in chars.by_ref() {
                    if prev == '*' && c2 == '/' {
                        closed = true;
                        break;
                    }
                    prev = c2;
                }
                if !closed {
                    return u32::MAX; // unterminated block comment
                }
            }
            '"' | '\'' => in_string = Some(c),
            '[' | '{' => {
                depth = depth.saturating_add(1);
                max_depth = max_depth.max(depth);
            }
            ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    if in_string.is_some() {
        return u32::MAX; // unterminated string
    }
    max_depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::flockfile::error::FlockfileError;
    use crate::config::flockfile::file::Flockfile;
    use crate::config::flockfile::format::FlockFormat;

    #[test]
    fn json5_beyond_max_nesting_depth_is_rejected_without_crashing() {
        // json5 stack-overflows (SIGABRT) around ~4500 levels, so the depth
        // guard must reject this before calling into it. 5000 unclosed `[`
        // is nonsense JSON5, but the guard runs before any real parsing.
        let src = "[".repeat(5000);
        assert_eq!(
            Flockfile::parse(&src, FlockFormat::Json5).unwrap_err(),
            FlockfileError::Json5("nesting depth exceeds 64".to_string())
        );
    }

    #[test]
    fn json5_nesting_depth_counts_concurrently_open_brackets() {
        let nested = format!("{}{}", "[".repeat(10), "]".repeat(10));
        assert_eq!(json5_nesting_depth(&nested), 10);
    }

    #[test]
    fn json5_nesting_depth_ignores_brackets_inside_strings() {
        let src = r#"{ "a": "[[[[[[[[[[", "b": "esc\"aped [ too" }"#;
        assert_eq!(json5_nesting_depth(src), 1); // only the outer `{`
    }

    #[test]
    fn json5_legitimately_nested_doc_still_parses() {
        // A probe object nested inside an app object inside the app array
        // inside the root object: depth 4, the deepest a real Flockfile
        // schema allows, well under the depth-64 guard.
        let src = r#"{
            app: [{
                name: "web",
                script: "./srv",
                readiness_probe: { kind: "http", target: "http://localhost/x" },
            }],
        }"#;
        let flock = Flockfile::parse(src, FlockFormat::Json5).unwrap();
        assert_eq!(flock.apps.len(), 1);
    }

    #[test]
    fn json5_line_comment_apostrophe_does_not_hide_deep_nesting() {
        // Regression: a `'` inside a `//` comment must not flip the scanner
        // into string mode and make it ignore every bracket that follows,
        // letting an over-deep document slip past the guard.
        let src = format!("// don't nest\n{}", "[".repeat(5000));
        assert_eq!(
            Flockfile::parse(&src, FlockFormat::Json5).unwrap_err(),
            FlockfileError::Json5("nesting depth exceeds 64".to_string())
        );
    }

    #[test]
    fn json5_block_comment_apostrophe_does_not_hide_deep_nesting() {
        let src = format!("/* it's fine */\n{}", "[".repeat(5000));
        assert_eq!(
            Flockfile::parse(&src, FlockFormat::Json5).unwrap_err(),
            FlockfileError::Json5("nesting depth exceeds 64".to_string())
        );
    }

    #[test]
    fn json5_benign_comment_does_not_undercount_a_real_document() {
        // Same depth-4 document as `json5_legitimately_nested_doc_still_parses`,
        // plus a comment (apostrophe included) that must be skipped cleanly
        // rather than throwing off the count.
        let src = r#"{
            /* it's the app list */
            app: [{
                name: "web",
                script: "./srv",
                readiness_probe: { kind: "http", target: "http://localhost/x" },
            }],
        }"#;
        let flock = Flockfile::parse(src, FlockFormat::Json5).unwrap();
        assert_eq!(flock.apps.len(), 1);
    }
}
