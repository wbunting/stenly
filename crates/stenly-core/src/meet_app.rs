//! Opening a Google Meet link in the installed Meet web app (a Chromium PWA).
//!
//! What was measured (Chrome 156, macOS 26, a throwaway profile; see the PR):
//!
//! * `open -a "<Google Meet.app>" <url>` does **not** pass the URL on. A
//!   running app just opens a new window on its own start page; a cold start
//!   launches Chrome with `--no-startup-window`.
//! * `open -n -b <browser> --args --user-data-dir=… --app-id=<id>
//!   --app-launch-url-for-shortcuts-menu-item=<url>` does: the URL opens in the
//!   web app's own window, query string intact, whether or not the browser and
//!   the app were already running (a second browser process hands the launch to
//!   the first and exits). `-n` matters: without it `open` drops `--args` for a
//!   running browser.
//!
//! The flag is Chrome's own "launch a web-app shortcut at a URL" switch, not a
//! documented interface, so callers must fall back to the default browser when
//! nothing is found or `open` fails.
//!
//! Everything here is pure or reads a few small files; the platform call is
//! just `open` with the arguments from [`open_args`].

use crate::speakers::ax::MEET_APP_ID;
use std::path::{Path, PathBuf};

/// Where Meet links open (`[app] meet_links`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeetLinks {
    /// The default browser, like every other link.
    #[default]
    Browser,
    /// The installed Google Meet web app, when there is one.
    App,
}

/// An installed Meet web app, as its macOS app shim describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetApp {
    /// The shim (`…/Chrome Apps.localized/Google Meet.app`).
    pub shim: PathBuf,
    /// Bundle id of the browser that runs the app (`com.google.Chrome`).
    pub browser: String,
    /// The browser's user data dir.
    pub user_data_dir: PathBuf,
    /// Profile to run it in, when the shim names one.
    pub profile: Option<String>,
}

/// The value of `<key>KEY</key><string>VALUE</string>` in an XML plist.
pub fn plist_string(xml: &str, key: &str) -> Option<String> {
    let tag = format!("<key>{key}</key>");
    let rest = xml[xml.find(&tag)? + tag.len()..].trim_start();
    let rest = rest.strip_prefix("<string>")?;
    Some(unescape(&rest[..rest.find("</string>")?]))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Read an app shim's `Info.plist`; `Some` only for the Meet web app.
pub fn parse_shim(shim: &Path, info_plist: &str) -> Option<MeetApp> {
    if plist_string(info_plist, "CrAppModeShortcutID")? != MEET_APP_ID {
        return None;
    }
    let browser = plist_string(info_plist, "CrBundleIdentifier")?;
    // `<user data dir>/<profile or "-">/Web Applications/_crx_<app id>`
    let app_dir = PathBuf::from(plist_string(info_plist, "CrAppModeUserDataDir")?);
    let web_apps = app_dir.parent()?;
    if !app_dir.file_name()?.to_str()?.starts_with("_crx_")
        || web_apps.file_name()? != "Web Applications"
    {
        return None;
    }
    let user_data_dir = web_apps.parent()?.parent()?.to_path_buf();
    let profile =
        plist_string(info_plist, "CrAppModeProfileDir").filter(|p| !p.is_empty() && p != "-");
    Some(MeetApp {
        shim: shim.to_path_buf(),
        browser,
        user_data_dir,
        profile,
    })
}

/// Look for the Meet web app in the `… Apps.localized` folders Chromium
/// browsers put app shims in, under each of `roots` (`~/Applications`, `/Applications`).
/// Only Google Chrome's: the launch flags were tried on Chrome 156 and nothing
/// else, so other Chromium browsers' apps are left to the default browser.
pub fn find_in(roots: &[PathBuf]) -> Option<MeetApp> {
    let mut found: Vec<MeetApp> = Vec::new();
    for root in roots {
        let Ok(dirs) = std::fs::read_dir(root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = dirs
            .flatten()
            .map(|d| d.path())
            .filter(|p| p.extension().is_some_and(|e| e == "localized") && p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let Ok(apps) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut apps: Vec<PathBuf> = apps
                .flatten()
                .map(|a| a.path())
                .filter(|p| p.extension().is_some_and(|e| e == "app"))
                .collect();
            apps.sort();
            for shim in apps {
                if let Some(app) = std::fs::read_to_string(shim.join("Contents/Info.plist"))
                    .ok()
                    .and_then(|x| parse_shim(&shim, &x))
                {
                    found.push(app);
                }
            }
        }
    }
    found.into_iter().find(|a| a.browser == VERIFIED_BROWSER)
}

/// The browser whose app shims `open_args` has been checked against.
const VERIFIED_BROWSER: &str = "com.google.Chrome";

/// The installed Meet web app, if any (macOS only).
pub fn find() -> Option<MeetApp> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots.push(PathBuf::from("/Applications"));
    find_in(&roots)
}

/// Whether `url` is an https link to Google Meet, and safe to hand to a command line.
pub fn is_meet_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("HTTPS://"))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    host.eq_ignore_ascii_case("meet.google.com")
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Arguments for `/usr/bin/open` that open `url` in `app`'s own window.
/// `None` if `url` isn't a Meet link.
pub fn open_args(app: &MeetApp, url: &str) -> Option<Vec<String>> {
    if !is_meet_url(url) {
        return None;
    }
    let mut a = vec![
        "-n".into(),
        "-b".into(),
        app.browser.clone(),
        "--args".into(),
        format!("--user-data-dir={}", app.user_data_dir.display()),
    ];
    if let Some(p) = &app.profile {
        a.push(format!("--profile-directory={p}"));
    }
    a.push(format!("--app-id={MEET_APP_ID}"));
    a.push(format!("--app-launch-url-for-shortcuts-menu-item={url}"));
    Some(a)
}

/// Open `url` in the installed Meet app. `false` (nothing opened, or `open`
/// failed) means the caller should open it in the browser instead.
pub fn open_in_app(url: &str) -> bool {
    let Some(app) = find() else { return false };
    let Some(args) = open_args(&app, url) else {
        return false;
    };
    std::process::Command::new("/usr/bin/open")
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shim plist shaped like Chrome's (synthetic paths).
    fn plist(id: &str, browser: &str, udd: &str, extra: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\t<key>CFBundleName</key>\n\t<string>Google Meet</string>\n\t<key>CrAppModeShortcutID</key>\n\t<string>{id}</string>\n\t<key>CrAppModeShortcutURL</key>\n\t<string>https://meet.google.com/landing?lfhs=2</string>\n\t<key>CrAppModeUserDataDir</key>\n\t<string>{udd}</string>\n\t<key>CrBundleIdentifier</key>\n\t<string>{browser}</string>\n{extra}</dict>\n</plist>\n"
        )
    }
    const UDD: &str = "/Users/test/Library/Application Support/Google/Chrome/-/Web Applications/_crx_kjgfgldnnfoeklkmfkjfagphfepbbdan";

    #[test]
    fn reads_plist_strings() {
        let p = plist(MEET_APP_ID, "com.google.Chrome", UDD, "");
        assert_eq!(
            plist_string(&p, "CrBundleIdentifier").as_deref(),
            Some("com.google.Chrome")
        );
        assert_eq!(plist_string(&p, "Missing"), None);
        // a key whose value isn't a string
        assert_eq!(plist_string("<key>A</key><true/>", "A"), None);
        assert_eq!(
            plist_string("<key>A</key>\n <string>x &amp; &lt;y&gt;</string>", "A").as_deref(),
            Some("x & <y>")
        );
    }

    #[test]
    fn parses_the_meet_shim_only() {
        let shim = Path::new("/Users/test/Applications/Chrome Apps.localized/Google Meet.app");
        let app = parse_shim(shim, &plist(MEET_APP_ID, "com.google.Chrome", UDD, "")).unwrap();
        assert_eq!(app.browser, "com.google.Chrome");
        assert_eq!(
            app.user_data_dir,
            Path::new("/Users/test/Library/Application Support/Google/Chrome")
        );
        assert_eq!(app.profile, None);
        let with_profile = plist(
            MEET_APP_ID,
            "com.google.Chrome",
            UDD,
            "\t<key>CrAppModeProfileDir</key>\n\t<string>Profile 2</string>\n",
        );
        assert_eq!(
            parse_shim(shim, &with_profile).unwrap().profile.as_deref(),
            Some("Profile 2")
        );
        // another web app, and a layout we don't recognise
        assert_eq!(
            parse_shim(
                shim,
                &plist(
                    "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm",
                    "com.google.Chrome",
                    UDD,
                    ""
                )
            ),
            None
        );
        assert_eq!(
            parse_shim(
                shim,
                &plist(MEET_APP_ID, "com.google.Chrome", "/somewhere/else", "")
            ),
            None
        );
        assert_eq!(parse_shim(shim, "not a plist"), None);
    }

    #[test]
    fn open_arguments() {
        let app = MeetApp {
            shim: "/x/Google Meet.app".into(),
            browser: "com.google.Chrome".into(),
            user_data_dir: "/u/Chrome Dir".into(),
            profile: Some("Default".into()),
        };
        let url = "https://meet.google.com/abc-defg-hij?authuser=1&pwd=x";
        assert_eq!(
            open_args(&app, url).unwrap(),
            vec![
                "-n",
                "-b",
                "com.google.Chrome",
                "--args",
                "--user-data-dir=/u/Chrome Dir",
                "--profile-directory=Default",
                "--app-id=kjgfgldnnfoeklkmfkjfagphfepbbdan",
                &format!("--app-launch-url-for-shortcuts-menu-item={url}"),
            ]
        );
        let no_profile = MeetApp {
            profile: None,
            ..app.clone()
        };
        assert!(
            !open_args(&no_profile, url)
                .unwrap()
                .iter()
                .any(|a| a.starts_with("--profile-directory"))
        );
    }

    #[test]
    fn only_meet_links_go_to_the_app() {
        assert!(is_meet_url("https://meet.google.com/abc-defg-hij"));
        assert!(is_meet_url(
            "https://MEET.google.com/abc-defg-hij?authuser=2"
        ));
        assert!(!is_meet_url("http://meet.google.com/abc-defg-hij"));
        assert!(!is_meet_url(
            "https://meet.google.com.evil.example/abc-defg-hij"
        ));
        assert!(!is_meet_url(
            "https://evil.example/?u=https://meet.google.com/abc"
        ));
        assert!(!is_meet_url("https://meet.google.com@evil.example/x"));
        assert!(!is_meet_url("https://us02web.zoom.us/j/1"));
        assert!(!is_meet_url(
            "https://meet.google.com/abc --user-data-dir=/tmp"
        ));
        let app = MeetApp {
            shim: "/x".into(),
            browser: "com.google.Chrome".into(),
            user_data_dir: "/u".into(),
            profile: None,
        };
        assert_eq!(open_args(&app, "https://us02web.zoom.us/j/1"), None);
    }

    /// Manual: `STENLY_TEST_MEET_URL=https://meet.google.com/abc-defg-hij cargo test -p stenly-core
    /// manual_open_in_app -- --ignored`. Needs the Meet app installed in Chrome; opens a real window.
    #[test]
    #[ignore]
    fn manual_open_in_app() {
        let url = std::env::var("STENLY_TEST_MEET_URL")
            .expect("set STENLY_TEST_MEET_URL to a synthetic Meet link");
        assert!(open_in_app(&url));
    }

    #[test]
    fn finds_the_chrome_shim() {
        let root = tempfile::tempdir().unwrap();
        let put = |folder: &str, name: &str, body: String| {
            let c = root.path().join(folder).join(name).join("Contents");
            std::fs::create_dir_all(&c).unwrap();
            std::fs::write(c.join("Info.plist"), body).unwrap();
        };
        let helium = "/Users/test/Library/Application Support/net.imput.helium/-/Web Applications/_crx_kjgfgldnnfoeklkmfkjfagphfepbbdan";
        put(
            "Chromium Apps.localized",
            "Google Meet.app",
            plist(MEET_APP_ID, "net.imput.helium", helium, ""),
        );
        put(
            "Chrome Apps.localized",
            "Gmail.app",
            plist(
                "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm",
                "com.google.Chrome",
                UDD,
                "",
            ),
        );
        let roots = [
            root.path().to_path_buf(),
            PathBuf::from("/nonexistent-stenly-test"),
        ];
        // Another Chromium browser's Meet app, or another Chrome app, is not it.
        assert_eq!(find_in(&roots), None);
        put(
            "Chrome Apps.localized",
            "Google Meet.app",
            plist(MEET_APP_ID, "com.google.Chrome", UDD, ""),
        );
        let app = find_in(&roots).unwrap();
        assert_eq!(app.browser, "com.google.Chrome");
        assert!(app.shim.ends_with("Chrome Apps.localized/Google Meet.app"));
        assert_eq!(find_in(&[PathBuf::from("/nonexistent-stenly-test")]), None);
    }
}
