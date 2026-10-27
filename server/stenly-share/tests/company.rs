//! Company mode (OIDC) integration tests: a real dev IdP on a local port,
//! the full authorization-code + PKCE flow, session cookies, viewer and API
//! gating, allowed domains, first-user admin, personal upload tokens, the
//! hearth admin table (load + mutate) and settings.

#![allow(clippy::result_large_err)]

mod common;

use std::{net::SocketAddr, sync::Arc};

use serde_json::{Value, json};
use stenly_share::{
    api::RateLimiter,
    build_router,
    config::{Config, Oidc, UploadAuth},
    policy::Policy,
    share, store,
};
use topcoat::router::{Body, Method, RemoteAddr, Router, StatusCode, to_bytes};

const VECTOR: &str = "AaChoqOkpaanqKmqq506Cg9_-i6dFgzzv2JY-vwkySpkssQ2DfJqU_Zdh1dsswQsm8BVPR9lvifoXR_wjWdoMikMtG8OHTBXMIlQ1tjdzKUGRIeZOWidVgcd55Jbwe3HNthABw";

#[derive(Debug)]
struct Res {
    status: StatusCode,
    headers: topcoat::router::HeaderMap,
    body: Vec<u8>,
}
impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn location(&self) -> String {
        self.headers.get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned()
    }
}

/// The dev IdP on a real socket (the server talks to it over HTTP).
async fn start_idp() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let router = stenly_share::dev_idp::router(issuer.clone(), "client".into(), "secret".into());
    tokio::spawn(async move { topcoat::serve_until(listener, router, std::future::pending()).await.unwrap() });
    issuer
}

struct App {
    router: Router,
    db: toasty::Db,
    issuer: String,
    _dir: std::path::PathBuf,
}

async fn app(f: impl FnOnce(&mut Config)) -> App {
    let issuer = start_idp().await;
    let dir = std::env::temp_dir().join(format!("stenly-share-co-{}", share::new_id()));
    let mut cfg = Config {
        data_dir: dir.clone(),
        base_url: Some("https://share.test".into()),
        oidc: Some(Oidc { issuer: issuer.clone(), client_id: "client".into(), client_secret: "secret".into() }),
        allowed_email_domains: vec!["acme.test".into()],
        admins: vec!["boss@acme.test".into()],
        upload_token: Some("team-token".into()),
        ..Config::default()
    };
    f(&mut cfg);
    cfg.validate().unwrap();
    let db = store::open(&cfg).await.unwrap();
    let assets = common::assets();
    let router = build_router(cfg.clone(), db.clone(), assets, Arc::new(RateLimiter::default()), Policy::from_config(&cfg));
    App { router, db, issuer, _dir: dir }
}

impl App {
    async fn req(&self, method: Method, path: &str, cookie: Option<&str>, headers: &[(&str, &str)], body: Option<String>) -> Res {
        let mut b = http::Request::builder().method(method).uri(path).header("host", "share.test");
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = b.body(body.map_or_else(Body::empty, Body::from)).unwrap();
        req.extensions_mut().insert(RemoteAddr("192.0.2.9:4000".parse::<SocketAddr>().unwrap()));
        let res = self.router.handle(req).await;
        let (status, headers) = (res.status(), res.headers().clone());
        Res { status, headers, body: to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec() }
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> Res {
        self.req(Method::GET, path, cookie, &[], None).await
    }

    /// Runs the whole OIDC flow for `email`; returns the session cookie.
    async fn sign_in(&self, email: &str, return_to: &str) -> Result<(String, String), Res> {
        let start = self.get(&format!("/auth/start?return_to={return_to}"), None).await;
        assert_eq!(start.status, StatusCode::SEE_OTHER, "{}", start.text());
        let auth_url = start.location();
        assert!(auth_url.starts_with(&self.issuer), "{auth_url}");
        assert!(auth_url.contains("code_challenge_method=S256") && auth_url.contains("nonce="));
        // "User" submits the IdP form.
        let query = auth_url.split_once('?').unwrap().1.to_owned();
        let form = format!("{query}&email={}&name=Test", email.replace('@', "%40"));
        let client = ureq::Agent::config_builder().max_redirects(0).http_status_as_error(false).build().new_agent();
        let issuer = self.issuer.clone();
        let loc = tokio::task::spawn_blocking(move || {
            let r = client.post(format!("{issuer}/authorize")).content_type("application/x-www-form-urlencoded").send(form).unwrap();
            r.headers().get("location").unwrap().to_str().unwrap().to_owned()
        })
        .await
        .unwrap();
        let cb = loc.strip_prefix("https://share.test").expect("redirect back to the server");
        let res = self.get(cb, None).await;
        if res.status != StatusCode::SEE_OTHER {
            return Err(res);
        }
        let cookie = res
            .headers
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .find(|c| c.contains("session="))
            .expect("session cookie");
        Ok((cookie, res.location()))
    }

    async fn upload(&self, token: &str) -> Res {
        self.req(
            Method::POST,
            "/api/v1/shares",
            None,
            &[("authorization", &format!("Bearer {token}")), ("content-type", "application/json")],
            Some(json!({"ciphertext": VECTOR, "title": "Board prep"}).to_string()),
        )
        .await
    }
}

#[tokio::test]
async fn info_reports_oidc() {
    let app = app(|_| {}).await;
    let v = app.get("/api/v1/info", None).await.json();
    assert_eq!(v["auth"], "oidc");
    assert_eq!(v["upload_token_required"], true);
}

#[tokio::test]
async fn viewing_requires_sign_in_from_allowed_domain() {
    let app = app(|_| {}).await;
    let id = app.upload("team-token").await.json()["id"].as_str().unwrap().to_owned();

    // Anonymous: API 401 (contract error shape), viewer shows the sign-in gate.
    let r = app.get(&format!("/api/v1/shares/{id}"), None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json()["error"], "sign_in_required");
    let r = app.get(&format!("/s/{id}"), None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.text().contains("Sign in to view this note") && r.text().contains(&format!("return_to=%2Fs%2F{id}")));

    // Wrong domain: refused at sign-in, no session.
    let err = app.sign_in("eve@evil.test", "/").await.unwrap_err();
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(err.text().contains("isn’t allowed here") || err.text().contains("isn&#x27;t") || err.text().contains("allowed"), "{}", err.text());

    // Allowed domain: full flow, back to the share, ciphertext readable.
    let (cookie, back) = app.sign_in("ana@acme.test", &format!("/s/{id}")).await.unwrap();
    assert_eq!(back, format!("/s/{id}"));
    let r = app.get(&format!("/api/v1/shares/{id}"), Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["ciphertext"], VECTOR);
    assert_eq!(app.get(&format!("/s/{id}"), Some(&cookie)).await.status, StatusCode::OK);

    // A state can't be replayed; logout ends the session.
    let r = app.req(Method::POST, "/auth/logout", Some(&cookie), &[], None).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(app.get(&format!("/api/v1/shares/{id}"), Some(&cookie)).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn first_user_and_listed_admins_are_admins() {
    let app = app(|_| {}).await;
    let (first, _) = app.sign_in("first@acme.test", "/admin").await.unwrap();
    let (second, _) = app.sign_in("second@acme.test", "/").await.unwrap();
    let (boss, _) = app.sign_in("boss@acme.test", "/").await.unwrap();

    assert_eq!(app.get("/admin", None).await.status, StatusCode::TEMPORARY_REDIRECT);
    let r = app.get("/admin", Some(&first)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("Server settings"));
    let csp = r.headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("script-src 'self';") && csp.contains("style-src 'self';"));
    assert!(!csp.contains("'unsafe-"), "{csp}");
    let html = r.text();
    assert!(html.contains(r#""on_unauthorized":{"redirect":"/auth/sign-in?return_to=%2Fadmin"}"#), "{html}");
    let account = app.get("/account", Some(&first)).await;
    assert_eq!(account.headers["content-security-policy"], r.headers["content-security-policy"]);
    assert!(app.get("/admin", Some(&second)).await.text().contains("Admins only"));
    assert!(app.get("/admin", Some(&boss)).await.text().contains("Server settings"));

    // The synced table endpoints enforce the same scope.
    assert_eq!(app.get("/_hearth/shares/load", Some(&second)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.get("/_hearth/shares/load", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.get("/_hearth/shares/load", Some(&first)).await.status, StatusCode::OK);
}

#[tokio::test]
async fn expired_admin_session_is_unauthorized_for_loads_and_mutations() {
    let app = app(|_| {}).await;
    let (cookie, _) = app.sign_in("ana@acme.test", "/admin").await.unwrap();
    toasty::sql::statement("UPDATE sessions SET expires_at = 1").exec(&mut app.db.clone()).await.unwrap();

    for path in ["/_hearth/shares/load", "/_hearth/users/load"] {
        assert_eq!(app.get(path, Some(&cookie)).await.status, StatusCode::UNAUTHORIZED);
    }
    let r = app
        .req(
            Method::POST,
            "/_hearth/shares/mutate",
            Some(&cookie),
            &[("content-type", "application/json")],
            Some(json!({"op":"update","key":"AAAAAAAAAAAAAAAAAAAAAA","changes":{"revoked":true}}).to_string()),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    for path in ["/admin", "/account"] {
        let r = app.get(path, Some(&cookie)).await;
        assert_eq!(r.status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(r.location(), stenly_share::app::sign_in_url(path));
    }
}

#[tokio::test]
async fn admin_table_lists_metadata_and_revokes_and_extends() {
    let app = app(|_| {}).await;
    let (admin, _) = app.sign_in("ana@acme.test", "/").await.unwrap();
    let id = app.upload("team-token").await.json()["id"].as_str().unwrap().to_owned();

    let rows = app.get("/_hearth/shares/load", Some(&admin)).await.json();
    let row = rows.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone();
    assert_eq!(row["title"], "Board prep");
    assert_eq!(row["revoked"], false);
    // Metadata only: no ciphertext or owner hash in the synced row.
    let keys: Vec<&str> = row.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys.len(), 8, "{keys:?}");
    assert!(!row.to_string().contains(VECTOR));
    assert!(!keys.iter().any(|k| k.contains("hash") || k.contains("cipher")));

    let mutate = |body: Value| app.req(Method::POST, "/_hearth/shares/mutate", Some(&admin), &[("content-type", "application/json")], Some(body.to_string()));
    let exp = row["expiresAt"].as_i64().unwrap() + 30 * 86_400;
    let r = mutate(json!({"op":"update","key":id,"changes":{"expiresAt":exp}})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["expiresAt"], exp);
    // Not whitelisted → rejected.
    let r = mutate(json!({"op":"update","key":id,"changes":{"views":0}})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Too far out → rejected.
    let r = mutate(json!({"op":"update","key":id,"changes":{"expiresAt": share::now() + 5000 * 86_400}})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Revoke deletes the ciphertext; un-revoke is refused.
    let r = mutate(json!({"op":"update","key":id,"changes":{"revoked":true}})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["revoked"], true);
    assert!(store::blob(&mut app.db.clone(), &id).await.unwrap().is_none());
    assert_eq!(app.get(&format!("/api/v1/shares/{id}"), Some(&admin)).await.status, StatusCode::GONE);
    let r = mutate(json!({"op":"update","key":id,"changes":{"revoked":false}})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn settings_change_policy_and_lock_out_domains() {
    let app = app(|_| {}).await;
    let (admin, _) = app.sign_in("ana@acme.test", "/").await.unwrap();
    let (other, _) = app.sign_in("bo@acme.test", "/").await.unwrap();
    let form = |b: &str| app.req(Method::POST, "/admin/settings", Some(&other), &[("content-type", "application/x-www-form-urlencoded")], Some(b.to_owned()));
    // Non-admins can't save settings.
    assert_eq!(form("default_ttl_days=7&max_kib=10&allowed_email_domains=acme.test").await.status, StatusCode::FORBIDDEN);
    let r = app
        .req(
            Method::POST,
            "/admin/settings",
            Some(&admin),
            &[("content-type", "application/x-www-form-urlencoded")],
            Some("default_ttl_days=7&max_kib=4&allowed_email_domains=acme.test%2C+beta.test".into()),
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let info = app.get("/api/v1/info", None).await.json();
    assert_eq!(info["default_ttl_days"], 7);
    assert_eq!(info["max_bytes"], 4096);
    // New max size applies to uploads; new domain can sign in.
    let big = format!("{{\"ciphertext\":\"{}\"}}", share::b64(&[1u8; 5000]));
    let r = app.req(Method::POST, "/api/v1/shares", None, &[("authorization", "Bearer team-token")], Some(big)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(app.sign_in("cy@beta.test", "/").await.is_ok());
    // Persisted: survives a reload of the policy.
    let p = stenly_share::policy::load(&mut app.db.clone(), &Config::default()).await.unwrap();
    assert_eq!(p.allowed_email_domains, vec!["acme.test", "beta.test"]);
    // Removing a domain locks its existing sessions out.
    app.req(
        Method::POST,
        "/admin/settings",
        Some(&admin),
        &[("content-type", "application/x-www-form-urlencoded")],
        Some("default_ttl_days=7&max_kib=4&allowed_email_domains=beta.test".into()),
    )
    .await;
    assert_eq!(app.get("/api/v1/shares/AAAAAAAAAAAAAAAAAAAAAA", Some(&other)).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn personal_upload_tokens_and_user_only_uploads() {
    let app = app(|c| c.upload_auth = UploadAuth::User).await;
    // The shared token no longer works in `user` mode.
    let r = app.upload("team-token").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.json()["message"].as_str().unwrap().contains("personal upload token"));

    let (cookie, _) = app.sign_in("ana@acme.test", "/").await.unwrap();
    let r =
        app.req(Method::POST, "/account/tokens", Some(&cookie), &[("content-type", "application/x-www-form-urlencoded")], Some("label=MacBook".into())).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let shown = r.headers.get_all("set-cookie").iter().map(|v| v.to_str().unwrap().to_owned()).find(|c| c.starts_with("new_upload_token=")).unwrap();
    assert!(shown.contains("HttpOnly") && shown.contains("Path=/account"));
    let token = shown.trim_start_matches("new_upload_token=").split(';').next().unwrap().to_owned();
    assert!(token.starts_with("ssu_"));
    // Only the hash is stored.
    let rows = toasty::sql::query("SELECT token_hash FROM upload_tokens").exec(&mut app.db.clone()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!format!("{rows:?}").contains(&token));

    let r = app.upload(&token).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_owned();
    // The share is attributed to Ana (admin table owner column).
    let rows = app.get("/_hearth/users/load", Some(&cookie)).await.json();
    let ana = rows.as_array().unwrap().iter().find(|u| u["email"] == "ana@acme.test").unwrap()["id"].clone();
    let shares = app.get("/_hearth/shares/load", Some(&cookie)).await.json();
    assert_eq!(shares.as_array().unwrap().iter().find(|s| s["id"] == id).unwrap()["ownerId"], ana);
    // The account page lists the token (label only).
    let page = app.get("/account", Some(&cookie)).await.text();
    assert!(page.contains("MacBook") && !page.contains(&token[8..]));
}

#[tokio::test]
async fn admin_list_and_stats_distinguish_expired() {
    let app = app(|_| {}).await;
    let (admin, _) = app.sign_in("ana@acme.test", "/").await.unwrap();
    let live = app.upload("team-token").await.json()["id"].as_str().unwrap().to_owned();
    let old = app.upload("team-token").await.json()["id"].as_str().unwrap().to_owned();
    toasty::sql::statement("UPDATE shares SET expires_at = ?1 WHERE id = ?2")
        .bind(share::now() - 10)
        .bind(old.clone())
        .exec(&mut app.db.clone())
        .await
        .unwrap();

    let html = app.get("/admin", Some(&admin)).await.text();
    assert!(html.contains(&live), "active share listed");
    assert!(!html.contains(&old), "expired share is not under the default Active filter");
    assert!(html.contains(r#"<option value="expired">"#));

    let s = stenly_share::app::admin::stats(&mut app.db.clone()).await.unwrap();
    assert_eq!((s.total, s.active, s.users), (2, 1, 1));
    assert_eq!(s.created_days.last().unwrap().1, 2);
}

#[tokio::test]
async fn sign_in_start_is_rate_limited_and_sessions_are_swept() {
    let app = app(|c| c.auth_rate_per_min = 5).await;
    let (cookie, _) = app.sign_in("ana@acme.test", "/").await.unwrap();
    let mut limited = false;
    for _ in 0..40 {
        if app.get("/auth/start", None).await.status == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(limited, "/auth/start must be rate limited");

    // Expired sessions are deleted by the sweeper.
    let mut db = app.db.clone();
    toasty::sql::statement("UPDATE sessions SET expires_at = 1").exec(&mut db).await.unwrap();
    store::sweep(&mut db, &Config::default(), share::now()).await.unwrap();
    let left = store::query_i64(&mut db, "SELECT COUNT(*) FROM sessions", &[], 1).await.unwrap();
    assert_eq!(left[0][0], 0);
    assert_eq!(app.get("/api/v1/shares/AAAAAAAAAAAAAAAAAAAAAA", Some(&cookie)).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn one_sign_in_uses_one_auth_token() {
    // start counts; the callback doesn't (it needs a state only start mints).
    let app = app(|c| c.auth_rate_per_min = 1).await;
    assert!(app.sign_in("ana@acme.test", "/").await.is_ok());
    assert_eq!(app.get("/auth/start", None).await.status, StatusCode::TOO_MANY_REQUESTS);
}
