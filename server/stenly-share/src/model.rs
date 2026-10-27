//! Toasty models. The server stores ciphertext plus the minimum metadata it
//! needs to serve, expire and revoke a share. Timestamps are unix seconds.

/// One share link. `title` is only present when the sharer opted in to a
/// plaintext title for link previews.
#[derive(Debug, Clone, toasty::Model)]
#[table = "shares"]
pub struct Share {
    #[key]
    pub id: String,
    /// Hex SHA-256 of the owner token; the token itself is never stored.
    pub owner_hash: String,
    pub title: Option<String>,
    /// Envelope size in bytes.
    pub size: i64,
    pub created_at: i64,
    pub updated_at: i64,
    #[index]
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    /// Ciphertext fetches through the API (the viewer fetches once per open).
    pub views: i64,
    /// Set once the sweeper has deleted the blob of an expired/revoked share.
    pub purged: bool,
    /// Company mode: the user whose personal upload token (or session)
    /// created the share. `None` for anonymous / shared-token uploads.
    #[index]
    pub owner_id: Option<String>,
}

/// The envelope bytes, kept apart from metadata so listing shares never
/// loads ciphertext.
#[derive(Debug, Clone, toasty::Model)]
#[table = "share_blobs"]
pub struct ShareBlob {
    #[key]
    pub share_id: String,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Active,
    Revoked,
    Expired,
}

impl Share {
    pub fn state(&self, now: i64) -> State {
        if self.revoked_at.is_some() {
            State::Revoked
        } else if self.expires_at <= now {
            State::Expired
        } else {
            State::Active
        }
    }
}

/// A signed-in person (company mode, OIDC). Created on first sign-in.
#[derive(Debug, Clone, toasty::Model)]
#[table = "users"]
pub struct User {
    #[key]
    pub id: String,
    #[unique]
    pub email: String,
    pub name: Option<String>,
    pub admin: bool,
    pub created_at: i64,
    pub last_login_at: i64,
}

/// Login session; stores only the token hash (see `topcoat::session`).
#[derive(Debug, Clone, toasty::Model)]
#[table = "sessions"]
pub struct Session {
    #[key]
    pub token_hash: Vec<u8>,
    #[index]
    pub user_id: String,
    pub expires_at: i64,
}

/// A personal upload token a signed-in user created for the desktop app.
#[derive(Debug, Clone, toasty::Model)]
#[table = "upload_tokens"]
pub struct UploadToken {
    #[key]
    pub id: String,
    #[unique]
    pub token_hash: String,
    #[index]
    pub user_id: String,
    pub label: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// Admin-edited server settings (override the environment defaults).
#[derive(Debug, Clone, toasty::Model)]
#[table = "settings"]
pub struct Setting {
    #[key]
    pub key: String,
    pub value: String,
}

/// Ciphertext fetches per UTC day, for the admin stats.
#[derive(Debug, Clone, toasty::Model)]
#[table = "view_days"]
pub struct ViewDay {
    #[key]
    pub day: i64,
    pub views: i64,
}
