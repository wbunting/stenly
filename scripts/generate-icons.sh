#!/usr/bin/env bash
# Regenerate app PNGs, macOS ICNS, and site favicon from the canonical SVG.
# Requires librsvg (rsvg-convert) and macOS iconutil for ICNS.
set -euo pipefail
cd "$(dirname "$0")/.."
ASSETS=crates/stenly/assets
command -v rsvg-convert >/dev/null || { echo "Install librsvg (rsvg-convert) first." >&2; exit 1; }
for size in 64 192 256; do
  rsvg-convert -w "$size" -h "$size" "$ASSETS/logo.svg" -o "$ASSETS/logo-$size.png"
done
cp "$ASSETS/logo.svg" site/static/favicon.svg
if command -v iconutil >/dev/null; then
  WORK=$(mktemp -d)
  trap 'rm -rf "$WORK"' EXIT
  mkdir "$WORK/stenly.iconset"
  for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" "$ASSETS/logo.svg" -o "$WORK/stenly.iconset/icon_${size}x${size}.png"
    double=$((size * 2))
    rsvg-convert -w "$double" -h "$double" "$ASSETS/logo.svg" -o "$WORK/stenly.iconset/icon_${size}x${size}@2x.png"
  done
  iconutil -c icns "$WORK/stenly.iconset" -o "$ASSETS/stenly.icns"
else
  echo "PNG and favicon updated; run on macOS to regenerate stenly.icns." >&2
fi
