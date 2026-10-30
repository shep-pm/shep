//! The ssh stand-ins `read_tests` dials a host through: a shell script that
//! answers one handshake and then a scripted reply per request.

use std::path::Path;

use shep_core::protocol::Response;

use crate::whistle::matching_ack;

/// One value as the length-prefixed frame a shepherd would put on the wire.
#[cfg(unix)]
pub(super) fn framed<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let payload = shep_core::protocol::encode_frame(value).unwrap();
    let mut bytes = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
    bytes.extend_from_slice(&payload);
    bytes
}

/// The three files a stand-in ssh writes, named the way the script names
/// them.
#[cfg(unix)]
pub(super) fn stand_in_file(dir: &Path, name: &str) -> std::path::PathBuf {
    dir.join(format!("ssh-{name}"))
}

/// Writes an executable stand-in for `ssh` into `dir` and returns its path,
/// answering the handshake with this binary's own version.
///
/// `Shepherd::connect` refuses a skew, and `matching_ack`'s version is the one
/// a host link is held to as well, so every test that wants a flock back
/// starts here.
#[cfg(unix)]
pub(super) fn ssh_stand_in(dir: &Path, response: Response) -> std::path::PathBuf {
    ssh_stand_in_answering(dir, Ok(matching_ack()), response)
}

/// [`ssh_stand_in`], with the handshake it answers.
///
/// The ack goes out between the `Hello` and the request, so a refusal there
/// reaches whistle as [`ConnectError::ProtocolMismatch`].
#[cfg(unix)]
pub(super) fn ssh_stand_in_answering(
    dir: &Path,
    handshake: shep_core::protocol::HelloReply,
    response: Response,
) -> std::path::PathBuf {
    ssh_stand_in_replying(dir, handshake, vec![response])
}

/// A stand-in that answers one request per entry of `responses`, in order.
///
/// It records its arguments and writes each request to `request-N.bin`, N
/// counting from 1. It answers a request only once that request has arrived:
/// an earlier reply is dropped.
#[cfg(unix)]
pub(super) fn ssh_stand_in_replying(
    dir: &Path,
    handshake: shep_core::protocol::HelloReply,
    responses: Vec<Response>,
) -> std::path::PathBuf {
    use shep_core::protocol::Reply;
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::write(stand_in_file(dir, "ack.bin"), framed(&handshake)).unwrap();
    let mut script = format!(
        "#!/bin/sh\n\
         read_frame() {{ set -- $(dd bs=1 count=4 2>/dev/null | od -An -tu1); \
         dd bs=1 count=$(( $1 * 16777216 + $2 * 65536 + $3 * 256 + $4 )) 2>/dev/null; }}\n\
         printf '%s\\n' \"$@\" > '{}'\n\
         read_frame > /dev/null\n\
         cat '{}'\n",
        stand_in_file(dir, "args.txt").display(),
        stand_in_file(dir, "ack.bin").display(),
    );
    // `shep_client::actor`'s `next_id` starts at 1 and counts up by one.
    for (index, response) in responses.into_iter().enumerate() {
        let id = index as u64 + 1;
        let reply = Reply {
            id,
            result: Ok(response),
        };
        let reply_file = stand_in_file(dir, &format!("reply-{id}.bin"));
        std::fs::write(&reply_file, framed(&reply)).unwrap();
        script.push_str(&format!(
            "read_frame > '{}'\ncat '{}'\n",
            stand_in_file(dir, &format!("request-{id}.bin")).display(),
            reply_file.display(),
        ));
    }
    let program = dir.join("ssh");
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}
