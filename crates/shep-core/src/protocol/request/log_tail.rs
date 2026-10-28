//! The request and answer for the tail of one sheep's log file.

use serde::{Deserialize, Serialize};

/// Which of a sheep's two log files a tail reads.
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LogStream {
    /// The file the sheep's stdout is written to (`out_file`).
    Out,
    /// The file the sheep's stderr is written to (`err_file`).
    Err,
}

/// What reading the end of one sheep's log file came to.
///
/// Every outcome of the read is a value here, never an `RpcError`, a
/// missing or unreadable file included: a client prints the same notice
/// a local `bleats` does.
///
/// One answer holds the tail of one file, at most [`TAIL_WINDOW_BYTES`](crate::logtail::TAIL_WINDOW_BYTES) of it.
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LogTail {
    /// The tail was read.
    Lines {
        /// The last lines of the file, oldest first, with the shepherd's
        /// stamp taken off when the sheep has one.
        lines: Vec<String>,
        /// Whether the byte window or the line count left earlier content
        /// behind, so these lines are not the whole file.
        truncated: bool,
    },
    /// The sheep has no path recorded for this stream.
    NoPath,
    /// The shepherd looked for the file and it is not there.
    Missing {
        /// The path the shepherd tried, as recorded on the sheep's row.
        path: String,
    },
    /// The file is there and could not be read.
    Unreadable {
        /// The path the shepherd tried, as recorded on the sheep's row.
        path: String,
        /// The operating system's reason, as text.
        reason: String,
    },
    /// An outcome this build has not been taught.
    ///
    /// Only ever produced by decoding: an unrecognized `kind` falls through
    /// to this variant via `#[serde(other)]` instead of failing the whole
    /// frame. Nothing constructs one to send, which is a call-site
    /// invariant rather than a type-level one: `#[serde(other)]` governs
    /// decoding only, so serializing this would emit `"unrecognized"`.
    #[serde(other)]
    Unrecognized,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logtail::TAIL_WINDOW_BYTES;
    use crate::protocol::{MAX_FRAME_BYTES, Response, encode_frame};

    /// An outcome added next year must decode, not fail the whole frame.
    #[test]
    fn an_unknown_tail_outcome_decodes_as_unrecognized() {
        let tail: LogTail = serde_json::from_str(r#"{"kind":"rotated","generation":3}"#).unwrap();

        assert_eq!(tail, LogTail::Unrecognized);
    }

    /// The worst case for the 16 MiB frame cap: every byte of the window is
    /// a control byte on its own line, which JSON spells with six bytes.
    #[test]
    fn a_full_window_of_control_bytes_still_encodes_under_the_frame_cap() {
        let window = usize::try_from(TAIL_WINDOW_BYTES).unwrap();
        // `\u0000` plus the line's own newline is two bytes of file per line.
        let lines = vec!["\0".to_string(); window / 2];
        let answer = Response::LogTail(LogTail::Lines {
            lines,
            truncated: true,
        });

        let frame = encode_frame(&answer).expect("under the frame cap");

        assert!(frame.len() < MAX_FRAME_BYTES);
    }
}
