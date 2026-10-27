#!/usr/bin/env python3
"""Standard-library tests for the isolated synthetic fixture generator."""
import importlib.util
import json
from pathlib import Path
import tempfile
import tomllib
import unittest

spec = importlib.util.spec_from_file_location("demo", Path(__file__).with_name("create-synthetic-demo.py"))
demo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(demo)


class SyntheticDemoTests(unittest.TestCase):
    def test_isolation_and_provenance(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "new fixture"
            demo.create(root)
            config = tomllib.loads((root / "config.toml").read_text())
            manifest = json.loads((root / "manifest.json").read_text())
            self.assertTrue(manifest["synthetic"])
            self.assertFalse(config["calendar"]["auto_record_titles"])
            self.assertEqual(config["calendar"]["source"], "ics")
            for section, key in (("dictation", "enabled"), ("dictation", "sounds"),
                                 ("llm", "enabled"), ("context", "enabled"),
                                 ("detect", "ad_hoc_calls"), ("mcp", "http"), ("mcp", "write_tools")):
                self.assertFalse(config[section][key])
            for field in ("audio_dir", "notes_dir"):
                self.assertTrue(Path(config["output"][field]).is_relative_to(root))
            session = Path(manifest["session"])
            meta = json.loads((session / "meta.json").read_text())
            self.assertEqual(meta["state"], "Done")
            self.assertIsNone(meta["event"])
            self.assertIn("Synthetic", meta["title"])
            self.assertFalse(list(root.rglob("*.wav")))
            self.assertFalse(meta["shares"])
            self.assertIn("authored", Path(manifest["note"]).read_text())
            transcript = json.loads((session / "transcript.json").read_text())
            self.assertEqual(len(transcript["segments"]), 4)
            self.assertTrue(all(s["source"] == "user" for s in transcript["speakers"]))

    def test_refuses_existing_destination_without_changes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / "keep.txt"
            marker.write_text("do not overwrite")
            with self.assertRaises(FileExistsError):
                demo.create(root)
            self.assertEqual(marker.read_text(), "do not overwrite")
            self.assertEqual(list(root.iterdir()), [marker])


if __name__ == "__main__":
    unittest.main()
