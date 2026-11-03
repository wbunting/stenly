//! Update safety lease shared by native updater callbacks and capture starts.
//! Acquire before opening a microphone, not from an eventually-delivered UI
//! observer. Installation and capture are mutually exclusive on every thread.
use std::sync::Mutex;

// ---- release notifications (GitHub Releases), notify-only ------------------

/// `OWNER/REPO` whose stable GitHub Releases are checked, set at build time by
/// `STENLY_RELEASE_REPOSITORY`. Official release builds set it; source and dev
/// builds have none (no guessed default) and never contact GitHub.
pub fn release_repository() -> Option<&'static str> {
    option_env!("STENLY_RELEASE_REPOSITORY").filter(|r| !r.is_empty())
}

/// Update checks default to on only in builds that have a release repository.
/// A saved `app.check_updates` (either value) always overrides this.
pub fn checks_default_on() -> bool {
    release_repository().is_some()
}

/// The running version. Debug builds only honour `STENLY_UPDATE_CURRENT_VERSION`
/// (a test hook for pretending to be older); release builds never read it.
pub fn current_version() -> String {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("STENLY_UPDATE_CURRENT_VERSION")
        && semver::Version::parse(&v).is_ok()
    {
        return v;
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// A newer stable release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Display form, always with a leading "v" ("v0.1.3").
    pub version: String,
    /// The release page on github.com for this repository.
    pub url: String,
}

/// Decide what a `GET /repos/{repo}/releases/latest` response means for the
/// running `current` version. `Ok(None)` = nothing newer (same, older, draft,
/// prerelease). `Err` = the response is unusable (malformed tag, missing or
/// unexpected URL); callers report "could not check" and change nothing.
pub fn evaluate(response: &serde_json::Value, repo: &str, current: &str) -> anyhow::Result<Option<Release>> {
    if response["draft"].as_bool().unwrap_or(true) || response["prerelease"].as_bool().unwrap_or(true) {
        return Ok(None);
    }
    let tag = response["tag_name"].as_str().ok_or_else(|| anyhow::anyhow!("missing tag"))?;
    let candidate = semver::Version::parse(tag.trim().trim_start_matches('v'))?;
    let current = semver::Version::parse(current.trim().trim_start_matches('v'))?;
    // Stable releases only, whatever the release flags say.
    if !candidate.pre.is_empty() || candidate <= current {
        return Ok(None);
    }
    let url = response["html_url"].as_str().ok_or_else(|| anyhow::anyhow!("missing release URL"))?;
    let prefix = format!("https://github.com/{repo}/releases/tag/");
    anyhow::ensure!(
        url.len() > prefix.len() && url[..prefix.len()].eq_ignore_ascii_case(&prefix) && !url.chars().any(char::is_whitespace),
        "unexpected release URL"
    );
    Ok(Some(Release { version: format!("v{candidate}"), url: url.to_string() }))
}

const ANNOUNCED: &str = "update-announced";

/// Whether the one-time "update available" notice for `version` is still due.
pub fn should_announce(version: &str) -> bool {
    announce_due(&crate::config::data_dir(), version)
}

/// Record that the notice for `version` was shown, so it is not repeated.
pub fn mark_announced(version: &str) {
    record_announced(&crate::config::data_dir(), version)
}

fn announce_due(dir: &std::path::Path, version: &str) -> bool {
    std::fs::read_to_string(dir.join(ANNOUNCED)).map(|s| s.trim() != version).unwrap_or(true)
}

fn record_announced(dir: &std::path::Path, version: &str) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(ANNOUNCED), version);
}

#[derive(Default)]
struct State {
    activities: usize,
    installing: bool,
}
static STATE: Mutex<State> = Mutex::new(State {
    activities: 0,
    installing: false,
});

pub struct Activity;
pub struct Installation;

impl Activity {
    pub fn begin() -> Option<Self> {
        let mut state = STATE.lock().ok()?;
        if state.installing {
            return None;
        }
        state.activities += 1;
        Some(Self)
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        if let Ok(mut state) = STATE.lock() {
            state.activities = state.activities.saturating_sub(1);
        }
    }
}
impl Installation {
    pub fn begin() -> Option<Self> {
        let mut state = STATE.lock().ok()?;
        if state.activities != 0 || state.installing {
            return None;
        }
        state.installing = true;
        Some(Self)
    }
}
impl Drop for Installation {
    fn drop(&mut self) {
        if let Ok(mut state) = STATE.lock() {
            state.installing = false;
        }
    }
}
pub fn busy() -> bool {
    STATE
        .lock()
        .map(|s| s.activities != 0 || s.installing)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_and_install_are_mutually_exclusive_through_finalization() {
        let meeting = Activity::begin().unwrap();
        let dictation = Activity::begin().unwrap();
        assert!(Installation::begin().is_none());
        let persist = Activity::begin().unwrap();
        drop(meeting);
        assert!(Installation::begin().is_none());
        drop(dictation);
        assert!(Installation::begin().is_none());
        drop(persist);
        let install = Installation::begin().unwrap();
        assert!(Activity::begin().is_none());
        assert!(Installation::begin().is_none());
        drop(install);
        assert!(Activity::begin().is_some());
    }

    fn response(tag: &str, draft: bool, prerelease: bool, url: &str) -> serde_json::Value {
        serde_json::json!({ "tag_name": tag, "draft": draft, "prerelease": prerelease, "html_url": url })
    }
    const REPO: &str = "wbunting/stenly";

    #[test]
    fn newer_release_is_reported_with_its_page() {
        let r = response("v0.1.3", false, false, "https://github.com/wbunting/stenly/releases/tag/v0.1.3");
        let found = evaluate(&r, REPO, "0.1.2").unwrap().unwrap();
        assert_eq!(found.version, "v0.1.3");
        assert_eq!(found.url, "https://github.com/wbunting/stenly/releases/tag/v0.1.3");
        // Tags without the leading v work too, and display normalizes it.
        let r = response("0.2.0", false, false, "https://github.com/wbunting/stenly/releases/tag/0.2.0");
        assert_eq!(evaluate(&r, REPO, "0.1.2").unwrap().unwrap().version, "v0.2.0");
        // Numeric (not lexical) comparison.
        let r = response("v0.10.0", false, false, "https://github.com/wbunting/stenly/releases/tag/v0.10.0");
        assert!(evaluate(&r, REPO, "0.9.0").unwrap().is_some());
    }

    #[test]
    fn same_or_older_release_is_not_news() {
        let url = "https://github.com/wbunting/stenly/releases/tag/v0.1.2";
        assert_eq!(evaluate(&response("v0.1.2", false, false, url), REPO, "0.1.2").unwrap(), None);
        assert_eq!(evaluate(&response("v0.1.1", false, false, url), REPO, "0.1.2").unwrap(), None);
        assert_eq!(evaluate(&response("v0.0.9", false, false, url), REPO, "0.1.2").unwrap(), None);
    }

    #[test]
    fn drafts_and_prereleases_are_ignored() {
        let url = "https://github.com/wbunting/stenly/releases/tag/v9.9.9";
        assert_eq!(evaluate(&response("v9.9.9", true, false, url), REPO, "0.1.2").unwrap(), None);
        assert_eq!(evaluate(&response("v9.9.9", false, true, url), REPO, "0.1.2").unwrap(), None);
        // A prerelease tag is ignored even if GitHub's flag says stable.
        assert_eq!(evaluate(&response("v9.9.9-rc.1", false, false, url), REPO, "0.1.2").unwrap(), None);
        // Missing flags are treated as not-stable.
        assert_eq!(evaluate(&serde_json::json!({ "tag_name": "v9.9.9", "html_url": url }), REPO, "0.1.2").unwrap(), None);
    }

    #[test]
    fn malformed_responses_are_errors() {
        let url = "https://github.com/wbunting/stenly/releases/tag/v1.0.0";
        assert!(evaluate(&response("latest", false, false, url), REPO, "0.1.2").is_err());
        assert!(evaluate(&response("v1.2", false, false, url), REPO, "0.1.2").is_err());
        assert!(evaluate(&response("", false, false, url), REPO, "0.1.2").is_err());
        assert!(evaluate(&serde_json::json!({ "draft": false, "prerelease": false, "html_url": url }), REPO, "0.1.2").is_err());
        assert!(evaluate(&serde_json::json!({ "draft": false, "prerelease": false, "tag_name": "v1.0.0" }), REPO, "0.1.2").is_err());
        assert!(evaluate(&response("v1.0.0", false, false, url), REPO, "not-a-version").is_err());
    }

    #[test]
    fn unexpected_release_urls_are_refused() {
        for url in [
            "http://github.com/wbunting/stenly/releases/tag/v1.0.0",
            "https://github.com/someone-else/stenly/releases/tag/v1.0.0",
            "https://github.com/wbunting/stenly-evil/releases/tag/v1.0.0",
            "https://evil.example/https://github.com/wbunting/stenly/releases/tag/v1.0.0",
            "https://github.com/wbunting/stenly/releases/tag/",
            "https://github.com/wbunting/stenly/releases/latest",
            "https://github.com.evil.example/wbunting/stenly/releases/tag/v1.0.0",
            "https://github.com/wbunting/stenly/releases/tag/v1.0.0 and more",
            "",
        ] {
            assert!(evaluate(&response("v1.0.0", false, false, url), REPO, "0.1.2").is_err(), "{url}");
        }
    }

    #[test]
    fn announce_once_per_version() {
        let dir = std::env::temp_dir().join(format!("stenly-announce-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(announce_due(&dir, "v0.1.3"));
        record_announced(&dir, "v0.1.3");
        assert!(!announce_due(&dir, "v0.1.3"));
        assert!(announce_due(&dir, "v0.1.4"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
