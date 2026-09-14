#!/bin/sh
# Builds the binary that `examples/Flockfile.polyglot.toml`'s go-http
# entry runs directly, with no `interpreter` -- a compiled program needs
# none. Not a static one: net/http reaches the system resolver, so this
# links libresolv, CoreFoundation and Security on macOS. Run once, from
# anywhere:
#
#   $ examples/polyglot/go-http/build.sh
set -eu
cd "$(dirname "$0")"
go build -o go-http .
echo "built $(pwd)/go-http"
echo "  its Flockfile entry names it polyglot/go-http/go-http, relative to examples/"
