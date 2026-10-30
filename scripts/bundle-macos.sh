#!/usr/bin/env bash
# Build a distributable, Developer ID-signed Stenly disk image.
#
# Usage:
#   scripts/bundle-macos.sh                     # ad-hoc signed development build
#   STENLY_SIGN_IDENTITY='Developer ID Application: Example Name (ABCDE12345)' \
#     STENLY_NOTARY_PROFILE=stenly-notary scripts/bundle-macos.sh
#   STENLY_BUNDLE_ID=ai.stenly.app.release-test scripts/bundle-macos.sh
#   STENLY_BUNDLE_NAME='Stenly Preview' scripts/bundle-macos.sh
#   STENLY_UNIVERSAL=1 scripts/bundle-macos.sh  # arm64 + Intel universal app
#   STENLY_SIGN_IDENTITY='Developer ID Application: Example Name (ABCDE12345)' \
#     scripts/bundle-macos.sh --install  # requires matching installed app identity
#
# STENLY_SIGN_IDENTITY names the Developer ID certificate to sign with. Without
# it the bundle is ad-hoc signed for local development (never --install). A release
# (STENLY_REQUIRE_DEVELOPER_ID=1 or STENLY_NOTARY_PROFILE set) refuses to run
# unless the identity is set and installed in a keychain.
#
# STENLY_ENFORCE_LICENSE=1 builds the free-trial and licence gate into the app.
# Only the official release workflow sets it, and only a Developer ID build may
# carry it (source and ad-hoc builds are never gated). STENLY_CHECKOUT_URL is
# the https checkout page the Buy button opens and STENLY_POLAR_ORG_ID the Polar
# organization the app activates keys against. Both are public, not secrets.
#
# STENLY_DIST_DIR selects the output directory. A non-default name or
# identifier is useful for a local permission-safe test and is refused with
# --install, so the daily-use /Applications/Stenly.app is never replaced by it.
set -euo pipefail

cd "$(dirname "$0")/.."

VERSION=$(awk -F '"' '/^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)
: "${VERSION:?could not read workspace version}"
BUNDLE_ID=${STENLY_BUNDLE_ID:-ai.stenly.app}
BUNDLE_NAME=${STENLY_BUNDLE_NAME:-Stenly}
DIST_DIR=${STENLY_DIST_DIR:-target/release/distribution}
APP="$DIST_DIR/${BUNDLE_NAME}.app"
DMG="$DIST_DIR/stenly-macos.dmg"
INSTALL=false
if [[ $# == 1 && "$1" == "--install" ]]; then
  INSTALL=true
elif [[ $# -gt 0 ]]; then
  echo "usage: $0 [--install]" >&2
  exit 2
fi
if [[ "$INSTALL" == true && ( "$BUNDLE_ID" != "ai.stenly.app" || "$BUNDLE_NAME" != "Stenly" ) ]]; then
  echo "--install only accepts the default Stenly bundle name and identifier" >&2
  exit 2
fi
ENTITLEMENTS="scripts/macos-entitlements.plist"
IDENTITY=${STENLY_SIGN_IDENTITY:-}
NOTARY_PROFILE=${STENLY_NOTARY_PROFILE:-}
NOTARY_KEYCHAIN=${STENLY_NOTARY_KEYCHAIN:-}
REQUIRE_DEVELOPER_ID=${STENLY_REQUIRE_DEVELOPER_ID:-0}
UNIVERSAL=${STENLY_UNIVERSAL:-0}
INTEL_TARGET=x86_64-apple-darwin

# Preflight precedes cargo, output cleanup, and every daily-app mutation.
# An explicitly supplied identity is always strict, including development builds.
source scripts/macos-signing.sh
validate_distribution_path "$DIST_DIR" /Applications/Stenly.app
DEVELOPER_ID_AVAILABLE=false
SIGN_ARGS=(--sign -)
if [[ -n "$IDENTITY" || "$INSTALL" == true || "$REQUIRE_DEVELOPER_ID" == "1" || -n "$NOTARY_PROFILE" ]]; then
  resolve_developer_id
  DEVELOPER_ID_AVAILABLE=true
  SIGN_ARGS=(--timestamp --options runtime --sign "$SIGN_IDENTITY_HASH")
  INSTALLED_REQUIREMENT=
  if [[ "$INSTALL" == true ]]; then
    read_installed_requirement /Applications/Stenly.app
  fi
  probe_signing_identity
  echo "Using explicitly selected Developer ID Application identity."
else
  echo "Creating an ad-hoc development DMG; it cannot be used with --install." >&2
fi

sign_file() {
  local file=$1
  codesign --force "${SIGN_ARGS[@]}" "$file"
}

sign_entitled_executable() {
  local file=$1
  codesign --force "${SIGN_ARGS[@]}" --entitlements "$ENTITLEMENTS" "$file"
}

# Sign Mach-O executables and dylibs before their containing app. sherpa-onnx
# currently links statically, but this remains correct if it switches to shared
# libraries (and for any future native runtime copied into Frameworks).
sign_nested_code() {
  local code
  while IFS= read -r -d '' code; do
    sign_file "$code"
  done < <(
    # The two audio executables receive entitlements below; do not sign them
    # generically first. Dylibs are included once even though they are +x.
    find "$APP/Contents" -type f \( -name '*.dylib' -o \( -perm -u+x ! -name 'stenly' ! -name 'stenly-cli' ! -name '*.dylib' \) \) -print0
  )
}

# The licence gate belongs to the official signed build only.
if [[ "${STENLY_ENFORCE_LICENSE:-0}" == "1" ]]; then
  if [[ "$DEVELOPER_ID_AVAILABLE" != true ]]; then
    echo "STENLY_ENFORCE_LICENSE=1 requires a Developer ID build (set STENLY_SIGN_IDENTITY)" >&2
    exit 2
  fi
  if [[ "${STENLY_CHECKOUT_URL:-}" != https://* ]]; then
    echo "STENLY_ENFORCE_LICENSE=1 requires STENLY_CHECKOUT_URL=https://… (the Buy page)" >&2
    exit 2
  fi
  if [[ -z "${STENLY_POLAR_ORG_ID:-}" ]]; then
    echo "STENLY_ENFORCE_LICENSE=1 requires STENLY_POLAR_ORG_ID (Polar organization ID)" >&2
    exit 2
  fi
  echo "Building with the free-trial and licence gate (official build)."
  export STENLY_ENFORCE_LICENSE=1 STENLY_CHECKOUT_URL STENLY_POLAR_ORG_ID
else
  # A stray value in the environment must not leak into a source-style bundle.
  unset STENLY_ENFORCE_LICENSE STENLY_CHECKOUT_URL STENLY_POLAR_ORG_ID STENLY_POLAR_API
fi

mkdir -p "$DIST_DIR"
rm -rf "$APP" "$DMG"

# Keep one release target tree. Local builds compile only the host architecture;
# tagged releases opt into the Intel slice below to make a universal DMG.
cargo build --release -p stenly -p stenly-cli -p stenly-mcp
if [[ "$UNIVERSAL" == "1" ]]; then
  cargo build --release --target "$INTEL_TARGET" -p stenly -p stenly-cli -p stenly-mcp
fi

install_binary() {
  local name=$1
  local output="$APP/Contents/MacOS/$name"
  if [[ "$UNIVERSAL" == "1" ]]; then
    lipo -create "target/release/$name" "target/$INTEL_TARGET/release/$name" -output "$output"
  else
    install -m 755 "target/release/$name" "$output"
  fi
}

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install_binary stenly
install_binary stenly-cli
install_binary stenly-mcp
install_binary stenly-speech
install -m 644 crates/stenly/assets/stenly.icns "$APP/Contents/Resources/stenly.icns"

# If the ASR dependency is built in shared mode, it emits dylibs next to each
# Cargo binary. Its build script uses @loader_path, so keep those dylibs in
# Contents/MacOS rather than relying on a developer-local rpath. Static builds
# simply have nothing to copy.
for dylib in target/release/*.dylib; do
  [[ -e "$dylib" ]] || continue
  name=$(basename "$dylib")
  if [[ "$UNIVERSAL" == "1" ]]; then
    lipo -create "$dylib" "target/$INTEL_TARGET/release/$name" -output "$APP/Contents/MacOS/$name"
  else
    install -m 755 "$dylib" "$APP/Contents/MacOS/$name"
  fi
done

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>${BUNDLE_NAME}</string>
  <key>CFBundleDisplayName</key><string>${BUNDLE_NAME}</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>stenly</string>
  <key>CFBundleIconFile</key><string>stenly</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>14.2</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Stenly records your side of meetings so it can transcribe them on this Mac.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Stenly records the other people in your meetings (system audio only) so it can transcribe them on this Mac.</string>
  <key>NSCalendarsFullAccessUsageDescription</key><string>Stenly reads Calendar.app events on this Mac to offer to record meetings with call links.</string>
  <key>NSCalendarsUsageDescription</key><string>Stenly reads Calendar.app events on this Mac to offer to record meetings with call links.</string>
</dict></plist>
PLIST

sign_nested_code
# Both the GUI and `stenly-cli record` may acquire an input device. Re-sign
# those leaf binaries with macos-entitlements.plist (audio input + Calendar)
# after generic nested signing, then sign the enclosing bundle. Accessibility
# has no signing entitlement: the user grants it in Privacy & Security. Network access is
# unrestricted for a non-sandboxed Developer ID app, so no entitlement is
# appropriate or required.
# Sign stenly-cli before the main executable: signing the bundle's main
# executable validates its sibling code, and lipo output for universal builds
# carries no linker signature until this point.
sign_entitled_executable "$APP/Contents/MacOS/stenly-cli"
sign_entitled_executable "$APP/Contents/MacOS/stenly"
codesign --force "${SIGN_ARGS[@]}" --entitlements "$ENTITLEMENTS" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
if [[ "$UNIVERSAL" == "1" ]]; then
  for binary in stenly stenly-cli stenly-mcp stenly-speech; do
    [[ "$(lipo -archs "$APP/Contents/MacOS/$binary")" == *"arm64"* ]]
    [[ "$(lipo -archs "$APP/Contents/MacOS/$binary")" == *"x86_64"* ]]
  done
fi

# Use a read-only compressed image. Its name is stable because site/config.toml
# points at releases/latest/download/stenly-macos.dmg.
hdiutil create -quiet -volname "Stenly" -srcfolder "$APP" -ov -format UDZO "$DMG"
codesign --force "${SIGN_ARGS[@]}" "$DMG"

NOTARIZED=false
if [[ -n "$NOTARY_PROFILE" && "$DEVELOPER_ID_AVAILABLE" == true ]]; then
  echo "Submitting $DMG for notarization with keychain profile '$NOTARY_PROFILE'..."
  notary_args=(--keychain-profile "$NOTARY_PROFILE")
  if [[ -n "$NOTARY_KEYCHAIN" ]]; then
    notary_args+=(--keychain "$NOTARY_KEYCHAIN")
  fi
  xcrun notarytool submit "$DMG" "${notary_args[@]}" --wait
  xcrun stapler staple -v "$DMG"
  xcrun stapler validate -v "$DMG"
  NOTARIZED=true
else
  cat >&2 <<'EOF'
Notarization was skipped: a Developer ID identity and STENLY_NOTARY_PROFILE are both required.
One-time local setup (run interactively; do not put credentials in this repo):
  xcrun notarytool store-credentials stenly-notary --apple-id YOUR_APPLE_ID --team-id YOUR_TEAM_ID
Then rerun:
  STENLY_SIGN_IDENTITY='Developer ID Application: Example Name (ABCDE12345)' \
    STENLY_NOTARY_PROFILE=stenly-notary scripts/bundle-macos.sh
For CI use an App Store Connect API key in repository secrets; see release.yml.
EOF
fi

if [[ "$DEVELOPER_ID_AVAILABLE" == true ]]; then
  echo "Built Developer ID-signed DMG: $DMG"
else
  echo "Built ad-hoc development DMG: $DMG"
fi
echo "Notarized: $NOTARIZED"

if [[ "$INSTALL" == true ]]; then
  install_signed_app "$APP" /Applications/Stenly.app
fi
