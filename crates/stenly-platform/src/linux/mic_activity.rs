//! Who is recording? Ask the sound server (works for PipeWire via pipewire-pulse
//! and for PulseAudio) using `pactl`, which ships with both.

use stenly_core::platform::{MicActivity, MicUser};

pub struct SoundServerMicActivity;

/// Parse `pactl list source-outputs` into (pid, binary/app name) pairs,
/// skipping monitor captures (those are someone recording *output*, not the mic).
pub(crate) fn parse_source_outputs(text: &str) -> Vec<MicUser> {
    let mut out = Vec::new();
    for block in text.split("Source Output #").skip(1) {
        let prop = |key: &str| {
            block.lines().find_map(|l| {
                let l = l.trim();
                let rest = l.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim();
                Some(rest.trim_matches('"').to_string())
            })
        };
        if prop("stream.capture.sink").as_deref() == Some("true") {
            continue;
        }
        let target = block.lines().find_map(|l| l.trim().strip_prefix("Source:").map(|s| s.trim().to_string()));
        if target.as_deref().is_some_and(|t| t.ends_with(".monitor")) {
            continue;
        }
        let pid = prop("application.process.id").and_then(|p| p.parse().ok());
        let app_id = prop("application.process.binary")
            .or_else(|| prop("application.id"))
            .or_else(|| prop("application.name"))
            .unwrap_or_default()
            .to_lowercase();
        // our own capture streams (another stenly process, or cpal's default stream name)
        if app_id.is_empty() || app_id.starts_with("stenly") || app_id.starts_with("cpal-") {
            continue;
        }
        out.push(MicUser { pid, app_id });
    }
    out
}

impl MicActivity for SoundServerMicActivity {
    fn mic_users(&self) -> anyhow::Result<Vec<MicUser>> {
        let me = std::process::id() as i32;
        let out = std::process::Command::new("pactl").args(["list", "source-outputs"]).env("LC_ALL", "C").output()?;
        if !out.status.success() {
            anyhow::bail!("pactl failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(parse_source_outputs(&String::from_utf8_lossy(&out.stdout)).into_iter().filter(|u| u.pid != Some(me)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pactl() {
        let text = r#"Source Output #42
	Driver: PipeWire
	Source: 55
	Properties:
		application.name = "Chromium"
		application.process.id = "1234"
		application.process.binary = "chromium"
Source Output #43
	Source: 56
	Properties:
		application.process.binary = "obs"
		stream.capture.sink = "true"
"#;
        assert_eq!(parse_source_outputs(text), vec![MicUser { pid: Some(1234), app_id: "chromium".into() }]);
    }
}
