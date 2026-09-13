#!/usr/bin/env python3
"""Speaks the shepherd channel: readiness, a metric, and custom actions.

The contract is docs/shepherd-channel.md, and this frames it by hand
because Python has no shep client library. examples/src/bin/chatty.rs is
the same app in Rust, where the shep-channel crate does this part.

Three things a hand-roll has to get right, all of them below:

  1. Reply to every action, including a name this app has never heard of.
     Silence and a slow handler look the same from the shepherd's side,
     so an operator's typo costs them the whole action_timeout.
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
    pipe = os.environ.get("SHEP_CHANNEL_PIPE")
    if pipe:
        return open(pipe, "rb+", buffering=0)
    fd = os.environ.get("SHEP_CHANNEL_FD")
    if fd:
        # A refusal an operator can act on, rather than the traceback a bare
        # int() would print.
        if not fd.isdigit():
            sys.exit(f"python-chatty: SHEP_CHANNEL_FD is {fd!r}, not a descriptor number")
        return os.fdopen(int(fd), "rb+", buffering=0)
    return None


def send(channel, message):
    channel.write(json.dumps(message).encode() + b"\n")


def say(text):
    """Prints a line shep collects as a bleat, now rather than at exit.

    Python block buffers stdout whenever it is not a terminal, and under a
    supervisor it never is. Without the flush an app can run for hours with
    an empty log and look hung.
    """
    print(text, flush=True)


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
    """
    if not params:
        return None
    words = params.split()
    if not words or words[0] not in LEVELS:
        return None
    return words[0]


def reply_to(action, params):
    """The body the operator reads back from shep trigger."""
    if action == "ping":
        return f"pong from python pid={os.getpid()}, up {time.monotonic() - STARTED:.1f}s"
    if action == "level":
        level = parse_level(params)
        if level is None:
            return f"usage: level <{'|'.join(LEVELS)}> [key=value ...]"
        return f"log level is now {level}"
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
        say(f"python-chatty: shepherd speaks channel {stamp}, this app speaks 1")

    send(channel, {"kind": "ready"})
    send(channel, {"kind": "metric", "name": "starts", "value": 1})
    say(f"python-chatty pid={os.getpid()} ready on the shepherd channel")

    # One thread reads and writes, which is what keeps the Windows arm
    # safe: a blocking read parked on a pipe handle would hold it against
    # this app's own writes. A metrics ticker is the usual way to end up
    # with two, so this app emits samples from the loop instead.
    samples = 0
    for line in iter(channel.readline, b""):
        try:
            message = json.loads(line)
        except ValueError as err:
            # The shepherd does not write these, so a frame that will not
            # parse means a wire this app has never seen. Say so and read
            # the next one; dying here would also drop the action after it.
            say(f"python-chatty: could not read a message: {err}")
            continue
        # Parsing is not the same as being a message. A bare number, list,
        # string or null is all valid JSON and none of them is one of ours.
        if not isinstance(message, dict):
            say(f"python-chatty: ignoring a frame that is not an object: {line!r}")
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
        if not isinstance(name, str) or ident is None:
            continue

        params = message.get("params")
        if name == "metric":
            samples += 1
            metric = metric_name(params)
            send(channel, {"kind": "metric", "name": metric, "value": samples})
            body = f"sent {metric}={samples}"
        else:
            body = reply_to(name, params) or f"unknown action: {name}"

        send(channel, {"kind": "action-reply", "action": name, "body": body, "id": ident})

    # The shepherd going away is not a reason to stop. shep-channel leaves a
    # Rust app running for the same reason: a channel is something an app
    # has, not what it is for, and a shepherd can be replaced under it.
    say("python-chatty: the shepherd went away; still running")
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
