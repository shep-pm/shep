#!/usr/bin/env bash
#
# Rasterise public/og-card.svg to public/og-card.png at 1200x630.
#
# Both files are committed. This one exists because the PNG cannot be
# regenerated without it: the card sets its text in Bricolage Grotesque,
# Space Mono and Space Grotesk, none of which is a system font, and an SVG
# renderer that cannot find a family silently draws nothing at all rather
# than falling back. The first render of this card came back as a correct
# sunrise with no words on it.
#
# The fonts are fetched rather than committed, and fetched through the CSS
# API rather than by a direct file URL: fonts.gstatic.com paths carry a
# version hash that changes when Google reissues a family, so a pinned URL
# rots while a query does not. The API also serves a different format per
# User-Agent, and only an old one is offered TrueType, which is the one
# format resvg reads.
#
# Usage: web/scripts/render-og-card.sh
set -euo pipefail

web="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fonts="$(mktemp -d)"
trap 'rm -rf "$fonts"' EXIT

# Old enough to be served TrueType instead of WOFF2.
ua="Mozilla/5.0 (Linux; U; Android 2.2; en-us) AppleWebKit/533.1 (KHTML, like Gecko) Version/4.0 Mobile Safari/533.1"

fetch_font() {
  local family="$1" out="$2" url
  url=$(curl -fsS -H "User-Agent: $ua" \
    "https://fonts.googleapis.com/css2?family=$family" |
    grep -oE 'https://[^)]+\.ttf' | head -1)
  if [ -z "$url" ]; then
    echo "no TrueType URL for $family" >&2
    exit 1
  fi
  curl -fsS -o "$fonts/$out" "$url"
}

fetch_font 'Bricolage+Grotesque:opsz,wght@48,800' bricolage.ttf
fetch_font 'Space+Mono:wght@700' space-mono.ttf
fetch_font 'Space+Grotesk:wght@500' space-grotesk.ttf

# --no-system-font keeps a local copy of one of these families from being
# picked up on one machine and not another, which would make the committed
# PNG depend on whose laptop rendered it.
npx --yes @resvg/resvg-js-cli --no-system-font \
  --font-file "$fonts/bricolage.ttf" \
  --font-file "$fonts/space-mono.ttf" \
  --font-file "$fonts/space-grotesk.ttf" \
  "$web/public/og-card.svg" "$web/public/og-card.png"

echo "wrote web/public/og-card.png"
