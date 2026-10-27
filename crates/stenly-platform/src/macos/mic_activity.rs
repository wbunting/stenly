//! Which processes are capturing audio right now (Core Audio process objects,
//! macOS 14+). Doesn't need any permission and never touches audio data.

use cidre::core_audio as ca;
use stenly_core::platform::{MicActivity, MicUser};

pub struct CoreAudioMicActivity;

impl MicActivity for CoreAudioMicActivity {
    fn mic_users(&self) -> anyhow::Result<Vec<MicUser>> {
        let me = std::process::id() as i32;
        let procs = ca::System::processes().map_err(|e| anyhow::anyhow!("listing audio processes: {e:?}"))?;
        let mut out = Vec::new();
        for p in procs {
            if !p.is_running_input().unwrap_or(false) {
                continue;
            }
            let pid = p.pid().ok();
            if pid == Some(me) {
                continue;
            }
            let app_id = p.bundle_id().map(|s| s.to_string()).unwrap_or_default();
            let app_id = if app_id.is_empty() { pid.map(|p| format!("pid:{p}")).unwrap_or_default() } else { app_id };
            out.push(MicUser { pid, app_id });
        }
        Ok(out)
    }

    fn window_display(&self, u: &MicUser) -> Option<u64> {
        super::screens::app_window_display(&u.app_id, u.pid).map(u64::from)
    }
}
