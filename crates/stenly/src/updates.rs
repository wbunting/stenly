//! Release notifications. Notify-only: nothing is ever downloaded or installed.
//!
//! Two sources, one choice made at launch:
//! * the native Sparkle probe (macOS builds that bundle a feed; `start() == 1`), or
//! * GitHub Releases for the build-time `STENLY_RELEASE_REPOSITORY` (everything
//!   else), the same code path on macOS and Linux.
//!
//! Source/dev builds have neither and never contact anything.
use crate::controller::Controller;
use gpui::{App, AppContext, AsyncApp, Context, Entity};

pub fn init(ctrl: Entity<Controller>, cx: &mut App) {
    let mut enabled = ctrl.read(cx).config.app.check_updates;
    let native = stenly_platform::updater::start(enabled);
    let native_ready = native == 1;
    let repo = stenly_core::update::release_repository();
    ctrl.update(cx, |c, cx| {
        c.update_native = native_ready;
        c.update_status = match (native_ready, repo) {
            (true, _) => "Release notifications ready. In-app installation is not enabled.".into(),
            (false, Some(_)) if enabled => "Release notifications ready. Checking GitHub Releases shortly.".into(),
            (false, Some(_)) => "Release notifications are off.".into(),
            (false, None) if cfg!(target_os = "macos") && native == -1 => "Updater could not start. Your installed build is unchanged.".into(),
            (false, None) if cfg!(target_os = "macos") => "Updates are not configured in this build.".into(),
            (false, None) => "Release notifications are not configured in this build.".into(),
        };
        cx.notify();
    });
    cx.observe(&ctrl, move |ctrl, cx| {
        let value = ctrl.read(cx).config.app.check_updates;
        if value != enabled {
            enabled = value;
            if !value {
                // Off clears any notice; a request already in flight is ignored on arrival.
                ctrl.update(cx, |c, _| c.clear_update());
            }
            let ctrl = ctrl.clone();
            cx.spawn(async move |cx: &mut AsyncApp| {
                if native_ready {
                    stenly_platform::updater::set_enabled(value);
                }
                if value && (native_ready || repo.is_some()) {
                    let _ = cx.update(|cx| check(ctrl, cx));
                }
            })
            .detach();
        }
    })
    .detach();
    if native_ready {
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
    if !native_ready && repo.is_none() {
        return;
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
    if c.update_native {
        // The native checker can display UI, so release the controller/App borrow.
        cx.spawn(async |_, _| stenly_platform::updater::check())
            .detach();
    } else {
        let Some(repo) = stenly_core::update::release_repository() else {
            return;
        };
        tracing::info!(repo, "checking GitHub Releases");
        c.update_status = "Checking GitHub Releases…".into();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { release(repo) }).await;
            let _ = this.update(cx, |c, cx| {
                if !c.config.app.check_updates { return; }
                match result {
                    Ok(Some(found)) => { tracing::info!(version = %found.version, url = %found.url, "newer release available"); c.set_update_available(found) }
                    Ok(None) => { tracing::info!("no newer stable release"); c.update_status = "You are on the latest stable release.".into(); c.clear_update_available(); }
                    Err(e) => {
                        tracing::info!(error = %format!("{e:#}"), "release check failed");
                        c.update_status = "Could not check releases. Your installed build is unchanged; try again later.".into();
                    }
                }
                cx.notify();
            });
        }).detach();
    }
    cx.notify();
}

/// One HTTPS request to GitHub's latest-release endpoint (stable releases only).
/// Sends the IP address and a `Stenly/<version>` user agent; nothing else.
fn release(repo: &str) -> anyhow::Result<Option<stenly_core::update::Release>> {
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
    stenly_core::update::evaluate(&response, repo, &stenly_core::update::current_version())
}
