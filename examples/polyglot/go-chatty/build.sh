#!/bin/sh
# Builds the binary `examples/Flockfile.polyglot.toml`'s go-chatty entry
# runs directly, with no `interpreter`. Run once, from anywhere:
#
#   $ examples/polyglot/go-chatty/build.sh
#
# `channel/wire.go` is a verbatim copy of the file shep generates from its
# own wire enums. The diff below is what keeps the copy from going quietly
# stale: regenerate the canonical file with
# `SHEP_CHANNEL_BLESS=1 cargo test -p shep-channel --test wire_export`,
# then copy it here again.
set -eu
cd "$(dirname "$0")"
canonical=../../../crates/shep-channel/wire/channel.go
if [ ! -f "$canonical" ]; then
  # This example is meant to be copied, and a copy lands somewhere with no
  # shep checkout above it. Nothing to compare against is not drift.
  echo "go-chatty: no shep checkout above this directory; skipping the wire check" >&2
elif ! diff -u "$canonical" channel/wire.go; then
  echo "go-chatty: channel/wire.go has drifted from $canonical; copy it again" >&2
  exit 1
fi
go build -o go-chatty .
echo "built polyglot/go-chatty/go-chatty, the path this app's Flockfile entry names"
