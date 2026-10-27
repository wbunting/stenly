#!/usr/bin/env python3
"""Build an isolated native Sparkle A/B fixture. No Stenly binary or user data.
Usage: prepare-fixture.py /absolute/Sparkle-release-dir /new/test-dir
Serves only loopback: python3 -m http.server 18742 --bind 127.0.0.1 --directory TEST/web
Then open TEST/install/Stenly Update Fixture.app; interact with its update UI.
"""
from pathlib import Path
import base64
import hashlib
import plistlib
import shutil
import subprocess
import sys

if len(sys.argv) not in (3, 4) or (len(sys.argv) == 4 and sys.argv[3] != "--probe-only"):
    sys.exit(__doc__)
framework = Path(sys.argv[1]).resolve()
root = Path(sys.argv[2]).resolve()
root.mkdir(parents=True, exist_ok=False)
(root / "web").mkdir()
repo = Path(__file__).resolve().parents[2]
# Ephemeral key is test-only, never Keychain/production; never print its bytes.
key = root / "test-key.pem"
subprocess.run(["openssl", "genpkey", "-algorithm", "ED25519", "-out", str(key)], check=True)
key.chmod(0o600)
public = base64.b64encode(subprocess.check_output(["openssl", "pkey", "-in", str(key), "-pubout", "-outform", "DER"])[-32:]).decode()
subprocess.run(["xcrun", "clang", ("-DSTENLY_UPDATER_TEST_FEED=1" if len(sys.argv) == 4 else "-DSTENLY_UPDATER_TEST_INSTALLER=1"), "-fobjc-arc", "-fmodules", "-framework", "AppKit", "-framework", "Sparkle", "-F", str(framework), "-Wl,-rpath,@executable_path/../Frameworks", str(repo / "crates/stenly-platform/src/macos/updater.m"), str(repo / "scripts/updater/fixture.m"), "-o", str(root / "fixture")], check=True)
for version in (1, 2):
    app = root / f"build{version}" / "Stenly Update Fixture.app"
    (app / "Contents/MacOS").mkdir(parents=True)
    (app / "Contents/Frameworks").mkdir()
    shutil.copy2(root / "fixture", app / "Contents/MacOS/fixture")
    subprocess.run(["ditto", str(framework / "Sparkle.framework"), str(app / "Contents/Frameworks/Sparkle.framework")], check=True)
    info = dict(CFBundleIdentifier="ai.stenly.updater-test.fixture", CFBundleName="Stenly Update Fixture", CFBundleExecutable="fixture", CFBundlePackageType="APPL", CFBundleVersion=str(version), CFBundleShortVersionString=f"0.0.{version}", LSMinimumSystemVersion="14.2", SUFeedURL="http://127.0.0.1:18742/appcast.xml", SUPublicEDKey=public, SUVerifyUpdateBeforeExtraction=True, SUEnableAutomaticChecks=False, SUAllowsAutomaticUpdates=False, SUSendProfileInfo=False, SUEnableInstallerLauncherService=False, SUEnableDownloaderService=False)
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
    # --deep/ad-hoc is intentional for this synthetic local fixture only.
    subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(app)], check=True)
archive = root / "web/update.zip"
subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(root / "build2/Stenly Update Fixture.app"), str(archive)], check=True)
signature = base64.b64encode(subprocess.check_output(["openssl", "pkeyutl", "-sign", "-inkey", str(key), "-rawin", "-in", str(archive)])).decode()
(root / "web/appcast.xml").write_text(f'''<?xml version="1.0"?><rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><title>Isolated synthetic update test</title><item><title>Synthetic build 2</title><sparkle:version>2</sparkle:version><sparkle:shortVersionString>0.0.2</sparkle:shortVersionString><enclosure url="http://127.0.0.1:18742/update.zip" sparkle:edSignature="{signature}" length="{archive.stat().st_size}" type="application/octet-stream" /></item></channel></rss>''')
shutil.copy2(archive, root / "signed-original.zip")
(root / "install").mkdir()
subprocess.run(["ditto", str(root / "build1/Stenly Update Fixture.app"), str(root / "install/Stenly Update Fixture.app")], check=True)
print(f"Prepared builds1/2 in {root}; ephemeral key stays local. Never publish it.")
print(f"Archive SHA256 {hashlib.sha256(archive.read_bytes()).hexdigest()}")
print("PROBE ONLY: Check updates changes title to available2 but must not fetch update.zip or offer install." if len(sys.argv) == 4 else "EXPERIMENTAL INSTALLER ONLY: install/relaunch → installed Info.plist version2. Not production-safe.")
print("Experimental-installer only: tamper archive → rejection; extraction-gap safety remains UNVERIFIED.")
print("No Cargo build enables installation. --probe-only tests notification without downloading archive.")
