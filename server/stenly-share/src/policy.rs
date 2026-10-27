//! Server policy the admin can change at runtime: default TTL, max size and
//! allowed email domains. Environment variables give the defaults; values
//! saved from `/admin` (the `settings` table) override them. Kept in memory
//! behind a lock so every request reads it cheaply.

use std::sync::RwLock;

use toasty::Db;

use crate::{config::Config, model::Setting};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub default_ttl_days: i64,
    pub max_bytes: usize,
    /// Lowercase domains (`example.com`). Empty: anyone the IdP signs in.
    pub allowed_email_domains: Vec<String>,
}

impl Policy {
    pub fn from_config(cfg: &Config) -> Self {
        Self { default_ttl_days: cfg.default_ttl_days, max_bytes: cfg.max_bytes, allowed_email_domains: cfg.allowed_email_domains.clone() }
    }

    /// Whether `email` may sign in (company mode).
    pub fn email_allowed(&self, email: &str) -> bool {
        if self.allowed_email_domains.is_empty() {
            return true;
        }
        let Some((_, domain)) = email.rsplit_once('@') else { return false };
        let domain = domain.to_lowercase();
        self.allowed_email_domains.contains(&domain)
    }
}

/// Shared, mutable policy (app context).
#[derive(Debug)]
pub struct PolicyCell(RwLock<Policy>);

impl PolicyCell {
    pub fn new(p: Policy) -> Self {
        Self(RwLock::new(p))
    }
    pub fn get(&self) -> Policy {
        self.0.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
    fn set(&self, p: Policy) {
        *self.0.write().unwrap_or_else(std::sync::PoisonError::into_inner) = p;
    }
}

const TTL: &str = "default_ttl_days";
const MAX: &str = "max_bytes";
const DOMAINS: &str = "allowed_email_domains";

/// Env defaults overlaid with saved settings.
pub async fn load(db: &mut Db, cfg: &Config) -> toasty::Result<Policy> {
    let mut p = Policy::from_config(cfg);
    for s in Setting::all().exec(db).await? {
        match s.key.as_str() {
            TTL => {
                if let Ok(v) = s.value.parse() {
                    p.default_ttl_days = v;
                }
            }
            MAX => {
                if let Ok(v) = s.value.parse() {
                    p.max_bytes = v;
                }
            }
            // A security setting: never fall back to "anyone" on bad data.
            DOMAINS => {
                p.allowed_email_domains =
                    parse_domains(&s.value).map_err(|e| toasty::Error::from_args(format_args!("invalid saved allowed_email_domains: {e}")))?;
            }
            _ => {}
        }
    }
    Ok(p)
}

/// Parses `a.com, b.org` into lowercase domains, rejecting junk.
pub fn parse_domains(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for d in s.split([',', ' ', '\n']).map(|d| d.trim().trim_start_matches('@').to_lowercase()).filter(|d| !d.is_empty()) {
        if !d.contains('.') || !d.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
            return Err(format!("{d:?} is not a domain like example.com"));
        }
        if !out.contains(&d) {
            out.push(d);
        }
    }
    Ok(out)
}

/// Validates and saves a new policy, then publishes it.
pub async fn save(db: &mut Db, cfg: &Config, cell: &PolicyCell, new: Policy) -> Result<Policy, String> {
    if !(1..=cfg.max_ttl_days).contains(&new.default_ttl_days) {
        return Err(format!("default expiry must be 1–{} days", cfg.max_ttl_days));
    }
    if !(1024..=64 * 1024 * 1024).contains(&new.max_bytes) {
        return Err("max size must be between 1 KiB and 64 MiB".into());
    }
    let mut tx = db.transaction().await.map_err(|e| e.to_string())?;
    for (k, v) in [(TTL, new.default_ttl_days.to_string()), (MAX, new.max_bytes.to_string()), (DOMAINS, new.allowed_email_domains.join(","))] {
        toasty::sql::statement("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(k.to_owned())
            .bind(v)
            .exec(&mut tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    cell.set(new.clone());
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bad_saved_domains_fail_closed_and_save_is_atomic() {
        let cfg = Config::default();
        let mut db = crate::store::open_url("sqlite::memory:").await.unwrap();
        let cell = PolicyCell::new(Policy::from_config(&cfg));
        let new = Policy { default_ttl_days: 7, max_bytes: 4096, allowed_email_domains: vec!["acme.test".into()] };
        save(&mut db, &cfg, &cell, new.clone()).await.unwrap();
        save(&mut db, &cfg, &cell, new.clone()).await.unwrap(); // upsert, no duplicate keys
        assert_eq!(load(&mut db, &cfg).await.unwrap(), new);
        toasty::sql::statement("UPDATE settings SET value = 'not a domain!' WHERE key = ?1").bind(DOMAINS.to_owned()).exec(&mut db).await.unwrap();
        assert!(load(&mut db, &cfg).await.is_err(), "must not fall back to an empty (open) allow-list");
    }

    #[test]
    fn domains() {
        assert_eq!(parse_domains(" Acme.com, @beta.io  acme.com").unwrap(), vec!["acme.com", "beta.io"]);
        assert!(parse_domains("not a domain!").is_err());
        let p = Policy { default_ttl_days: 1, max_bytes: 1, allowed_email_domains: vec!["acme.com".into()] };
        assert!(p.email_allowed("ana@ACME.com"));
        assert!(!p.email_allowed("ana@acme.com.evil.io"));
        assert!(!p.email_allowed("nobody"));
        let open = Policy { allowed_email_domains: vec![], ..p };
        assert!(open.email_allowed("x@y.z"));
    }
}
