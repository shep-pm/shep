#!/bin/sh
# Builds the static binary that `examples/Flockfile.polyglot.toml`'s
# go-http entry runs directly, with no `interpreter` -- a compiled program
# needs none. Run once, from anywhere:
#
#   $ examples/polyglot/go-http/build.sh
set -eu
cd "$(dirname "$0")"
go build -o go-http .
echo "built $(pwd)/go-http"
echo "  its Flockfile entry names it polyglot/go-http/go-http, relative to examples/"
