# Stenly

> **Local meeting notes. Useful context for your coding agent.**

Stenly is a local-first meeting recorder and dictation app written in Rust. It captures microphone and system audio with no bot joining the call, transcribes on-device, and writes Markdown notes to a folder or Obsidian vault you choose.

**Platform status:** macOS is the primary tested desktop platform, with a signed, notarized app (v0.1.2) you can [download](#install). Linux has no release yet (coming soon): the capture and dictation backends are implemented and you can build from source, but support depends on your audio stack, compositor and permissions, and Linux does not currently read meeting-app speaker names. Windows meeting recording is not supported. See the [platform and speaker-name limitations](docs/platform-support.md) before relying on a particular setup.

## What is different about Stenly

- **Meetings and dictation in one app that is MIT-licensed and free to build.** Dictation never needs a licence, and you can build it from source for Linux.
- **End-to-end-encrypted share links on a server you can host yourself.** The default is [share.stenly.ai](https://share.stenly.ai); see the [sharing guide](docs/cli.md#sharing).
- **Notes written straight into Obsidian** (or any Markdown folder). Notes are edited where they live, in Obsidian, Notion or your editor of choice, so there is no second editor to learn.
- **A context agent that reads your own MCP servers** (plus past meetings, your vault and Notion) before it writes the summary. It is off by default.
- **Import from VoiceInk** (`stenly-cli import-voiceink`): vocabulary, replacements, microphone priority and history.

## Meetings as context for coding agents

`stenly-mcp install` connects Stenly's local meeting tools to **Claude Code, Codex, Cursor** and other supported clients. Ask your agent to find a meeting, retrieve its transcript or read its notes. The desktop app does not need to stay open for the stdio server to read local recordings.

The optional meeting-context agent can gather relevant past meetings, local vault notes and sources you configure before a summary is produced. It is disabled by default and requires a suitable model and configured sources.

## Speaker names, with honest limits

Stenly can combine meeting-app Accessibility signals, locally remembered voiceprints and calendar invite information to suggest names. You can rename speakers and forget stored voiceprints in Settings. Attribution is not guaranteed: overlapping speech, off-screen tiles and app updates can reduce accuracy.

**Google Meet roster, active-speaker and self attribution have not yet passed live Chrome and Arc testing.** Do not rely on automatic Meet names today. Existing Zoom and Teams driver observations cover specific capabilities, not every version or layout; see the [capability matrix](docs/platform-support.md#speaker-identity).

## Also in the box

- Meeting capture and shortcut-driven dictation in one native application.
- On-device Parakeet transcription.
- Summaries with the account or tool you already have: **sign in with ChatGPT** (your plan pays), use the **Codex or Claude Code** command-line tool you have installed, run a local **Ollama** model, or add any **OpenAI-compatible API key**. Stenly does not run a hosted summary service, and with any provider except local Ollama the transcript text is sent to that provider.
- Calendars: **Calendar.app on Mac** (through EventKit, which covers the Google, iCloud and Exchange accounts already in it) and **ICS links**. Sign in with Google is built but not enabled in released builds yet; see [Google Calendar](docs/google-calendar.md).
- Markdown notes, templates and recipes.
- **End-to-end-encrypted share links are available**, with the hosted default at [share.stenly.ai](https://share.stenly.ai) and a self-hostable share server. Sharing uploads encrypted meeting content; anyone holding the full link can read it, and a reader can retain a copy. The key is in the URL fragment. See the [sharing guide](docs/cli.md#sharing).

## Install

### macOS

Download the signed, notarized universal app (Apple Silicon and Intel, macOS 14.2 or later) from the [latest release](https://github.com/wbunting/stenly/releases/latest) (v0.1.2 at the time of writing). It includes 10 free meetings; see [pricing](#pricing-free-to-build-10-for-the-signed-app).

### Linux

There is no Linux release yet (coming soon). You can build from source today.

### Build from source

Stenly uses stable Rust. On macOS, install Xcode Command Line Tools. On Debian/Ubuntu Linux, install the development packages for PipeWire, ALSA, Wayland/X11, Vulkan, Fontconfig, DBus and Clang before building.

```sh
cargo run -p stenly --release
```

For Linux, enable the desktop features appropriate to your system:

```sh
cargo run -p stenly --release --features pipewire,wayland,x11
```

The first run creates a configuration file and lets you select where notes and recordings live. See [the CLI guide](docs/cli.md) for headless workflows and [the architecture guide](docs/architecture.md) for the crate layout.

## Pricing: free to build, $10 for the signed app

Stenly's source is MIT and free to build yourself. **A source build has no trial, no licence key and no limits.**

The signed, notarized macOS app from the official release is **$10 one-time, for one Mac (a key can be activated on up to 3), updates included**, after **10 free meetings**. Only that official build enforces it, and only by pausing *new meeting recordings* once the free meetings are used up. Existing notes, search, sharing, MCP and dictation always keep working. Settings → Licence shows the status, opens checkout, activates a key and deactivates this Mac (so you can move the licence). `stenly-cli license status|activate|deactivate` does the same from a terminal.

**Updates:** new versions are included in the price and are downloaded from the [releases page](https://github.com/wbunting/stenly/releases/latest). The app does not install updates itself. It has an opt-in, off-by-default release-notification setting (notification only), but the v0.1.2 macOS build has no update feed configured, so it cannot notify you yet; watch the releases page.

The licence is re-checked about weekly and keeps working offline for 30 days. The gate is compiled in only when `scripts/bundle-macos.sh` runs with `STENLY_ENFORCE_LICENSE=1` and a Developer ID identity (the release workflow does this); plain `cargo build` and ad-hoc bundles never enforce anything. For testing the gate with an isolated profile (`STENLY_CONFIG`, `STENLY_DATA_DIR`), set `STENLY_FORCE_LICENSE_GATE=1`.

## Bring meetings into your agent

Use the `stenly-mcp` copy included in the app bundle, or install it from a source checkout:

```sh
cargo install --path crates/stenly-mcp
stenly-mcp install                       # every detected supported client
stenly-mcp install claude codex cursor   # choose specific clients
```

Clients start `stenly-mcp` over stdio and it reads your local recordings. The server exposes meeting search, notes and transcripts; recipes can call your configured summary model and optionally save results. Sharing tools require an explicit configuration flag. **Local MCP transport does not make a cloud agent local:** content returned to an AI client may be sent to that client's model provider. See [CLI and MCP](docs/cli.md#mcp).

## Privacy, control, and consent

Recordings, transcripts, remembered voiceprints and Markdown notes are stored locally. Stenly does not send a bot into a call, but it does record audio: **no bot does not mean no consent is needed**.

Configured OpenAI-compatible model endpoints process text sent to them. Cloud destinations, remote context sources and MCP clients may also receive meeting information. Sharing is an explicit upload of encrypted content, not a claim that nothing leaves your computer. Review each integration before enabling it.

Tell everyone that you intend to record audio and transcribe it, explain any sharing or external processing, and obtain the agreement required for every participant's location and organization **before starting**. A chat notice is not proof of consent. Stenly does not currently post a recording notice or verify participant consent for you; you are responsible for consent. If anyone objects, do not record. For general background, see the [Reporters Committee recording guide](https://www.rcfp.org/reporters-recording-guide/); it is US-focused, not legal advice or a substitute for checking local rules.

## How it compares

Speaker naming and MCP are not unique to Stenly. These are statements from each project's public documentation, last checked on 3 November 2026. They are not hands-on certification, pricing promises or a complete feature comparison, and editions and platforms matter.

| Project | Documented capabilities relevant to Stenly |
| --- | --- |
| **Stenly** | Meetings and dictation in one free, MIT-licensed app (macOS app; Linux from source). Local Markdown and Obsidian notes, local meeting MCP, optional context agent over your own MCP servers, end-to-end-encrypted share links on a self-hostable server, VoiceInk import. Google Meet speaker names are unverified, and Linux does not read app speaker names. |
| [Meetily](https://meetily.ai/) | Meetily **Pro** documents shipped speaker diarization in [1.8.2](https://meetily.ai/releases/pro/1.8.2) and an MCP server in [1.11.0](https://meetily.ai/releases/pro/1.11.0). Do not confuse the free Community Edition with Pro. |
| [Anarlog](https://docs.anarlog.so/) | Voiceprint speaker naming and 1:1 labelling ([1.4.14](https://github.com/fastrepl/anarlog/blob/main/packages/changelog/content/1.4.14.md)), pre-meeting briefs ([1.4.12](https://github.com/fastrepl/anarlog/blob/main/packages/changelog/content/1.4.12.md)), [chat with your meetings](https://docs.anarlog.so/chat), a local [MCP server](https://docs.anarlog.so/reference/mcp) and, with Pro, a remote [Cloud API and MCP](https://docs.anarlog.so/reference/api-cloud). Its [dictation](https://docs.anarlog.so/dictation) is part of Anarlog Pro. |
| [Granola](https://docs.granola.ai/) | Documents [Google Meet speaker tags](https://docs.granola.ai/help-center/taking-notes/speaker-attribution-google-meet) and [MCP integration](https://docs.granola.ai/help-center/sharing/integrations/mcp). |
| [Wispr Flow](https://docs.wisprflow.ai/) | Documents a [remote MCP server](https://docs.wisprflow.ai/articles/9551372685-connect-an-mcp-client-to-wispr-flow-remote-mcp-server) for its Notetaker, subject to account access. |
| [Handy](https://github.com/cjpais/Handy) / [VoiceInk](https://github.com/Beingpax/VoiceInk) | Open-source dictation apps: Handy describes itself as speech-to-text that works completely offline, and VoiceInk as a macOS voice-to-text app with no subscription. |

Re-check these pages before quoting the comparison; documentation changes.

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
