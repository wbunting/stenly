//! Launching stenly while it's already running shows the running window.
//!
//! macOS does this for us (`on_reopen`). On Linux a second launch from the app
//! launcher just found the pid lock and exited, so nothing happened. Instead
//! it now tells the running instance over a Unix socket next to the pid file,
//! which opens its window as the tray's "Open Stenly…" would.

use crossbeam_channel::Sender;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;
use stenly_core::platform::TrayCommand;

const OPEN: &str = "open";

fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("stenly.sock")
}

/// Ask the running instance to show its window. True if it acknowledged.
pub fn ask_running_instance(data_dir: &Path) -> bool {
    let send = || -> std::io::Result<String> {
        let mut s = UnixStream::connect(socket_path(data_dir))?;
        s.set_read_timeout(Some(Duration::from_secs(2)))?;
        writeln!(s, "{OPEN}")?;
        let mut reply = String::new();
        BufReader::new(s).read_line(&mut reply)?;
        Ok(reply)
    };
    send().is_ok_and(|r| r.trim() == "ok")
}

/// Listen for later launches. Only call while holding the single-instance
/// lock: any socket file left at the path is from a previous run.
pub fn listen(data_dir: &Path, commands: Sender<TrayCommand>) {
    let path = socket_path(data_dir);
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "relaunch socket unavailable");
            return;
        }
    };
    std::thread::Builder::new()
        .name("relaunch-listener".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                if reader.read_line(&mut line).is_ok() && line.trim() == OPEN {
                    tracing::info!("launched again; opening the window");
                    let _ = commands.send(TrayCommand::OpenWindow);
                    let _ = (&stream).write_all(b"ok\n");
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_launch_opens_the_window() {
        let dir = std::env::temp_dir().join(format!("stenly-reopen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!ask_running_instance(&dir), "nothing listening yet");
        let (tx, rx) = crossbeam_channel::unbounded();
        listen(&dir, tx);
        assert!(ask_running_instance(&dir));
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)),
            Ok(TrayCommand::OpenWindow)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
