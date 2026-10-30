//! Free trial and one-Mac licence for the official signed macOS build.
//!
//! Stenly's source is MIT and free to build. Only the official Developer ID
//! release build (built with `STENLY_ENFORCE_LICENSE=1`, see
//! `scripts/bundle-macos.sh`) enforces anything, and then only one thing:
//! *starting a new meeting recording* after the free meetings are used up and
//! without a valid licence. Notes, search, sharing, MCP and dictation never
//! depend on it, and nothing here ever hides or deletes existing meetings.
//!
//! This is for honest users, not DRM: the trial count lives in a file and a
//! Keychain item and the larger of the two wins, so a config reset doesn't
//! hand out a fresh trial, but nobody is trying to defeat a determined user.
//!
//! Everything is testable without a network or Keychain: the provider is a
//! trait ([`Provider`]) and the storage is a trait ([`Vault`]).

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Meetings that can be recorded before a licence is needed. Kept in step with
/// `free_meetings` in `site/config.toml` (a site test checks it).
pub const FREE_MEETINGS: u32 = 10;
/// A recording shorter than this is an accidental start, not a meeting.
pub const MIN_COUNTED_SECS: i64 = 60;
/// Re-check the licence with the provider this often while online.
pub const REVALIDATE_DAYS: i64 = 7;
/// How long a licence keeps working with no successful check (offline).
pub const GRACE_DAYS: i64 = 30;
/// Most trial session ids remembered (they only prevent double counting).
const COUNTED_CAP: usize = 200;

/// Whether this binary is the official build that enforces the trial/licence.
/// `STENLY_FORCE_LICENSE_GATE=1` turns it on at run time in any build, for
/// testing and screenshots with an isolated profile.
pub fn enforced() -> bool {
    cfg!(stenly_enforce_license) || std::env::var("STENLY_FORCE_LICENSE_GATE").is_ok_and(|v| v == "1")
}

/// Where "Buy" opens: the hosted checkout page, set when the official build is
/// made (`STENLY_CHECKOUT_URL`). Source builds have none.
pub fn checkout_url() -> Option<&'static str> {
    option_env!("STENLY_CHECKOUT_URL").filter(|u| u.starts_with("https://"))
}

/// The licence service this build talks to, if one is configured. `None` in
/// source builds, so they never make a licence request.
pub fn default_provider() -> Option<Box<dyn Provider + Send + Sync>> {
    None
}

// ---------------------------------------------------------------- provider

/// A licence-key service (Polar, Lemon Squeezy …). Implementations only talk
/// to the network; all policy lives in [`LicenseManager`].
pub trait Provider {
    /// Claim one activation slot for `key`. `label` names this Mac in the
    /// provider's dashboard.
    fn activate(&self, key: &str, label: &str) -> Result<Activation, LicenseError>;
    /// Is this key still valid and this activation still present?
    fn validate(&self, key: &str, activation_id: &str) -> Result<(), LicenseError>;
    /// Release the activation slot.
    fn deactivate(&self, key: &str, activation_id: &str) -> Result<(), LicenseError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    pub activation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LicenseError {
    /// The provider answered and said no: unknown, revoked, expired or disabled key.
    #[error("{0}")]
    Rejected(String),
    /// Every activation of the key is in use on other Macs.
    #[error("This key is already active on its maximum number of Macs. Deactivate one in Stenly's Licence settings, then try again.")]
    LimitReached,
    /// No definite answer (offline, timeout, server error). Never revokes anything.
    #[error("Couldn't reach the licence server: {0}")]
    Offline(String),
}

// ---------------------------------------------------------------- storage

/// Small secret storage (the Keychain in the app, memory in tests).
pub trait Vault {
    fn get(&self, name: &str) -> Option<String>;
    fn set(&self, name: &str, value: &str) -> Result<()>;
}

/// The login Keychain (or a 0600 file off macOS), one item per profile.
pub struct KeychainVault;

impl Vault for KeychainVault {
    fn get(&self, name: &str) -> Option<String> {
        crate::secrets::license_item(name)
    }
    fn set(&self, name: &str, value: &str) -> Result<()> {
        crate::secrets::set_license_item(name, value)
    }
}

/// What is stored (as JSON, in the vault) once a key is activated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredLicense {
    pub key: String,
    pub activation_id: String,
    pub activated_at: DateTime<Utc>,
    /// Last time the provider confirmed the key. Starts the offline grace.
    pub last_validated: DateTime<Utc>,
    /// The provider said this key is no longer valid (refunded, revoked …).
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TrialState {
    count: u32,
    /// Session ids already counted, so reprocessing never counts twice.
    #[serde(default)]
    counted: Vec<String>,
}

impl TrialState {
    /// Combine two copies of the counter (file and Keychain): never lose a meeting.
    fn merge(a: TrialState, b: TrialState) -> TrialState {
        let mut counted = a.counted;
        for id in b.counted {
            if !counted.contains(&id) {
                counted.push(id);
            }
        }
        let count = a.count.max(b.count).max(counted.len() as u32);
        let skip = counted.len().saturating_sub(COUNTED_CAP);
        TrialState { count, counted: counted.split_off(skip) }
    }
}

// ---------------------------------------------------------------- status

/// Why a stored licence stopped counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lapse {
    /// The provider rejected the key.
    Revoked,
    /// Not confirmed for longer than the offline grace period.
    NeedsValidation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseInfo {
    /// `••••ABCD`, never the whole key.
    pub masked_key: String,
    pub activated_at: DateTime<Utc>,
    pub last_validated: DateTime<Utc>,
    /// Days the licence keeps working without reaching the server.
    pub grace_days_left: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// A source build (or any build that doesn't enforce): nothing is gated.
    Open,
    Licensed,
    Trial,
    /// Trial used up and no valid licence: new recordings are blocked.
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub enforced: bool,
    pub free_meetings: u32,
    pub used: u32,
    /// A valid licence, if any.
    pub license: Option<LicenseInfo>,
    /// A stored licence that no longer counts, and why.
    pub lapse: Option<Lapse>,
}

impl Snapshot {
    pub fn left(&self) -> u32 {
        self.free_meetings.saturating_sub(self.used)
    }
    pub fn state(&self) -> State {
        if !self.enforced {
            State::Open
        } else if self.license.is_some() {
            State::Licensed
        } else if self.left() > 0 {
            State::Trial
        } else {
            State::Expired
        }
    }
    pub fn can_record(&self) -> bool {
        self.state() != State::Expired
    }
    /// One line for Settings.
    pub fn headline(&self) -> String {
        match self.state() {
            State::Open => "Not required in this build".into(),
            State::Licensed => "Licensed".into(),
            State::Trial => format!("Free trial · {} of {} meetings left", self.left(), self.free_meetings),
            State::Expired => "Free trial finished".into(),
        }
    }
    /// The reason recording is blocked, for the notice at record time.
    pub fn blocked_message(&self) -> Option<String> {
        if self.can_record() {
            return None;
        }
        Some(match self.lapse {
            Some(Lapse::Revoked) => "Your Stenly licence is no longer valid, so new recordings are paused. Your notes are untouched. See Settings → Licence.".to_string(),
            Some(Lapse::NeedsValidation) => "Stenly couldn't confirm your licence for over a month. Connect to the internet once and open Settings → Licence. Your notes are untouched.".to_string(),
            None => format!("You've used your {} free meetings. Buy a licence ($10, one Mac, updates included) to keep recording. Your notes, search and dictation keep working. See Settings → Licence.", self.free_meetings),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Checked recently (or nothing to check).
    Skipped,
    Validated,
    /// Couldn't reach the server; the grace period keeps the licence working.
    Offline,
    Revoked,
}

// ---------------------------------------------------------------- manager

pub struct LicenseManager {
    vault: Box<dyn Vault + Send + Sync>,
    trial_file: PathBuf,
    enforced: bool,
    free_meetings: u32,
}

const LICENSE_ITEM: &str = "license";
const TRIAL_ITEM: &str = "trial";

impl LicenseManager {
    /// The app's real storage: Keychain plus `trial.json` in the data dir.
    pub fn system() -> Self {
        Self { vault: Box::new(KeychainVault), trial_file: crate::config::data_dir().join("trial.json"), enforced: enforced(), free_meetings: FREE_MEETINGS }
    }

    pub fn new(vault: Box<dyn Vault + Send + Sync>, trial_file: PathBuf, enforced: bool, free_meetings: u32) -> Self {
        Self { vault, trial_file, enforced, free_meetings }
    }

    fn read_trial(&self) -> TrialState {
        let file = std::fs::read_to_string(&self.trial_file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let kept = self.vault.get(TRIAL_ITEM).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        TrialState::merge(file, kept)
    }

    fn write_trial(&self, t: &TrialState) {
        let json = serde_json::to_string(t).unwrap_or_default();
        if let Some(dir) = self.trial_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&self.trial_file, &json) {
            tracing::warn!(error = %e, "couldn't save the trial counter file");
        }
        if let Err(e) = self.vault.set(TRIAL_ITEM, &json) {
            tracing::warn!(error = %format!("{e:#}"), "couldn't save the trial counter to the Keychain");
        }
    }

    /// Count a finished meeting against the trial. Idempotent per `session_id`;
    /// accidental short recordings don't count. Returns the new count.
    pub fn record_meeting(&self, session_id: &str, secs: i64) -> u32 {
        let mut t = self.read_trial();
        if secs >= MIN_COUNTED_SECS && !t.counted.iter().any(|c| c == session_id) {
            t.counted.push(session_id.to_string());
            t.count += 1;
            let skip = t.counted.len().saturating_sub(COUNTED_CAP);
            t.counted.drain(..skip);
            self.write_trial(&t);
        } else if !self.trial_file.exists() || self.vault.get(TRIAL_ITEM).is_none() {
            // Heal a missing copy from the other one.
            self.write_trial(&t);
        }
        t.count
    }

    fn read_license(&self) -> Option<StoredLicense> {
        serde_json::from_str(&self.vault.get(LICENSE_ITEM)?).ok()
    }

    fn write_license(&self, l: &StoredLicense) -> Result<()> {
        self.vault.set(LICENSE_ITEM, &serde_json::to_string(l)?)
    }

    pub fn snapshot(&self, now: DateTime<Utc>) -> Snapshot {
        let used = self.read_trial().count;
        let (license, lapse) = match self.read_license() {
            None => (None, None),
            Some(l) if l.revoked => (None, Some(Lapse::Revoked)),
            Some(l) => {
                // A clock set back never extends the grace: age is clamped at 0
                // and a future last_validated simply counts as "just now".
                let age = (now - l.last_validated).max(Duration::zero());
                let left = GRACE_DAYS - age.num_days();
                if age > Duration::days(GRACE_DAYS) {
                    (None, Some(Lapse::NeedsValidation))
                } else {
                    (Some(LicenseInfo { masked_key: mask(&l.key), activated_at: l.activated_at, last_validated: l.last_validated, grace_days_left: left }), None)
                }
            }
        };
        Snapshot { enforced: self.enforced, free_meetings: self.free_meetings, used, license, lapse }
    }

    /// Activate `key` on this Mac and keep the licence.
    pub fn activate(&self, provider: &dyn Provider, key: &str, now: DateTime<Utc>) -> Result<Snapshot, LicenseError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(LicenseError::Rejected("Enter your licence key.".into()));
        }
        // Re-entering the key already active here must not use a second slot.
        if let Some(l) = self.read_license()
            && l.key == key
            && !l.revoked
            && provider.validate(&l.key, &l.activation_id).is_ok()
        {
            let renewed = StoredLicense { last_validated: now, ..l };
            self.write_license(&renewed).map_err(|e| LicenseError::Offline(format!("{e:#}")))?;
            return Ok(self.snapshot(now));
        }
        let a = provider.activate(key, &machine_label())?;
        let l = StoredLicense { key: key.to_string(), activation_id: a.activation_id, activated_at: now, last_validated: now, revoked: false };
        if let Err(e) = self.write_license(&l) {
            // Don't strand a used slot we can't remember.
            let _ = provider.deactivate(&l.key, &l.activation_id);
            return Err(LicenseError::Offline(format!("couldn't save the licence to the Keychain: {e:#}")));
        }
        Ok(self.snapshot(now))
    }

    /// Release this Mac's slot and forget the licence. If the server can't be
    /// reached the licence is kept and the error returned, so the slot isn't
    /// silently stranded.
    pub fn deactivate(&self, provider: &dyn Provider, now: DateTime<Utc>) -> Result<Snapshot, LicenseError> {
        if let Some(l) = self.read_license() {
            match provider.deactivate(&l.key, &l.activation_id) {
                // Already gone on the server is fine.
                Ok(()) | Err(LicenseError::Rejected(_)) => {}
                Err(e) => return Err(e),
            }
            self.vault.set(LICENSE_ITEM, "").map_err(|e| LicenseError::Offline(format!("{e:#}")))?;
        }
        Ok(self.snapshot(now))
    }

    /// Re-check the licence if it hasn't been confirmed for [`REVALIDATE_DAYS`]
    /// (or `force`). Only a definite rejection revokes; network trouble never does.
    pub fn refresh(&self, provider: &dyn Provider, now: DateTime<Utc>, force: bool) -> Refresh {
        let Some(l) = self.read_license() else { return Refresh::Skipped };
        if l.revoked {
            return Refresh::Skipped;
        }
        if !force && now - l.last_validated < Duration::days(REVALIDATE_DAYS) {
            return Refresh::Skipped;
        }
        match provider.validate(&l.key, &l.activation_id) {
            Ok(()) => {
                let _ = self.write_license(&StoredLicense { last_validated: now, ..l });
                Refresh::Validated
            }
            Err(LicenseError::Rejected(_)) => {
                let _ = self.write_license(&StoredLicense { revoked: true, ..l });
                Refresh::Revoked
            }
            Err(_) => Refresh::Offline,
        }
    }
}

fn mask(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("••••{tail}")
}

/// Names this Mac in the provider's dashboard without sending its hostname.
fn machine_label() -> String {
    let mut b = [0u8; 3];
    let _ = getrandom::fill(&mut b);
    format!("Stenly {} on macOS ({:02x}{:02x}{:02x})", env!("CARGO_PKG_VERSION"), b[0], b[1], b[2])
}

#[cfg(test)]
mod tests;
