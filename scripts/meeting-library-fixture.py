#!/usr/bin/env python3
"""Create a private-data-free meeting-library UI fixture in a NEW directory.

Usage: scripts/meeting-library-fixture.py /tmp/stenly-library-probe
Then launch the isolated test app with the printed config/data environment.
Never point this at a real recording library.
"""
import datetime
import json
import pathlib
import sys

if len(sys.argv) != 2:
    sys.exit(__doc__)
root = pathlib.Path(sys.argv[1]).expanduser().resolve()
root.mkdir(parents=True, exist_ok=False)
recordings = root / "recordings"
recordings.mkdir()
now = datetime.datetime.now(datetime.timezone.utc)
for i in range(605):
    session = recordings / f"session-{i:04}"
    session.mkdir()
    start = now - datetime.timedelta(days=i)
    meta = dict(title=f"Synthetic meeting {i:04}", started=start.isoformat(),
                ended=(start + datetime.timedelta(minutes=30)).isoformat(),
                event=None, state="Done", note_path=None, error=None, routes=[])
    if i == 603:
        meta["event"] = dict(id="synthetic", uid="synthetic", title=meta["title"],
                             start=start.isoformat(), end=start.isoformat(),
                             link=None, organizer=None,
                             attendees=[dict(name="Avery Example", email="avery@example.test", status=None)],
                             description=None, calendar=None, declined=False, recurring=False)
    (session / "meta.json").write_text(json.dumps(meta))
    if i == 604:
        segments = [dict(channel="Mic", start=j * 20., end=j * 20. + 15.,
                         text=f"Synthetic discussion section {j}.") for j in range(40)]
        segments[30]["text"] = "The cobalt migration has approval. Exact search result proof."
        (session / "transcript.json").write_text(json.dumps(dict(segments=segments)))
        (session / "summary.json").write_text(json.dumps(dict(summary="A durable archive for violet projects")))
# JSON basic strings are also valid TOML strings (without literal control data).
quote = lambda p: json.dumps(str(p))
config = root / "config.toml"
config.write_text(f"""[app]
onboarded = true
[output]
audio_dir = {quote(recordings)}
notes_dir = {quote(root / 'notes')}
[calendar]
source = "ics"
[mcp]
http = false
[context]
enabled = false
""")
print(f"Created 605 synthetic sessions under {recordings}")
print(f"STENLY_CONFIG={config} STENLY_DATA_DIR={root / 'data'} STENLY_SHOW_WINDOW=1 <isolated test app binary>")
print("Verify: Show older meetings → 100; keep paging → 605. Cmd-F: Avery / violet / cobalt / 0604 / not-present.")
print("Open cobalt result: highlighted transcript segment at 10:00 must be visible without scrolling.")
