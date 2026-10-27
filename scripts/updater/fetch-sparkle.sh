#!/bin/bash
# Fetch a pinned upstream SDK for development/explicit release-owner integration.
# Does not change production packaging or configure a feed.
set -euo pipefail
out=${1:?Pass a NEW output directory}
[[ ! -e "$out" ]] || { echo 'Refusing existing output directory' >&2; exit 2; }
archive=$(mktemp); trap 'rm -f "$archive"' EXIT
curl --fail --location --proto '=https' --tlsv1.2 https://github.com/sparkle-project/Sparkle/releases/download/2.10.0/Sparkle-2.10.0.tar.xz -o "$archive"
echo "c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c  $archive" | shasum -a 256 -c -
mkdir -p "$out"; tar -xJf "$archive" -C "$out"
printf 'Sparkle2.10.0 SDK: %s\n' "$out"
