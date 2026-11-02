#!/usr/bin/env bash
# Regenerate the installer-window background PNGs (1x and @2x) from
# crates/stenly/assets/dmg-background.svg. Requires librsvg (rsvg-convert).
# The PNGs are committed, so the release build does not need librsvg.
set -euo pipefail
cd "$(dirname "$0")/.."
ASSETS=crates/stenly/assets
command -v rsvg-convert >/dev/null || { echo "Install librsvg (rsvg-convert) first." >&2; exit 1; }
rsvg-convert -w 660 -h 400 "$ASSETS/dmg-background.svg" -o "$ASSETS/dmg-background.png"
rsvg-convert -w 1320 -h 800 "$ASSETS/dmg-background.svg" -o "$ASSETS/dmg-background@2x.png"
# tiffutil -cathidpicheck (used by dmgbuild) requires the @2x file to be 144 dpi.
sips -s dpiWidth 72 -s dpiHeight 72 "$ASSETS/dmg-background.png" >/dev/null
sips -s dpiWidth 144 -s dpiHeight 144 "$ASSETS/dmg-background@2x.png" >/dev/null
