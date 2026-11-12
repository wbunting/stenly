//! What dictation needs from a Linux desktop. There are no permission prompts
//! here (PipeWire hands over the mic and system audio), so instead of asking
//! the OS we look: can we read a keyboard, and are the paste tools installed?

use super::hotkey::{KeyboardAccess, keyboard_access};
use super::inject::missing_tools;
use stenly_core::platform::SetupCheck;

pub(crate) fn checks() -> Vec<SetupCheck> {
    vec![
        shortcut_check(keyboard_access(), in_input_group_file()),
        paste_check(&missing_tools(), package_manager()),
    ]
}

fn shortcut_check(access: KeyboardAccess, listed_in_group: bool) -> SetupCheck {
    let title = "Dictation shortcut";
    match access {
        KeyboardAccess::Readable => SetupCheck { title, detail: "Stenly can hear your shortcut key.".into(), ok: true, fix: None },
        KeyboardAccess::NoKeyboard => SetupCheck { title, detail: "No keyboard found under /dev/input.".into(), ok: false, fix: None },
        KeyboardAccess::Denied if listed_in_group => SetupCheck {
            title,
            detail: "You're in the input group, but this session started before that. Log out and back in.".into(),
            ok: false,
            fix: None,
        },
        KeyboardAccess::Denied => SetupCheck {
            title,
            detail: "Stenly listens for your shortcut on the keyboard device. Join the input group, then log out and back in.".into(),
            ok: false,
            fix: Some("sudo usermod -aG input $USER".into()),
        },
    }
}

fn paste_check(missing: &[&str], pm: Option<&str>) -> SetupCheck {
    let title = "Typing text";
    if missing.is_empty() {
        return SetupCheck {
            title,
            detail: "Stenly can paste where your cursor is.".into(),
            ok: true,
            fix: None,
        };
    }
    let list = missing.join(" ");
    SetupCheck {
        title,
        detail: format!(
            "Install {} so Stenly can paste where your cursor is.",
            missing.join(" and ")
        ),
        ok: false,
        fix: pm.map(|pm| format!("{pm} {list}")),
    }
}

/// The current user is listed in the `input` group in /etc/group, whether or
/// not this login session has picked that up yet.
fn in_input_group_file() -> bool {
    let Ok(user) = std::env::var("USER") else {
        return false;
    };
    let groups = std::fs::read_to_string("/etc/group").unwrap_or_default();
    listed_in_group(&groups, "input", &user)
}

fn listed_in_group(etc_group: &str, group: &str, user: &str) -> bool {
    etc_group.lines().any(|l| {
        let mut f = l.split(':');
        f.next() == Some(group)
            && f.nth(2)
                .is_some_and(|members| members.split(',').any(|m| m.trim() == user))
    })
}

fn package_manager() -> Option<&'static str> {
    use super::inject::have;
    [
        ("pacman", "sudo pacman -S"),
        ("apt", "sudo apt install"),
        ("dnf", "sudo dnf install"),
        ("zypper", "sudo zypper install"),
    ]
    .into_iter()
    .find(|(bin, _)| have(bin))
    .map(|(_, cmd)| cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_membership() {
        let g = "wheel:x:998:will\ninput:x:994:alice,will\nvideo:x:985:\n";
        assert!(listed_in_group(g, "input", "will"));
        assert!(!listed_in_group(g, "input", "bob"));
        assert!(!listed_in_group(g, "video", "will"));
        assert!(!listed_in_group("input:x:994\n", "input", "will"));
    }

    #[test]
    fn shortcut_states() {
        assert!(shortcut_check(KeyboardAccess::Readable, false).ok);
        let denied = shortcut_check(KeyboardAccess::Denied, false);
        assert!(!denied.ok);
        assert_eq!(denied.fix.as_deref(), Some("sudo usermod -aG input $USER"));
        let relog = shortcut_check(KeyboardAccess::Denied, true);
        assert!(!relog.ok && relog.fix.is_none() && relog.detail.contains("Log out"));
    }

    #[test]
    fn paste_states() {
        assert!(paste_check(&[], Some("sudo pacman -S")).ok);
        let c = paste_check(&["wl-clipboard", "wtype"], Some("sudo pacman -S"));
        assert!(!c.ok);
        assert_eq!(c.fix.as_deref(), Some("sudo pacman -S wl-clipboard wtype"));
        assert_eq!(
            c.detail,
            "Install wl-clipboard and wtype so Stenly can paste where your cursor is."
        );
        assert_eq!(paste_check(&["xclip"], None).fix, None);
    }
}
