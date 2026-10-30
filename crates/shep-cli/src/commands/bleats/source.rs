//! Where a `bleats` tail reads its lines: this machine's files, or a host's
//! shepherd.

use std::io;
use std::path::Path;

use shep_client::{Client, RequestError};
use shep_core::protocol::{LogStream, LogTail, ProcessInfo, RpcErrorCode};

use super::missing_log_note;
use super::read_tail;
use crate::commands::rpc::typed_request_error;
use crate::exit::ExitCode;
use crate::output::Streams;
use crate::remote::ssh_hint;

/// Where a tail's lines are read from.
#[derive(Clone, Copy)]
pub(crate) enum LogSource<'a> {
    /// The files `out_file`/`err_file` name, opened by this process.
    Local,
    /// The shepherd of host `name`, which reads its own files: the paths in
    /// its rows are not files here.
    Host {
        /// The link to that shepherd.
        client: &'a Client,
        /// The `--host` destination, for the notices that name it.
        name: &'a str,
    },
}

/// What reading one log file came to, whichever side read it.
pub(crate) enum FileTail {
    /// The tail, oldest line first.
    Lines {
        /// The lines read.
        lines: Vec<String>,
        /// Whether the line count or the byte window left earlier content
        /// behind.
        truncated: bool,
    },
    /// The sheep has no path recorded for the stream.
    NoPath,
    /// The file is not there. Holds the whole note, since only the source
    /// knows which machine's directory a relative path names.
    Missing(String),
    /// The file is there and could not be read. Holds the whole message.
    Unreadable(String),
}

impl LogSource<'_> {
    /// The last `lines` lines of `stream`'s file for `info`, recorded at `path`.
    ///
    /// # Errors
    /// A host's shepherd did not give an answer: see [`Self::stop`]. A sheep
    /// the host no longer has is not one: it reads as [`FileTail::Missing`].
    pub(crate) async fn read(
        &self,
        info: &ProcessInfo,
        stream: LogStream,
        path: &str,
        lines: usize,
    ) -> Result<FileTail, RequestError> {
        let label = stream_label(stream);
        match self {
            Self::Local => {
                // The row that reaches the file first decides whether it is
                // stamped: instances of one app share both the path and its
                // `log_timestamps`.
                let stamped = info.log_timestamps.unwrap_or(true);
                Ok(match read_tail(Path::new(path), lines, stamped) {
                    Ok((lines, truncated)) => FileTail::Lines { lines, truncated },
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        FileTail::Missing(missing_log_note(label, Path::new(path)))
                    }
                    Err(err) => FileTail::Unreadable(format!("failed to read {path}: {err}")),
                })
            }
            Self::Host { client, name } => {
                let lines = u32::try_from(lines).unwrap_or(u32::MAX);
                let tail = match client.tail_log(info.id, stream, lines).await {
                    Ok(tail) => tail,
                    // The host is answering, only this sheep left its rows
                    // between the listing and the read: skip it, as a local
                    // run does not stop over one sheep.
                    Err(RequestError::Rpc(rpc)) if rpc.code == RpcErrorCode::NotFound => {
                        return Ok(FileTail::Missing(format!(
                            "no {label} log to read: the sheep is gone from {name}"
                        )));
                    }
                    Err(err) => return Err(err),
                };
                Ok(match tail {
                    LogTail::Lines { lines, truncated } => FileTail::Lines { lines, truncated },
                    LogTail::NoPath => FileTail::NoPath,
                    // The path as the host recorded it, never made absolute
                    // here: this machine's directory says nothing about it.
                    LogTail::Missing { path } => {
                        FileTail::Missing(format!("no {label} log at {path} on {name} yet"))
                    }
                    LogTail::Unreadable { path, reason } => {
                        FileTail::Unreadable(format!("failed to read {path} on {name}: {reason}"))
                    }
                    // `LogTail` is `#[non_exhaustive]`: `Unrecognized` and an
                    // outcome a newer shepherd added are the same here.
                    _ => FileTail::Unreadable(format!(
                        "failed to read {path} on {name}: this client does not understand \
                         the answer"
                    )),
                })
            }
        }
    }

    /// Reports a request that got no answer, and hands back the code the
    /// whole verb stops on.
    ///
    /// A shepherd that predates `Request::TailLog` refuses every file alike,
    /// so it is one failure naming its host and the fix, not a notice per
    /// file.
    pub(super) fn stop(&self, streams: &mut Streams<'_>, err: &RequestError) -> ExitCode {
        match (self, err) {
            (Self::Host { name, .. }, RequestError::Rpc(rpc))
                if rpc.code == RpcErrorCode::Unsupported =>
            {
                let words = ["shep", "daemon", "reload"].map(String::from);
                let message = format!(
                    "the shepherd on {name} predates remote log reads; run `{}` to load a \
                     newer one",
                    ssh_hint(name, &words)
                );
                streams.fail(ExitCode::Unsupported, &message)
            }
            _ => typed_request_error(streams, err),
        }
    }
}

/// The word `bleats` prints for `stream`, the same one a `BleatLine` carries.
pub(super) fn stream_label(stream: LogStream) -> &'static str {
    match stream {
        LogStream::Err => "err",
        _ => "out",
    }
}
