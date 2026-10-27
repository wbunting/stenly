//! Server configuration, read once from `STENLY_SHARE_*` environment variables.

use std::{env, path::PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    /// Public origin used to build share URLs, e.g. `https://share.stenly.ai`
    /// (no trailing slash). `None`: derived from the request's Host header.
    pub base_url: Option<String>,
    pub data_dir: PathBuf,
    /// Largest accepted envelope (decoded bytes).
    pub max_bytes: usize,
    pub default_ttl_days: i64,
    /// Longest expiry a client may ask for; longer requests are clamped.
    pub max_ttl_days: i64,
    /// When set, `POST`/`PUT` need `Authorization: Bearer <token>`.
    pub upload_token: Option<String>,
    /// Write requests (create/replace/revoke) per IP per minute. 0 disables.
    pub write_rate_per_min: u32,
    /// Read requests (fetch ciphertext, viewer) per IP per minute. 0 disables.
    pub read_rate_per_min: u32,
    /// Company mode: sign-in starts per IP per minute. 0 disables. An office
    /// behind one NAT shares this; raise it (or set trusted proxies) there.
    pub auth_rate_per_min: u32,
    /// Reverse proxies (CIDRs) whose `X-Forwarded-For` is trusted for the
    /// client IP used by rate limiting.
    pub trusted_proxies: Vec<String>,
    /// How long a revoked/expired share keeps answering 410 before its
    /// metadata row is purged (after which it is 404).
    pub tombstone_days: i64,
    /// Sweep interval in seconds.
    pub sweep_secs: u64,
    /// Company mode: sign-in through an OpenID Connect provider.
    pub oidc: Option<Oidc>,
    /// Emails allowed to sign in, by domain (`example.com`). Empty: any
    /// account the IdP accepts. Admins can change this at runtime.
    pub allowed_email_domains: Vec<String>,
    /// Emails that are always admins (`STENLY_SHARE_ADMINS`). The first user
    /// to sign in also becomes an admin.
    pub admins: Vec<String>,
    /// Company mode upload rule: `token` (default) accepts the shared
    /// `STENLY_SHARE_UPLOAD_TOKEN` or a user's personal token; `user` accepts
    /// only personal tokens / sessions, so every share has an owner.
    pub upload_auth: UploadAuth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UploadAuth {
    /// The shared upload token (if set) or any signed-in user's token.
    #[default]
    Token,
    /// Only signed-in users (personal upload tokens).
    User,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: None,
            data_dir: PathBuf::from("data"),
            max_bytes: 2 * 1024 * 1024,
            default_ttl_days: 90,
            max_ttl_days: 365,
            upload_token: None,
            write_rate_per_min: 30,
            read_rate_per_min: 300,
            auth_rate_per_min: 30,
            trusted_proxies: Vec::new(),
            tombstone_days: 30,
            sweep_secs: 600,
            oidc: None,
            allowed_email_domains: Vec::new(),
            admins: Vec::new(),
            upload_auth: UploadAuth::Token,
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let d = Self::default();
        let cfg = Self {
            base_url: var("STENLY_SHARE_BASE_URL").map(|u| u.trim_end_matches('/').to_owned()),
            data_dir: var("STENLY_SHARE_DATA_DIR").map(PathBuf::from).unwrap_or(d.data_dir),
            max_bytes: num("STENLY_SHARE_MAX_BYTES")?.unwrap_or(d.max_bytes),
            default_ttl_days: num("STENLY_SHARE_DEFAULT_TTL_DAYS")?.unwrap_or(d.default_ttl_days),
            max_ttl_days: num("STENLY_SHARE_MAX_TTL_DAYS")?.unwrap_or(d.max_ttl_days),
            upload_token: var("STENLY_SHARE_UPLOAD_TOKEN"),
            write_rate_per_min: num("STENLY_SHARE_WRITE_RATE_PER_MIN")?.unwrap_or(d.write_rate_per_min),
            read_rate_per_min: num("STENLY_SHARE_READ_RATE_PER_MIN")?.unwrap_or(d.read_rate_per_min),
            auth_rate_per_min: num("STENLY_SHARE_AUTH_RATE_PER_MIN")?.unwrap_or(d.auth_rate_per_min),
            trusted_proxies: var("STENLY_SHARE_TRUSTED_PROXIES")
                .map(|v| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default(),
            tombstone_days: num("STENLY_SHARE_TOMBSTONE_DAYS")?.unwrap_or(d.tombstone_days),
            sweep_secs: num("STENLY_SHARE_SWEEP_SECS")?.unwrap_or(d.sweep_secs),
            oidc: match (var("STENLY_SHARE_OIDC_ISSUER"), var("STENLY_SHARE_OIDC_CLIENT_ID"), var("STENLY_SHARE_OIDC_CLIENT_SECRET")) {
                (Some(issuer), Some(client_id), Some(client_secret)) => {
                    Some(Oidc { issuer: issuer.trim_end_matches('/').to_owned(), client_id, client_secret })
                }
                (None, None, None) => None,
                _ => return Err("set all of STENLY_SHARE_OIDC_ISSUER, STENLY_SHARE_OIDC_CLIENT_ID and STENLY_SHARE_OIDC_CLIENT_SECRET (or none)".into()),
            },
            allowed_email_domains: list("STENLY_SHARE_ALLOWED_EMAIL_DOMAINS").into_iter().map(|d| d.trim_start_matches('@').to_owned()).collect(),
            admins: list("STENLY_SHARE_ADMINS"),
            upload_auth: match var("STENLY_SHARE_UPLOAD_AUTH").as_deref() {
                None | Some("token") => UploadAuth::Token,
                Some("user") => UploadAuth::User,
                Some(other) => return Err(format!("STENLY_SHARE_UPLOAD_AUTH must be `token` or `user`, got {other:?}")),
            },
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_bytes < 64 {
            return Err("STENLY_SHARE_MAX_BYTES must be at least 64".into());
        }
        if self.default_ttl_days < 1 || self.max_ttl_days < 1 || self.default_ttl_days > self.max_ttl_days {
            return Err("TTL days must be >= 1 and STENLY_SHARE_DEFAULT_TTL_DAYS <= STENLY_SHARE_MAX_TTL_DAYS".into());
        }
        if let Some(u) = &self.base_url
            && !(u.starts_with("https://") || u.starts_with("http://"))
        {
            return Err("STENLY_SHARE_BASE_URL must start with https:// or http://".into());
        }
        if let Some(o) = &self.oidc {
            if !(o.issuer.starts_with("https://") || o.issuer.starts_with("http://")) {
                return Err("STENLY_SHARE_OIDC_ISSUER must be an https:// URL".into());
            }
            if self.base_url.is_none() {
                return Err("company mode (OIDC) needs STENLY_SHARE_BASE_URL for the sign-in redirect URL".into());
            }
        }
        if self.upload_auth == UploadAuth::User && self.oidc.is_none() {
            return Err("STENLY_SHARE_UPLOAD_AUTH=user needs OIDC sign-in configured".into());
        }
        Ok(())
    }

    pub fn company_mode(&self) -> bool {
        self.oidc.is_some()
    }

    pub fn database_url(&self) -> String {
        format!("sqlite:{}", self.data_dir.join("stenly-share.db").display())
    }
}

fn var(name: &str) -> Option<String> {
    env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Comma-separated, lowercased, trimmed.
fn list(name: &str) -> Vec<String> {
    var(name).map(|v| v.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

fn num<T: std::str::FromStr>(name: &str) -> Result<Option<T>, String> {
    var(name).map(|v| v.parse().map_err(|_| format!("{name} must be a number, got {v:?}"))).transpose()
}
