//! Opt-in release checks. No destination is guessed for development builds.
use crate::controller::Controller;
#[cfg(not(target_os = "macos"))]
use gpui::AppContext;
use gpui::{App, AsyncApp, Context, Entity};

pub fn init(ctrl: Entity<Controller>, cx: &mut App) {
    let mut enabled = ctrl.read(cx).config.app.check_updates;
    let native = stenly_platform::updater::start(enabled);
    ctrl.update(cx, |c, cx| {
        c.update_status = if cfg!(target_os = "macos") {
            match native {
                1 => "Release notifications ready. In-app installation is not enabled.",
                -1 => "Updater could not start. Your installed build is unchanged.",
                _ => "Updates are not configured in this build.",
            }
            .into()
        } else if option_env!("STENLY_RELEASE_REPOSITORY").is_some() {
            "Release notifications ready; checks are opt-in.".into()
        } else {
            "Release notifications are not configured in this build.".into()
        };
        cx.notify();
    });
    let native_ready = native == 1;
    cx.observe(&ctrl, move |ctrl, cx| {
        let value = ctrl.read(cx).config.app.check_updates;
        if value != enabled {
            enabled = value;
            let ctrl = ctrl.clone();
            cx.spawn(async move |cx: &mut AsyncApp| {
                if !native_ready && cfg!(target_os = "macos") {
                    return;
                }
                stenly_platform::updater::set_enabled(value);
                if value {
                    let _ = cx.update(|cx| check(ctrl, cx));
                }
            })
            .detach();
        }
    })
    .detach();
    #[cfg(target_os = "macos")]
    {
        let ctrl = ctrl.clone();
        cx.spawn(async move |cx: &mut AsyncApp| loop {
            cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
            let available = stenly_platform::updater::available();
            let _ = ctrl.update(cx, |c, cx| {
                if !c.config.app.check_updates { c.update_download = None; return; }
                if let Some((version, url)) = available {
                    let message = format!("Version {version} is available. In-app installation is not enabled; download when convenient.");
                    if c.update_status != message || c.update_download.as_ref() != Some(&url) { c.update_status = message; c.update_download = Some(url); cx.notify(); }
                }
            });
        }).detach();
    }
    // Avoid presenting native update UI during App/Window borrows.
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut delay = std::time::Duration::from_secs(30);
        loop {
            cx.background_executor().timer(delay).await;
            delay = std::time::Duration::from_secs(6 * 60 * 60);
            let _ = ctrl.update(cx, |c, cx| {
                if c.config.app.check_updates && !stenly_core::update::busy() {
                    request(c, cx);
                }
            });
        }
    })
    .detach();
}

pub fn check(ctrl: Entity<Controller>, cx: &mut App) {
    cx.defer(move |cx| ctrl.update(cx, request));
}

fn request(c: &mut Controller, cx: &mut Context<Controller>) {
    if !c.config.app.check_updates {
        c.update_status = "Turn on update checks to contact the release server.".into();
        cx.notify();
        return;
    }
    if stenly_core::update::busy() {
        c.update_status = "Recording or dictation is active. Check again when idle.".into();
        cx.notify();
        return;
    }
    #[cfg(target_os = "macos")]
    {
        // The native checker can display UI, so release the controller/App borrow.
        cx.spawn(async |_, _| stenly_platform::updater::check())
            .detach();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let Some(repo) = option_env!("STENLY_RELEASE_REPOSITORY") else {
            return;
        };
        c.update_status = "Checking GitHub Releases…".into();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { release(repo) }).await;
            let _ = this.update(cx, |c, cx| {
                if !c.config.app.check_updates { return; }
                match result {
                    Ok(Some((tag, url))) => { c.update_status = format!("Version {tag} is available. Download and install when convenient."); c.update_download = Some(url); }
                    Ok(None) => { c.update_status = "You are on the latest stable release.".into(); c.update_download = None; }
                    Err(_) => { c.update_status = "Could not check releases. Your installed build is unchanged; try again later.".into(); }
                }
                cx.notify();
            });
        }).detach();
    }
    cx.notify();
}

#[cfg(not(target_os = "macos"))]
fn release(repo: &str) -> anyhow::Result<Option<(String, String)>> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let response: serde_json::Value = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build()
        .new_agent()
        .get(&url)
        .header("User-Agent", concat!("Stenly/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_json()?;
    if response["draft"].as_bool().unwrap_or(true)
        || response["prerelease"].as_bool().unwrap_or(true)
    {
        return Ok(None);
    }
    let tag = response["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing tag"))?;
    let candidate = semver::Version::parse(tag.trim_start_matches('v'))?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    if candidate <= current {
        return Ok(None);
    }
    let download = response["html_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing release URL"))?;
    anyhow::ensure!(
        download.starts_with(&format!("https://github.com/{repo}/releases/tag/")),
        "unexpected release URL"
    );
    Ok(Some((tag.to_string(), download.to_string())))
}
