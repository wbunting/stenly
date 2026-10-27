//! Fallbacks for platforms (or sessions) without global shortcuts / key
//! injection: dictation still works from the app's own button, and the result
//! is left on the clipboard.

use crossbeam_channel::Sender;
use stenly_core::platform::{Hotkey, HotkeyEvent, Hotkeys, TextInjector};

pub struct NoHotkeys;

impl Hotkeys for NoHotkeys {
    fn watch(&self, _bindings: Vec<Option<Hotkey>>, _sink: Sender<HotkeyEvent>) -> anyhow::Result<()> {
        anyhow::bail!("global shortcuts aren't available on this system")
    }
}

/// Copies to the clipboard via the first tool that exists.
pub struct ClipboardOnly;

pub fn copy_with_tools(text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    for (cmd, args) in [("wl-copy", &[][..]), ("xclip", &["-selection", "clipboard"][..]), ("xsel", &["--clipboard", "--input"][..]), ("pbcopy", &[][..])] {
        if let Ok(mut child) = std::process::Command::new(cmd).args(args).stdin(std::process::Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(text.as_bytes())?;
            }
            child.wait()?;
            return Ok(());
        }
    }
    anyhow::bail!("no clipboard tool found (install wl-clipboard or xclip)")
}

impl TextInjector for ClipboardOnly {
    fn paste(&self, text: &str, _restore: Option<std::time::Duration>) -> anyhow::Result<()> {
        copy_with_tools(text)?;
        anyhow::bail!("copied to the clipboard; automatic pasting isn't available here")
    }
    fn press_return(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn copy(&self, text: &str) -> anyhow::Result<()> {
        copy_with_tools(text)
    }
}
