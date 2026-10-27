"""Hermetic packaging regression tests. Never build, sign, or install a real app.

Run: python3 -m unittest discover -s scripts -p 'test_bundle_macos.py'
The script is copied into a temporary checkout; its fixed daily-app path is
rewritten ONLY in that copy. All side-effect commands are guarded mocks.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
IDENTITY = "Developer ID Application: Example Developer (ABCDE12345)"
HASH = "A" * 40

MOCK = r'''
import json, os, pathlib, shutil, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
root = pathlib.Path(os.environ['FIXTURE']).resolve()
with open(root / 'commands.jsonl', 'a') as log:
    log.write(json.dumps([name] + args) + '\n')
def safe(path):
    p = pathlib.Path(path).resolve()
    if not p.is_relative_to(root):
        raise RuntimeError('mock refused non-fixture path: ' + str(p))
    return p
if name == 'security':
    print(os.environ.get('IDENTITIES', ''))
    sys.exit(int(os.environ.get('SECURITY_FAIL', '0')))
if name == 'codesign':
    if '-d' in args:
        if '-r-' in args:
            print('designated => ' + os.environ.get('OLD_REQUIREMENT', 'identifier "ai.stenly.app" and anchor apple generic'), file=sys.stderr)
        else:
            print('TeamIdentifier=' + os.environ.get('OLD_TEAM', 'ABCDE12345'), file=sys.stderr)
    if os.environ.get('SIGN_FAIL') and '--force' in args:
        sys.exit(1)
    if '--verify' in args:
        if os.environ.get('CERT_FAIL') and any('1.2.840' in a for a in args):
            sys.exit(1)
        if os.environ.get('REQUIREMENT_FAIL') and any('special requirement' in a for a in args):
            sys.exit(1)
        if os.environ.get('VERIFY_FAIL') == 'installed' and args[-1].endswith('/Applications/Stenly.app'):
            sys.exit(1)
        if os.environ.get('VERIFY_FAIL') == 'candidate' and args[-1].endswith('/dist/Stenly.app'):
            sys.exit(1)
        if os.environ.get('VERIFY_FAIL') == 'staged' and '/.stenly-install.' in args[-1]:
            sys.exit(1)
    sys.exit(0)
if name == 'cargo':
    target = 'target/release'
    if '--target' in args:
        target = 'target/' + args[args.index('--target') + 1] + '/release'
    dest = safe(root / target)
    dest.mkdir(parents=True, exist_ok=True)
    for binary in ['stenly', 'stenly-cli', 'stenly-mcp', 'stenly-speech']:
        (dest / binary).write_text('synthetic ' + binary)
elif name == 'cp':
    source, dest = args[-2:]
    dest = safe(dest)
    if os.environ.get('COPY_FAIL') and '/.stenly-install.' in str(dest):
        dest.mkdir(); (dest / 'partial').write_text('partial copy'); sys.exit(1)
    if source == '/usr/bin/true':
        dest.write_text('synthetic executable')
    elif pathlib.Path(source).is_dir():
        shutil.copytree(safe(source), dest)
    else:
        shutil.copy2(safe(source), dest)
elif name == 'mv':
    source, dest = map(safe, args)
    if os.environ.get('SWAP_FAIL') and source.name == 'Stenly.app' and '/.stenly-install.' in str(source):
        sys.exit(1)
    shutil.move(source, dest)
elif name == 'rm':
    for arg in args:
        if not arg.startswith('-'):
            p = safe(arg)
            if p.is_dir(): shutil.rmtree(p)
            elif p.exists(): p.unlink()
elif name == 'install':
    shutil.copy2(safe(args[-2]), safe(args[-1]))
elif name == 'hdiutil':
    safe(args[-1]).write_text('synthetic dmg')
elif name == 'lipo':
    if '-archs' in args:
        print('arm64 x86_64')
    else:
        safe(args[-1]).write_text('synthetic universal executable')
elif name in ['open', 'pkill', 'xcrun']:
    if name == 'open' and os.environ.get('OPEN_FAIL'):
        sys.exit(1)
else:
    raise RuntimeError('unknown mock command: ' + name)
'''


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="stenly-bundle-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        (self.root / "tmp").mkdir()
        (self.root / "Applications").mkdir()
        self.daily = self.root / "Applications/Stenly.app"
        self.daily.mkdir()
        (self.daily / "old-working-app").write_text("keep")
        (self.root / "dist/Stenly.app").mkdir(parents=True)
        (self.root / "dist/Stenly.app/old-output").write_text("keep")
        (self.root / "dist/stenly-macos.dmg").write_text("keep")
        for script in ["bundle-macos.sh", "macos-signing.sh"]:
            text = (ROOT / "scripts" / script).read_text()
            text = text.replace("/Applications/Stenly.app", str(self.daily))
            (self.root / "scripts" / script).write_text(text)
        (self.root / "Cargo.toml").write_text('version = "0.0.0"\n')
        assets = self.root / "crates/stenly/assets"
        assets.mkdir(parents=True)
        (assets / "stenly.icns").write_text("synthetic icon")
        (self.root / "bin").mkdir()
        dispatch = self.root / "bin/dispatch"
        dispatch.write_text("#!" + os.sys.executable + "\n" + MOCK)
        dispatch.chmod(0o755)
        for name in ["security", "codesign", "cargo", "cp", "rm", "mv", "install",
                     "hdiutil", "lipo", "xcrun", "pkill", "open"]:
            (self.root / "bin" / name).symlink_to(dispatch)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("STENLY_")}
        self.env.update(FIXTURE=str(self.root), TMPDIR=str(self.root / "tmp"),
                        PATH=str(self.root / "bin") + os.pathsep + os.environ["PATH"],
                        STENLY_DIST_DIR=str(self.root / "dist"))

    def run_bundle(self, *args, **env):
        self.env.update(env)
        self.result = subprocess.run(["bash", "scripts/bundle-macos.sh", *args],
                                     cwd=self.root, env=self.env, text=True,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        log = self.root / "commands.jsonl"
        self.commands = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
        return self.result.returncode

    def valid(self, **env):
        return dict(STENLY_SIGN_IDENTITY=IDENTITY,
                    IDENTITIES=f'  1) {HASH} "{IDENTITY}"\n     1 valid identities found', **env)

    def assert_preflight_failure(self):
        self.assertNotEqual(self.result.returncode, 0, self.result.stdout)
        self.assertTrue((self.daily / "old-working-app").exists())
        self.assertFalse(any(c[0] in ["cargo", "install", "pkill", "open", "mv", "hdiutil"] for c in self.commands))
        self.assertEqual((self.root / "dist/Stenly.app/old-output").read_text(), "keep")
        self.assertEqual((self.root / "dist/stenly-macos.dmg").read_text(), "keep")
        for command in self.commands:
            if command[0] in ["rm", "cp"]:
                self.assertNotIn(str(self.daily), command)

    def test_missing_identity_install(self):
        self.run_bundle("--install")
        self.assert_preflight_failure()

    def test_strict_release_and_profile_without_identity(self):
        for env in [dict(STENLY_REQUIRE_DEVELOPER_ID="1"), dict(STENLY_NOTARY_PROFILE="fixture-profile")]:
            with self.subTest(env=env):
                self.run_bundle(**env)
                self.assert_preflight_failure()

    def test_invalid_supplied_identity_never_downgrades(self):
        self.run_bundle(STENLY_SIGN_IDENTITY=IDENTITY)
        self.assert_preflight_failure()

    def test_wrong_identity_type(self):
        identity = "Apple Development: Example Developer (ABCDE12345)"
        self.run_bundle("--install", STENLY_SIGN_IDENTITY=identity,
                        IDENTITIES=f'1) {HASH} "{identity}"')
        self.assert_preflight_failure()

    def test_ambiguous_name(self):
        self.run_bundle("--install", STENLY_SIGN_IDENTITY=IDENTITY,
                        IDENTITIES=f'1) {HASH} "{IDENTITY}"\n2) {"B" * 40} "{IDENTITY}"')
        self.assert_preflight_failure()

    def test_keychain_failure(self):
        self.run_bundle("--install", **self.valid(SECURITY_FAIL="1"))
        self.assert_preflight_failure()

    def test_unusable_key_and_false_developer_certificate(self):
        for env in [dict(SIGN_FAIL="1"), dict(CERT_FAIL="1")]:
            with self.subTest(env=env):
                self.run_bundle("--install", **self.valid(**env))
                self.assert_preflight_failure()

    def test_installed_team_mismatch(self):
        self.run_bundle("--install", **self.valid(OLD_TEAM="VWXYZ98765"))
        self.assert_preflight_failure()

    def test_unverifiable_or_unknown_installed_signature(self):
        for env in [dict(VERIFY_FAIL="installed"), dict(OLD_TEAM="not set"), dict(OLD_REQUIREMENT="")]:
            with self.subTest(env=env):
                self.env = {k: v for k, v in self.env.items() if k not in ["VERIFY_FAIL", "OLD_TEAM", "OLD_REQUIREMENT"]}
                self.run_bundle("--install", **self.valid(**env))
                self.assert_preflight_failure()

    def test_installed_requirement_mismatch(self):
        self.run_bundle("--install", **self.valid(OLD_REQUIREMENT="special requirement", REQUIREMENT_FAIL="1"))
        self.assert_preflight_failure()

    def test_custom_bundle_still_refused(self):
        self.run_bundle("--install", **self.valid(STENLY_BUNDLE_ID="example.preview"))
        self.assert_preflight_failure()

    def test_distribution_cannot_overlap_daily_app(self):
        for path in [self.daily.parent, self.daily / "output"]:
            with self.subTest(path=path):
                self.run_bundle(STENLY_DIST_DIR=str(path))
                self.assert_preflight_failure()

    def test_multiple_leading_slashes_refused(self):
        for prefix in ["/", "//", "///"]:
            with self.subTest(prefix=prefix):
                self.run_bundle(STENLY_DIST_DIR=prefix + str(self.daily.parent))
                self.assert_preflight_failure()

    def test_bundle_name_path_aliases_refused(self):
        for name in ["./Stenly", "../Applications/Stenly", "preview/../../Applications/Stenly"]:
            with self.subTest(name=name):
                self.run_bundle(STENLY_DIST_DIR=str(self.daily.parent), STENLY_BUNDLE_NAME=name)
                self.assert_preflight_failure()

    def test_distribution_symlink_alias_refused(self):
        alias = self.root / "app-alias"
        alias.symlink_to(self.daily, target_is_directory=True)
        self.run_bundle(STENLY_DIST_DIR=str(alias / "output"))
        self.assert_preflight_failure()

    def test_extra_arguments_refused(self):
        self.run_bundle("--install", "extra", **self.valid())
        self.assert_preflight_failure()

    def test_ad_hoc_non_install_includes_speech_worker(self):
        self.assertEqual(self.run_bundle(), 0, self.result.stderr)
        self.assertTrue((self.root / "dist/Stenly.app/Contents/MacOS/stenly-speech").exists())
        self.assertTrue(any(c[0] == "codesign" and "-" in c for c in self.commands))
        self.assertFalse(any(c[0] in ["security", "pkill", "open"] for c in self.commands))
        self.assertTrue((self.daily / "old-working-app").exists())

    def test_valid_install_uses_pinned_hash_after_copy_validation(self):
        self.assertEqual(self.run_bundle("--install", **self.valid()), 0, self.result.stderr)
        self.assertTrue((self.daily / "Contents/MacOS/stenly-speech").exists())
        sign = [c for c in self.commands if c[0] == "codesign" and "--force" in c]
        self.assertTrue(sign)
        self.assertTrue(all(HASH in c for c in sign))
        self.assertFalse(any("--deep" in c and any(a.startswith("-R=") for a in c) for c in self.commands))
        kill_index = next(i for i, c in enumerate(self.commands) if c[0] == "pkill")
        self.assertTrue(all(i < kill_index for i, c in enumerate(self.commands) if c[0] in ["cp", "mv"]))
        self.assertFalse(any(c[0] == "rm" and str(self.daily) in c for c in self.commands))

    def test_first_install(self):
        shutil.rmtree(self.daily)
        self.assertEqual(self.run_bundle("--install", **self.valid()), 0, self.result.stderr)
        self.assertTrue((self.daily / "Contents/MacOS/stenly-speech").exists())

    def test_launch_failure_preserves_backup(self):
        self.assertNotEqual(self.run_bundle("--install", **self.valid(OPEN_FAIL="1")), 0)
        backups = list((self.root / "Applications").glob(".stenly-install.*/Previous.app/old-working-app"))
        self.assertEqual(len(backups), 1, self.result.stderr)
        self.assertIn("Previous app preserved at", self.result.stderr)

    def test_fingerprint_selection(self):
        env = self.valid()
        env["STENLY_SIGN_IDENTITY"] = HASH
        self.assertEqual(self.run_bundle(**env), 0, self.result.stderr)

    def test_valid_strict_and_notary_paths(self):
        self.assertEqual(self.run_bundle(**self.valid(STENLY_REQUIRE_DEVELOPER_ID="1", STENLY_NOTARY_PROFILE="fixture-profile", STENLY_NOTARY_KEYCHAIN="fixture-keychain", STENLY_UNIVERSAL="1")), 0, self.result.stderr)
        self.assertTrue(any(c[0] == "xcrun" and "submit" in c and "fixture-keychain" in c for c in self.commands))
        self.assertTrue(any(c[0] == "lipo" and c[-1].endswith("stenly-speech") for c in self.commands))

    def test_candidate_staging_and_swap_failures_keep_old_app_running(self):
        for env in [dict(COPY_FAIL="1"), dict(VERIFY_FAIL="candidate"), dict(VERIFY_FAIL="staged"), dict(SWAP_FAIL="1")]:
            with self.subTest(env=env):
                self.env = {k: v for k, v in self.env.items() if k not in ["COPY_FAIL", "VERIFY_FAIL", "SWAP_FAIL"]}
                self.assertNotEqual(self.run_bundle("--install", **self.valid(**env)), 0)
                self.assertTrue((self.daily / "old-working-app").exists(), self.result.stderr)
                self.assertFalse(any(c[0] in ["pkill", "open"] for c in self.commands))


if __name__ == "__main__":
    unittest.main()
