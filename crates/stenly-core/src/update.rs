//! Update safety lease shared by native updater callbacks and capture starts.
//! Acquire before opening a microphone, not from an eventually-delivered UI
//! observer. Installation and capture are mutually exclusive on every thread.
use std::sync::Mutex;

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
}
