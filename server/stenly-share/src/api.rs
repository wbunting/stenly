//! JSON plumbing shared by the `/api/v1` routes: contract-shaped errors,
//! request parsing, auth headers and the per-IP rate limiter.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use hearth::{
    context::{Cx, app_context},
    router::{
        Body, StatusCode, header,
        request::{client_ip, headers},
        response::Response,
    },
};

use crate::{config::Config, share};

/// `{"error":"<code>","message":"…"}` with an HTTP status.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retry_after: Option<u64>,
}

pub type ApiResult<T = Response> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into(), retry_after: None }
    }
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no share with this id")
    }
    pub fn gone(revoked: bool) -> Self {
        if revoked {
            Self::new(StatusCode::GONE, "revoked", "this share was revoked by its owner")
        } else {
            Self::new(StatusCode::GONE, "expired", "this share has expired")
        }
    }
    pub fn too_large(max: usize) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", format!("the encrypted note is larger than this server's limit of {max} bytes"))
    }
    pub fn internal(err: impl std::fmt::Display) -> Self {
        eprintln!("stenly-share: internal error: {err}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal server error")
    }

    pub fn into_response(self) -> Response {
        let mut res = json_response(self.status, &ErrorBody { error: self.code, message: &self.message });
        if let Some(secs) = self.retry_after {
            res.headers_mut().insert(header::RETRY_AFTER, secs.into());
        }
        if self.status == StatusCode::UNAUTHORIZED {
            res.headers_mut().insert(header::WWW_AUTHENTICATE, header::HeaderValue::from_static("Bearer"));
        }
        res
    }
}

/// The one `ApiError` → hearth error mapping, for HTML routes (sign-in)
/// that call API helpers. Each status the HTML routes can meet keeps its
/// meaning; anything else is a logged 500.
impl From<ApiError> for hearth::Error {
    fn from(e: ApiError) -> Self {
        use hearth::router::error as err;
        match e.status {
            StatusCode::TOO_MANY_REQUESTS => err::too_many_requests(e.retry_after.unwrap_or(60)).into(),
            // The identity provider is unreachable: ask the browser to retry.
            StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE => {
                eprintln!("stenly-share: {}", e.message);
                err::service_unavailable(30).into()
            }
            StatusCode::NOT_FOUND => err::not_found().into(),
            StatusCode::FORBIDDEN => err::forbidden().into(),
            StatusCode::BAD_REQUEST => err::bad_request(e.message).into(),
            status => {
                eprintln!("stenly-share: {status} {}: {}", e.code, e.message);
                err::internal_server_error(std::io::Error::other(e.message)).into()
            }
        }
    }
}

impl From<toasty::Error> for ApiError {
    fn from(e: toasty::Error) -> Self {
        Self::internal(e)
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: &'a str,
}

pub fn json_response(status: StatusCode, value: &impl Serialize) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut res = Response::new(Body::from(body));
    *res.status_mut() = status;
    res.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/json"));
    res
}

pub fn empty_response(status: StatusCode) -> Response {
    let mut res = Response::new(Body::empty());
    *res.status_mut() = status;
    res
}

/// Body of `POST /api/v1/shares` and `PUT /api/v1/shares/{id}`. Unknown
/// fields are ignored.
#[derive(Debug, Deserialize)]
pub struct UploadBody {
    pub ciphertext: String,
    #[serde(default)]
    pub expires_in_days: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
}

/// A validated upload: decoded envelope, clamped TTL and cleaned title.
pub struct Upload {
    pub envelope: Vec<u8>,
    /// `None` when the client didn't ask for a TTL.
    pub ttl_days: Option<i64>,
    pub title: Option<String>,
}

/// Reads and validates an upload body. Bodies are read with a limit derived
/// from `max_bytes` (base64 inflates by 4/3), so a huge request is cut off
/// early with 413 instead of being buffered.
pub async fn read_upload(cfg: &Config, max_bytes: usize, body: Body) -> ApiResult<Upload> {
    let limit = max_bytes / 3 * 4 + 8 + 16 * 1024;
    let bytes = hearth::router::to_bytes(body, limit).await.map_err(|e| {
        if e.downcast_ref::<hearth::router::error::ContentTooLargeError>().is_some() {
            ApiError::too_large(max_bytes)
        } else {
            ApiError::bad_request("bad_body", "couldn't read the request body")
        }
    })?;
    let body: UploadBody =
        serde_json::from_slice(&bytes).map_err(|e| ApiError::bad_request("bad_json", format!("body must be JSON like {{\"ciphertext\":\"…\"}}: {e}")))?;
    let envelope =
        share::b64_decode(&body.ciphertext).ok_or_else(|| ApiError::bad_request("bad_ciphertext", "ciphertext must be base64url without padding"))?;
    if envelope.len() > max_bytes {
        return Err(ApiError::too_large(max_bytes));
    }
    share::check_envelope(&envelope).map_err(|m| ApiError::bad_request("bad_ciphertext", m))?;
    let ttl_days = match body.expires_in_days {
        None => None,
        Some(d) if d < 1 => return Err(ApiError::bad_request("bad_expiry", "expires_in_days must be at least 1")),
        Some(d) => Some(d.min(cfg.max_ttl_days)),
    };
    Ok(Upload { envelope, ttl_days, title: share::clean_title(body.title.as_deref()) })
}

fn header_str<'a>(cx: &'a Cx, name: &str) -> Option<&'a str> {
    headers(cx).get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty())
}

/// `X-Stenly-Owner-Token`, required for PUT and DELETE.
pub fn owner_token(cx: &Cx) -> ApiResult<&str> {
    header_str(cx, "x-stenly-owner-token")
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "owner_token_required", "send the share's owner token in X-Stenly-Owner-Token"))
}

/// Public origin for share URLs: `STENLY_SHARE_BASE_URL`, else the request's
/// Host header (fine for local use; set the variable in production).
pub fn base_url(cx: &Cx, cfg: &Config) -> String {
    if let Some(u) = &cfg.base_url {
        return u.clone();
    }
    let host = header_str(cx, "host").filter(|h| h.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))).unwrap_or("localhost");
    format!("http://{host}")
}

// ---------------------------------------------------------------- rate limiting

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    Read,
    Write,
    /// Sign-in starts (company mode).
    Auth,
}

/// Fixed one-minute windows per (client IP, bucket). Small and in-memory:
/// the server is a single process. Behind a proxy, set
/// `STENLY_SHARE_TRUSTED_PROXIES` so the real client IP is used.
/// `(client, bucket) → (window start, requests in window)`.
type Windows = HashMap<(Option<IpAddr>, Bucket), (Instant, u32)>;

#[derive(Debug, Default)]
pub struct RateLimiter {
    windows: Mutex<Windows>,
}

const WINDOW: Duration = Duration::from_secs(60);

impl RateLimiter {
    pub fn check(&self, ip: Option<IpAddr>, bucket: Bucket, limit: u32) -> ApiResult<()> {
        if limit == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let mut map = self.windows.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.len() > 50_000 {
            map.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
        }
        let entry = map.entry((ip, bucket)).or_insert((now, 0));
        if now.duration_since(entry.0) >= WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= limit {
            let retry = WINDOW.saturating_sub(now.duration_since(entry.0)).as_secs().max(1);
            let mut err = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many requests; slow down and retry shortly");
            err.retry_after = Some(retry);
            return Err(err);
        }
        entry.1 += 1;
        Ok(())
    }

    pub fn prune(&self) {
        let now = Instant::now();
        self.windows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
    }
}

/// Composable guard: call at the top of every rate-limited handler.
pub fn rate_limit(cx: &Cx, bucket: Bucket) -> ApiResult<()> {
    let cfg = app_context::<Config>(cx);
    let limit = match bucket {
        Bucket::Read => cfg.read_rate_per_min,
        Bucket::Write => cfg.write_rate_per_min,
        Bucket::Auth => cfg.auth_rate_per_min,
    };
    app_context::<crate::ArcLimiter>(cx).0.check(client_ip(cx), bucket, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_keep_their_meaning_in_html_routes() {
        let status = |e: ApiError| {
            let cx = hearth::context::CxTestBuilder::new().build();
            hearth::router::response::IntoResponse::into_response(hearth::Error::from(e), &cx).unwrap().status()
        };
        let mut limited = ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "x");
        limited.retry_after = Some(5);
        assert_eq!(status(limited), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(status(ApiError::new(StatusCode::BAD_GATEWAY, "idp_unavailable", "x")), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(status(ApiError::not_found()), StatusCode::NOT_FOUND);
        assert_eq!(status(ApiError::internal("x")), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn limiter_blocks_after_limit() {
        let rl = RateLimiter::default();
        let ip = Some("10.0.0.1".parse().unwrap());
        for _ in 0..3 {
            rl.check(ip, Bucket::Write, 3).unwrap();
        }
        let err = rl.check(ip, Bucket::Write, 3).unwrap_err();
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(err.retry_after.unwrap() >= 1);
        // Other buckets and IPs are independent.
        rl.check(ip, Bucket::Read, 3).unwrap();
        rl.check(Some("10.0.0.2".parse().unwrap()), Bucket::Write, 3).unwrap();
    }
}
