#!/usr/bin/env python3
"""Create an isolated, clearly synthetic meeting fixture; never record audio.

Requires a NEW destination directory. No user config, recordings or credentials
are read. Names/attribution and summary are authored fixture data, not model or
speaker-recognition results. This is suitable for demonstrating note rendering,
MCP retrieval and (with explicit operator action) sharing, not capture accuracy.
"""
import argparse
import json
from pathlib import Path


def create(root: Path) -> None:
    root = root.expanduser().absolute()
    root.mkdir(parents=True, exist_ok=False)
    recordings, notes, data = (root / name for name in ("recordings", "notes", "data"))
    for directory in (recordings, notes, data):
        directory.mkdir()
    session = recordings / "2026-10-25-0900-synthetic-launch-planning"
    session.mkdir()
    note = notes / "Synthetic launch planning.md"
    title = "Synthetic demo — launch planning"
    turns = [
        ("Mic", "Alex", 0, 5, "This is a synthetic demo, not a real meeting recording."),
        ("System", "Sam", 6, 11, "What should we finish before the test release?"),
        ("Mic", "Alex", 12, 18, "I will add a retry test for the demo upload by Friday."),
        ("System", "Sam", 19, 25, "I will review the test checklist. Let's keep the launch gated on testing."),
    ]
    transcript = {
        "segments": [dict(channel=c, name=n, start=s, end=e, text=t) for c, n, s, e, t in turns],
        "in_person": False,
        "speakers": [dict(channel=c, speaker=None, name=n, source="user", confidence=1.0,
                          talk_secs=sum(e-s for ch, _, s, e, _ in turns if ch == c))
                     for c, n in (("Mic", "Alex"), ("System", "Sam"))],
    }
    summary = {
        "title": title,
        "summary": "Synthetic fixture: Alex will add a retry test; Sam will review the checklist.",
        "sections": [{"heading": "Test release", "bullets": ["Keep launch gated on testing."]}],
        "key_points": [], "decisions": ["Keep launch gated on testing."],
        "action_items": [{"owner": "Alex", "task": "Add a retry test for the demo upload by Friday."},
                         {"owner": "Sam", "task": "Review the test checklist."}],
        "tags": ["synthetic-demo"],
    }
    meta = dict(title=title, started="2026-10-25T09:00:00+00:00", ended="2026-10-25T09:00:26+00:00",
                event=None, state="Done", note_path=str(note), error=None,
                routes=["synthetic fixture — no audio recorded"], sends=[], shares=[])
    for name, value in (("meta", meta), ("transcript", transcript), ("summary", summary)):
        (session / (name + ".json")).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
    note.write_text(f"# {title}\n\n> Synthetic fixture. Names, transcript and summary were authored for this demo. "
                    "No real participants. Not proof of audio capture, AI summary quality or automatic identity.\n\n"
                    "## Action items\n\n- [ ] Alex: Add a retry test for the demo upload by Friday.\n"
                    "- [ ] Sam: Review the test checklist.\n\n## Decision\n\nKeep launch gated on testing.\n\n"
                    "## Transcript\n\n" + "\n\n".join(f"**{n} [{s//60:02d}:{s%60:02d}]** {t}" for _, n, s, _, t in turns) + "\n")
    # JSON quoted strings are also valid TOML basic strings for these paths.
    q = lambda p: json.dumps(str(p))
    config = f'''# Isolated synthetic demo. No calendar, capture, hotkeys, model calls or MCP listener.
[app]
onboarded = true
launch_at_login = false
[me]
name = "Alex"
auto = false
[output]
notes_dir = {q(notes)}
audio_dir = {q(recordings)}
[llm]
enabled = false
[context]
enabled = false
[calendar]
source = "ics"
ics_urls = []
kody_url = ""
auto_record_titles = []
[detect]
ad_hoc_calls = false
[dictation]
enabled = false
sounds = false
[mcp]
http = false
write_tools = false
'''
    (root / "config.toml").write_text(config)
    manifest = dict(synthetic=True, provenance="Hand-authored fixture; no audio, ASR, model or identity inference.",
                    config=str(root / "config.toml"), data=str(data), session=str(session), note=str(note))
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="new directory (refuses existing directories)")
    args = parser.parse_args()
    create(args.destination)


if __name__ == "__main__":
    main()
