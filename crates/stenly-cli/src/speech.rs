//! Private, pipe-only inference helper. No microphone or Accessibility access.
fn main() -> anyhow::Result<()> {
    stenly_asr::run_worker()
}
