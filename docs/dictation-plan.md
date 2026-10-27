# Dictation implementation notes

Stenly’s dictation feature shares the same local speech pipeline and microphone preferences as meeting recording. It is implemented in the workspace rather than as a separate app.

## Components

- `stenly-core`: dictation state machine, text cleanup, replacements, vocabulary, optional AI cleanup, and history.
- `stenly-platform`: global shortcut listeners and text injection for supported desktops.
- `stenly-asr`: short-clip transcription and streaming support.
- `stenly`: the heads-up display, settings, and history views.

## Current behavior

- Configurable modifier or key-combination shortcut with toggle, push-to-talk, and hybrid modes.
- Local transcription, cleanup, personal vocabulary, replacements, and history.
- Optional AI cleanup through the summary-model configuration.
- Configurable paste and optional Return after transcription.
- macOS and Linux platform backends behind the same core interfaces.

See [CLI and MCP](cli.md) for the supported command-line surface. This document intentionally avoids machine-specific test setup and personal application paths.
