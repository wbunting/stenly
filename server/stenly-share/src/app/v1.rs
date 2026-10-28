//! `/api/v1`: the share contract's HTTP API.

use serde::Serialize;
use hearth::{
    Result,
    context::{Cx, app_context},
    router::{Body, StatusCode, path_param, response::Response, route},
};

use crate::{
    api::{ApiError, ApiResult, Bucket, base_url, empty_response, json_response, owner_token, rate_limit, read_upload},
    auth,
    config::Config,
    model::State,
    policy::PolicyCell,
    share::{self, DAY},
    store,
};

path_param!(id);

#[derive(Serialize)]
struct Info {
    name: &'static str,
    version: &'static str,
    max_bytes: usize,
    default_ttl_days: i64,
    upload_token_required: bool,
    auth: &'static str,
}

#[route(GET "/api/v1/info")]
async fn info(cx: &Cx) -> Result<Response> {
    let cfg = app_context::<Config>(cx);
    let policy = app_context::<PolicyCell>(cx).get();
    Ok(json_response(
        StatusCode::OK,
        &Info {
            name: "stenly-share",
            version: env!("CARGO_PKG_VERSION"),
            max_bytes: policy.max_bytes,
            default_ttl_days: policy.default_ttl_days,
            upload_token_required: cfg.upload_token.is_some() || (cfg.company_mode() && cfg.upload_auth == crate::config::UploadAuth::User),
            auth: if cfg.company_mode() { "oidc" } else { "none" },
        },
    ))
}

#[derive(Serialize)]
struct Saved {
    id: String,
    url: String,
    owner_token: String,
    expires_at: String,
}

fn saved(cx: &Cx, cfg: &Config, id: &str, owner_token: &str, expires_at: i64) -> Saved {
    Saved { id: id.to_owned(), url: format!("{}/s/{id}", base_url(cx, cfg)), owner_token: owner_token.to_owned(), expires_at: share::rfc3339(expires_at) }
}

/// Runs an API handler and renders its contract error as JSON.
fn respond(r: ApiResult) -> Result<Response> {
    Ok(r.unwrap_or_else(ApiError::into_response))
}

#[route(POST "/api/v1/shares")]
async fn create(cx: &Cx, body: Body) -> Result<Response> {
    respond(create_inner(cx, body).await)
}

async fn create_inner(cx: &Cx, body: Body) -> ApiResult {
    rate_limit(cx, Bucket::Write)?;
    let cfg = app_context::<Config>(cx);
    let policy = app_context::<PolicyCell>(cx).get();
    let uploader = auth::require_uploader(cx).await?;
    let up = read_upload(cfg, policy.max_bytes, body).await?;
    let id = share::new_id();
    let token = share::new_owner_token();
    let expires_at = share::now() + up.ttl_days.unwrap_or(policy.default_ttl_days) * DAY;
    let mut db = hearth::db(cx);
    store::create(
        &mut db,
        store::NewShare {
            id: &id,
            owner_hash: share::token_hash(&token),
            title: up.title,
            envelope: up.envelope,
            expires_at,
            owner_id: uploader.map(|u| u.id),
        },
    )
    .await?;
    Ok(json_response(StatusCode::CREATED, &saved(cx, cfg, &id, &token, expires_at)))
}

/// Loads a share for an owner operation: 404 unknown, 403 wrong token,
/// 410 revoked/expired.
async fn owned(db: &mut toasty::Db, id: &str, token: &str, allow_gone: bool) -> ApiResult<crate::model::Share> {
    if !share::valid_id(id) {
        return Err(ApiError::not_found());
    }
    let s = store::find(db, id).await?.ok_or_else(ApiError::not_found)?;
    if !share::ct_eq(share::token_hash(token).as_bytes(), s.owner_hash.as_bytes()) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", "wrong owner token for this share"));
    }
    match s.state(share::now()) {
        State::Active => Ok(s),
        _ if allow_gone => Ok(s),
        state => Err(ApiError::gone(state == State::Revoked)),
    }
}

#[route(PUT "/api/v1/shares/{id}")]
async fn replace(cx: &Cx, body: Body) -> Result<Response> {
    respond(replace_inner(cx, body).await)
}

async fn replace_inner(cx: &Cx, body: Body) -> ApiResult {
    rate_limit(cx, Bucket::Write)?;
    let cfg = app_context::<Config>(cx);
    auth::require_uploader(cx).await?;
    let id = path_param::<Id>(cx).to_owned();
    let token = owner_token(cx)?;
    let mut db = hearth::db(cx);
    // Check ownership before reading a possibly large body; `store::replace`
    // re-checks that the share is still active when it writes.
    let existing = owned(&mut db, &id, token, false).await?;
    let up = read_upload(cfg, app_context::<PolicyCell>(cx).get().max_bytes, body).await?;
    // A re-share without an explicit TTL keeps the current expiry.
    let expires_at = up.ttl_days.map_or(existing.expires_at, |d| share::now() + d * DAY);
    let Some(s) = store::replace(&mut db, &id, up.envelope, up.title, expires_at).await? else {
        let now_revoked = store::find(&mut db, &id).await?.is_none_or(|s| s.revoked_at.is_some());
        return Err(ApiError::gone(now_revoked));
    };
    Ok(json_response(StatusCode::OK, &saved(cx, cfg, &s.id, token, s.expires_at)))
}

#[route(DELETE "/api/v1/shares/{id}")]
async fn revoke(cx: &Cx) -> Result<Response> {
    respond(revoke_inner(cx).await)
}

async fn revoke_inner(cx: &Cx) -> ApiResult {
    rate_limit(cx, Bucket::Write)?;
    let id = path_param::<Id>(cx).to_owned();
    let token = owner_token(cx)?;
    let mut db = hearth::db(cx);
    // Revoking an expired share is allowed (it drops nothing new but is
    // harmless); revoking twice is idempotent.
    let s = owned(&mut db, &id, token, true).await?;
    store::revoke(&mut db, s).await?;
    Ok(empty_response(StatusCode::NO_CONTENT))
}

#[derive(Serialize)]
struct Fetched {
    ciphertext: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    created_at: String,
    expires_at: String,
}

#[route(GET "/api/v1/shares/{id}")]
async fn fetch(cx: &Cx) -> Result<Response> {
    respond(fetch_inner(cx).await)
}

async fn fetch_inner(cx: &Cx) -> ApiResult {
    rate_limit(cx, Bucket::Read)?;
    auth::require_viewer(cx).await?;
    let id = path_param::<Id>(cx);
    if !share::valid_id(id) {
        return Err(ApiError::not_found());
    }
    let mut db = hearth::db(cx);
    let s = store::find(&mut db, id).await?.ok_or_else(ApiError::not_found)?;
    match s.state(share::now()) {
        State::Active => {}
        state => return Err(ApiError::gone(state == State::Revoked)),
    }
    let blob = store::blob(&mut db, id).await?.ok_or_else(|| ApiError::gone(false))?;
    store::count_view(&mut db, id).await?;
    let mut res = json_response(
        StatusCode::OK,
        &Fetched { ciphertext: share::b64(&blob), title: s.title, created_at: share::rfc3339(s.created_at), expires_at: share::rfc3339(s.expires_at) },
    );
    res.headers_mut().insert(hearth::router::header::CACHE_CONTROL, hearth::router::HeaderValue::from_static("no-store"));
    Ok(res)
}
