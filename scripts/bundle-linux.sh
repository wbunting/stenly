#!/usr/bin/env bash
# Build a portable Linux release tarball on Linux (locally or in CI).
#
# Examples:
#   scripts/bundle-linux.sh
#   TARGET=aarch64-unknown-linux-gnu scripts/bundle-linux.sh
#
# The app dynamically uses the host's PipeWire/Wayland/X11 stack. The archive
# deliberately does not bundle those system libraries; distro setup is in
# site/content/install/linux.md. ONNX/sherpa is statically linked by default.
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION=$(awk -F '"' '/^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)
: "${VERSION:?could not read workspace version}"
TARGET=${TARGET:-}
DIST_DIR=${STENLY_DIST_DIR:-target/release/distribution}

if [[ -n "$TARGET" ]]; then
  cargo build --release --target "$TARGET" \
    --features pipewire,wayland,x11 -p stenly -p stenly-cli -p stenly-mcp
  BINDIR="target/$TARGET/release"
  ARCH=${TARGET%%-*}
else
  cargo build --release --features pipewire,wayland,x11 -p stenly -p stenly-cli -p stenly-mcp
  BINDIR="target/release"
  ARCH=$(uname -m)
fi

case "$ARCH" in
  x86_64) ASSET=stenly-linux.tar.gz ;;
  aarch64|arm64) ASSET=stenly-linux-aarch64.tar.gz ;;
  *) ASSET="stenly-linux-${ARCH}.tar.gz" ;;
esac

ROOT="$DIST_DIR/stenly-${VERSION}-linux-${ARCH}"
rm -rf "$ROOT"
mkdir -p "$ROOT/bin" "$DIST_DIR"
install -m 755 "$BINDIR/stenly" "$ROOT/bin/stenly"
install -m 755 "$BINDIR/stenly-cli" "$ROOT/bin/stenly-cli"
install -m 755 "$BINDIR/stenly-mcp" "$ROOT/bin/stenly-mcp"
install -m 755 "$BINDIR/stenly-speech" "$ROOT/bin/stenly-speech"
for lib in "$BINDIR"/*.so*; do
  [[ -e "$lib" ]] || continue
  install -m 755 "$lib" "$ROOT/bin/$(basename "$lib")"
done
install -m 644 README.md "$ROOT/README.md"
cat > "$ROOT/INSTALL.txt" <<EOF
Stenly ${VERSION} for Linux (${ARCH})

1. Install the runtime prerequisites described at:
   https://stenly.ai/install/linux/
2. Run ./bin/stenly

For a user-local install, keep this folder under ~/.local/opt/stenly and add
~/.local/opt/stenly/bin to PATH. Do not use sudo for the app itself. Dictation
hotkeys require membership of the input group; log out and in after adding
yourself.
EOF

tar -C "$DIST_DIR" -czf "$DIST_DIR/$ASSET" "$(basename "$ROOT")"
echo "Built Linux release archive: $DIST_DIR/$ASSET"
