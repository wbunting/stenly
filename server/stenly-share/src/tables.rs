//! hearth synced tables for the admin view.
//!
//! `ShareRow` is the `shares` table as the admin's browser sees it: metadata
//! only (no ciphertext, no owner hash), readable by admins only (`scope`),
//! with two whitelisted writes (`revoked`, `expiresAt`) validated by
//! `update_share`. hearth derives the browser collection, the load/mutate
//! endpoints, SSR and optimistic writes from this.
//!
//! Never add a column that is, or is derived from, note contents.

use hearth::Table;
use serde::{Deserialize, Serialize};
use toasty::stmt::Expr;
use hearth::{
    Result,
    context::Cx,
    router::error::{bad_request, forbidden, not_found, unauthorized},
};

use crate::{
    auth,
    model::{Share, State, User},
    share::{self, DAY},
    store,
};

#[derive(Table, Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[table(name = "shares", model = Share, scope = admin_scope, writable(revoked, expires_at), update = update_share)]
pub struct ShareRow {
    #[key]
    pub id: String,
    /// Opt-in plaintext title (link previews), else `None`.
    pub title: Option<String>,
    #[index]
    pub created_at: i64,
    #[index]
    pub expires_at: i64,
    pub size: i64,
    pub views: i64,
    /// Computed from `revoked_at` (the browser writes a bool).
    #[computed]
    pub revoked: bool,
    pub owner_id: Option<String>,
}

impl From<Share> for ShareRow {
    fn from(s: Share) -> Self {
        Self {
            id: s.id,
            title: s.title,
            created_at: s.created_at,
            expires_at: s.expires_at,
            size: s.size,
            views: s.views,
            revoked: s.revoked_at.is_some(),
            owner_id: s.owner_id,
        }
    }
}

/// Admins see every share; nobody else sees any.
async fn admin_scope(cx: &Cx) -> Result<Option<Expr<bool>>> {
    require_admin(cx).await?;
    Ok(None)
}

pub async fn require_admin(cx: &Cx) -> Result<User> {
    match auth::user(cx).await? {
        Some(u) if u.admin => Ok(u),
        Some(_) => Err(forbidden().into()),
        None => Err(unauthorized().into()),
    }
}

/// Admin writes: revoke (one-way) and extend/shorten expiry.
async fn update_share(cx: &Cx, id: String, patch: ShareRowPatch) -> Result<ShareRow> {
    require_admin(cx).await?;
    let mut db = hearth::db(cx);
    let s = store::find(&mut db, &id).await?.ok_or_else(not_found)?;
    let s = match patch.revoked {
        Some(true) => {
            store::revoke(&mut db, s).await?;
            store::find(&mut db, &id).await?.ok_or_else(not_found)?
        }
        Some(false) if s.revoked_at.is_some() => return Err(bad_request("a revoked share can't be restored; its ciphertext is deleted").into()),
        _ => s,
    };
    if let Some(exp) = patch.expires_at {
        if s.state(share::now()) == State::Revoked || s.purged {
            return Err(bad_request("this share is gone; its ciphertext was deleted").into());
        }
        let cfg = hearth::context::app_context::<crate::config::Config>(cx);
        let now = share::now();
        if exp <= now || exp > now + cfg.max_ttl_days * DAY {
            return Err(bad_request(format!("expiry must be within the next {} days", cfg.max_ttl_days)).into());
        }
        let mut s = s.clone();
        s.update().expires_at(exp).exec(&mut db).await?;
    }
    Ok(store::find(&mut db, &id).await?.ok_or_else(not_found)?.into())
}

/// Users, for the owner column (admin-only, read-only).
#[derive(Table, Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[table(name = "users", model = User, scope = admin_scope)]
pub struct UserRow {
    #[key]
    pub id: String,
    #[index]
    pub email: String,
}

impl From<User> for UserRow {
    fn from(u: User) -> Self {
        Self { id: u.id, email: u.email }
    }
}

hearth::routes!(shares: ShareRow, users: UserRow);
