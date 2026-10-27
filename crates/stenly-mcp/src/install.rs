//! `stenly-mcp install`: register the server with every AI harness found on
//! this machine and drop the `stenly` agent skill where they look for skills.
//!
//! CLIs with an `mcp add` command (Claude Code, Codex, pi) are driven through
//! it; JSON-config apps (Cursor, Claude Desktop) get their `mcpServers` entry
//! edited in place (with a `.bak` copy).

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SERVER_NAME: &str = "stenly";
pub const SKILL_MD: &str = include_str!("../../../skills/stenly/SKILL.md");
const DESCRIPTION: &str = "Stenly: the user's locally recorded call transcripts, meeting summaries and action items";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    ClaudeCode,
    Codex,
    Pi,
    Cursor,
    ClaudeDesktop,
}

impl Client {
    pub const ALL: [Client; 5] = [Client::ClaudeCode, Client::Codex, Client::Pi, Client::Cursor, Client::ClaudeDesktop];

    pub fn id(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude",
            Client::Codex => "codex",
            Client::Pi => "pi",
            Client::Cursor => "cursor",
            Client::ClaudeDesktop => "claude-desktop",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == s || (s == "claude-code" && *c == Client::ClaudeCode))
    }

    fn json_config(self) -> Option<PathBuf> {
        let home = home();
        match self {
            Client::Cursor => Some(home.join(".cursor/mcp.json")),
            Client::ClaudeDesktop => Some(if cfg!(target_os = "macos") {
                home.join("Library/Application Support/Claude/claude_desktop_config.json")
            } else {
                home.join(".config/Claude/claude_desktop_config.json")
            }),
            _ => None,
        }
    }

    fn cli(self) -> Option<&'static str> {
        match self {
            Client::ClaudeCode => Some("claude"),
            Client::Codex => Some("codex"),
            Client::Pi => Some("pi"),
            _ => None,
        }
    }

    /// Present on this machine?
    pub fn detected(self) -> bool {
        if let Some(cli) = self.cli() {
            return which(cli).is_some();
        }
        // only touch app configs whose directory already exists (app installed)
        self.json_config().and_then(|p| p.parent().map(Path::exists)).unwrap_or(false)
    }

    /// Skill directories this harness reads.
    fn skill_dirs(self) -> Vec<PathBuf> {
        let home = home();
        match self {
            Client::ClaudeCode => vec![home.join(".claude/skills")],
            Client::Codex => vec![home.join(".codex/skills")],
            Client::Pi => vec![home.join(".agents/skills")],
            Client::Cursor | Client::ClaudeDesktop => vec![],
        }
    }
}

#[derive(Debug, Clone)]
pub enum Transport {
    Stdio { command: PathBuf },
    Http { url: String },
}

fn home() -> PathBuf {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("~"))
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(cmd)).find(|p| p.is_file())
}

/// The binary harnesses should spawn: the installed app's copy if present
/// (survives `cargo clean`), else this executable.
pub fn default_command() -> Result<PathBuf> {
    let app = PathBuf::from("/Applications/Stenly.app/Contents/MacOS/stenly-mcp");
    if cfg!(target_os = "macos") && app.is_file() {
        return Ok(app);
    }
    let exe = std::env::current_exe()?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd).args(args).output().with_context(|| format!("running {cmd}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    anyhow::ensure!(out.status.success(), "{cmd} {}: {}", args.join(" "), text.trim());
    Ok(text)
}

pub fn install_client(client: Client, transport: &Transport) -> Result<String> {
    if let Some(path) = client.json_config() {
        return edit_json_config(&path, Some(json_entry(client, transport)));
    }
    let cli = client.cli().unwrap();
    // Remove first so re-running install updates the entry (ignore "not found").
    let _ = uninstall_cli(client);
    match (client, transport) {
        (Client::ClaudeCode, Transport::Stdio { command }) => {
            run(cli, &["mcp", "add", "-s", "user", SERVER_NAME, "--", &command.to_string_lossy()])?;
        }
        (Client::ClaudeCode, Transport::Http { url }) => {
            run(cli, &["mcp", "add", "-s", "user", "--transport", "http", SERVER_NAME, url])?;
        }
        (Client::Codex, Transport::Stdio { command }) => {
            run(cli, &["mcp", "add", SERVER_NAME, "--", &command.to_string_lossy()])?;
        }
        (Client::Codex, Transport::Http { url }) => {
            run(cli, &["mcp", "add", SERVER_NAME, "--url", url])?;
        }
        (Client::Pi, Transport::Stdio { command }) => {
            run(cli, &["mcp", "add", SERVER_NAME, "--description", DESCRIPTION, "--", &command.to_string_lossy()])?;
        }
        (Client::Pi, Transport::Http { url }) => {
            run(cli, &["mcp", "add", SERVER_NAME, "--description", DESCRIPTION, "--url", url])?;
        }
        _ => unreachable!(),
    }
    Ok(format!("registered via `{cli} mcp add`"))
}

fn uninstall_cli(client: Client) -> Result<String> {
    let cli = client.cli().unwrap();
    match client {
        Client::ClaudeCode => run(cli, &["mcp", "remove", "-s", "user", SERVER_NAME]),
        _ => run(cli, &["mcp", "remove", SERVER_NAME]),
    }
}

pub fn uninstall_client(client: Client) -> Result<String> {
    match client.json_config() {
        Some(path) => edit_json_config(&path, None),
        None => uninstall_cli(client).map(|_| "removed".into()),
    }
}

fn json_entry(client: Client, transport: &Transport) -> Value {
    match transport {
        Transport::Stdio { command } => json!({ "command": command, "args": [] }),
        // Claude Desktop only speaks stdio from its JSON config; bridge with npx mcp-remote.
        Transport::Http { url } if client == Client::ClaudeDesktop => json!({ "command": "npx", "args": ["-y", "mcp-remote", url] }),
        Transport::Http { url } => json!({ "url": url }),
    }
}

/// Set (or with `None`, remove) `mcpServers.stenly` in a JSON config file.
fn edit_json_config(path: &Path, entry: Option<Value>) -> Result<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: Value = if text.trim().is_empty() { json!({}) } else { serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))? };
    let obj = doc.as_object_mut().with_context(|| format!("{} is not a JSON object", path.display()))?;
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().context("mcpServers is not an object")?;
    let msg = match entry {
        Some(e) => {
            if servers.get(SERVER_NAME) == Some(&e) {
                return Ok(format!("already configured in {}", path.display()));
            }
            servers.insert(SERVER_NAME.into(), e);
            format!("wrote {}", path.display())
        }
        None => {
            if servers.remove(SERVER_NAME).is_none() {
                return Ok("not configured".into());
            }
            format!("removed from {}", path.display())
        }
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if !text.is_empty() {
        std::fs::write(path.with_extension("json.bak"), &text)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&doc)? + "\n")?;
    Ok(msg)
}

/// Write `<dir>/stenly/SKILL.md` into each harness's skill directory (and the
/// shared `~/.agents/skills`). Returns the files written.
pub fn install_skill(clients: &[Client]) -> Result<Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = vec![home().join(".agents/skills")];
    for c in clients {
        for d in c.skill_dirs() {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    let mut written = Vec::new();
    for d in dirs {
        let dir = d.join(SERVER_NAME);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let file = dir.join("SKILL.md");
        std::fs::write(&file, SKILL_MD)?;
        written.push(file);
    }
    Ok(written)
}

pub fn uninstall_skill() -> Vec<PathBuf> {
    let home = home();
    [".agents/skills", ".claude/skills", ".codex/skills"]
        .iter()
        .map(|d| home.join(d).join(SERVER_NAME))
        .filter(|d| d.join("SKILL.md").exists() && std::fs::remove_dir_all(d).is_ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_config_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mcp.json");
        std::fs::write(&p, r#"{"mcpServers":{"other":{"command":"x"}},"keep":1}"#).unwrap();
        let t = Transport::Stdio { command: "/bin/stenly-mcp".into() };
        edit_json_config(&p, Some(json_entry(Client::Cursor, &t))).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["stenly"]["command"], "/bin/stenly-mcp");
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["keep"], 1);
        assert!(edit_json_config(&p, Some(json_entry(Client::Cursor, &t))).unwrap().starts_with("already"));
        edit_json_config(&p, None).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v["mcpServers"].get("stenly").is_none());
    }
}
