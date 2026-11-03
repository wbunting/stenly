# Stenly roadmap

Stenly is a local-first meeting recorder and dictation app. The roadmap below describes product direction, not release commitments.

## Shipped

- Signed, notarized macOS app (v0.1.2) with a 10-meeting free trial and a $10 one-time licence; the source is MIT and free to build.
- Meeting capture and shortcut-driven dictation, with on-device transcription.
- Summaries through a ChatGPT sign-in, the installed Codex or Claude Code CLI, local Ollama or any OpenAI-compatible API key.
- Calendars through Calendar.app (EventKit) on Mac and through ICS links.
- End-to-end-encrypted share links on share.stenly.ai, with a self-hostable server.
- Notes written to a Markdown folder or Obsidian vault, plus Notion, Slack and webhook destinations.
- Local MCP server and an optional meeting-context agent that can read your own MCP servers.

## In flight

- **Real speaker names.** Combine meeting-app active-speaker state, local voiceprints, invite information, and user corrections to identify speakers more usefully than generic labels. Google Meet is not verified yet.
- **Pre-meeting brief.** Show a brief drawn from relevant prior notes before the call. The context agent already gathers this context; showing it in the app is not finished.
- **Sign in with Google for calendars.** Built, but not enabled in released builds yet.
- **Templates, recipes, and destinations.** Custom note structures, repeatable prompts, and clearly labelled local or cloud destinations.

## Planned or exploratory

- A Linux release (today Linux is built from source).
- Importing audio or video files for a transcript or a full note.
- In-app update installation. Update notification (a GitHub Releases check in official builds) ships from the release after v0.1.2; installation stays manual.

## Not planned

- A notepad or note editor inside Stenly. Notes are edited where they live: Obsidian, Notion or your Markdown editor.
- A hosted team workspace or mandatory cloud sync.
- A meeting bot that joins calls.
- Mobile applications.

Stenly aims to keep ownership and consent clear: users choose when to record, where notes go, and which optional services receive their content.
