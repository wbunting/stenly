# Contributor and agent instructions

These rules apply to people and coding agents working in this repository.
Unless the user explicitly authorizes an exception for a specific task, they
are mandatory.

Machine-specific setup (audio loopback devices, local helper scripts, installed
app paths) belongs in an untracked, gitignored local file and must never be
committed. Some agent harnesses load `AGENTS.override.md` *instead of* this
file, so a local override must repeat these rules, not just add to them.

## Safety rules (read before starting)

- **Never play audio through the user's speakers.** Audio tests must use a
  virtual loopback output device (for example BlackHole on macOS) or synthetic
  WAV files. Do not run test paths that call `say` or `afplay` unless output is
  routed to a loopback device.
- **Never type or click into the user's own apps.** Paste/inject tests must
  target a disposable test app; set `STENLY_PASTE_ONLY_INTO=<test app name>` so
  dictation pastes only into that app.
- **Never ask for secrets in chat or print them.** API keys belong in the OS
  keychain (via the app UI or `secrets.rs`), never in tracked files, logs or
  PR text. Do not read local `.env` files unless the task requires it.
- **Never disclose private data.** Do not read, copy, upload or quote the
  user's recordings, transcripts, notes, calendars, voiceprints or config in
  commits, logs, issues, PRs or screenshots.
- **Leave an installed daily-use build alone.** Test with a separately built
  binary and an isolated profile (`STENLY_CONFIG=<copy>`, `STENLY_DATA_DIR=<dir>`)
  rather than replacing an installed app or touching the user's config,
  recordings or voiceprints. If the user authorizes replacing it, leave a
  stable, working build installed and running afterwards.
- **Use only synthetic, authored data** in fixtures, demos, screenshots and
  docs. Never commit real meeting audio, transcripts, calendar entries,
  participant names/emails or meeting links.

## Build and test

- Build: `cargo build --release -p stenly -p stenly-cli` (also builds the
  `stenly-speech` helper).
- Portable tests: `cargo test -p stenly-core -p stenly-platform -p stenly-asr`
- Agent/MCP tests: `cargo test -p stenly-agent -p stenly-mcp`
- Synthetic demo tests: `python3 -m unittest discover -s scripts -p 'test_synthetic_demo.py'`
- Site: `cd site && npm ci && npm test`
- Share server (separate workspace, pinned toolchain): see `server/README.md`.
- Format only the files you change (`rustfmt --edition 2024 <file>`); the
  workspace predates rustfmt enforcement.

## UI conventions

- Native macOS look with no layout shift, neutral uniform icons and lists that
  are easy to read. Verify UI changes with a screenshot of an isolated test
  instance using synthetic data; dev variables such as `STENLY_PAGE=…`,
  `STENLY_OPEN_MEETING=…` and `STENLY_FAKE_RECORDING=1` help reach a state.
- Open native NSMenus from a spawned task (`ui::menu_in_view`), never while a
  GPUI borrow is held.

## Useful paths (macOS defaults)

- Config: `~/Library/Application Support/stenly/config.toml`
- Recordings: `~/Documents/stenly/Recordings`
- Logs: `~/Library/Logs/stenly.log`
- To reprocess a session: set `meta.json` `state` to `Recorded` (re-transcribe)
  or `Transcribed` (re-summarize), then run `stenly-cli process <dir>`.

## Third-party references

Other meeting/dictation apps are references for behaviour only. Do not copy
their code, prompts, strings or assets; GPL-licensed projects (for example
VoiceInk) must not be copied at all. Describe equivalent behaviour in our own
words.
