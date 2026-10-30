use super::tests_support::MemVault;
use super::*;
use std::sync::Mutex;

/// Shares the vault so a test can wipe/inspect it while the manager owns a box.
struct SharedVault(std::sync::Arc<MemVault>);
impl Vault for SharedVault {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name)
    }
    fn set(&self, name: &str, value: &str) -> Result<()> {
        self.0.set(name, value)
    }
}

/// A provider with a fixed key list and activation limit, and a switch for "offline".
struct Mock {
    keys: Vec<&'static str>,
    limit: usize,
    state: Mutex<MockState>,
}
#[derive(Default)]
struct MockState {
    active: Vec<(String, String)>, // (key, activation id)
    next: u32,
    offline: bool,
    revoked: Vec<String>,
    calls: Vec<String>,
}
impl Mock {
    fn new(limit: usize) -> Self {
        Mock {
            keys: vec!["STENLY-AAAA-1111"],
            limit,
            state: Mutex::default(),
        }
    }
    fn set_offline(&self, v: bool) {
        self.state.lock().unwrap().offline = v;
    }
    fn revoke(&self, key: &str) {
        self.state.lock().unwrap().revoked.push(key.into());
    }
    fn active(&self) -> usize {
        self.state.lock().unwrap().active.len()
    }
    fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }
}
impl Provider for Mock {
    fn activate(&self, key: &str, _label: &str) -> Result<Activation, LicenseError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push("activate".into());
        if s.offline {
            return Err(LicenseError::Offline("down".into()));
        }
        if !self.keys.contains(&key) || s.revoked.iter().any(|r| r == key) {
            return Err(LicenseError::Rejected("That key isn't valid.".into()));
        }
        if s.active.iter().filter(|(k, _)| k == key).count() >= self.limit {
            return Err(LicenseError::LimitReached);
        }
        s.next += 1;
        let id = format!("act-{}", s.next);
        s.active.push((key.into(), id.clone()));
        Ok(Activation { activation_id: id })
    }
    fn validate(&self, key: &str, id: &str) -> Result<(), LicenseError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push("validate".into());
        if s.offline {
            return Err(LicenseError::Offline("down".into()));
        }
        if s.revoked.iter().any(|r| r == key) || !s.active.iter().any(|(k, a)| k == key && a == id)
        {
            return Err(LicenseError::Rejected("revoked".into()));
        }
        Ok(())
    }
    fn deactivate(&self, key: &str, id: &str) -> Result<(), LicenseError> {
        let mut s = self.state.lock().unwrap();
        s.calls.push("deactivate".into());
        if s.offline {
            return Err(LicenseError::Offline("down".into()));
        }
        s.active.retain(|(k, a)| !(k == key && a == id));
        Ok(())
    }
}

fn t0() -> DateTime<Utc> {
    "2026-11-01T12:00:00Z".parse().unwrap()
}

fn manager(dir: &std::path::Path, enforced: bool) -> (LicenseManager, std::sync::Arc<MemVault>) {
    let vault = std::sync::Arc::new(MemVault::default());
    (
        LicenseManager::new(
            Box::new(SharedVault(vault.clone())),
            dir.join("trial.json"),
            enforced,
            10,
        ),
        vault,
    )
}

const KEY: &str = "STENLY-AAAA-1111";

// ------------------------------------------------------------ trial & gate

#[test]
fn trial_counts_each_meeting_once_and_ignores_accidental_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    assert_eq!(m.snapshot(t0()).left(), 10);
    assert_eq!(m.record_meeting("a", 600), 1);
    assert_eq!(
        m.record_meeting("a", 600),
        1,
        "reprocessing the same meeting doesn't count twice"
    );
    assert_eq!(
        m.record_meeting("b", 5),
        1,
        "a 5 second accidental start isn't a meeting"
    );
    assert_eq!(m.record_meeting("c", 61), 2);
    let s = m.snapshot(t0());
    assert_eq!((s.used, s.left(), s.state()), (2, 8, State::Trial));
    assert_eq!(s.headline(), "Free trial · 8 of 10 meetings left");
}

#[test]
fn recording_is_blocked_only_after_the_last_free_meeting() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    for i in 0..9 {
        m.record_meeting(&format!("s{i}"), 300);
    }
    assert!(
        m.snapshot(t0()).can_record(),
        "the 10th meeting is still free"
    );
    m.record_meeting("s9", 300);
    let s = m.snapshot(t0());
    assert_eq!(s.state(), State::Expired);
    assert!(!s.can_record());
    assert!(s.blocked_message().unwrap().contains("10 free meetings"));
}

#[test]
fn source_builds_never_gate() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), false);
    for i in 0..30 {
        m.record_meeting(&format!("s{i}"), 300);
    }
    let s = m.snapshot(t0());
    assert_eq!(s.state(), State::Open);
    assert!(s.can_record());
    assert!(s.blocked_message().is_none());
}

#[test]
fn trial_survives_losing_the_file_or_the_keychain_copy() {
    let dir = tempfile::tempdir().unwrap();
    let (m, vault) = manager(dir.path(), true);
    for i in 0..4 {
        m.record_meeting(&format!("s{i}"), 300);
    }
    // config/data reset: the file is gone, the Keychain copy remains
    std::fs::remove_file(dir.path().join("trial.json")).unwrap();
    assert_eq!(m.snapshot(t0()).used, 4);
    // and the other way round
    m.record_meeting("s4", 300);
    vault.0.lock().unwrap().clear();
    assert_eq!(m.snapshot(t0()).used, 5);
    // a corrupt file doesn't reset it either
    m.record_meeting("s5", 300);
    std::fs::write(dir.path().join("trial.json"), "{ not json").unwrap();
    assert_eq!(m.snapshot(t0()).used, 6);
}

#[test]
fn a_missing_copy_is_healed_on_the_next_meeting() {
    let dir = tempfile::tempdir().unwrap();
    let (m, vault) = manager(dir.path(), true);
    m.record_meeting("a", 300);
    std::fs::remove_file(dir.path().join("trial.json")).unwrap();
    m.record_meeting("a", 300); // duplicate: nothing counted, but the file is rewritten
    assert!(dir.path().join("trial.json").exists());
    assert!(vault.get("trial").is_some());
}

// ------------------------------------------------------------ activation

#[test]
fn activating_a_key_unlocks_recording_even_after_the_trial() {
    let dir = tempfile::tempdir().unwrap();
    let (m, vault) = manager(dir.path(), true);
    let p = Mock::new(1);
    for i in 0..10 {
        m.record_meeting(&format!("s{i}"), 300);
    }
    assert!(!m.snapshot(t0()).can_record());
    let s = m.activate(&p, &format!("  {KEY}\n"), t0()).unwrap();
    assert_eq!(s.state(), State::Licensed);
    assert!(s.can_record());
    assert_eq!(s.license.unwrap().masked_key, "••••1111");
    assert_eq!(p.active(), 1);
    // persisted for the next launch, and the plain key never shows in the status
    assert!(vault.get("license").unwrap().contains(KEY));
}

#[test]
fn unknown_keys_are_rejected_without_changing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let (m, vault) = manager(dir.path(), true);
    let p = Mock::new(1);
    let e = m.activate(&p, "nope", t0()).unwrap_err();
    assert!(matches!(e, LicenseError::Rejected(_)));
    assert!(m.activate(&p, "   ", t0()).is_err());
    assert!(vault.get("license").is_none());
    assert_eq!(m.snapshot(t0()).state(), State::Trial);
}

#[test]
fn one_mac_limit_then_deactivate_and_move_to_another_mac() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let (mac_a, _) = manager(dir_a.path(), true);
    let (mac_b, _) = manager(dir_b.path(), true);
    let p = Mock::new(1);
    mac_a.activate(&p, KEY, t0()).unwrap();
    assert_eq!(
        mac_b.activate(&p, KEY, t0()).unwrap_err(),
        LicenseError::LimitReached
    );
    assert!(
        mac_b
            .activate(&p, KEY, t0())
            .unwrap_err()
            .to_string()
            .contains("Deactivate")
    );
    // free the slot on the old Mac
    let s = mac_a.deactivate(&p, t0()).unwrap();
    assert!(s.license.is_none());
    assert_eq!(p.active(), 0);
    assert_eq!(
        mac_b.activate(&p, KEY, t0()).unwrap().state(),
        State::Licensed
    );
}

#[test]
fn spare_activations_allow_a_second_mac() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (m1, _) = manager(a.path(), true);
    let (m2, _) = manager(b.path(), true);
    let p = Mock::new(3);
    m1.activate(&p, KEY, t0()).unwrap();
    m2.activate(&p, KEY, t0()).unwrap();
    assert_eq!(p.active(), 2);
}

#[test]
fn entering_the_same_key_again_does_not_use_a_second_slot() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    let p = Mock::new(1);
    m.activate(&p, KEY, t0()).unwrap();
    m.activate(&p, KEY, t0() + Duration::days(3)).unwrap();
    assert_eq!(p.active(), 1);
    assert_eq!(p.calls().iter().filter(|c| *c == "activate").count(), 1);
    assert_eq!(
        m.snapshot(t0() + Duration::days(3))
            .license
            .unwrap()
            .last_validated,
        t0() + Duration::days(3)
    );
}

#[test]
fn activating_while_offline_fails_cleanly_and_keeps_the_trial() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    let p = Mock::new(1);
    p.set_offline(true);
    assert!(matches!(
        m.activate(&p, KEY, t0()).unwrap_err(),
        LicenseError::Offline(_)
    ));
    assert_eq!(m.snapshot(t0()).state(), State::Trial);
}

#[test]
fn deactivating_offline_keeps_the_licence_instead_of_stranding_the_slot() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    let p = Mock::new(1);
    m.activate(&p, KEY, t0()).unwrap();
    p.set_offline(true);
    assert!(matches!(
        m.deactivate(&p, t0()).unwrap_err(),
        LicenseError::Offline(_)
    ));
    assert_eq!(m.snapshot(t0()).state(), State::Licensed);
    p.set_offline(false);
    m.deactivate(&p, t0()).unwrap();
    assert_eq!(p.active(), 0);
}

// ------------------------------------------------------------ validation & grace

#[test]
fn validation_runs_weekly_not_on_every_launch() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    let p = Mock::new(1);
    m.activate(&p, KEY, t0()).unwrap();
    assert_eq!(
        m.refresh(&p, t0() + Duration::days(6), false),
        Refresh::Skipped
    );
    assert_eq!(
        m.refresh(&p, t0() + Duration::days(7), false),
        Refresh::Validated
    );
    assert_eq!(
        m.snapshot(t0() + Duration::days(8))
            .license
            .unwrap()
            .last_validated,
        t0() + Duration::days(7)
    );
    assert_eq!(
        m.refresh(&p, t0() + Duration::days(8), true),
        Refresh::Validated,
        "force always checks"
    );
}

#[test]
fn offline_licence_keeps_working_for_30_days_then_asks_for_one_check() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    let p = Mock::new(1);
    m.activate(&p, KEY, t0()).unwrap();
    p.set_offline(true);
    assert_eq!(
        m.refresh(&p, t0() + Duration::days(10), false),
        Refresh::Offline
    );
    let day30 = m.snapshot(t0() + Duration::days(30));
    assert!(
        day30.can_record(),
        "day 30 is still inside the grace period"
    );
    assert_eq!(day30.license.unwrap().grace_days_left, 0);
    let day31 = m.snapshot(t0() + Duration::days(31));
    assert_eq!(
        day31.state(),
        State::Trial,
        "falls back to the remaining free meetings"
    );
    assert_eq!(day31.lapse, Some(Lapse::NeedsValidation));
    assert!(
        day31.blocked_message().is_none(),
        "while the trial isn't used up nothing is blocked"
    );
    // back online: one successful check restores it
    p.set_offline(false);
    assert_eq!(
        m.refresh(&p, t0() + Duration::days(31), true),
        Refresh::Validated
    );
    assert_eq!(
        m.snapshot(t0() + Duration::days(31)).state(),
        State::Licensed
    );
}

#[test]
fn lapsed_licence_with_a_used_up_trial_blocks_new_recordings_with_a_clear_reason() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    let p = Mock::new(1);
    for i in 0..10 {
        m.record_meeting(&format!("s{i}"), 300);
    }
    m.activate(&p, KEY, t0()).unwrap();
    let s = m.snapshot(t0() + Duration::days(40));
    assert!(!s.can_record());
    assert!(s.blocked_message().unwrap().contains("couldn't confirm"));
}

#[test]
fn setting_the_clock_back_does_not_extend_the_grace_period() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    let p = Mock::new(1);
    m.activate(&p, KEY, t0() + Duration::days(100)).unwrap();
    // "now" is before last_validated: treated as just validated, never as extra time
    let s = m.snapshot(t0());
    assert_eq!(s.license.unwrap().grace_days_left, GRACE_DAYS);
}

#[test]
fn a_definite_rejection_revokes_but_network_errors_never_do() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    let p = Mock::new(1);
    for i in 0..10 {
        m.record_meeting(&format!("s{i}"), 300);
    }
    m.activate(&p, KEY, t0()).unwrap();
    p.set_offline(true);
    assert_eq!(
        m.refresh(&p, t0() + Duration::days(8), false),
        Refresh::Offline
    );
    assert!(m.snapshot(t0() + Duration::days(8)).can_record());
    p.set_offline(false);
    p.revoke(KEY);
    assert_eq!(
        m.refresh(&p, t0() + Duration::days(9), false),
        Refresh::Revoked
    );
    let s = m.snapshot(t0() + Duration::days(9));
    assert!(!s.can_record());
    assert_eq!(s.lapse, Some(Lapse::Revoked));
    assert!(s.blocked_message().unwrap().contains("no longer valid"));
    // revoked licences aren't re-checked forever
    assert_eq!(
        m.refresh(&p, t0() + Duration::days(30), false),
        Refresh::Skipped
    );
}

#[test]
fn existing_notes_never_depend_on_the_licence() {
    // The gate is only consulted when starting a recording; there is no API here
    // that touches sessions, notes or dictation. A used-up, unlicensed profile
    // still reports a clean snapshot instead of failing.
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    for i in 0..12 {
        m.record_meeting(&format!("s{i}"), 300);
    }
    let s = m.snapshot(t0());
    assert_eq!(s.state(), State::Expired);
    assert_eq!(s.left(), 0);
}

#[test]
fn counted_ids_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    let (m, _) = manager(dir.path(), true);
    for i in 0..(COUNTED_CAP + 20) {
        m.record_meeting(&format!("s{i}"), 300);
    }
    assert_eq!(m.snapshot(t0()).used as usize, COUNTED_CAP + 20);
    let t: TrialState =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("trial.json")).unwrap())
            .unwrap();
    assert_eq!(t.counted.len(), COUNTED_CAP);
}
