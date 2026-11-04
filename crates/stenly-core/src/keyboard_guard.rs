//! Status of OS-wide keyboard protection. A live lock is not a permission error.
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecureInputOwner {
    pub pid: i32,
    pub name: String,
    pub bundle_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutBlock {
    /// Missing when macOS reports protection but cannot attribute it reliably.
    pub owner: Option<SecureInputOwner>,
}

impl ShortcutBlock {
    pub fn title(&self) -> String {
        match &self.owner {
            Some(owner) => format!("Dictation shortcut blocked by {}", owner.name),
            None => "Dictation shortcut blocked by Secure Keyboard Entry".into(),
        }
    }

    pub fn guidance(&self) -> &'static str {
        "Leave the password field or dismiss the password prompt. If it stays blocked, save your work and restart the blocking app. Stenly will recover automatically."
    }
}

/// Show status immediately, but only nudge after a sustained lock. Password
/// fields are normal; short visits must not create a stream of notifications.
#[derive(Default)]
pub struct KeyboardGuard {
    current: Option<ShortcutBlock>,
    since: Option<Instant>,
    notified: bool,
}

pub struct GuardChange {
    pub changed: bool,
    pub notify: bool,
}

impl KeyboardGuard {
    pub fn current(&self) -> Option<&ShortcutBlock> {
        self.current.as_ref()
    }

    pub fn update(&mut self, block: Option<ShortcutBlock>, now: Instant) -> GuardChange {
        let changed = block != self.current;
        if block.is_none() {
            self.since = None;
            self.notified = false;
        } else if self.current.is_none() {
            self.since = Some(now);
        }
        self.current = block;
        let notify = !self.notified
            && self.since.is_some_and(|since| {
                now.saturating_duration_since(since) >= Duration::from_secs(5)
            });
        self.notified |= notify;
        GuardChange { changed, notify }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn blocked(name: &str) -> Option<ShortcutBlock> {
        Some(ShortcutBlock {
            owner: Some(SecureInputOwner {
                pid: 42,
                name: name.into(),
                bundle_id: Some("test.fixture".into()),
            }),
        })
    }
    #[test]
    fn short_password_visits_are_visible_but_quiet() {
        let mut guard = KeyboardGuard::default();
        let now = Instant::now();
        let change = guard.update(blocked("Test Browser"), now);
        assert!(change.changed && !change.notify);
        assert!(
            !guard
                .update(blocked("Test Browser"), now + Duration::from_secs(2))
                .notify
        );
        assert!(guard.update(None, now + Duration::from_secs(3)).changed);
        assert!(guard.current().is_none());
        assert!(!guard.update(None, now + Duration::from_secs(9)).notify);
    }
    #[test]
    fn sustained_lock_notifies_once_and_recovers_without_restarting_listener() {
        let mut guard = KeyboardGuard::default();
        let now = Instant::now();
        guard.update(blocked("Test Browser"), now);
        assert!(
            guard
                .update(blocked("Test Browser"), now + Duration::from_secs(5))
                .notify
        );
        assert!(
            !guard
                .update(blocked("Test Browser"), now + Duration::from_secs(600))
                .notify
        );
        // Better attribution / a different owner must not repeat the nudge.
        let change = guard.update(blocked("Test Terminal"), now + Duration::from_secs(601));
        assert!(change.changed && !change.notify);
        guard.update(None, now + Duration::from_secs(602));
        assert!(
            !guard
                .update(blocked("Test Browser"), now + Duration::from_secs(603))
                .notify
        );
        assert!(
            guard
                .update(blocked("Test Browser"), now + Duration::from_secs(608))
                .notify
        );
    }
    #[test]
    fn unknown_owner_is_honest_and_can_be_resolved_later() {
        let mut guard = KeyboardGuard::default();
        let now = Instant::now();
        guard.update(Some(ShortcutBlock { owner: None }), now);
        assert!(
            guard
                .current()
                .unwrap()
                .title()
                .contains("Secure Keyboard Entry")
        );
        assert!(
            guard
                .update(blocked("Synthetic Browser"), now + Duration::from_secs(1))
                .changed
        );
        assert!(
            guard
                .current()
                .unwrap()
                .title()
                .contains("Synthetic Browser")
        );
    }
}
