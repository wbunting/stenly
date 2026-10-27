//! `stenly-mcp`: MCP server for Stenly meetings.
//!
//!   stenly-mcp                      stdio server (what harnesses spawn)
//!   stenly-mcp serve [--port N]     streamable HTTP on 127.0.0.1:N/mcp
//!   stenly-mcp install [clients…]   register with Claude Code / Codex / pi / Cursor / Claude Desktop + install skill
//!   stenly-mcp uninstall
//!   stenly-mcp call <tool> [json]   run one tool locally (debugging)

use anyhow::{Context, Result, bail};
use std::net::{Ipv4Addr, SocketAddr};
use stenly_mcp::install::{self, Client, Transport};

const USAGE: &str = "stenly-mcp — MCP server for Stenly meeting transcripts

USAGE:
  stenly-mcp [stdio]                 serve MCP on stdin/stdout (default)
  stenly-mcp serve [--port 4791]     serve streamable HTTP at http://127.0.0.1:<port>/mcp
  stenly-mcp install [CLIENT…] [--http [URL]] [--no-skill]
                                     register with AI harnesses + install the agent skill.
                                     CLIENT: claude codex pi cursor claude-desktop (default: all detected)
  stenly-mcp uninstall [CLIENT…]
  stenly-mcp clients                 show which harnesses were detected
  stenly-mcp call <tool> ['{json args}']
  stenly-mcp tools                   list tools";

fn main() -> Result<()> {
    // stdout is the protocol channel in stdio mode: logs go to stderr only.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("STENLY_MCP_LOG").unwrap_or_else(|_| "warn".into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("stdio");
    let rest = args.get(1..).unwrap_or(&[]);
    match cmd {
        "stdio" => stenly_mcp::serve_stdio(),
        "serve" | "http" => {
            let port = flag_value(rest, "--port").map(|p| p.parse::<u16>()).transpose()?.unwrap_or(stenly_mcp::DEFAULT_PORT);
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            eprintln!("stenly MCP on http://{addr}/mcp");
            stenly_mcp::serve_http(addr)
        }
        "install" => cmd_install(rest),
        "uninstall" => cmd_uninstall(rest),
        "clients" => {
            for c in Client::ALL {
                println!("{} {}", if c.detected() { "✓" } else { " " }, c.id());
            }
            Ok(())
        }
        "tools" => {
            let reply = stenly_mcp::mcp::Server.handle(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).unwrap();
            for t in reply["result"]["tools"].as_array().into_iter().flatten() {
                println!("{:<20} {}", t["name"].as_str().unwrap_or(""), t["description"].as_str().unwrap_or(""));
            }
            Ok(())
        }
        "call" => {
            let tool = rest.first().context("usage: stenly-mcp call <tool> ['{json}']")?;
            let args = match rest.get(1) {
                Some(j) => serde_json::from_str(j).context("arguments must be a JSON object")?,
                None => serde_json::json!({}),
            };
            println!("{}", stenly_mcp::mcp::call_tool(tool, &args)?);
            Ok(())
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        "-V" | "--version" => {
            println!("stenly-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn pick_clients(args: &[String], only_detected: bool) -> Result<Vec<Client>> {
    let named: Vec<&String> = args.iter().filter(|a| !a.starts_with("--") && !a.starts_with("http")).collect();
    if named.is_empty() {
        return Ok(Client::ALL.into_iter().filter(|c| !only_detected || c.detected()).collect());
    }
    named.into_iter().map(|n| Client::parse(n).with_context(|| format!("unknown client {n:?} (claude, codex, pi, cursor, claude-desktop)"))).collect()
}

fn cmd_install(args: &[String]) -> Result<()> {
    let transport = match args.iter().position(|a| a == "--http") {
        Some(i) => Transport::Http {
            url: args
                .get(i + 1)
                .filter(|u| u.starts_with("http"))
                .cloned()
                .unwrap_or_else(|| format!("http://127.0.0.1:{}/mcp", stenly_mcp::DEFAULT_PORT)),
        },
        None => Transport::Stdio { command: install::default_command()? },
    };
    let clients = pick_clients(args, true)?;
    match &transport {
        Transport::Stdio { command } => println!("server: {} (stdio)", command.display()),
        Transport::Http { url } => println!("server: {url} (needs `stenly-mcp serve` or the Stenly app running with [mcp] http = true)"),
    }
    if clients.is_empty() {
        println!("no supported AI harness detected; pass one explicitly, e.g. `stenly-mcp install cursor`");
    }
    let mut ok = Vec::new();
    for c in &clients {
        match install::install_client(*c, &transport) {
            Ok(msg) => {
                println!("  ✓ {:<15} {msg}", c.id());
                ok.push(*c);
            }
            Err(e) => println!("  ✗ {:<15} {e:#}", c.id()),
        }
    }
    if !args.iter().any(|a| a == "--no-skill") {
        for f in install::install_skill(&ok)? {
            println!("  ✓ skill          {}", f.display());
        }
    }
    println!("restart the harness (or /reload, /mcp) to pick it up");
    Ok(())
}

fn cmd_uninstall(args: &[String]) -> Result<()> {
    for c in pick_clients(args, true)? {
        match install::uninstall_client(c) {
            Ok(msg) => println!("  ✓ {:<15} {msg}", c.id()),
            Err(e) => println!("  - {:<15} {}", c.id(), format!("{e:#}").lines().next().unwrap_or("")),
        }
    }
    for d in install::uninstall_skill() {
        println!("  ✓ removed        {}", d.display());
    }
    Ok(())
}
