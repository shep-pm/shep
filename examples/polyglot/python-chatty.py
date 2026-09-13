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
        message = json.loads(line)
        kind = message.get("kind")
        if kind == "shutdown":
            say("python-chatty: the shepherd asked us to stop")
            return
        if kind != "action":
            continue

        name, params = message["name"], message.get("params")
        if name == "metric":
            samples += 1
            metric = params or "triggers"
            send(channel, {"kind": "metric", "name": metric, "value": samples})
            body = f"sent {metric}={samples}"
        else:
            body = reply_to(name, params) or f"unknown action: {name}"

        send(channel, {"kind": "action-reply", "action": name, "body": body, "id": message["id"]})


if __name__ == "__main__":
    main()
