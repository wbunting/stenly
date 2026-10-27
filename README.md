# Stenly

> **Local meeting notes. Useful context for your coding agent.**

Stenly is a local-first meeting recorder and dictation app written in Rust. It captures microphone and system audio without joining the call as a bot, transcribes on-device, and writes Markdown notes to a folder or Obsidian vault you choose.

**Platform status:** macOS is the primary tested desktop platform. Linux capture and dictation backends are implemented, but support depends on your audio stack, compositor and permissions; Linux does not currently read meeting-app speaker names. Windows meeting recording is not supported. See the [platform and speaker-name limitations](docs/platform-support.md) before relying on a particular setup.

## Meetings as context for coding agents

`stenly-mcp install` connects Stenly's local meeting tools to **Claude Code, Codex, Cursor** and other supported clients. Ask your agent to find a meeting, retrieve its transcript or read its notes. The desktop app does not need to stay open for the stdio server to read local recordings.

The optional meeting-context agent can gather relevant past meetings, local vault notes and sources you configure before a summary is produced. It is disabled by default and requires a suitable model and configured sources.

## Speaker names, with honest limits

Stenly can combine meeting-app Accessibility signals, locally remembered voiceprints and calendar invite information to suggest names. You can rename speakers and forget stored voiceprints in Settings. Attribution is not guaranteed: overlapping speech, off-screen tiles and app updates can reduce accuracy.

**Google Meet roster, active-speaker and self attribution have not yet passed live Chrome and Arc testing.** Do not rely on automatic Meet names today. Existing Zoom and Teams driver observations cover specific capabilities, not every version or layout; see the [capability matrix](docs/platform-support.md#speaker-identity).

## Also in the box

- Meeting capture and shortcut-driven dictation in one native application.
- On-device Parakeet transcription. Summaries require a separately configured local Ollama model or an OpenAI-compatible endpoint; a hosted summary service is not included.
- Markdown notes, templates and recipes.
- **End-to-end-encrypted share links are available**, with the hosted default at [share.stenly.ai](https://share.stenly.ai) and a self-hostable share server. Sharing uploads encrypted meeting content; anyone holding the full link can read it, and a reader can retain a copy. The key is in the URL fragment. See the [sharing guide](docs/cli.md#sharing).

## Install

Release downloads and final public source URLs are being prepared. There is no verified public installer linked here yet; do not use a guessed release URL. This README will link the signed/notarized release only after it is published and checked.

### Build from an available source checkout

Stenly uses stable Rust. On macOS, install Xcode Command Line Tools. On Debian/Ubuntu Linux, install the development packages for PipeWire, ALSA, Wayland/X11, Vulkan, Fontconfig, DBus and Clang before building.

```sh
cargo run -p stenly --release
```

For Linux, enable the desktop features appropriate to your system:

```sh
cargo run -p stenly --release --features pipewire,wayland,x11
```

The first run creates a configuration file and lets you select where notes and recordings live. See [the CLI guide](docs/cli.md) for headless workflows and [the architecture guide](docs/architecture.md) for the crate layout.

## Bring meetings into your agent

Install the MCP server from a source checkout (or use the copy included in the app bundle):

```sh
cargo install --path crates/stenly-mcp
stenly-mcp install                       # every detected supported client
stenly-mcp install claude codex cursor   # choose specific clients
```

Clients start `stenly-mcp` over stdio and it reads your local recordings. The server exposes meeting search, notes and transcripts; recipes can call your configured summary model and optionally save results. Sharing tools require an explicit configuration flag. **Local MCP transport does not make a cloud agent local:** content returned to an AI client may be sent to that client's model provider. See [CLI and MCP](docs/cli.md#mcp).

## Privacy, control, and consent

Recordings, transcripts, remembered voiceprints and Markdown notes are stored locally. Stenly does not send a bot into a call, but it does record audio: **no bot does not mean no consent is needed**.

Configured OpenAI-compatible model endpoints process text sent to them. Cloud destinations, remote context sources and MCP clients may also receive meeting information. Sharing is an explicit upload of encrypted content, not a claim that nothing leaves your computer. Review each integration before enabling it.

Tell everyone that you intend to record audio and transcribe it, explain any sharing or external processing, and obtain the agreement required for every participant's location and organization **before starting**. A chat notice is not proof of consent. Stenly does not currently post a recording notice or verify participant consent for you. If anyone objects, do not record. For general background, see the [Reporters Committee recording guide](https://www.rcfp.org/reporters-recording-guide/); it is US-focused, not legal advice or a substitute for checking local rules.

## How it compares

Speaker naming and MCP are not unique to Stenly. These are statements about current public documentation, not hands-on certification, pricing promises or a complete feature comparison. Editions and platform limitations matter.

| Project | Documented capabilities relevant to Stenly |
| --- | --- |
| **Stenly** | Meeting capture plus dictation, local Markdown notes, local meeting MCP, optional context gathering, encrypted share links and a self-hostable server. Meet attribution remains unverified; Linux does not read app speaker names. |
| [Meetily](https://meetily.ai/) | Meetily **Pro** documents shipped speaker diarization in [1.8.2](https://meetily.ai/releases/pro/1.8.2) and an MCP server in [1.11.0](https://meetily.ai/releases/pro/1.11.0). Do not confuse the free Community Edition roadmap with Pro availability. |
| [Anarlog](https://docs.anarlog.so/) | [1.4.14](https://github.com/fastrepl/anarlog/blob/main/packages/changelog/content/1.4.14.md) documents 1:1 naming and high-confidence voiceprint matching; its [MCP reference](https://docs.anarlog.so/reference/mcp) covers local and Cloud connections. |
| [Granola](https://docs.granola.ai/) | Documents [Google Meet speaker tags](https://docs.granola.ai/help-center/taking-notes/speaker-attribution-google-meet) and [MCP integration](https://docs.granola.ai/help-center/sharing/integrations/mcp). |
| [Wispr Flow](https://docs.wisprflow.ai/) | Documents Notetaker and [remote MCP](https://docs.wisprflow.ai/articles/9551372685-connect-an-mcp-client-to-wispr-flow-remote-mcp-server), subject to account access. |
| [Handy](https://github.com/cjpais/Handy) / [VoiceInk](https://github.com/Beingpax/VoiceInk) | Offline/local speech-to-text and dictation are central workflows; consult each project's current documentation for platform and model details. |

The linked Meetily, Anarlog, Granola and Wispr documentation was retrieved successfully during this copy review. Re-check it before publishing comparisons; documentation can change.

## Documentation

- [macOS packaging](docs/macos-packaging.md) — signing preflight, safe local installation, and packaging tests.
- [Platform support](docs/platform-support.md) — implementation versus verified behavior.
- [Architecture](docs/architecture.md) — crate boundaries and platform seams.
- [CLI and MCP](docs/cli.md) — commands, configuration, and agent integration.
- [Roadmap](docs/roadmap.md) — product direction and deferred work.

## License

Stenly's own source in this checkout is [MIT licensed](LICENSE) © William Bunting. This is not a promise about future binary pricing, hosted-service availability or support duration.

Third-party material keeps its own license and notices, which the root license does not replace:

- `vendor/accesskit_macos/`: AccessKit's macOS adapter (MIT OR Apache-2.0, with Chromium-derived code under the Chromium BSD-style license), lightly patched. See its `PROVENANCE.md` and license files.
- Fonts under `crates/stenly/assets/fonts/` and `server/stenly-share/assets/fonts/`: SIL Open Font License (see the accompanying OFL files).
- `crates/stenly-core/data/*.txt`: word lists under the licenses stated in each file's header.
- `site/static/icons/`: see `site/static/icons/LICENSE.md`.
- Service logos in `crates/stenly/assets/services/` are trademarks of their owners; see that directory's README.

Dependencies fetched by Cargo and npm are covered by their own licenses. Speech and summary models downloaded at runtime are separate works under their own terms.

### Speech recognition memory

Speech recognition runs in the bundled `stenly-speech` helper, not in the app's
UI process. Dictation, live meeting transcription, and post-recording processing
share a worker when they use the same speech model and provider. Audio capture
stays in the app; the worker does not need microphone or Accessibility access.
Optional speaker embeddings use a separate, app-owned speech worker so a damaged speaker
model cannot abort active speech recognition. Ollama summarization remains a
separate, independent service.

Settings → Dictation → Speech model → Memory use defaults to **Always ready**.
Choose **Unload after 5, 10, or 30 minutes** to reclaim model memory while idle.
Recording still begins immediately and buffers audio during reload; short
utterances may take longer to finish. Active dictation and meeting jobs are
never interrupted by the idle timeout. The equivalent config is
`[dictation] model_idle_secs = 0` (always ready), `300`, `600`, or `1800`.

Keep `stenly-speech` alongside `stenly` and `stenly-cli` when distributing or
copying builds. `cargo build --release -p stenly -p stenly-cli` builds all three;
the macOS and Linux bundle scripts include the helper automatically.
