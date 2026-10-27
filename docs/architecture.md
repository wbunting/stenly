# Architecture

Stenly is a Rust workspace split so product logic does not need to know which desktop it runs on.

| Crate | Responsibility |
| --- | --- |
| `stenly-core` | Configuration, calendar parsing, recording sessions, transcript and note processing, speaker naming, destinations, and the platform traits. |
| `stenly-platform` | OS integrations behind those traits: audio capture, microphone activity, desktop shell, tray, accessibility, global shortcuts, and text injection. |
| `stenly-asr` | Voice activity detection, Parakeet inference, live transcription, and voice embeddings. |
| `stenly-agent` | The meeting-context agent and its configured read-only context sources. |
| `stenly-mcp` | The read-only Model Context Protocol server and client installation support. |
| `stenly-cli` | Headless recording, processing, administration, and diagnostics. |
| `stenly` | The GPUI desktop application: windows, meeting views, settings, tray, and onboarding. |

## Platform boundary

`stenly-core` declares traits such as `AudioBackend`, `DesktopShell`, `Tray`, `MicActivity`, `Hotkeys`, and `TextInjector`. The application calls `stenly_platform::detect()` once and works with those trait objects. A platform-specific implementation belongs in `stenly-platform`, not in product logic.

## Data flow

1. A calendar trigger or explicit action starts a recording session.
2. The platform captures the microphone and available system audio; `stenly-core` persists session metadata and audio.
3. `stenly-asr` produces transcript segments. Speaker processing may use meeting-app accessibility state, local voiceprints, and invite information.
4. `stenly-core` builds the note and can send it to only the destinations the user configured.
5. `stenly-mcp` exposes completed local meeting data read-only to agent clients.

Configuration paths, data folders, models, note output, and retention are user-controlled. Secrets for optional integrations are kept out of the normal configuration file.

For commands and configuration entry points, see [CLI and MCP](cli.md).
