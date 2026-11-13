//! Bring one of our windows to the front on the user's current workspace.
//!
//! Wayland compositors only honour an xdg-activation request that carries a
//! token from a recent user interaction with the requesting app. A click on
//! our tray icon happens in the panel (Waybar, …), not in stenly, so
//! `activate_window` is rejected and the window opens or stays underneath
//! whatever the user is looking at. Where the compositor offers an IPC to do
//! this on the app's behalf, use it. Only Hyprland is handled so far; other
//! desktops keep the plain activation request.

use std::process::Command;
use std::time::{Duration, Instant};

/// How long to wait for a just-opened window to be mapped.
const MAP_TIMEOUT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(50);

/// Move our window titled `title` to the active workspace, focus it and put it
/// on top. Runs in the background; does nothing outside Hyprland.
pub fn raise_window(title: &str) {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    // Note the user's workspace now: the window's own activation request,
    // if the compositor grants it, switches to the window's workspace instead.
    let target = match hyprland::active_workspace() {
        Ok(id) => id,
        Err(e) => {
            tracing::debug!(error = %e, "couldn't read the active Hyprland workspace");
            return;
        }
    };
    let title = title.to_string();
    let pid = std::process::id();
    std::thread::Builder::new()
        .name("raise-window".into())
        .spawn(move || {
            if let Err(e) = hyprland::raise(pid, &title, target) {
                tracing::debug!(error = %e, "couldn't raise window via Hyprland");
            }
        })
        .ok();
}

mod hyprland {
    use super::*;
    use anyhow::{Context, Result, bail};

    fn hyprctl(args: &[&str]) -> Result<String> {
        let out = Command::new("hyprctl")
            .args(args)
            .output()
            .context("running hyprctl")?;
        if !out.status.success() {
            bail!(
                "hyprctl {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn json(args: &[&str]) -> Result<serde_json::Value> {
        let mut all = vec!["-j"];
        all.extend_from_slice(args);
        serde_json::from_str(&hyprctl(&all)?).context("parsing hyprctl output")
    }

    /// The address and workspace id of our mapped window with this title.
    pub(super) fn find(
        clients: &serde_json::Value,
        pid: u32,
        title: &str,
    ) -> Option<(String, i64)> {
        clients.as_array()?.iter().find_map(|c| {
            let ours = c["pid"].as_u64() == Some(pid as u64)
                && c["title"].as_str() == Some(title)
                && c["mapped"].as_bool() != Some(false);
            ours.then(|| {
                Some((
                    c["address"].as_str()?.to_string(),
                    c["workspace"]["id"].as_i64()?,
                ))
            })?
        })
    }

    /// Hyprland ≥ 0.55 with a Lua config takes Lua dispatcher expressions;
    /// older (hyprlang) configs take the classic `dispatcher args` form.
    /// Neither understands the other, so try Lua first and fall back.
    pub(super) fn dispatch(lua: &str, legacy: &[&str]) -> Result<()> {
        let ok = |out: &str| out == "ok" || out.is_empty();
        if let Ok(out) = hyprctl(&["dispatch", lua])
            && ok(&out)
        {
            return Ok(());
        }
        let mut args = vec!["dispatch"];
        args.extend_from_slice(legacy);
        let out = hyprctl(&args)?;
        if !ok(&out) {
            bail!("hyprctl dispatch {legacy:?}: {out}");
        }
        Ok(())
    }

    pub(super) fn active_workspace() -> Result<i64> {
        json(&["activeworkspace"])?["id"]
            .as_i64()
            .context("no active workspace")
    }

    pub(super) fn raise(pid: u32, title: &str, active_id: i64) -> Result<()> {
        // A window opened a moment ago may not be mapped yet.
        let start = Instant::now();
        let (address, workspace) = loop {
            if let Some(found) = find(&json(&["clients"])?, pid, title) {
                break found;
            }
            if start.elapsed() > MAP_TIMEOUT {
                bail!("window {title:?} not found");
            }
            std::thread::sleep(POLL);
        };
        let target = format!("address:{address}");

        if workspace != active_id {
            let ws = active_id.to_string();
            dispatch(
                &format!(
                    "hl.dsp.window.move({{ workspace = \"{ws}\", window = \"{target}\", follow = false }})"
                ),
                &["movetoworkspacesilent", &format!("{ws},{target}")],
            )?;
        }
        dispatch(
            &format!("hl.dsp.focus({{ window = \"{target}\" }})"),
            &["focuswindow", &target],
        )?;
        // Focus doesn't always restack floating windows; ignore failures.
        let _ = dispatch(
            &format!("hl.dsp.window.alter_zorder({{ window = \"{target}\", mode = \"top\" }})"),
            &["alterzorder", &format!("top,{target}")],
        );
        tracing::info!(
            address,
            from = workspace,
            to = active_id,
            "raised window via Hyprland"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::hyprland::find;

    #[test]
    fn finds_only_our_mapped_window_by_title() {
        let clients = serde_json::json!([
            { "pid": 7, "title": "Stenly", "mapped": true, "address": "0xother", "workspace": { "id": 1 } },
            { "pid": 42, "title": "Recording", "mapped": true, "address": "0xpill", "workspace": { "id": 1 } },
            { "pid": 42, "title": "Stenly", "mapped": false, "address": "0xunmapped", "workspace": { "id": 2 } },
            { "pid": 42, "title": "Stenly", "mapped": true, "address": "0xmain", "workspace": { "id": 3 } },
        ]);
        assert_eq!(find(&clients, 42, "Stenly"), Some(("0xmain".into(), 3)));
        assert_eq!(find(&clients, 42, "Missing"), None);
        assert_eq!(find(&serde_json::json!({}), 42, "Stenly"), None);
    }
}
