# CLI and MCP

Run the CLI from a checkout with `cargo run -p stenly-cli -- <command>`, or install a built binary. Run `stenly-cli config` to print the configuration and resolved data paths on your machine.

## Common commands

```sh
# Start the desktop app
cargo run -p stenly --release

# Inspect configuration and upcoming meetings
cargo run -p stenly-cli -- config
cargo run -p stenly-cli -- upcoming

# Record a short local test session, then process a session
cargo run -p stenly-cli -- record --title "Meeting" --secs 30
cargo run -p stenly-cli -- process <session-directory>

# Work with templates, recipes, speakers, and destinations
cargo run -p stenly-cli -- templates
cargo run -p stenly-cli -- recipes
cargo run -p stenly-cli -- speakers drivers
cargo run -p stenly-cli -- destinations
```

`stenly-cli --help` and `stenly-cli <subcommand> --help` are the source of truth for the installed version. Recording captures your microphone and the system audio available on your computer; obtain any required consent before you use it.

## Summaries through Codex or Claude Code

If OpenAI's `codex` or Anthropic's `claude` (Claude Code) is installed and signed in, Stenly can write summaries with it (Settings → Summaries, or onboarding). It runs `codex exec` / `claude -p` with the prompt on stdin, in an empty temporary folder, with tools off (Codex read-only with no approvals; Claude Code with an empty tool list), and kills the process tree on timeout. Your own tool and login pay; Stenly never handles a token. The transcript is sent to OpenAI or Anthropic through that tool. The context agent needs tool calls and can't use this provider.

```toml
[llm]
provider = "cli"
[llm.cli]
tool = "claude"   # or "codex"
path = ""         # only if Stenly can't find it
model = ""        # "" = the tool's default
```

## MCP

`stenly-mcp` makes local meeting history available to an agent over the Model Context Protocol. Clients can list and search meetings, retrieve transcripts and notes, and inspect upcoming meetings. Recipes can call your configured summary model and optionally save their results, so not every tool is read-only. Sharing tools are exposed only when `[mcp] write_tools = true` is explicitly configured; check `stenly-mcp tools` for the installed version.

```sh
# From a source checkout
cargo install --path crates/stenly-mcp

# Register with detected clients, or choose clients explicitly
stenly-mcp install
stenly-mcp install claude codex cursor

# Exercise one tool without an agent client
stenly-mcp call get_transcript '{"id":"latest"}'
```

The default installation is a local stdio server: the agent client launches `stenly-mcp`, which reads the user’s local Stenly data. An optional loopback HTTP server is also available for local clients that support streamable HTTP:

```sh
stenly-mcp serve
stenly-mcp install --http
```

Only configure cloud models or destinations when you intend for selected meeting content to leave your machine. A local MCP transport does not prevent the connected client from sending tool results to its model provider.

## Sharing

The hosted default at `https://share.stenly.ai` is live. From an installed bundle or source-built CLI:

```sh
stenly-cli share <session-directory>
stenly-cli share <session-directory> --revoke
```

The first command creates or updates an end-to-end-encrypted share and prints the full reader URL. Treat it as sensitive: the key in the URL fragment gives access to the content. The hosted server stores ciphertext; an optional public preview title is a separate disclosure. Review the selected session before sharing. Revoking a link prevents future server retrieval; it cannot retract copies already retained by readers.

The CLI also supports named share destinations via `--to`; use `stenly-cli share --help` and `stenly-cli destinations` for the installed version. The self-hostable implementation and deployment files are in `server/stenly-share/`. Hosting terms and support duration are separate decisions, not guarantees made by these commands.
