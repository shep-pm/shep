#!/usr/bin/env python3
"""Speaks the shepherd channel: readiness, a metric, and custom actions.

The contract is docs/shepherd-channel.md, and this frames it by hand
because Python has no shep client library. examples/src/bin/chatty.rs is
the same app in Rust, where the shep-channel crate does this part.

Three things a hand-roll has to get right, all of them below:

  1. Reply to every action this app can answer, including one whose name
     it has never heard of. Silence and a slow handler look the same from
     the shepherd's side, so an operator's typo costs them the whole
     action_timeout. A frame carrying no name or no id is not answerable:
     nothing to name, nowhere to send it.
  2. Echo the action's id on the reply. Without it shep matches replies
     by name and by order, which goes wrong once two of one name are
     outstanding.
  3. Own the grammar of params. It is one opaque string that shep never
     splits. See parse_level.

Usage: python-chatty.py
"""

import json
import os
import sys
import time

LEVELS = ("trace", "debug", "info", "warn", "error")
# What an unparsable level gets back. Built from LEVELS rather than spelled
# out, so adding one cannot leave the message listing the old set. Says the
# rest is dropped rather than inviting arguments this app does not read.
USAGE = f"usage: level <{'|'.join(LEVELS)}> [rest is ignored]"
STARTED = time.monotonic()


def open_channel():
    """Returns the one file object this app reads and writes, or None.

    Exactly one of the two variables is ever set, so branch on which one
    is present rather than on the platform. Windows gets a named pipe
    path to open; unix gets a descriptor it already holds.

    Binary and unbuffered, for two separate reasons. Text mode on Windows
    would translate every "\\n" this app writes into "\\r\\n". And a unix
    channel is not seekable, which rules out Python's buffered read-write
    object: open(path, "r+") raises UnsupportedOperation there. A Windows
    named pipe does report seekable, so this spelling is the one that
    works on both.
    """
    # Both opens can fail on a value that looked fine: a pipe path that is
    # not there, a descriptor number that is not open. Every other
    # misconfiguration here exits with a sentence, so these do too.
    pipe = os.environ.get("SHEP_CHANNEL_PIPE")
    if pipe:
        try:
            return open(pipe, "rb+", buffering=0)
        except OSError as err:
            sys.exit(f"python-chatty: cannot open SHEP_CHANNEL_PIPE {pipe!r}: {err}")
    fd = os.environ.get("SHEP_CHANNEL_FD")
    if fd:
        if not fd.isdigit():
            sys.exit(f"python-chatty: SHEP_CHANNEL_FD is {fd!r}, not a descriptor number")
        try:
            return os.fdopen(int(fd), "rb+", buffering=0)
        except OSError as err:
            sys.exit(f"python-chatty: cannot use SHEP_CHANNEL_FD {fd!r}: {err}")
    return None


def wire(message):
    """Encodes one message the way the other three examples encode it.

    serde_json, encoding/json and JSON.stringify all emit compact JSON and
    leave non-ASCII alone. json.dumps does neither by default.
    """
    return json.dumps(message, ensure_ascii=False, separators=(",", ":"))


# Set once the shepherd stops accepting writes, so the first failure is
# reported and the rest stay quiet.
_shepherd_gone = False

# ping reads this back, so the reply that says the level changed is
# answerable. Module scope because reply_to is not a method and one
# thread owns it, the same shape _shepherd_gone above uses.
_level = "info"


def read_lines(channel):
    """Yields one message a line, and stops rather than raising.

    A read fails the same way a write does once the shepherd has gone:
    EIO on unix, a broken-pipe OSError on a Windows named pipe. Letting
    that escape would end the process, which is the opposite of what this
    app decided about a closed channel.
    """
    while True:
        try:
            line = channel.readline()
        except OSError as err:
            warn(f"python-chatty: could not read from the shepherd: {err}")
            return
        if not line:
            return
        yield line


def send(channel, message):
    """Writes one message, or says once that the shepherd stopped listening.

    A failed write means the same thing a closed channel does, so it gets
    the same answer: say so and carry on. The read loop ends on its own
    next pass.
    """
    global _shepherd_gone
    try:
        # Not bare json.dumps: its defaults put a space after every colon and
        # comma and escape non-ASCII, so Python's line differed from the three
        # other examples byte for byte while decoding to the same message.
        # An unbuffered binary write is one write(2): it returns short
        # rather than failing when the pipe fills, and the next frame would
        # then start mid-line. The other three never see this, since Go,
        # libuv and Rust all loop for you.
        rest = memoryview(wire(message).encode() + b"\n")
        while rest:
            written = channel.write(rest)
            if not written:
                raise OSError("wrote no bytes to the shepherd")
            rest = rest[written:]
    except OSError as err:
        if not _shepherd_gone:
            _shepherd_gone = True
            warn(f"python-chatty: could not write to the shepherd: {err}")


def say(text):
    """Prints a line shep collects as a bleat, now rather than at exit.

    Python block buffers stdout whenever it is not a terminal, and under a
    supervisor it never is. Without the flush an app can run for hours with
    an empty log and look hung.
    """
    print(text, flush=True)


def warn(text):
    """The same, for something that went wrong, so shep files it as stderr."""
    print(text, file=sys.stderr, flush=True)


def snippet(line):
    """Enough of a frame to recognise it, bounded.

    A frame has no length limit and a log line should not inherit one.
    Both doors in `open_channel` open binary, so a line is always bytes.
    """
    text = line.decode("utf-8", "replace").rstrip("\n")
    return wire(text[:80] + "..." if len(text) > 80 else text)


def metric_name(params):
    """Names the metric one `metric` action should send.

    params reaches an app exactly as the operator typed it, so an empty or
    blank one is ordinary rather than a mistake. Both fall back, since a
    metric named "" is worse on the bus than no custom name at all.
    """
    return (params or "").strip() or "triggers"


def parse_level(params):
    """Reads a log level out of one action's params, in this app's grammar.

    shep passes whatever the operator typed as a single string and never
    looks inside it, so every app decides how its own actions are spelled.
    This one splits on whitespace and reads the first word, which means a
    level can never contain a space. An app needing one would put JSON in
    this string instead.

    The remainder comes back with the level so the reply can say it was
    dropped, because an app that silently ignores half of what it was
    handed is the thing this action exists to warn about.
    """
    if not params:
        return None
    # split(None, 1), not split(): the second keeps every word and would
    # report a tab as a space, and the point is saying what was dropped.
    words = params.strip().split(None, 1)
    if not words or words[0] not in LEVELS:
        return None
    return words[0], words[1].strip() if len(words) > 1 else ""


def reply_to(action, params):
    """The body the operator reads back, or None for a name this app
    does not know.

    None rather than an empty string, and the caller tests for it rather
    than for falsiness, so a reply body that is legitimately empty stays
    a reply instead of turning into "unknown action".
    """
    global _level
    if action == "ping":
        return (
            f"pong from python pid={os.getpid()}, "
            f"up {time.monotonic() - STARTED:.1f}s, level {_level}"
        )
    if action == "level":
        parsed = parse_level(params)
        if parsed is None:
            return USAGE
        _level, rest = parsed
        if not rest:
            return f"log level is now {_level}"
        # wire(), not !r: it quotes and escapes the way the Rust, Go and
        # JavaScript examples do, so all four reply with the same bytes.
        return f"log level is now {_level}, ignored {wire(rest)}"
    return None


def main():
    channel = open_channel()
    if channel is None:
        sys.exit(
            "python-chatty: no shepherd channel. Set channel = true on this "
            "app in the Flockfile, or wait_ready, or shutdown_with_message."
        )

    # Warn and carry on, unlike the missing-channel case above, which exits.
    # The contract asks an app to notice a wire it has never seen and say so,
    # not to refuse one: a later version may still carry these messages.
    stamp = os.environ.get("SHEP_CHANNEL_VERSION")
    if stamp is not None and stamp != "1":
        warn(f"python-chatty: shepherd speaks channel {stamp}, this app speaks 1")

    send(channel, {"kind": "ready"})
    send(channel, {"kind": "metric", "name": "starts", "value": 1})
    say(f"python-chatty pid={os.getpid()} ready on the shepherd channel")

    # One thread reads and writes, which is what keeps the Windows arm
    # safe: a blocking read parked on a pipe handle would hold it against
    # this app's own writes. A metrics ticker is the usual way to end up
    # with two, so this app emits samples from the loop instead.
    samples = 0
    for line in read_lines(channel):
        try:
            message = json.loads(line)
        except ValueError as err:
            # The shepherd does not write these, so a frame that will not
            # parse means a wire this app has never seen. Say so and read
            # the next one; dying here would also drop the action after it.
            warn(f"python-chatty: could not read a message: {err}, in {snippet(line)}")
            continue
        # Parsing is not the same as being a message. A bare number, list,
        # string or null is all valid JSON and none of them is one of ours.
        if not isinstance(message, dict):
            warn(f"python-chatty: ignoring a frame that is not an object: {line!r}")
            continue

        kind = message.get("kind")
        if kind == "shutdown":
            say("python-chatty: the shepherd asked us to stop")
            return
        if kind != "action":
            continue

        # Every action carries both, so one that does not is not something
        # this app can answer, and carries nowhere to send the answer.
        name, ident = message.get("name"), message.get("id")
        if not isinstance(name, str):
            warn("python-chatty: ignoring an action with no name")
            continue
        # The wire types declare id as a u64, so the reply has to carry one
        # back. A typed example gets this free; here it is written out. bool
        # is excluded on purpose, since True is an int in Python and would
        # otherwise reply to action 1.
        if not isinstance(ident, int) or isinstance(ident, bool) or not 0 <= ident < 2**64:
            warn(f"python-chatty: ignoring {name}, its id is not a u64")
            continue

        # params is a string or it is absent. A typed language gets this
        # free: serde and encoding/json both refuse a number here and
        # reject the whole frame, so hand-rolling is where the check has
        # to be written out.
        params = message.get("params")
        if params is not None and not isinstance(params, str):
            warn(f"python-chatty: ignoring {name}, its params is not a string")
            continue
        if name == "metric":
            samples += 1
            metric = metric_name(params)
            send(channel, {"kind": "metric", "name": metric, "value": samples})
            body = f"sent {metric}={samples}"
        else:
            body = reply_to(name, params)
            if body is None:
                body = f"unknown action: {name}"

        send(channel, {"kind": "action-reply", "action": name, "body": body, "id": ident})

    # The shepherd going away is not a reason to stop. shep-channel leaves a
    # Rust app running for the same reason: a channel is something an app
    # has, not what it is for, and a shepherd can be replaced under it.
    say("python-chatty: the shepherd went away; still running")
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
