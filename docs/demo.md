# Synthetic demo capture kit

Generate a new isolated profile with Python 3.11+:

```sh
python3 scripts/create-synthetic-demo.py /tmp/stenly-demo-new
python3 -m unittest discover -s scripts -p 'test_synthetic_demo.py'
```

It refuses to overwrite an existing directory. It does not read your profile, record audio, call a model, connect a calendar, enable hotkeys or start an HTTP MCP listener. It writes one **authored** synthetic transcript/summary/note with names explicitly sourced as user-provided. This is not a test of audio capture, ASR, automatic attribution or AI summarization.

Use `manifest.json` for the generated paths. Launch the existing app binary without replacing an installed bundle, with `STENLY_CONFIG=<generated config>`, `STENLY_DATA_DIR=<generated data>`, `STENLY_SHOW_WINDOW=1`, `STENLY_OPEN_MEETING=<generated session>` and `STENLY_SHOW_TRANSCRIPT=1`. Capture only that synthetic window, never the whole desktop. Quit only your test instance afterward.

The bundled CLI/MCP commands can use the same config/data environment:

```sh
stenly-mcp call list_meetings
stenly-mcp call get_meeting '{"id":"latest"}'
stenly-mcp call get_transcript '{"id":"latest"}'
# Explicit optional upload, only after reviewing the synthetic content:
stenly-cli share <generated-session>
# Revoke after the test; keep the full reader link private until approved:
stenly-cli share <generated-session> --revoke
```

## 36-second draft cut

Keep **“SYNTHETIC DATA · actual app/tool output”** visible throughout. Captions must explain what is and is not being shown. No voiceover/music is required; do not play audio through speakers.

| Time | Actual evidence | Caption |
| --- | --- | --- |
| 0–4 s | Synthetic meeting open in Stenly | Local meeting notes, ready for your tools. |
| 4–12 s | Same app's summary, action items and transcript | Synthetic transcript and summary, authored for this demo. Names are user-supplied, not automatic Meet attribution. |
| 12–24 s | Real MCP `get_meeting` response | An agent can retrieve these action items through local MCP. This is actual tool output; no agent edit or code change is being staged. |
| 24–32 s | Real hosted viewer decrypting the synthetic share at phone-sized viewport | Open an encrypted share in a browser; this is mobile-size emulation, not a physical-phone test. |
| 32–36 s | End card | Stenly · local meeting notes + dictation. Release and platform checks are still in progress. |

A labeled evidence cut can use held screenshots plus actual tool output, but must not be presented as continuous interaction. Replace held shots with a real-time workflow recording before claiming to show end-to-end interaction. Do not show “call ended → automatically named transcript” until the consenting meeting test passes. Do not show an agent starting work unless it really did, on a disposable synthetic project. Never composite a fake product control or fabricated response. Only add public download URLs, pricing, automatic updates or broad platform badges after their own checks pass.

Before publishing: approve the final wording, re-check the live/default share path, verify captions at phone size, strip unrelated desktop/profile data, and obtain explicit publication approval. Evidence files belong in temporary/gitignored storage, not Git.
