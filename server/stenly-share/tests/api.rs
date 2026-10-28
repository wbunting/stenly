//! Integration tests: the full router (routes, layers, assets, SQLite) driven
//! in-process through `Router::handle`.
//!
//! Needs the asset bundle for the viewer page: `hearth asset bundle --release`
//! for release tests (or set `STENLY_SHARE_ASSETS_DIR`).

mod common;

use std::{net::SocketAddr, sync::Arc};

use serde_json::{Value, json};
use stenly_share::{api::RateLimiter, build_router, config::Config, policy::Policy, share, store};
use hearth::router::{Body, Method, RemoteAddr, Router, StatusCode, to_bytes};

const VECTOR: &str = "AaChoqOkpaanqKmqq506Cg9_-i6dFgzzv2JY-vwkySpkssQ2DfJqU_Zdh1dsswQsm8BVPR9lvifoXR_wjWdoMikMtG8OHTBXMIlQ1tjdzKUGRIeZOWidVgcd55Jbwe3HNthABw";

struct App {
    router: Router,
    db: toasty::Db,
    cfg: Config,
    _dir: TempDir,
}

/// Minimal temp dir (no extra dev-dependency).
struct TempDir(std::path::PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn app_with(f: impl FnOnce(&mut Config)) -> App {
    let dir = std::env::temp_dir().join(format!("stenly-share-test-{}", share::new_id()));
    let mut cfg = Config { data_dir: dir.clone(), base_url: Some("https://share.test".into()), ..Config::default() };
    f(&mut cfg);
    let db = store::open(&cfg).await.unwrap();
    let router = build_router(cfg.clone(), db.clone(), common::assets(), Arc::new(RateLimiter::default()), Policy::from_config(&cfg));
    App { router, db, cfg, _dir: TempDir(dir) }
}

async fn app() -> App {
    app_with(|_| {}).await
}

struct Res {
    status: StatusCode,
    headers: hearth::router::HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl App {
    async fn req(&self, method: Method, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> Res {
        self.req_from("192.0.2.1:5000", method, path, headers, body.map(|b| b.to_string().into_bytes())).await
    }

    async fn req_from(&self, peer: &str, method: Method, path: &str, headers: &[(&str, &str)], body: Option<Vec<u8>>) -> Res {
        let mut b = http::Request::builder().method(method).uri(path).header("host", "share.test");
        if body.is_some() {
            b = b.header("content-type", "application/json");
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = b.body(body.map_or_else(Body::empty, Body::from)).unwrap();
        req.extensions_mut().insert(RemoteAddr(peer.parse::<SocketAddr>().unwrap()));
        let res = self.router.handle(req).await;
        let status = res.status();
        let headers = res.headers().clone();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec();
        Res { status, headers, body }
    }

    async fn create(&self, body: Value) -> Res {
        self.req(Method::POST, "/api/v1/shares", &[], Some(body)).await
    }
}

fn envelope(len: usize) -> String {
    let mut v = vec![1u8];
    v.extend(std::iter::repeat_n(7u8, len - 1));
    share::b64(&v)
}

#[tokio::test]
async fn info_and_health() {
    let app = app().await;
    let r = app.req(Method::GET, "/api/v1/info", &[], None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"name":"stenly-share","version":env!("CARGO_PKG_VERSION"),"max_bytes":2097152,"default_ttl_days":90,"upload_token_required":false,"auth":"none"})
    );
    assert_eq!(r.headers["referrer-policy"], "no-referrer");
    let r = app.req(Method::GET, "/healthz", &[], None).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn create_fetch_update_revoke() {
    let app = app().await;

    // create (the contract test vector is a valid envelope)
    let r = app.create(json!({"ciphertext": VECTOR, "title": "Standup"})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let v = r.json();
    let id = v["id"].as_str().unwrap().to_owned();
    let owner = v["owner_token"].as_str().unwrap().to_owned();
    assert_eq!(id.len(), 22);
    assert_eq!(owner.len(), 43);
    assert_eq!(v["url"], format!("https://share.test/s/{id}"));
    assert!(!v["url"].as_str().unwrap().contains('#'));
    let expires = v["expires_at"].as_str().unwrap();
    assert!(expires.ends_with('Z'), "{expires}");
    let exp = time::OffsetDateTime::parse(expires, &time::format_description::well_known::Rfc3339).unwrap().unix_timestamp();
    assert!((exp - share::now() - 90 * 86_400).abs() < 60);

    // only the token's hash is stored
    let row = store::find(&mut app.db.clone(), &id).await.unwrap().unwrap();
    assert_eq!(row.owner_hash, share::token_hash(&owner));
    assert_ne!(row.owner_hash, owner);

    // fetch
    let r = app.req(Method::GET, &format!("/api/v1/shares/{id}"), &[], None).await;
    assert_eq!(r.status, StatusCode::OK);
    let f = r.json();
    assert_eq!(f["ciphertext"], VECTOR);
    assert_eq!(f["title"], "Standup");
    assert_eq!(f["expires_at"], expires);
    assert!(f["created_at"].as_str().unwrap().ends_with('Z'));

    // update keeps id + owner token, replaces ciphertext, drops the title
    let new_ct = envelope(64);
    let r = app
        .req(Method::PUT, &format!("/api/v1/shares/{id}"), &[("x-stenly-owner-token", &owner)], Some(json!({"ciphertext": new_ct, "expires_in_days": 7})))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let u = r.json();
    assert_eq!(u["id"], id);
    assert_eq!(u["owner_token"], owner);
    assert_eq!(u["url"], format!("https://share.test/s/{id}"));
    let f = app.req(Method::GET, &format!("/api/v1/shares/{id}"), &[], None).await.json();
    assert_eq!(f["ciphertext"], new_ct);
    assert!(f.get("title").is_none());
    assert_eq!(f["expires_at"], u["expires_at"]);

    // view counting
    let row = store::find(&mut app.db.clone(), &id).await.unwrap().unwrap();
    assert_eq!(row.views, 2);

    // revoke → 204, then 410 (API and viewer)
    let r = app.req(Method::DELETE, &format!("/api/v1/shares/{id}"), &[("x-stenly-owner-token", &owner)], None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = app.req(Method::GET, &format!("/api/v1/shares/{id}"), &[], None).await;
    assert_eq!(r.status, StatusCode::GONE);
    assert_eq!(r.json()["error"], "revoked");
    let r = app.req(Method::GET, &format!("/s/{id}"), &[], None).await;
    assert_eq!(r.status, StatusCode::GONE);
    assert!(r.text().contains("This note was unshared"));
    // ciphertext is gone immediately
    assert!(store::blob(&mut app.db.clone(), &id).await.unwrap().is_none());
    // updating a revoked share is 410; revoking again is idempotent
    let r = app.req(Method::PUT, &format!("/api/v1/shares/{id}"), &[("x-stenly-owner-token", &owner)], Some(json!({"ciphertext": new_ct}))).await;
    assert_eq!(r.status, StatusCode::GONE);
    let r = app.req(Method::DELETE, &format!("/api/v1/shares/{id}"), &[("x-stenly-owner-token", &owner)], None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn unknown_and_malformed() {
    let app = app().await;
    let r = app.req(Method::GET, "/api/v1/shares/AAAAAAAAAAAAAAAAAAAAAA", &[], None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["error"], "not_found");
    assert!(r.json()["message"].is_string());
    let r = app.req(Method::GET, "/api/v1/shares/nope", &[], None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app.req(Method::GET, "/s/AAAAAAAAAAAAAAAAAAAAAA", &[], None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    for bad in [
        json!({}),
        json!({"ciphertext": "!!!"}),
        json!({"ciphertext": share::b64(&[1; 10])}),
        json!({"ciphertext": share::b64(&[2; 40])}),
        json!({"ciphertext": VECTOR, "expires_in_days": 0}),
    ] {
        let r = app.create(bad.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
        assert!(r.json()["error"].is_string());
    }
    let r = app.req_from("192.0.2.1:5000", Method::POST, "/api/v1/shares", &[], Some(b"not json".to_vec())).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // unknown fields are ignored; TTL is clamped to the server maximum
    let r = app.create(json!({"ciphertext": VECTOR, "expires_in_days": 100_000, "extra": true})).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let exp = time::OffsetDateTime::parse(r.json()["expires_at"].as_str().unwrap(), &time::format_description::well_known::Rfc3339).unwrap().unix_timestamp();
    assert!((exp - share::now() - app.cfg.max_ttl_days * 86_400).abs() < 60);
}

#[tokio::test]
async fn owner_token_is_required_and_checked() {
    let app = app().await;
    let v = app.create(json!({"ciphertext": VECTOR})).await.json();
    let id = v["id"].as_str().unwrap();
    let path = format!("/api/v1/shares/{id}");
    let wrong = share::new_owner_token();

    let r = app.req(Method::PUT, &path, &[("x-stenly-owner-token", &wrong)], Some(json!({"ciphertext": VECTOR}))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"], "forbidden");
    let r = app.req(Method::DELETE, &path, &[("x-stenly-owner-token", &wrong)], None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app.req(Method::DELETE, &path, &[], None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // still alive
    assert_eq!(app.req(Method::GET, &path, &[], None).await.status, StatusCode::OK);
    // unknown id with a token: 404
    let r = app.req(Method::DELETE, "/api/v1/shares/AAAAAAAAAAAAAAAAAAAAAA", &[("x-stenly-owner-token", &wrong)], None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn too_large_is_413() {
    let app = app_with(|c| c.max_bytes = 1000).await;
    let r = app.create(json!({"ciphertext": envelope(1000)})).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = app.create(json!({"ciphertext": envelope(1001)})).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(r.json()["error"], "too_large");
    // A huge body is cut off while reading, still with the contract's JSON error.
    let huge = format!("{{\"ciphertext\":\"{}\"}}", "A".repeat(5_000_000));
    let r = app.req_from("192.0.2.1:5000", Method::POST, "/api/v1/shares", &[], Some(huge.into_bytes())).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(r.json()["error"], "too_large");
    assert_eq!(app.req(Method::GET, "/api/v1/info", &[], None).await.json()["max_bytes"], 1000);
}

#[tokio::test]
async fn upload_token_is_401_without_it() {
    let app = app_with(|c| c.upload_token = Some("s3cret-upload".into())).await;
    assert_eq!(app.req(Method::GET, "/api/v1/info", &[], None).await.json()["upload_token_required"], true);
    let r = app.create(json!({"ciphertext": VECTOR})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json()["error"], "unauthorized");
    let r = app.req(Method::POST, "/api/v1/shares", &[("authorization", "Bearer nope")], Some(json!({"ciphertext": VECTOR}))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = app.req(Method::POST, "/api/v1/shares", &[("authorization", "Bearer s3cret-upload")], Some(json!({"ciphertext": VECTOR}))).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let v = r.json();
    let path = format!("/api/v1/shares/{}", v["id"].as_str().unwrap());
    let owner = v["owner_token"].as_str().unwrap();
    // PUT needs it too; reads and DELETE (owner token) don't.
    let r = app.req(Method::PUT, &path, &[("x-stenly-owner-token", owner)], Some(json!({"ciphertext": VECTOR}))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r =
        app.req(Method::PUT, &path, &[("x-stenly-owner-token", owner), ("authorization", "Bearer s3cret-upload")], Some(json!({"ciphertext": VECTOR}))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(app.req(Method::GET, &path, &[], None).await.status, StatusCode::OK);
    assert_eq!(app.req(Method::DELETE, &path, &[("x-stenly-owner-token", owner)], None).await.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn expiry_and_sweeper() {
    let app = app_with(|c| c.tombstone_days = 30).await;
    let v = app.create(json!({"ciphertext": VECTOR, "expires_in_days": 1})).await.json();
    let id = v["id"].as_str().unwrap().to_owned();
    let owner = v["owner_token"].as_str().unwrap().to_owned();
    let mut db = app.db.clone();

    // Move the expiry into the past.
    let row = store::find(&mut db, &id).await.unwrap().unwrap();
    let mut r = row.clone();
    r.update().expires_at(share::now() - 10).exec(&mut db).await.unwrap();

    let res = app.req(Method::GET, &format!("/api/v1/shares/{id}"), &[], None).await;
    assert_eq!(res.status, StatusCode::GONE);
    assert_eq!(res.json()["error"], "expired");
    let res = app.req(Method::GET, &format!("/s/{id}"), &[], None).await;
    assert_eq!(res.status, StatusCode::GONE);
    assert!(res.text().contains("This link has expired"));
    let res = app.req(Method::PUT, &format!("/api/v1/shares/{id}"), &[("x-stenly-owner-token", &owner)], Some(json!({"ciphertext": VECTOR}))).await;
    assert_eq!(res.status, StatusCode::GONE);

    // The sweeper deletes the ciphertext, keeps a 410 tombstone…
    assert!(store::blob(&mut db, &id).await.unwrap().is_some());
    let rep = store::sweep(&mut db, &app.cfg, share::now()).await.unwrap();
    assert_eq!((rep.purged, rep.forgotten), (1, 0));
    assert!(store::blob(&mut db, &id).await.unwrap().is_none());
    assert_eq!(app.req(Method::GET, &format!("/api/v1/shares/{id}"), &[], None).await.status, StatusCode::GONE);
    // …and forgets it after the tombstone period (then 404).
    let rep = store::sweep(&mut db, &app.cfg, share::now() + 31 * 86_400).await.unwrap();
    assert_eq!((rep.purged, rep.forgotten), (0, 1));
    assert_eq!(app.req(Method::GET, &format!("/api/v1/shares/{id}"), &[], None).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rate_limit_is_429() {
    let app = app_with(|c| {
        c.write_rate_per_min = 2;
        c.read_rate_per_min = 3;
    })
    .await;
    for _ in 0..2 {
        assert_eq!(app.create(json!({"ciphertext": VECTOR})).await.status, StatusCode::CREATED);
    }
    let r = app.create(json!({"ciphertext": VECTOR})).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.json()["error"], "rate_limited");
    assert!(r.headers.contains_key("retry-after"));
    // Another IP is unaffected.
    let r = app.req_from("198.51.100.7:1", Method::POST, "/api/v1/shares", &[], Some(json!({"ciphertext": VECTOR}).to_string().into_bytes())).await;
    assert_eq!(r.status, StatusCode::CREATED);
    for _ in 0..3 {
        assert_eq!(app.req(Method::GET, "/api/v1/shares/AAAAAAAAAAAAAAAAAAAAAA", &[], None).await.status, StatusCode::NOT_FOUND);
    }
    assert_eq!(app.req(Method::GET, "/api/v1/shares/AAAAAAAAAAAAAAAAAAAAAA", &[], None).await.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn viewer_page_headers_and_og() {
    let app = app().await;
    let id = app.create(json!({"ciphertext": VECTOR})).await.json()["id"].as_str().unwrap().to_owned();
    let r = app.req(Method::GET, &format!("/s/{id}"), &[], None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["referrer-policy"], "no-referrer");
    let csp = r.headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("script-src 'self'") && !csp.contains("unsafe"), "{csp}");
    let html = r.text();
    assert!(html.contains(r#"<meta property="og:title" content="Meeting notes · Stenly">"#), "{html}");
    assert!(html.contains("Get Stenly") && html.contains("https://stenly.ai/?ref=share"));
    assert!(!html.contains("<script>"), "no inline scripts");
    // The viewer page doesn't count as a view or embed the ciphertext.
    assert!(!html.contains(VECTOR));

    // Opt-in plaintext title appears in link previews (escaped).
    // Attribute values escape `&` and `"`; `<` inside a quoted attribute is inert.
    let id = app.create(json!({"ciphertext": VECTOR, "title": "Q4 \"plan\" <b> & co"})).await.json()["id"].as_str().unwrap().to_owned();
    let html = app.req(Method::GET, &format!("/s/{id}"), &[], None).await.text();
    assert!(html.contains(r#"<meta property="og:title" content="Q4 &quot;plan&quot; <b> &amp; co · Stenly">"#), "{html}");
    assert!(
        html.contains("<title>Q4 &quot;plan&quot; &lt;b&gt; &amp; co · Stenly</title>")
            || html.contains("<title>Q4 \"plan\" &lt;b&gt; &amp; co · Stenly</title>"),
        "{html}"
    );
}

/// A re-share whose body arrives after the share was revoked must not write
/// the ciphertext back (review finding: revoke/replace race).
#[tokio::test]
async fn replace_after_revoke_never_resurrects_ciphertext() {
    let app = app().await;
    let v = app.create(json!({"ciphertext": VECTOR})).await.json();
    let id = v["id"].as_str().unwrap().to_owned();
    let mut db = app.db.clone();

    // The PUT handler has checked ownership; meanwhile a DELETE commits…
    let s = store::find(&mut db, &id).await.unwrap().unwrap();
    store::revoke(&mut db, s.clone()).await.unwrap();
    // …then the PUT's write runs with what it read earlier.
    let res = store::replace(&mut db, &id, share::b64_decode(&envelope(64)).unwrap(), None, s.expires_at).await.unwrap();
    assert!(res.is_none(), "replace must refuse a revoked share");
    assert!(store::blob(&mut db, &id).await.unwrap().is_none());
    let row = store::find(&mut db, &id).await.unwrap().unwrap();
    assert!(row.revoked_at.is_some() && row.purged);

    // Same for expiry.
    let id2 = app.create(json!({"ciphertext": VECTOR})).await.json()["id"].as_str().unwrap().to_owned();
    let mut s2 = store::find(&mut db, &id2).await.unwrap().unwrap();
    s2.update().expires_at(share::now() - 1).exec(&mut db).await.unwrap();
    assert!(store::replace(&mut db, &id2, share::b64_decode(&envelope(64)).unwrap(), None, share::now() + 100).await.unwrap().is_none());
}

/// The sweeper removes any ciphertext whose share is purged or gone.
#[tokio::test]
async fn sweeper_removes_orphaned_ciphertext() {
    let app = app().await;
    let mut db = app.db.clone();
    let keep = app.create(json!({"ciphertext": VECTOR})).await.json()["id"].as_str().unwrap().to_owned();
    let gone = app.create(json!({"ciphertext": VECTOR})).await.json()["id"].as_str().unwrap().to_owned();
    // Simulate stray rows: a blob for a purged share, and one with no share.
    toasty::sql::statement("UPDATE shares SET purged = 1, revoked_at = 1 WHERE id = ?1").bind(gone.clone()).exec(&mut db).await.unwrap();
    toasty::sql::statement("INSERT INTO share_blobs (share_id, ciphertext) VALUES ('orphan-xxxxxxxxxxxxxxxx', x'01')").exec(&mut db).await.unwrap();
    store::sweep(&mut db, &app.cfg, share::now()).await.unwrap();
    assert!(store::blob(&mut db, &keep).await.unwrap().is_some());
    assert!(store::blob(&mut db, &gone).await.unwrap().is_none());
    assert!(store::blob(&mut db, "orphan-xxxxxxxxxxxxxxxx").await.unwrap().is_none());
}
