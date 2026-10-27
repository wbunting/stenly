---
name: stenly
description: Pull the user's recorded call transcripts, meeting summaries, decisions and action items from Stenly (local meeting recorder) into context. Use when the user mentions a call, meeting, standup, sync, "what did X say", "my last call", follow-ups, action items, or wants to prep for an upcoming meeting with context from past ones. Also covers installing / connecting the Stenly MCP server. Keywords: stenly, meeting, call, transcript, summary, action items, notes, standup, 1:1, granola.
---

# Stenly meetings

Stenly records the user's calls on their machine (their mic = **"Me"**, system audio =
**"Them"**), transcribes on-device, and writes an Obsidian note per meeting. The
`stenly` MCP server exposes all of it read-only.

"Them" can be several people — the transcript doesn't separate remote speakers. Use the
calendar attendees (from `get_meeting`) and context to attribute who said what, and say
so when it's an inference.

## Tools (MCP server `stenly`)

| tool | use it for |
| --- | --- |
| `list_meetings` `{since?, until?, title?, limit?}` | find calls by date / title; returns ids |
| `search_meetings` `{query, since?, until?, limit?}` | "when did we talk about X" — snippets with id + timestamp |
| `get_meeting` `{id}` | summary, sections, decisions, action items, attendees, calendar event |
| `get_transcript` `{id, offset?, max_chars?, speaker?}` | full "Me/Them [mm:ss]: …" transcript, paged |
| `get_note` `{id}` | the markdown note as it is in the vault (incl. the user's edits) |
| `upcoming_meetings` `{hours?}` | next calendar events — for prepping with past context |
| `list_recipes` `{}` | the user's saved prompts ("recipes": follow-up email, action items by owner, coach me, …) |
| `run_recipe` `{recipe, id, extra?, save?}` | run one of their recipes on a meeting with Stenly's own model; use when they ask for "my recipe" |

`id` accepts the id from a listing, `"latest"`, or a unique title fragment.

## How to work

1. Locate: `list_meetings` (dates like `2026-10-22`) or `search_meetings` for a topic.
   "Today's call" → `list_meetings {since: <today>}`; "my last call" → `id: "latest"`.
2. Start with `get_meeting` — the summary is usually enough for recaps and action items.
3. Pull `get_transcript` only when you need exact wording, quotes, or detail the summary
   lacks. For long calls follow the `offset=` hint in the truncation footer until done;
   don't guess at the missing part.
4. Quote with timestamps (`[12:34]`) so the user can find the moment.
5. Transcripts come from ASR: fix obvious mis-hearings of names/products when context
   is clear, never invent content.

Resources are also exposed as `stenly://meetings/<id>/transcript` and `.../note`
for harnesses that support attaching MCP resources (e.g. `@stenly:` in Claude Code).

## If the tools aren't available

The server isn't connected to this harness. Install it:

```sh
# Stenly.app installed (macOS):
/Applications/Stenly.app/Contents/MacOS/stenly-mcp install
# from source:
cargo install --path crates/stenly-mcp && stenly-mcp install
```

`install` registers the stdio server with every harness it finds (Claude Code, Codex,
pi, Cursor, Claude Desktop) and copies this skill into their skill folders. Then restart
the harness / `/reload`. Pass names to limit it (`stenly-mcp install claude cursor`).

Over HTTP instead (one long-running server, any client that speaks streamable HTTP):
`stenly-mcp serve` → `http://127.0.0.1:4791/mcp` (or turn on `[mcp] http = true` in
Stenly's config so the menubar app hosts it), then `stenly-mcp install --http`.

Without MCP at all, `stenly-mcp call <tool> '<json args>'` prints the same output in a shell,
e.g. `stenly-mcp call get_transcript '{"id":"latest"}'`.
