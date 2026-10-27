# Optional release notifications and installer experiment

**This experiment is not complete. Production is notification-only and unconfigured.**
No public repository/feed/key is guessed; release packaging is owned separately.
Normal Cargo builds never compile the experimental installation driver and call
only Sparkle's `checkForUpdateInformation` probing API. Automatic Sparkle checks,
downloads, installation and relaunch are off, even when opt-in checking is on.
The app can report an available version and offer its HTTPS download link.
A manual download must be installed/verified by the user; a notification is not
proof that archive bytes were downloaded or signature-validated.

## Release-owner interface

- `fetch-sparkle.sh NEW_DIR` fetches pinned upstream Sparkle2.10.0 with SHA256
  verification. Preserve SDK licenses/notices. Optional macOS build:
  `STENLY_SPARKLE_FRAMEWORK_DIR=/absolute/SDK_DIR`.
- Embed the complete matching framework in Contents/Frameworks; sign nested
  helpers/framework then app, notarize final distribution. Never use fixture
  ad-hoc/deep signing for production. Both adapter architectures compile; final
  Apple-signed universal release remains release-owner verification.
- Bundle real HTTPS `SUFeedURL`, base64 32-byte `SUPublicEDKey`,
  `SUAllowsAutomaticUpdates=false`, `SUSendProfileInfo=false`, and
  `SUVerifyUpdateBeforeExtraction=true`. No production ATS dictionary is allowed.
  The delegate pins validated bundled feed over persisted defaults overrides.
- `app.check_updates` defaults false. Requests expose IP and user agent to the
  feed host, never meeting content or system profile. Disabling clears results;
  there is no queued restart that can override a later disable. Turning off
  stops NEW checks; an already-started metadata request (including redirects)
  may finish. Its result is suppressed while disabled. This is not immediate
  network revocation; no stronger cancellation guarantee is claimed.
- Linux build-time `STENLY_RELEASE_REPOSITORY=OWNER/REPO` deliberately chooses
  real stable GitHub Releases metadata. HTTPS-only requests, notification/link
  only. No implicit private repo or invented public name.

## Why production installation is blocked

Independent security review found that Sparkle prepares an install-on-quit agent
before its final Relaunch button. A reference drop cannot establish cancellation:
its active driver can retain the updater. Public cancellation callbacks do not
cover the entire extraction/preparation interval. Therefore **in-app installation
is not enabled in Cargo builds**. Do not remove this gate based on happy-path A/B
or Ready-stage cancellation alone. Required remaining proof: cancellation during
extraction/preparation → immediate quit, all capture/finalization paths, disable
ordering, and no unintended install-on-quit. Automatic rollback after successful
installation of a broken release is also not implemented; prior signed known-good
manual reinstall is the documented recovery strategy.

## Capture safety foundation

RAII activity leases are acquired before audio opens and held through meeting
finalization and dictation transcription/paste/history/WAV persistence. The test
installer acquires an exclusive install lease before the first Download/Install
choice; a prepared installer cannot race a new capture start. Leases are unit
tested, but real capture + production update integration is not runtime verified.

## Synthetic local fixtures only

`prepare-fixture.py SDK_DIR NEW_TEST_DIR --probe-only` compiles normal probing
behavior with a loopback-feed exception permitted only by a separate test compiler
flag, explicit native test argument, and bundle ID `ai.stenly.updater-test.*`.
Serve TEST/web on 127.0.0.1:18742. Check updates: window reports available0.0.2,
feed log has appcast requests only, no update.zip, no install dialog, build1 stays.
Disable/re-enable/off again must remain off with no request; normal termination
must leave build1 and its executable hash unchanged.

Without `--probe-only`, the script explicitly compiles
`STENLY_UPDATER_TEST_INSTALLER=1`. This **unsafe/non-production** experiment
preserves local research proof: signed1→2 install/relaunch; tampered archive
rejection with build1/hash retained; Ready→disable→quit preserves build1; prepared
quit installs2. It does NOT prove extraction-gap cancellation, and is not used by
Cargo or release scripts. No runtime env enables that compile-time installer.

Fixture keys are ephemeral files (0600), never Keychain, Git or production. Delete
with fixtures, never upload. No Stenly capture/ASR binary, user config or recordings
are included. Check updates may respect a skipped fixture version; delete only
its synthetic `SUSkippedVersion` default before retesting, never real app defaults.

Production real HTTPS feed, Apple-signed/notarized Stenly A→B, native/model
provenance, Linux GUI/real metadata, extraction-gap safety and automatic rollback
remain blocked/unverified. No working production updater is advertised.
