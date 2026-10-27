//! Company mode: users, sessions, admins, and the guards every route calls.
//!
//! With OIDC configured, viewing a share (the `/s/{id}` page and the
//! ciphertext API) needs a signed-in user from an allowed email domain.
//! Uploads need the shared upload token or a signed-in user's personal
//! upload token (or only the latter with `STENLY_SHARE_UPLOAD_AUTH=user`).
//! Without OIDC everything stays anonymous, as in contract v1.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, UNIX_EPOCH},
};

use topcoat::{
    context::{Cx, app_context, memoize},
    router::{StatusCode, request::headers},
    session,
};

use crate::{
    api::{ApiError, ApiResult},
    config::{Config, UploadAuth},
    model::{Session, UploadToken, User},
    oidc,
    policy::PolicyCell,
    share,
};

/// Signed-in session lifetime.
pub const SESSION_LIFETIME: Duration = Duration::from_secs(14 * 86_400);

/// The current user, if a valid session cookie came with the request.
#[memoize(as_ref)]
pub async fn current_user(cx: &Cx) -> topcoat::Result<Option<User>> {
    if !app_context::<Config>(cx).company_mode() {
        return Ok(None);
    }
    let Some(hash) = session::token_hash(cx).await? else { return Ok(None) };
    let mut db = hearth::db(cx);
    let Some(s) = Session::filter_by_token_hash(hash.to_vec()).first().exec(&mut db).await? else { return Ok(None) };
    if s.expires_at < share::now() {
        return Ok(None);
    }
    let Some(user) = User::filter_by_id(s.user_id).first().exec(&mut db).await? else { return Ok(None) };
    // A domain removed by an admin locks its users out immediately.
    if !app_context::<PolicyCell>(cx).get().email_allowed(&user.email) {
        return Ok(None);
    }
    Ok(Some(user))
}

/// The signed-in user. Database errors propagate (5xx) rather than
/// looking like "signed out".
pub async fn user(cx: &Cx) -> topcoat::Result<Option<User>> {
    current_user(cx).await.cloned().map_err(Clone::clone)
}

/// Guard for reads in company mode: the share viewer and ciphertext API.
pub async fn require_viewer(cx: &Cx) -> ApiResult<Option<User>> {
    if !app_context::<Config>(cx).company_mode() {
        return Ok(None);
    }
    match user(cx).await.map_err(|e| ApiError::internal(format!("{e:?}")))? {
        Some(u) => Ok(Some(u)),
        None => Err(ApiError::new(StatusCode::UNAUTHORIZED, "sign_in_required", "sign in to view notes shared on this server")),
    }
}

/// Who is uploading: `Some(user)` for personal tokens/sessions, `None` for
/// anonymous or shared-token uploads.
pub async fn require_uploader(cx: &Cx) -> ApiResult<Option<User>> {
    let cfg = app_context::<Config>(cx);
    let bearer = headers(cx)
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().strip_prefix("Bearer ").or_else(|| v.trim().strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty());

    // Personal upload token (company mode).
    if cfg.company_mode()
        && let Some(t) = bearer
        && let Some(u) = personal_token_user(cx, t).await?
    {
        return Ok(Some(u));
    }
    // A signed-in browser session also counts as a user.
    if let Some(u) = user(cx).await.map_err(|e| ApiError::internal(format!("{e:?}")))? {
        return Ok(Some(u));
    }
    let only_users = cfg.company_mode() && cfg.upload_auth == UploadAuth::User;
    match (&cfg.upload_token, bearer) {
        (Some(expected), Some(t)) if !only_users && share::ct_eq(t.as_bytes(), expected.as_bytes()) => Ok(None),
        (None, _) if !only_users => Ok(None),
        (_, Some(_)) if only_users => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "this server needs a personal upload token: sign in on the server and create one under Upload tokens",
        )),
        (_, Some(_)) => Err(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", "the upload token is wrong")),
        (_, None) => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            if only_users {
                "this server needs a personal upload token: sign in on the server and create one under Upload tokens"
            } else {
                "this server needs an upload token (Authorization: Bearer …)"
            },
        )),
    }
}

async fn personal_token_user(cx: &Cx, token: &str) -> ApiResult<Option<User>> {
    let mut db = hearth::db(cx);
    let Some(mut t) = UploadToken::filter_by_token_hash(share::token_hash(token)).first().exec(&mut db).await? else { return Ok(None) };
    let Some(u) = User::filter_by_id(t.user_id.clone()).first().exec(&mut db).await? else { return Ok(None) };
    if !app_context::<PolicyCell>(cx).get().email_allowed(&u.email) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", "this account's email domain is no longer allowed on this server"));
    }
    t.update().last_used_at(Some(share::now())).exec(&mut db).await?;
    Ok(Some(u))
}

// ---------------------------------------------------------------- sign-in

/// In-memory pending logins (state → PKCE verifier + nonce), 10 min TTL.
#[derive(Debug, Default)]
pub struct PendingLogins(Mutex<HashMap<String, oidc::Pending>>);

impl PendingLogins {
    pub fn put(&self, state: String, p: oidc::Pending) {
        let mut m = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = share::now();
        m.retain(|_, v| now - v.created < 600);
        // Bounded, but a flood can't block real logins: evict the oldest.
        while m.len() >= 10_000 {
            let Some(oldest) = m.iter().min_by_key(|(_, v)| v.created).map(|(k, _)| k.clone()) else { break };
            m.remove(&oldest);
        }
        m.insert(state, p);
    }
    pub fn take(&self, state: &str) -> Option<oidc::Pending> {
        let p = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(state)?;
        (share::now() - p.created < 600).then_some(p)
    }
}

/// Cached OIDC discovery document.
#[derive(Debug, Default)]
pub struct DiscoveryCache(Mutex<Option<(i64, oidc::Discovery)>>);

pub async fn discovery(cx: &Cx) -> ApiResult<oidc::Discovery> {
    let cache = app_context::<DiscoveryCache>(cx);
    if let Some((at, d)) = cache.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
        && share::now() - at < 3600
    {
        return Ok(d);
    }
    let issuer = app_context::<Config>(cx).oidc.as_ref().map(|o| o.issuer.clone()).ok_or_else(ApiError::not_found)?;
    let d = tokio::task::spawn_blocking(move || oidc::discover(&issuer)).await.map_err(ApiError::internal)?.map_err(|e| {
        eprintln!("stenly-share: OIDC discovery failed: {e}");
        ApiError::new(StatusCode::BAD_GATEWAY, "idp_unavailable", "couldn't reach the sign-in provider")
    })?;
    *cache.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((share::now(), d.clone()));
    Ok(d)
}

/// Finds or creates the user for a verified identity and starts a session.
/// The first user ever, and anyone in `STENLY_SHARE_ADMINS`, is an admin.
pub async fn sign_in(cx: &Cx, id: &oidc::Identity) -> ApiResult<User> {
    let cfg = app_context::<Config>(cx);
    if !app_context::<PolicyCell>(cx).get().email_allowed(&id.email) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "domain_not_allowed", format!("{} isn't allowed on this server", id.email)));
    }
    let mut db = hearth::db(cx);
    let listed_admin = cfg.admins.contains(&id.email);
    let now = share::now();
    let user = match User::filter_by_email(id.email.clone()).first().exec(&mut db).await? {
        Some(mut u) => {
            let admin = u.admin || listed_admin;
            let name = id.name.clone().or(u.name.clone());
            u.update().last_login_at(now).admin(admin).name(name).exec(&mut db).await?;
            u
        }
        None => {
            let first = User::all().first().exec(&mut db).await?.is_none();
            toasty::create!(User {
                id: share::new_id(),
                email: id.email.clone(),
                name: id.name.clone(),
                admin: first || listed_admin,
                created_at: now,
                last_login_at: now,
            })
            .exec(&mut db)
            .await?
        }
    };
    let s = session::start(cx).await.map_err(|e| ApiError::internal(format!("{e:?}")))?;
    let expires_at = s.expires_at.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(now);
    toasty::create!(Session { token_hash: s.token_hash.to_vec(), user_id: user.id.clone(), expires_at }).exec(&mut db).await?;
    Ok(user)
}

pub async fn sign_out(cx: &Cx) -> topcoat::Result<()> {
    if let Some(hash) = session::stop(cx).await? {
        Session::filter_by_token_hash(hash.to_vec()).delete().exec(&mut hearth::db(cx)).await?;
    }
    Ok(())
}

/// Only same-site relative paths are valid post-login destinations.
pub fn safe_return_to(r: &str) -> String {
    if r.starts_with('/') && !r.starts_with("//") && !r.starts_with("/\\") && r.len() < 512 { r.to_owned() } else { "/".to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_logins_evict_oldest_when_full() {
        let p = PendingLogins::default();
        let (first, pending) = oidc::new_pending("/");
        p.put(first.clone(), oidc::Pending { created: share::now() - 100, ..pending });
        for i in 0..10_000 {
            p.put(format!("flood-{i}"), oidc::new_pending("/").1);
        }
        // A flood pushes out the oldest, never the newest (a real user's).
        let (real, pending) = oidc::new_pending("/s/x");
        p.put(real.clone(), pending);
        assert!(p.take(&real).is_some());
        assert!(p.take(&first).is_none());
    }

    #[test]
    fn return_to_is_local_only() {
        assert_eq!(safe_return_to("/s/abc"), "/s/abc");
        assert_eq!(safe_return_to("//evil.com"), "/");
        assert_eq!(safe_return_to("https://evil.com"), "/");
        assert_eq!(safe_return_to("/\\evil.com"), "/");
    }
}
