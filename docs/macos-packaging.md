# macOS packaging and local installation

`cargo build --release -p stenly -p stenly-cli` builds the GUI, CLI, and
`stenly-speech` worker. `scripts/bundle-macos.sh` also builds and packages
`stenly-mcp`; all four executables are included in the app. Set
`STENLY_UNIVERSAL=1` to build/package both arm64 and x86_64 slices.

## Development bundles (no installation)

Without a signing identity, the script creates an **ad-hoc development DMG**.
This is not a distributable release and must not replace a daily-use signed app.
Use a separate bundle name/identifier and isolated data for development:

```sh
STENLY_BUNDLE_NAME='Stenly Preview' STENLY_BUNDLE_ID=example.stenly.preview \
  scripts/bundle-macos.sh
```

## Developer ID builds

Explicitly configure a **Developer ID Application** certificate with its private
key available in a local keychain. Use its exact unique name or SHA-1 fingerprint
from `security find-identity -v -p codesigning`. The script never auto-selects an
identity. An unavailable, ambiguous, expired, wrong-type, or unusable identity
fails; supplying an invalid identity never falls back to ad-hoc signing.

```sh
STENLY_SIGN_IDENTITY='Developer ID Application: Example Developer (ABCDE12345)' \
  STENLY_REQUIRE_DEVELOPER_ID=1 scripts/bundle-macos.sh
```

`STENLY_REQUIRE_DEVELOPER_ID=1`, `STENLY_NOTARY_PROFILE`, and `--install` all
require Developer ID preflight **before build or output cleanup**. To notarize,
configure a notarytool keychain profile interactively (never commit credentials),
then also set `STENLY_NOTARY_PROFILE=your-profile`. An optional
`STENLY_NOTARY_KEYCHAIN` selects its keychain. Notarytool/stapling errors stop the
script; identity validation alone does not prove notarization succeeded.

## Replacing an installed app (only when authorized)

```sh
STENLY_SIGN_IDENTITY='Developer ID Application: Example Developer (ABCDE12345)' \
  scripts/bundle-macos.sh --install
```

Installation accepts only the default bundle name/identifier. Before building,
it verifies the installed signature (if present), matches its team, and tests the
selected certificate against Apple's Developer ID Application requirement and
the installed app's exact designated requirement using a temporary signed probe.
It refuses unknown/broken/ad-hoc installed signatures or a signing-identity
migration. Do not bypass this guard to switch teams or certificate requirements:
plan a separate migration and privacy-permission recovery instead.

The candidate is copied to staging on the destination filesystem and validated
before replacement or process termination. The old app is moved aside, not
deleted first; a failed swap attempts rollback. If rollback or launch fails,
the old bundle is retained at the reported backup path for manual recovery.
The installed app's designated requirement is re-read immediately before staging.
These checks reduce accidental TCC invalidation; **they are not proof that macOS
will retain every privacy grant** or that a new build works at runtime.

## Hermetic regression tests

```sh
bash -n scripts/bundle-macos.sh scripts/macos-signing.sh
shellcheck -x scripts/bundle-macos.sh scripts/macos-signing.sh
python3 -m unittest discover -s scripts -p 'test_bundle_macos.py'
```

The tests copy the scripts into a temporary fixture checkout, rewrite the fixed
installation path only there, and guard mocked side-effect commands to stay
inside that fixture. They do not run Cargo, real signing, notarization, installed
apps, or privacy-permission checks. Keep those integration checks separate and
explicitly authorized; never use a daily-use app as a test fixture.
