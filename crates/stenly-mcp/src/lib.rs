//! Stenly MCP server: exposes recorded meetings (metadata, summaries, notes,
//! speaker-attributed transcripts) and upcoming calendar events to any MCP
//! client — Claude Code / Desktop, Codex, Cursor, pi, …
//!
//! Read-only and file-based: it reads the same session directories the app
//! writes, so it works whether or not the menubar app is running.
//!
//! Transports:
//! - stdio: `stenly-mcp` (the harness spawns it; recommended)
//! - streamable HTTP: `stenly-mcp serve` on 127.0.0.1:4791/mcp, or hosted by the
//!   menubar app while it runs (`[mcp] http = true`).

pub mod install;
pub mod mcp;
pub mod store;
pub mod transport;

pub use transport::{DEFAULT_PORT, serve_http, serve_stdio, spawn_http};
