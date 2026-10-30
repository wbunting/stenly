//! API keys kept out of config.toml. On macOS they live in the login Keychain
//! (via `/usr/bin/security`, so no entitlements or extra crates); elsewhere in
//! 0600 files next to the config.

const SERVICE: &str = "ai.stenly.llm-api";
const ACCOUNT: &str = "default";
/// Destination credentials (Notion tokens, Slack/webhook URLs), one Keychain
/// item per destination id.
const DEST_SERVICE: &str = "ai.stenly.destination";

pub fn llm_api_key() -> Option<String> {
    get(SERVICE, ACCOUNT)
}

pub fn set_llm_api_key(key: &str) -> anyhow::Result<()> {
    if key.trim().is_empty() { delete(SERVICE, ACCOUNT) } else { set(SERVICE, ACCOUNT, key.trim(), "Stenly summary API key") }
}

/// A destination's stored credential. `account` is e.g. `notion-crm` or
/// `my-hook.signing`.
pub fn destination_secret(account: &str) -> Option<String> {
    get(DEST_SERVICE, account)
}

/// Store (or with "" remove) a destination credential.
pub fn set_destination_secret(account: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(valid_account(account), "invalid secret name");
    if value.trim().is_empty() { delete(DEST_SERVICE, account) } else { set(DEST_SERVICE, account, value.trim(), &format!("Stenly destination ({account})")) }
}

/// Context sources (Notion token, an MCP server's bearer key or OAuth tokens),
/// one Keychain item per source: `notion`, `mcp.<id>`, `mcp.<id>.oauth`.
const CONTEXT_SERVICE: &str = "ai.stenly.context";

pub fn context_secret(account: &str) -> Option<String> {
    get(CONTEXT_SERVICE, account)
}

/// Store (or with "" remove) a context-source credential.
pub fn set_context_secret(account: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(valid_account(account), "invalid secret name");
    if value.trim().is_empty() { delete(CONTEXT_SERVICE, account) } else { set(CONTEXT_SERVICE, account, value.trim(), &format!("Stenly context ({account})")) }
}

/// ChatGPT sign-in (OAuth tokens as JSON, see `chatgpt`). One item per
/// profile: an isolated profile (`STENLY_CONFIG`) never reads or replaces the
/// sign-in of the user's everyday app.
const CHATGPT_SERVICE: &str = "ai.stenly.chatgpt";

fn chatgpt_account() -> String {
    match std::env::var("STENLY_CONFIG") {
        Ok(p) if !p.trim().is_empty() => {
            use sha2::Digest;
            let h = sha2::Sha256::digest(crate::config::config_path().to_string_lossy().as_bytes());
            format!("profile-{}", h.iter().take(6).map(|b| format!("{b:02x}")).collect::<String>())
        }
        _ => ACCOUNT.to_string(),
    }
}

pub fn chatgpt_tokens() -> Option<String> {
    get(CHATGPT_SERVICE, &chatgpt_account())
}

/// Store (or with "" remove) the ChatGPT sign-in.
pub fn set_chatgpt_tokens(json: &str) -> anyhow::Result<()> {
    let account = chatgpt_account();
    if json.trim().is_empty() { delete(CHATGPT_SERVICE, &account) } else { set(CHATGPT_SERVICE, &account, json.trim(), "Stenly ChatGPT sign-in") }
}

/// Licence and trial state (see `license`). One item per profile and name
/// (`license`, `trial`), so an isolated profile never touches the licence of
/// the user's everyday app. The trial counter lives here too so a config reset
/// doesn't hand out a fresh trial.
const LICENSE_SERVICE: &str = "ai.stenly.license";

fn license_account(name: &str) -> String {
    let profile = match std::env::var("STENLY_CONFIG") {
        Ok(p) if !p.trim().is_empty() => {
            use sha2::Digest;
            let h = sha2::Sha256::digest(crate::config::config_path().to_string_lossy().as_bytes());
            format!("profile-{}", h.iter().take(6).map(|b| format!("{b:02x}")).collect::<String>())
        }
        _ => "default".to_string(),
    };
    format!("{name}.{profile}")
}

pub fn license_item(name: &str) -> Option<String> {
    get(LICENSE_SERVICE, &license_account(name))
}

/// Store (or with "" remove) a licence item.
pub fn set_license_item(name: &str, value: &str) -> anyhow::Result<()> {
    let account = license_account(name);
    if value.trim().is_empty() { delete(LICENSE_SERVICE, &account) } else { set(LICENSE_SERVICE, &account, value.trim(), "Stenly licence") }
}

fn valid_account(a: &str) -> bool {
    !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(target_os = "macos")]
fn get(service: &str, account: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/security").args(["find-generic-password", "-s", service, "-a", account, "-w"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn set(service: &str, account: &str, key: &str, label: &str) -> anyhow::Result<()> {
    // -U updates an existing item in place.
    let st = std::process::Command::new("/usr/bin/security")
        .args(["add-generic-password", "-U", "-s", service, "-a", account, "-l", label, "-w", key])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    anyhow::ensure!(st.success(), "couldn't save the key to the Keychain");
    Ok(())
}

#[cfg(target_os = "macos")]
fn delete(service: &str, account: &str) -> anyhow::Result<()> {
    let _ = std::process::Command::new("/usr/bin/security").args(["delete-generic-password", "-s", service, "-a", account]).output();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn file(service: &str, account: &str) -> std::path::PathBuf {
    let cfg = crate::config::config_path();
    match service {
        SERVICE => cfg.with_file_name("api-key"),
        CONTEXT_SERVICE => cfg.with_file_name("secrets").join(format!("context.{account}")),
        CHATGPT_SERVICE => cfg.with_file_name("secrets").join("chatgpt"),
        _ => cfg.with_file_name("secrets").join(account),
    }
}

#[cfg(not(target_os = "macos"))]
fn get(service: &str, account: &str) -> Option<String> {
    std::fs::read_to_string(file(service, account)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(not(target_os = "macos"))]
fn set(service: &str, account: &str, key: &str, _label: &str) -> anyhow::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let path = file(service, account);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    o.mode(0o600);
    o.open(path)?.write_all(key.as_bytes())?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn delete(service: &str, account: &str) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(file(service, account));
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn account_names() {
        assert!(super::valid_account("notion-crm.signing"));
        assert!(!super::valid_account("a b"));
        assert!(!super::valid_account(""));
        assert!(!super::valid_account("../x"));
    }
}
