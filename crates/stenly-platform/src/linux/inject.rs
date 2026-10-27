//! Typing dictated text on Linux. Wayland: wl-copy + wtype (wlroots,
//! Hyprland, KDE) or ydotool (anything, needs the ydotoold daemon). X11:
//! xclip/xsel + xdotool. The clipboard is restored afterwards when the tools
//! can read it back.

use anyhow::{Result, bail};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;
use stenly_core::platform::TextInjector;

fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn have(cmd: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {cmd}")]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn pipe_into(cmd: &str, args: &[&str], input: &str) -> Result<()> {
    let mut c = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    c.stdin.take().map(|mut s| s.write_all(input.as_bytes())).transpose()?;
    // wl-copy forks and keeps serving the selection; don't wait on it.
    if cmd != "wl-copy" {
        c.wait()?;
    }
    Ok(())
}

fn read_clipboard() -> Option<String> {
    let out = if wayland() && have("wl-paste") {
        Command::new("wl-paste").args(["--no-newline"]).output().ok()?
    } else if have("xclip") {
        Command::new("xclip").args(["-selection", "clipboard", "-o"]).output().ok()?
    } else if have("xsel") {
        Command::new("xsel").args(["--clipboard", "--output"]).output().ok()?
    } else {
        return None;
    };
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn write_clipboard(text: &str) -> Result<()> {
    if wayland() && have("wl-copy") {
        return pipe_into("wl-copy", &[], text);
    }
    if have("xclip") {
        return pipe_into("xclip", &["-selection", "clipboard"], text);
    }
    if have("xsel") {
        return pipe_into("xsel", &["--clipboard", "--input"], text);
    }
    bail!("install wl-clipboard (Wayland) or xclip (X11) so Stenly can paste")
}

/// Send Ctrl+V (or Return) to the focused window.
fn send_keys(paste: bool) -> Result<()> {
    if wayland() && have("wtype") {
        let ok = if paste {
            Command::new("wtype").args(["-M", "ctrl", "v", "-m", "ctrl"]).status()?
        } else {
            Command::new("wtype").args(["-k", "Return"]).status()?
        };
        if ok.success() {
            return Ok(());
        }
    }
    if !wayland() && have("xdotool") {
        let st = Command::new("xdotool").args(["key", "--clearmodifiers", if paste { "ctrl+v" } else { "Return" }]).status()?;
        if st.success() {
            return Ok(());
        }
    }
    if have("ydotool") {
        // 29 = KEY_LEFTCTRL, 47 = KEY_V, 28 = KEY_ENTER
        let args: &[&str] = if paste { &["key", "29:1", "47:1", "47:0", "29:0"] } else { &["key", "28:1", "28:0"] };
        if Command::new("ydotool").args(args).status()?.success() {
            return Ok(());
        }
    }
    bail!("install wtype (Wayland), xdotool (X11) or ydotool so Stenly can paste")
}

pub struct LinuxInjector;

impl TextInjector for LinuxInjector {
    fn paste(&self, text: &str, restore_after: Option<Duration>) -> Result<()> {
        let previous = restore_after.and_then(|_| read_clipboard());
        write_clipboard(text)?;
        std::thread::sleep(Duration::from_millis(60));
        send_keys(true)?;
        if let (Some(d), Some(prev)) = (restore_after, previous) {
            std::thread::spawn(move || {
                std::thread::sleep(d);
                let _ = write_clipboard(&prev);
            });
        }
        Ok(())
    }

    fn press_return(&self) -> Result<()> {
        send_keys(false)
    }

    fn copy(&self, text: &str) -> Result<()> {
        write_clipboard(text)
    }
}
