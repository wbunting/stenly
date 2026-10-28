//! Summaries through a coding-agent CLI the user already has: OpenAI's `codex`
//! or Anthropic's `claude` (Claude Code), signed in with their own account.
//!
//! Stenly runs the tool non-interactively (`codex exec`, `claude -p`) with the
//! prompt on stdin, in an empty temporary directory, with every tool turned
//! off, so the model can only answer. The tool's own login pays for it and
//! Stenly never sees a token. The transcript does go to the tool's provider
//! (OpenAI / Anthropic); the UI says so wherever this is offered.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The two supported tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Codex,
    Claude,
}

impl Tool {
    pub const ALL: [Tool; 2] = [Tool::Codex, Tool::Claude];

    /// Config value and executable name.
    pub fn id(self) -> &'static str {
        match self {
            Tool::Codex => "codex",
            Tool::Claude => "claude",
        }
    }

    pub fn parse(s: &str) -> Option<Tool> {
        match s.trim().to_ascii_lowercase().as_str() {
            "codex" => Some(Tool::Codex),
            "claude" | "claude-code" | "claude_code" => Some(Tool::Claude),
            _ => None,
        }
    }

    /// Product name for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Tool::Codex => "Codex",
            Tool::Claude => "Claude Code",
        }
    }

    /// Who receives the transcript.
    pub fn vendor(self) -> &'static str {
        match self {
            Tool::Codex => "OpenAI",
            Tool::Claude => "Anthropic",
        }
    }

    /// How to sign in, as a sentence for error messages.
    pub fn login_hint(self) -> &'static str {
        match self {
            Tool::Codex => "Run “codex login” in Terminal, then try again.",
            Tool::Claude => "Run “claude” in Terminal and sign in with /login, then try again.",
        }
    }

    /// One honest sentence about where the transcript goes.
    pub fn disclosure(self) -> String {
        format!(
            "Your transcript is sent to {} through the {} tool, using the account it is signed in to. Stenly never sees your login; audio never leaves this Mac.",
            self.vendor(),
            self.id()
        )
    }
}

/// Which tool to run and how, resolved from `[llm.cli]`.
#[derive(Debug, Clone, PartialEq)]
pub struct CliSpec {
    pub tool: Tool,
    /// Explicit executable ("" = look for it).
    pub path: String,
    /// Model name passed to the tool ("" = the tool's own default).
    pub model: String,
}

/// Why a run failed. `Display` is the message shown to the user.
#[derive(Debug, Clone, PartialEq)]
pub enum CliError {
    NotInstalled(Tool, String),
    NotLoggedIn(Tool),
    UsageLimit(Tool, String),
    Timeout(Tool, u64),
    BadModel(Tool, String),
    /// The installed version doesn't know a flag Stenly needs.
    TooOld(Tool, String),
    Cancelled(Tool),
    Empty(Tool),
    Failed(Tool, Option<i32>, String),
}

impl CliError {
    /// A retry won't help.
    pub fn is_permanent(&self) -> bool {
        !matches!(
            self,
            CliError::Timeout(..) | CliError::Failed(..) | CliError::Empty(..)
        )
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::NotInstalled(t, path) if path.is_empty() => {
                write!(
                    f,
                    "{} isn't installed, or Stenly can't find the “{}” command. Install it, or set its path in Settings → Summaries.",
                    t.label(),
                    t.id()
                )
            }
            CliError::NotInstalled(t, path) => write!(
                f,
                "Stenly can't run {} at {path}. Check the path in Settings → Summaries.",
                t.label()
            ),
            CliError::NotLoggedIn(t) => {
                write!(f, "{} isn't signed in. {}", t.label(), t.login_hint())
            }
            CliError::UsageLimit(t, d) => {
                write!(
                    f,
                    "Your {} usage limit has been reached{}. Try again later, or pick another summary model in Settings.",
                    t.label(),
                    if d.is_empty() {
                        String::new()
                    } else {
                        format!(" ({d})")
                    }
                )
            }
            CliError::Timeout(t, secs) => write!(
                f,
                "{} didn't answer within {secs} s, so Stenly stopped it.",
                t.label()
            ),
            CliError::BadModel(t, d) => write!(
                f,
                "{} doesn't accept this model name{}. Clear the model in Settings → Summaries to use its default.",
                t.label(),
                if d.is_empty() {
                    String::new()
                } else {
                    format!(" ({d})")
                }
            ),
            CliError::TooOld(t, flag) => write!(
                f,
                "The installed {} doesn't support {flag}. Update it, then try again.",
                t.label()
            ),
            CliError::Cancelled(t) => write!(f, "{} was stopped.", t.label()),
            CliError::Empty(t) => write!(f, "{} returned nothing.", t.label()),
            CliError::Failed(t, code, d) => {
                let code = code.map(|c| format!(" (exit {c})")).unwrap_or_default();
                if d.is_empty() {
                    write!(f, "{} failed{code}.", t.label())
                } else {
                    write!(f, "{} failed{code}: {d}", t.label())
                }
            }
        }
    }
}

impl std::error::Error for CliError {}

// ------------------------------------------------------------------ finding the tools

/// Directories where these tools usually live. A GUI app launched from Finder
/// gets a minimal `PATH`, so the usual install locations are searched as well.
pub fn search_dirs(home: Option<&Path>, path_env: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(p) = path_env {
        for d in std::env::split_paths(p) {
            if !d.as_os_str().is_empty() {
                add(d);
            }
        }
    }
    if let Some(h) = home {
        for rel in [
            ".local/bin",
            ".claude/local",
            ".codex/bin",
            ".bun/bin",
            ".volta/bin",
            ".npm-global/bin",
            ".npm-packages/bin",
            ".yarn/bin",
            ".cargo/bin",
            "Library/pnpm",
            ".local/share/pnpm",
            ".local/share/mise/shims",
            ".asdf/shims",
        ] {
            add(h.join(rel));
        }
        // version managers keep one bin directory per installed Node
        for (base, tail) in [
            (".nvm/versions/node", "bin"),
            (
                "Library/Application Support/fnm/node-versions",
                "installation/bin",
            ),
            (".local/share/fnm/node-versions", "installation/bin"),
            (".fnm/node-versions", "installation/bin"),
            (".local/share/mise/installs/node", "bin"),
        ] {
            let Ok(rd) = std::fs::read_dir(h.join(base)) else {
                continue;
            };
            let mut versions: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            versions.sort();
            // newest first
            for v in versions.into_iter().rev() {
                add(v.join(tail));
            }
        }
    }
    for d in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/opt/local/bin",
        "/snap/bin",
    ] {
        add(PathBuf::from(d));
    }
    out
}

fn real_search_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    search_dirs(home.as_deref(), std::env::var_os("PATH").as_deref())
}

fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn exe_names(tool: Tool) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{}.exe", tool.id()),
            format!("{}.cmd", tool.id()),
            tool.id().to_string(),
        ]
    } else {
        vec![tool.id().to_string()]
    }
}

/// The executable for `tool`: `explicit` when set (an absolute path, `~/…`, or
/// a bare name looked up in `dirs`), otherwise the first match in `dirs`.
pub fn find_in(tool: Tool, explicit: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let explicit = explicit.trim();
    if !explicit.is_empty() {
        let p = crate::util::expand_tilde(explicit);
        if p.components().count() > 1 || p.is_absolute() {
            return is_executable(&p).then_some(p);
        }
        return dirs.iter().map(|d| d.join(&p)).find(|c| is_executable(c));
    }
    let names = exe_names(tool);
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|c| is_executable(c))
}

/// Find the tool on this machine.
pub fn find(tool: Tool, explicit: &str) -> Option<PathBuf> {
    find_in(tool, explicit, &real_search_dirs())
}

/// An installed tool.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    pub tool: Tool,
    pub path: PathBuf,
    /// First line of `--version`.
    pub version: Option<String>,
}

/// Which of the two tools are installed. `cfg`'s explicit path applies to the tool it names.
pub fn detect(cfg: &crate::config::CliLlmConfig) -> Vec<Detected> {
    let dirs = real_search_dirs();
    let chosen = Tool::parse(&cfg.tool);
    Tool::ALL
        .into_iter()
        .filter_map(|tool| {
            let explicit = if chosen == Some(tool) {
                cfg.path.as_str()
            } else {
                ""
            };
            let path = find_in(tool, explicit, &dirs)?;
            let version = version(&path, &dirs);
            Some(Detected {
                tool,
                path,
                version,
            })
        })
        .collect()
}

/// `codex-cli 0.153.4`, `2.1.300 (Claude Code)`.
fn version(path: &Path, dirs: &[PathBuf]) -> Option<String> {
    let wd = Workdir::new().ok()?;
    let run = exec(
        path,
        &["--version".into()],
        wd.path(),
        None,
        tool_path(path, dirs),
        Duration::from_secs(8),
        None,
    )
    .ok()?;
    let Ended::Done(o) = run else { return None };
    let line = o
        .stdout
        .lines()
        .chain(o.stderr.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())?;
    Some(line.chars().take(60).collect())
}

/// `PATH` for the child: the tool's own directory first (npm shims start with
/// `#!/usr/bin/env node`, which must resolve), then the usual places.
fn tool_path(exe: &Path, dirs: &[PathBuf]) -> OsString {
    let mut all: Vec<PathBuf> = Vec::new();
    if let Some(d) = exe.parent() {
        all.push(d.to_path_buf());
    }
    for d in dirs {
        if !all.contains(d) {
            all.push(d.clone());
        }
    }
    std::env::join_paths(all).unwrap_or_default()
}

// ------------------------------------------------------------------ the command

/// Optional flags: nice to have, so a CLI that doesn't know one is run without it.
const CODEX_OPTIONAL: &[&str] = &["--ephemeral", "--ignore-rules"];
const CLAUDE_OPTIONAL: &[&str] = &[
    "--no-session-persistence",
    "--disable-slash-commands",
    "--safe-mode",
];

/// Codex features that give the model tools or reach beyond the prompt.
const CODEX_FEATURES_OFF: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "view_image",
    "apps",
    "plugins",
    "hooks",
    "memories",
    "browser_use",
    "computer_use",
    "image_generation",
    "multi_agent",
];

/// What Claude is told when the caller gave no system prompt.
const CLAUDE_SYSTEM: &str = "You are a text-writing function inside a note-taking app. You have no tools and cannot read files or browse. Reply with exactly what the task asks for and nothing else.";

/// The arguments for one run (the prompt goes on stdin). `skip` lists optional
/// flags to leave out. Read-only and no approvals for Codex; no tools at all
/// for Claude Code.
pub fn command_args(
    tool: Tool,
    model: &str,
    system: Option<&str>,
    workdir: &Path,
    out_file: &Path,
    skip: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let flag = |a: &mut Vec<String>, f: &str| {
        if !skip.iter().any(|s| s == f) {
            a.push(f.to_string());
        }
    };
    match tool {
        Tool::Codex => {
            a.push("exec".into());
            a.push("--skip-git-repo-check".into());
            flag(&mut a, "--ephemeral");
            // don't load the user's config.toml (custom providers, MCP servers, hooks); sign-in still comes from CODEX_HOME
            a.push("--ignore-user-config".into());
            flag(&mut a, "--ignore-rules");
            a.extend(["-s", "read-only", "-C"].map(String::from));
            a.push(workdir.to_string_lossy().into_owned());
            a.extend(
                [
                    "-c",
                    "approval_policy=\"never\"",
                    "-c",
                    "web_search=\"disabled\"",
                ]
                .map(String::from),
            );
            for f in CODEX_FEATURES_OFF {
                a.push("-c".into());
                a.push(format!("features.{f}=false"));
            }
            if !model.trim().is_empty() {
                a.extend(["-m".into(), model.trim().to_string()]);
            }
            a.push("--json".into());
            a.push("-o".into());
            a.push(out_file.to_string_lossy().into_owned());
            a.push("-".into());
        }
        Tool::Claude => {
            a.extend(
                [
                    "-p",
                    "--output-format",
                    "json",
                    "--tools",
                    "",
                    "--strict-mcp-config",
                    "--setting-sources",
                    "",
                ]
                .map(String::from),
            );
            flag(&mut a, "--no-session-persistence");
            flag(&mut a, "--disable-slash-commands");
            flag(&mut a, "--safe-mode");
            a.push("--system-prompt".into());
            a.push(
                system
                    .map(|s| format!("{s}\n\n{CLAUDE_SYSTEM}"))
                    .unwrap_or_else(|| CLAUDE_SYSTEM.to_string()),
            );
            if !model.trim().is_empty() {
                a.extend(["--model".into(), model.trim().to_string()]);
            }
        }
    }
    a
}

/// What goes on stdin. Claude takes the system prompt as a flag; Codex has no
/// such flag, so it is placed ahead of the task.
fn stdin_text(tool: Tool, system: Option<&str>, prompt: &str) -> String {
    match (tool, system) {
        (Tool::Codex, Some(s)) if !s.trim().is_empty() => {
            format!("{}\n\n---\n\n{prompt}", s.trim())
        }
        _ => prompt.to_string(),
    }
}

// ------------------------------------------------------------------ running it

/// A fresh, empty, private directory the tool runs in; removed afterwards.
struct Workdir(PathBuf);

impl Workdir {
    fn new() -> std::io::Result<Self> {
        let mut r = [0u8; 8];
        let _ = getrandom::fill(&mut r);
        let name: String = r.iter().map(|b| format!("{b:02x}")).collect();
        let p = std::env::temp_dir().join(format!("stenly-cli-llm-{}-{name}", std::process::id()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&p)?;
        Ok(Workdir(p))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Default)]
struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

enum Ended {
    Done(Out),
    TimedOut,
    Cancelled,
}

/// Kills the child's whole process group when dropped before it finished
/// (timeout, cancel, or a panic on our side).
struct Reaper {
    child: Child,
    finished: bool,
}

impl Reaper {
    fn kill_tree(&mut self) {
        kill_tree(&mut self.child);
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if !self.finished {
            self.kill_tree();
            let _ = self.child.wait();
        }
    }
}

fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // the child leads its own process group (see `exec`), so -pid reaches its descendants
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// Cap on what is kept from a child's stdout/stderr.
const MAX_CAPTURE: u64 = 16 * 1024 * 1024;

fn drain<R: Read + Send + 'static>(mut r: R) -> (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let b = buf.clone();
    let h = std::thread::spawn(move || {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut g = b.lock().unwrap();
                    if (g.len() as u64) < MAX_CAPTURE {
                        g.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
    });
    (buf, h)
}

/// Run `program`, feed `stdin`, wait up to `timeout`. On timeout or `cancel`
/// the whole process tree is killed.
fn exec(
    program: &Path,
    args: &[String],
    cwd: &Path,
    stdin: Option<String>,
    path_env: OsString,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> std::io::Result<Ended> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .env("PATH", path_env)
        .env("NO_COLOR", "1")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    if let (Some(text), Some(mut si)) = (stdin, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = si.write_all(text.as_bytes());
        });
    }
    let (out_buf, out_h) = drain(child.stdout.take().expect("piped"));
    let (err_buf, err_h) = drain(child.stderr.take().expect("piped"));
    let mut reaper = Reaper {
        child,
        finished: false,
    };
    let started = Instant::now();
    let (status, end) = loop {
        match reaper.child.try_wait()? {
            Some(s) => break (Some(s), 0u8),
            None => {}
        }
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            break (None, 1);
        }
        if started.elapsed() >= timeout {
            break (None, 2);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if end != 0 {
        reaper.kill_tree();
        let _ = reaper.child.wait();
    }
    reaper.finished = true;
    // The pipes close when the process (group) is gone. A straggler that holds one open must not hang us.
    let wait_until = Instant::now() + Duration::from_secs(2);
    for h in [&out_h, &err_h] {
        while !h.is_finished() && Instant::now() < wait_until {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let text = |b: &Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&b.lock().unwrap()).into_owned();
    let out = Out {
        code: status.and_then(|s| s.code()),
        stdout: text(&out_buf),
        stderr: text(&err_buf),
    };
    Ok(match end {
        0 => Ended::Done(out),
        1 => Ended::Cancelled,
        _ => Ended::TimedOut,
    })
}

/// Login checks are cheap and spare a logged-out Codex its long retry loop.
/// Success is remembered for a while.
static LOGGED_IN: Mutex<Vec<(PathBuf, Instant)>> = Mutex::new(Vec::new());
const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);

fn check_login(
    tool: Tool,
    exe: &Path,
    path_env: &OsString,
    use_cache: bool,
) -> Result<(), CliError> {
    if use_cache
        && LOGGED_IN
            .lock()
            .unwrap()
            .iter()
            .any(|(p, t)| p == exe && t.elapsed() < LOGIN_TTL)
    {
        return Ok(());
    }
    let args: Vec<String> = match tool {
        Tool::Codex => vec!["login".into(), "status".into()],
        Tool::Claude => vec!["auth".into(), "status".into()],
    };
    let wd = Workdir::new().map_err(|e| {
        CliError::Failed(tool, None, format!("couldn't make a temporary folder: {e}"))
    })?;
    // anything unexpected here (an old CLI without the subcommand, a hiccup) is not "signed out": the real run decides
    let Ok(Ended::Done(o)) = exec(
        exe,
        &args,
        wd.path(),
        None,
        path_env.clone(),
        Duration::from_secs(15),
        None,
    ) else {
        return Ok(());
    };
    let text = format!("{}\n{}", o.stdout, o.stderr).to_lowercase();
    let compact: String = text.split_whitespace().collect();
    let out = o.code != Some(0)
        && (text.contains("not logged in")
            || text.contains("not signed in")
            || compact.contains("\"loggedin\":false"));
    if out {
        return Err(CliError::NotLoggedIn(tool));
    }
    if o.code == Some(0) {
        let mut g = LOGGED_IN.lock().unwrap();
        g.retain(|(p, _)| p != exe);
        g.push((exe.to_path_buf(), Instant::now()));
    }
    Ok(())
}

/// Run the tool on a prompt and return its reply text. `timeout_secs` bounds the whole call.
pub fn run(
    spec: &CliSpec,
    system: Option<&str>,
    prompt: &str,
    timeout_secs: u64,
) -> anyhow::Result<String> {
    Ok(run_cancellable(
        spec,
        system,
        prompt,
        Duration::from_secs(timeout_secs.max(1)),
        None,
    )?)
}

/// Like [`run`], stopping (and killing the tool's processes) when `cancel` is set.
pub fn run_cancellable(
    spec: &CliSpec,
    system: Option<&str>,
    prompt: &str,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<String, CliError> {
    run_in(
        spec,
        system,
        prompt,
        timeout,
        cancel,
        &real_search_dirs(),
        true,
    )
}

fn run_in(
    spec: &CliSpec,
    system: Option<&str>,
    prompt: &str,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
    dirs: &[PathBuf],
    cached_login: bool,
) -> Result<String, CliError> {
    let tool = spec.tool;
    let exe = find_in(tool, &spec.path, dirs)
        .ok_or_else(|| CliError::NotInstalled(tool, spec.path.trim().to_string()))?;
    let path_env = tool_path(&exe, dirs);
    let started = Instant::now();
    check_login(tool, &exe, &path_env, cached_login)?;
    let wd = Workdir::new().map_err(|e| {
        CliError::Failed(tool, None, format!("couldn't make a temporary folder: {e}"))
    })?;
    let out_file = wd.path().join("last-message.txt");
    let stdin = stdin_text(tool, system, prompt);
    let mut skip: Vec<String> = Vec::new();
    loop {
        let args = command_args(tool, &spec.model, system, wd.path(), &out_file, &skip);
        let left = timeout
            .saturating_sub(started.elapsed())
            .max(Duration::from_secs(1));
        let ended = exec(
            &exe,
            &args,
            wd.path(),
            Some(stdin.clone()),
            path_env.clone(),
            left,
            cancel,
        )
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
                CliError::NotInstalled(tool, exe.to_string_lossy().into_owned())
            }
            _ => CliError::Failed(tool, None, e.to_string()),
        })?;
        let o = match ended {
            Ended::Cancelled => return Err(CliError::Cancelled(tool)),
            Ended::TimedOut => return Err(CliError::Timeout(tool, timeout.as_secs())),
            Ended::Done(o) => o,
        };
        if o.code != Some(0) {
            if let Some(flag) = rejected_flag(&o.stderr, &o.stdout) {
                let optional = match tool {
                    Tool::Codex => CODEX_OPTIONAL,
                    Tool::Claude => CLAUDE_OPTIONAL,
                };
                if optional.contains(&flag.as_str()) && !skip.contains(&flag) {
                    tracing::info!(tool = tool.id(), flag = %flag, "this version doesn't know the flag; running without it");
                    skip.push(flag);
                    let _ = std::fs::remove_file(&out_file);
                    continue;
                }
                return Err(CliError::TooOld(tool, flag));
            }
        }
        return interpret(tool, &o, &out_file);
    }
}

/// The flag a CLI complained about ("unknown option '--x'", "unexpected argument '--x' found").
fn rejected_flag(stderr: &str, stdout: &str) -> Option<String> {
    stderr.lines().chain(stdout.lines().take(3)).find_map(|l| {
        let low = l.to_lowercase();
        if !(low.contains("unknown option")
            || low.contains("unexpected argument")
            || low.contains("unrecognized option")
            || low.contains("unrecognized argument"))
        {
            return None;
        }
        let start = l.find("--")?;
        let flag: String = l[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        (flag.len() > 2).then_some(flag)
    })
}

/// Turn a finished process into the reply text or a classified error.
fn interpret(tool: Tool, o: &Out, out_file: &Path) -> Result<String, CliError> {
    let (reply, failure) = match tool {
        Tool::Codex => parse_codex(&o.stdout, std::fs::read_to_string(out_file).ok()),
        Tool::Claude => parse_claude(&o.stdout),
    };
    if let Some(msg) = failure {
        return Err(classify(tool, &msg, o.code));
    }
    if o.code != Some(0) {
        let detail = if o.stderr.trim().is_empty() {
            reply.clone().unwrap_or_default()
        } else {
            tail(&o.stderr)
        };
        return Err(classify(tool, &detail, o.code));
    }
    let text = clean_output(&reply.unwrap_or_default());
    if text.is_empty() {
        return Err(CliError::Empty(tool));
    }
    Ok(text)
}

/// (reply, failure message) from `codex exec --json` stdout plus the `-o` file.
fn parse_codex(stdout: &str, last_message: Option<String>) -> (Option<String>, Option<String>) {
    let mut agent: Option<String> = None;
    let mut failed: Option<String> = None;
    for l in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l.trim()) else {
            continue;
        };
        match v["type"].as_str() {
            Some("item.completed") if v["item"]["type"] == "agent_message" => {
                agent = v["item"]["text"].as_str().map(str::to_string)
            }
            // "error" events include harmless "Reconnecting… 2/5" notices; only a failed turn is a failure
            Some("turn.failed") => failed = v["error"]["message"].as_str().map(flatten_error),
            _ => {}
        }
    }
    let reply = last_message.filter(|m| !m.trim().is_empty()).or(agent);
    (reply, failed)
}

/// Codex wraps API errors as JSON inside the message; show the inner text.
fn flatten_error(m: &str) -> String {
    serde_json::from_str::<serde_json::Value>(m)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or(v["message"].as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| m.to_string())
}

/// (reply, failure message) from `claude -p --output-format json` stdout.
fn parse_claude(stdout: &str) -> (Option<String>, Option<String>) {
    let result = stdout
        .lines()
        .rev()
        .chain(std::iter::once(stdout))
        .find_map(|l| {
            serde_json::from_str::<serde_json::Value>(l.trim())
                .ok()
                .filter(|v| v["type"] == "result")
        });
    let Some(v) = result else { return (None, None) };
    let text = v["result"].as_str().unwrap_or_default().to_string();
    if v["is_error"].as_bool().unwrap_or(false) {
        return (
            None,
            Some(if text.is_empty() {
                v["subtype"].as_str().unwrap_or("error").to_string()
            } else {
                text
            }),
        );
    }
    (Some(text), None)
}

/// Map a failure message to the right user-facing error.
pub fn classify(tool: Tool, detail: &str, code: Option<i32>) -> CliError {
    let low = detail.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| low.contains(n));
    let short: String = detail
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect();
    if has(&[
        "not logged in",
        "not signed in",
        "please run /login",
        "run codex login",
        "codex login",
        "401 unauthorized",
        "missing bearer",
        "invalid authentication",
        "authentication_error",
        "invalid x-api-key",
        "invalid api key",
        "token expired",
        "refresh token",
    ]) {
        return CliError::NotLoggedIn(tool);
    }
    if has(&[
        "usage limit",
        "rate limit",
        "rate_limit",
        "limit reached",
        "hit your limit",
        "hit your usage",
        "out of credits",
        "insufficient_quota",
        "quota",
        "too many requests",
        "status 429",
        "(429)",
        " 429 ",
    ]) {
        return CliError::UsageLimit(tool, short);
    }
    if has(&[
        "model_not_found",
        "not supported when using",
        "unrecognized_model",
        "model not found",
        "invalid model",
        "unknown model",
        "issue with the selected model",
        "selected model",
    ]) {
        return CliError::BadModel(tool, short);
    }
    CliError::Failed(tool, code, short)
}

/// Last few lines of stderr (the start is usually banner noise).
fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(4)..].join(" ")
}

/// Drop a reasoning block and a code fence wrapped around the whole reply.
pub fn clean_output(s: &str) -> String {
    let t = crate::summary::strip_think(s).trim();
    if let Some(inner) = t.strip_prefix("```").and_then(|r| r.strip_suffix("```")) {
        // ```json\n{...}\n```
        let body = inner
            .split_once('\n')
            .map(|(first, rest)| {
                if first.trim().chars().all(|c| c.is_ascii_alphanumeric()) {
                    rest
                } else {
                    inner
                }
            })
            .unwrap_or(inner);
        if !body.contains("```") {
            return body.trim().to_string();
        }
    }
    t.to_string()
}

/// One tiny request to check that the tool is installed, signed in and answers.
/// Returns the latency in ms.
pub fn ping(spec: &CliSpec) -> anyhow::Result<u128> {
    let dirs = real_search_dirs();
    let t0 = Instant::now();
    run_in(
        spec,
        None,
        "Reply with OK.",
        Duration::from_secs(90),
        None,
        &dirs,
        false,
    )?;
    Ok(t0.elapsed().as_millis())
}

/// What the Settings page shows next to the tool: where it is and its version.
pub fn describe(d: &Detected) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let p = d.path.to_string_lossy();
    let p = if !home.is_empty() && p.starts_with(&home) {
        format!("~{}", &p[home.len()..])
    } else {
        p.into_owned()
    };
    match &d.version {
        Some(v) => format!("{v} · {p}"),
        None => p,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn spec(tool: Tool, path: &Path) -> CliSpec {
        CliSpec {
            tool,
            path: path.to_string_lossy().into_owned(),
            model: String::new(),
        }
    }

    /// What a fake script needs on its PATH (cat, sleep, ls…).
    fn sys() -> Vec<PathBuf> {
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
    }

    fn go(spec: &CliSpec, prompt: &str, secs: u64) -> Result<String, CliError> {
        run_in(
            spec,
            None,
            prompt,
            Duration::from_secs(secs),
            None,
            &sys(),
            false,
        )
    }

    /// A fake `codex`: logged in; `exec` echoes its stdin as the last message, into the `-o` file.
    const FAKE_CODEX_OK: &str = r#"
if [ "$1" = login ]; then echo "Logged in using ChatGPT"; exit 0; fi
out=""; prev=""
for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; prev="$a"; done
input=$(cat)
printf '%s' "echo: $input" > "$out"
echo '{"type":"thread.started","thread_id":"t"}'
echo '{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"an earlier preamble"}}'
echo '{"type":"turn.completed"}'
"#;

    #[test]
    fn codex_command_is_locked_down() {
        let a = command_args(
            Tool::Codex,
            "gpt-x",
            None,
            Path::new("/tmp/w"),
            Path::new("/tmp/w/o.txt"),
            &[],
        );
        let j = a.join(" ");
        assert_eq!(a[0], "exec");
        assert!(j.contains("-s read-only"), "{j}");
        assert!(j.contains("approval_policy=\"never\""), "{j}");
        assert!(j.contains("--ignore-user-config"), "{j}");
        assert!(j.contains("-C /tmp/w"), "{j}");
        assert!(j.contains("features.shell_tool=false"), "{j}");
        assert!(j.contains("-m gpt-x"), "{j}");
        assert!(j.contains("--json"), "{j}");
        assert_eq!(a.last().unwrap(), "-", "prompt is read from stdin");
        assert!(!j.contains("danger"), "{j}");
        // optional flags can be dropped, required ones can't
        let a = command_args(
            Tool::Codex,
            "",
            None,
            Path::new("/w"),
            Path::new("/w/o"),
            &["--ephemeral".into(), "--ignore-user-config".into()],
        );
        assert!(!a.contains(&"--ephemeral".to_string()));
        assert!(a.contains(&"--ignore-user-config".to_string()));
        assert!(
            !a.contains(&"-m".to_string()),
            "no model flag when none is chosen"
        );
    }

    #[test]
    fn claude_command_has_no_tools() {
        let a = command_args(
            Tool::Claude,
            "sonnet",
            Some("Be brief."),
            Path::new("/w"),
            Path::new("/w/o"),
            &[],
        );
        let i = a.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(a[i + 1], "", "an empty tool list");
        assert!(a.contains(&"-p".to_string()));
        assert!(a.contains(&"--strict-mcp-config".to_string()));
        let j = a.iter().position(|x| x == "--setting-sources").unwrap();
        assert_eq!(a[j + 1], "");
        let k = a.iter().position(|x| x == "--system-prompt").unwrap();
        assert!(a[k + 1].starts_with("Be brief."));
        assert!(a.windows(2).any(|w| w == ["--model", "sonnet"]));
        assert!(!a.iter().any(|x| x.contains("dangerously")));
    }

    #[test]
    fn codex_reply_comes_from_the_last_message_file() {
        let d = tempfile::tempdir().unwrap();
        let bin = script(d.path(), "codex", FAKE_CODEX_OK);
        let r = go(&spec(Tool::Codex, &bin), "hello transcript", 20).unwrap();
        assert_eq!(
            r, "echo: hello transcript",
            "the prompt arrives on stdin and the -o file wins over earlier messages"
        );
    }

    #[test]
    fn codex_gets_the_system_prompt_ahead_of_the_task() {
        let d = tempfile::tempdir().unwrap();
        let bin = script(d.path(), "codex", FAKE_CODEX_OK);
        let r = run_in(
            &spec(Tool::Codex, &bin),
            Some("SYS"),
            "TASK",
            Duration::from_secs(20),
            None,
            &sys(),
            false,
        )
        .unwrap();
        assert!(
            r.contains("SYS") && r.contains("TASK") && r.find("SYS") < r.find("TASK"),
            "{r}"
        );
    }

    #[test]
    fn runs_in_an_empty_directory_that_is_removed_afterwards() {
        let d = tempfile::tempdir().unwrap();
        let bin = script(
            d.path(),
            "codex",
            r#"
if [ "$1" = login ]; then exit 0; fi
out=""; prev=""
for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; prev="$a"; done
cat >/dev/null
n=$(ls -A | wc -l | tr -d ' ')
printf 'cwd=%s entries=%s' "$PWD" "$n" > "$out"
"#,
        );
        let r = go(&spec(Tool::Codex, &bin), "x", 20).unwrap();
        let cwd = r
            .split_whitespace()
            .next()
            .unwrap()
            .trim_start_matches("cwd=");
        assert!(r.ends_with("entries=0"), "{r}");
        assert!(cwd.contains("stenly-cli-llm-"), "{cwd}");
        assert!(!Path::new(cwd).exists(), "temp dir is cleaned up");
    }

    #[test]
    fn claude_json_result_is_returned() {
        let d = tempfile::tempdir().unwrap();
        let bin = script(
            d.path(),
            "claude",
            r#"
if [ "$1" = auth ]; then echo '{"loggedIn": true}'; exit 0; fi
input=$(cat)
printf '{"type":"result","is_error":false,"result":"```json\\n{\\"summary\\":\\"%s\\"}\\n```"}\n' "$input"
"#,
        );
        let r = go(&spec(Tool::Claude, &bin), "hi", 20).unwrap();
        assert_eq!(r, r#"{"summary":"hi"}"#, "code fence removed");
        assert_eq!(crate::summary::parse_summary(&r).unwrap().summary, "hi");
    }

    #[test]
    fn claude_not_signed_in_is_caught_before_running() {
        let d = tempfile::tempdir().unwrap();
        let bin = script(
            d.path(),
            "claude",
            r#"if [ "$1" = auth ]; then echo '{"loggedIn": false}'; exit 1; fi; echo should-not-run > ran.txt"#,
        );
        let e = go(&spec(Tool::Claude, &bin), "hi", 20).unwrap_err();
        assert_eq!(e, CliError::NotLoggedIn(Tool::Claude));
        assert!(e.to_string().contains("/login"), "{e}");
    }

    #[test]
    fn claude_error_result_maps_to_a_clear_error() {
        let d = tempfile::tempdir().unwrap();
        let mk = |msg: &str| {
            script(
                d.path(),
                "claude",
                &format!(
                    "if [ \"$1\" = auth ]; then exit 0; fi\ncat >/dev/null\necho '{{\"type\":\"result\",\"is_error\":true,\"result\":\"{msg}\"}}'\nexit 1"
                ),
            )
        };
        let bin = mk("Not logged in · Please run /login");
        assert_eq!(
            go(&spec(Tool::Claude, &bin), "x", 20).unwrap_err(),
            CliError::NotLoggedIn(Tool::Claude)
        );
        let bin = mk("5-hour limit reached ∙ resets 3pm");
        let e = go(&spec(Tool::Claude, &bin), "x", 20).unwrap_err();
        assert!(
            matches!(e, CliError::UsageLimit(Tool::Claude, ref d) if d.contains("resets 3pm")),
            "{e:?}"
        );
        assert!(e.to_string().contains("usage limit"), "{e}");
    }

    #[test]
    fn codex_failures_map_to_clear_errors() {
        let d = tempfile::tempdir().unwrap();
        let mk = |ev: &str| {
            script(
                d.path(),
                "codex",
                &format!(
                    "if [ \"$1\" = login ]; then exit 0; fi\ncat >/dev/null\ncat <<'EOF'\n{ev}\nEOF\nexit 1"
                ),
            )
        };
        let bin = mk(
            r#"{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized: Missing bearer or basic authentication in header"}}"#,
        );
        assert_eq!(
            go(&spec(Tool::Codex, &bin), "x", 20).unwrap_err(),
            CliError::NotLoggedIn(Tool::Codex)
        );
        let bin = mk(
            r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit. Try again at 4:15 PM."}}"#,
        );
        assert!(matches!(
            go(&spec(Tool::Codex, &bin), "x", 20).unwrap_err(),
            CliError::UsageLimit(Tool::Codex, _)
        ));
        let bin = mk(
            r#"{"type":"turn.failed","error":{"message":"{\"type\":\"error\",\"status\":400,\"error\":{\"message\":\"The 'zzz' model is not supported when using Codex with a ChatGPT account.\",\"code\":\"model_not_found\"}}"}}"#,
        );
        let e = go(&spec(Tool::Codex, &bin), "x", 20).unwrap_err();
        assert!(
            matches!(e, CliError::BadModel(Tool::Codex, ref d) if d.contains("'zzz'")),
            "{e:?}"
        );
        let bin = mk(r#"{"type":"turn.failed","error":{"message":"something odd happened"}}"#);
        let e = go(&spec(Tool::Codex, &bin), "x", 20).unwrap_err();
        assert_eq!(
            e,
            CliError::Failed(Tool::Codex, Some(1), "something odd happened".into())
        );
        assert!(e.to_string().contains("exit 1"));
    }

    #[test]
    fn non_zero_exit_without_json_uses_stderr() {
        let d = tempfile::tempdir().unwrap();
        let bin = script(
            d.path(),
            "codex",
            "if [ \"$1\" = login ]; then exit 0; fi\ncat >/dev/null\necho 'boom: disk on fire' >&2\nexit 3",
        );
        assert_eq!(
            go(&spec(Tool::Codex, &bin), "x", 20).unwrap_err(),
            CliError::Failed(Tool::Codex, Some(3), "boom: disk on fire".into())
        );
    }

    #[test]
    fn empty_reply_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let bin = script(
            d.path(),
            "codex",
            "if [ \"$1\" = login ]; then exit 0; fi\ncat >/dev/null",
        );
        assert_eq!(
            go(&spec(Tool::Codex, &bin), "x", 20).unwrap_err(),
            CliError::Empty(Tool::Codex)
        );
    }

    #[test]
    fn not_installed() {
        let d = tempfile::tempdir().unwrap();
        let e = run_in(
            &CliSpec {
                tool: Tool::Codex,
                path: String::new(),
                model: String::new(),
            },
            None,
            "x",
            Duration::from_secs(5),
            None,
            &[d.path().to_path_buf()],
            false,
        )
        .unwrap_err();
        assert_eq!(e, CliError::NotInstalled(Tool::Codex, String::new()));
        assert!(e.to_string().contains("isn't installed"));
        let e = go(
            &CliSpec {
                tool: Tool::Claude,
                path: "/nonexistent/claude".into(),
                model: String::new(),
            },
            "x",
            5,
        )
        .unwrap_err();
        assert!(
            matches!(e, CliError::NotInstalled(Tool::Claude, ref p) if p == "/nonexistent/claude")
        );
    }

    #[test]
    fn timeout_kills_the_whole_process_tree() {
        let d = tempfile::tempdir().unwrap();
        let pidfile = d.path().join("grandchild.pid");
        let bin = script(
            d.path(),
            "codex",
            &format!(
                "if [ \"$1\" = login ]; then exit 0; fi\ncat >/dev/null\nsleep 300 &\necho $! > {}\nwait",
                pidfile.display()
            ),
        );
        let t0 = Instant::now();
        let e = run_in(
            &spec(Tool::Codex, &bin),
            None,
            "x",
            Duration::from_millis(1500),
            None,
            &sys(),
            false,
        )
        .unwrap_err();
        assert!(matches!(e, CliError::Timeout(Tool::Codex, _)), "{e:?}");
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "returned promptly: {:?}",
            t0.elapsed()
        );
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            unsafe { libc::kill(pid, 0) } != 0,
            "the grandchild `sleep` is gone"
        );
    }

    #[test]
    fn cancel_stops_the_tool() {
        let d = tempfile::tempdir().unwrap();
        let pidfile = d.path().join("child.pid");
        let bin = script(
            d.path(),
            "codex",
            &format!(
                "if [ \"$1\" = login ]; then exit 0; fi\necho $$ > {}\nsleep 300",
                pidfile.display()
            ),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            c2.store(true, Ordering::Relaxed);
        });
        let e = run_in(
            &spec(Tool::Codex, &bin),
            None,
            "x",
            Duration::from_secs(60),
            Some(&cancel),
            &sys(),
            false,
        )
        .unwrap_err();
        assert_eq!(e, CliError::Cancelled(Tool::Codex));
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(unsafe { libc::kill(pid, 0) } != 0, "process is gone");
    }

    #[test]
    fn an_unknown_optional_flag_is_dropped_and_retried() {
        let d = tempfile::tempdir().unwrap();
        // an old codex: rejects --ephemeral
        let bin = script(
            d.path(),
            "codex",
            r#"
if [ "$1" = login ]; then exit 0; fi
for a in "$@"; do if [ "$a" = "--ephemeral" ]; then echo "error: unexpected argument '--ephemeral' found" >&2; exit 2; fi; done
out=""; prev=""
for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; prev="$a"; done
cat >/dev/null; printf ok > "$out"
"#,
        );
        assert_eq!(go(&spec(Tool::Codex, &bin), "x", 20).unwrap(), "ok");
    }

    #[test]
    fn an_unknown_required_flag_says_to_update() {
        let d = tempfile::tempdir().unwrap();
        let bin = script(
            d.path(),
            "claude",
            "if [ \"$1\" = auth ]; then exit 0; fi\necho \"error: unknown option '--strict-mcp-config'\" >&2\nexit 1",
        );
        let e = go(&spec(Tool::Claude, &bin), "x", 20).unwrap_err();
        assert_eq!(
            e,
            CliError::TooOld(Tool::Claude, "--strict-mcp-config".into())
        );
        assert!(e.to_string().contains("Update"));
    }

    #[test]
    fn finds_tools_in_the_usual_places_without_a_path() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        for rel in [
            ".local/bin",
            ".claude/local",
            "Library/Application Support/fnm/node-versions/v22.1.0/installation/bin",
            ".nvm/versions/node/v20.0.0/bin",
        ] {
            std::fs::create_dir_all(h.join(rel)).unwrap();
        }
        let dirs = search_dirs(Some(h), Some(std::ffi::OsStr::new("/usr/bin:/bin")));
        assert!(dirs.contains(&h.join(".local/bin")) && dirs.contains(&h.join(".claude/local")));
        assert!(dirs.contains(
            &h.join("Library/Application Support/fnm/node-versions/v22.1.0/installation/bin")
        ));
        assert!(dirs.contains(&h.join(".nvm/versions/node/v20.0.0/bin")));
        assert_eq!(find_in(Tool::Claude, "", &dirs), None);
        let claude = script(&h.join(".claude/local"), "claude", "exit 0");
        let codex = script(&h.join(".nvm/versions/node/v20.0.0/bin"), "codex", "exit 0");
        assert_eq!(find_in(Tool::Claude, "", &dirs), Some(claude));
        assert_eq!(find_in(Tool::Codex, "", &dirs), Some(codex.clone()));
        // a non-executable file is not a tool
        let plain = h.join(".local/bin/codex");
        std::fs::write(&plain, "x").unwrap();
        assert_ne!(find_in(Tool::Codex, "", &dirs), Some(plain));
        // an explicit path wins and must exist
        assert_eq!(
            find_in(Tool::Codex, &codex.to_string_lossy(), &[]),
            Some(codex)
        );
        assert_eq!(find_in(Tool::Codex, "/nope/codex", &dirs), None);
    }

    #[test]
    fn the_childs_path_includes_the_tools_directory() {
        let p = tool_path(Path::new("/opt/x/bin/codex"), &[PathBuf::from("/usr/bin")]);
        let parts: Vec<PathBuf> = std::env::split_paths(&p).collect();
        assert_eq!(parts[0], PathBuf::from("/opt/x/bin"));
        assert!(parts.contains(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn classify_covers_the_usual_messages() {
        let c = |m: &str| classify(Tool::Claude, m, Some(1));
        assert_eq!(
            c("Invalid API key · Please run /login"),
            CliError::NotLoggedIn(Tool::Claude)
        );
        assert!(matches!(
            c("Claude AI usage limit reached|1761000000"),
            CliError::UsageLimit(..)
        ));
        assert!(matches!(
            c("429 Too Many Requests"),
            CliError::UsageLimit(..)
        ));
        assert!(matches!(
            c("There's an issue with the selected model (bogus). It may not exist"),
            CliError::BadModel(..)
        ));
        assert!(matches!(
            c("network down"),
            CliError::Failed(Tool::Claude, Some(1), _)
        ));
        assert!(
            !CliError::Timeout(Tool::Codex, 5).is_permanent()
                && CliError::NotLoggedIn(Tool::Codex).is_permanent()
        );
    }

    #[test]
    fn clean_output_strips_think_and_fences() {
        assert_eq!(clean_output("<think>hmm</think>\n  hi  "), "hi");
        assert_eq!(clean_output("```\nplain\n```"), "plain");
        assert_eq!(clean_output("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(
            clean_output("keep ```inner``` text"),
            "keep ```inner``` text"
        );
    }
}
