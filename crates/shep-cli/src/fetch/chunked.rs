//! The `chunked` transfer coding, the body framing a GET reads when a
//! response carries no `Content-Length`.
//!
//! Reading stops at the last chunk. Trailers are left unread: every request
//! here says `Connection: close`, so nothing follows on this stream.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

use super::FetchError;

/// The most one chunk's size line may hold, line ending included.
///
/// The size needs at most 16 hex digits, and the rest is room for the chunk
/// extensions this reader skips.
const MAX_SIZE_LINE_BYTES: u64 = 1024;

/// Decodes the chunked body at `reader` into its bytes, up to `limit`.
///
/// # Errors
/// - [`FetchError::TooLarge`] when a chunk would take the body past `limit`.
/// - [`FetchError::Transport`] on a malformed chunk, or a stream that closes
///   before the last chunk.
pub(super) async fn read_body<R: AsyncBufRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> Result<Vec<u8>, FetchError> {
    let mut body = Vec::new();
    loop {
        let size = parse_size(&read_line(&mut reader).await?)?;
        if size == 0 {
            return Ok(body);
        }
        if size > (limit - body.len()) as u64 {
            return Err(FetchError::TooLarge { limit });
        }
        let read = (&mut reader).take(size).read_to_end(&mut body).await?;
        if (read as u64) < size {
            return Err(malformed("chunked body ended inside a chunk"));
        }
        if !read_line(&mut reader).await?.is_empty() {
            return Err(malformed("chunk data ran past its declared size"));
        }
    }
}

/// The next line at `reader`, without its line ending.
async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<String, FetchError> {
    let mut line = String::new();
    (&mut *reader)
        .take(MAX_SIZE_LINE_BYTES)
        .read_line(&mut line)
        .await?;
    let Some(line) = line.strip_suffix('\n') else {
        return Err(malformed(
            "chunked body ended early, or a chunk line ran too long",
        ));
    };
    Ok(line.strip_suffix('\r').unwrap_or(line).to_owned())
}

/// The chunk size `line` declares, in hex, with any extensions dropped.
///
/// The digits are checked first because `from_str_radix` also takes a sign.
fn parse_size(line: &str) -> Result<u64, FetchError> {
    let digits = line.split(';').next().unwrap_or(line).trim_end();
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(malformed("chunk size was not a hex number"));
    }
    u64::from_str_radix(digits, 16).map_err(|_overflow| malformed("chunk size was too large"))
}

/// A [`FetchError::Transport`] saying what was wrong with the framing.
fn malformed(reason: &str) -> FetchError {
    FetchError::Transport(std::io::Error::other(reason))
}
