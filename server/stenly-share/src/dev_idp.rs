//! A tiny OpenID Connect provider for testing company mode
//! locally. **Not for production.** It signs ES256 ID tokens for whichever
//! email you type, checks the client secret, redirect URI and PKCE, and
//! serves discovery + JWKS like a real IdP.
//!
//!   cargo run --bin dev-idp            # http://127.0.0.1:8790
//!   STENLY_SHARE_OIDC_ISSUER=http://127.0.0.1:8790 \
//!   STENLY_SHARE_OIDC_CLIENT_ID=stenly-share STENLY_SHARE_OIDC_CLIENT_SECRET=dev-secret …
//!
//! Env: `DEV_IDP_PORT` (8790), `DEV_IDP_CLIENT_ID` (stenly-share),
//! `DEV_IDP_CLIENT_SECRET` (dev-secret).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crate::{oidc, share};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use serde_json::json;
use topcoat::{
    context::{Cx, app_context},
    router::{
        Body, HeaderValue, Method, RouteFn, RouteFuture, Router, StatusCode, header,
        request::{Bytes, FromRequest, uri},
        response::Response,
    },
};

struct Idp {
    issuer: String,
    client_id: String,
    client_secret: String,
    key: EcdsaKeyPair,
    rng: SystemRandom,
    /// code → (client redirect_uri, nonce, PKCE challenge, email, name)
    codes: Mutex<HashMap<String, Grant>>,
}

#[derive(Clone)]
struct Grant {
    redirect_uri: String,
    nonce: String,
    challenge: String,
    email: String,
    name: String,
}

fn query(cx: &Cx) -> HashMap<String, String> {
    form(uri(cx).query().unwrap_or(""))
}

fn form(s: &str) -> HashMap<String, String> {
    form_urlencoded::parse(s.as_bytes()).into_owned().collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn respond(status: StatusCode, ctype: &'static str, body: String) -> Response {
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn json_res(status: StatusCode, v: serde_json::Value) -> Response {
    respond(status, "application/json", v.to_string())
}

fn discovery(cx: &Cx, _: Body) -> RouteFuture<'_> {
    Box::pin(async move {
        let idp = app_context::<Arc<Idp>>(cx);
        let i = &idp.issuer;
        Ok(json_res(
            StatusCode::OK,
            json!({
                "issuer": i, "authorization_endpoint": format!("{i}/authorize"), "token_endpoint": format!("{i}/token"),
                "jwks_uri": format!("{i}/jwks"), "response_types_supported": ["code"], "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["ES256"], "code_challenge_methods_supported": ["S256"],
                "scopes_supported": ["openid","email","profile"]
            }),
        ))
    })
}

fn jwks(cx: &Cx, _: Body) -> RouteFuture<'_> {
    Box::pin(async move {
        let idp = app_context::<Arc<Idp>>(cx);
        let p = idp.key.public_key().as_ref();
        Ok(json_res(
            StatusCode::OK,
            json!({"keys":[{"kty":"EC","crv":"P-256","use":"sig","alg":"ES256","kid":"dev","x":share::b64(&p[1..33]),"y":share::b64(&p[33..65])}]}),
        ))
    })
}

/// GET shows a "who are you" form; POST issues the code.
fn authorize(cx: &Cx, body: Body) -> RouteFuture<'_> {
    Box::pin(async move {
        let idp = app_context::<Arc<Idp>>(cx);
        let mut q = query(cx);
        if topcoat::router::request::method(cx) == Method::POST {
            let b = Bytes::from_request(cx, body).await?;
            q.extend(form(std::str::from_utf8(&b).unwrap_or("")));
        }
        let get = |k: &str| q.get(k).cloned().unwrap_or_default();
        if get("client_id") != idp.client_id || get("response_type") != "code" || get("code_challenge_method") != "S256" {
            return Ok(respond(StatusCode::BAD_REQUEST, "text/plain", "dev-idp: bad client_id / response_type / PKCE".into()));
        }
        if topcoat::router::request::method(cx) != Method::POST || get("email").is_empty() {
            let hidden: String = ["client_id", "redirect_uri", "response_type", "scope", "state", "nonce", "code_challenge", "code_challenge_method"]
                .iter()
                .map(|k| format!(r#"<input type="hidden" name="{k}" value="{}">"#, esc(&get(k))))
                .collect();
            let page = format!(
                r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Dev IdP</title>
<style>body{{font:15px system-ui;background:#eef1f5;margin:0;display:grid;place-items:center;min-height:100vh}}form{{background:#fff;padding:28px;border-radius:10px;box-shadow:0 4px 20px #0001;width:340px}}h1{{font-size:18px;margin:0 0 4px}}p{{color:#666;font-size:13px;margin:0 0 18px}}label{{display:block;font-size:13px;margin:10px 0 4px}}input[type=email],input[type=text]{{width:100%;box-sizing:border-box;padding:9px;border:1px solid #ccd;border-radius:6px;font:inherit}}button{{margin-top:18px;width:100%;padding:10px;border:0;border-radius:6px;background:#3451d1;color:#fff;font:inherit;font-weight:600}}</style>
<form method="post" action="/authorize">{hidden}<h1>Dev identity provider</h1><p>Local test IdP for stenly-share. Not a real sign-in.</p>
<label>Email<input type="email" name="email" required value="ana@acme.test"></label><label>Name<input type="text" name="name" value="Ana Ito"></label><button>Sign in</button></form>"#
            );
            return Ok(respond(StatusCode::OK, "text/html; charset=utf-8", page));
        }
        let code = share::b64(&share::random::<18>());
        idp.codes.lock().unwrap().insert(
            code.clone(),
            Grant { redirect_uri: get("redirect_uri"), nonce: get("nonce"), challenge: get("code_challenge"), email: get("email"), name: get("name") },
        );
        let redirect = get("redirect_uri");
        let sep = if redirect.contains('?') { '&' } else { '?' };
        let loc = format!("{redirect}{sep}code={code}&state={}", crate::share::urlencode(&get("state")));
        let mut r = respond(StatusCode::SEE_OTHER, "text/plain", String::new());
        r.headers_mut().insert(header::LOCATION, HeaderValue::from_str(&loc).map_err(topcoat::router::error::internal_server_error)?);
        Ok(r)
    })
}

fn token(cx: &Cx, body: Body) -> RouteFuture<'_> {
    Box::pin(async move {
        let idp = app_context::<Arc<Idp>>(cx);
        let b = Bytes::from_request(cx, body).await?;
        let f = form(std::str::from_utf8(&b).unwrap_or(""));
        let get = |k: &str| f.get(k).cloned().unwrap_or_default();
        let bad = |e: &str| Ok(json_res(StatusCode::BAD_REQUEST, json!({"error": e})));
        if get("client_id") != idp.client_id || !share::ct_eq(get("client_secret").as_bytes(), idp.client_secret.as_bytes()) {
            return bad("invalid_client");
        }
        let Some(g) = idp.codes.lock().unwrap().remove(&get("code")) else { return bad("invalid_grant") };
        if g.redirect_uri != get("redirect_uri") || oidc::challenge(&get("code_verifier")) != g.challenge {
            return bad("invalid_grant");
        }
        let now = share::now();
        let claims = json!({"iss": idp.issuer, "aud": idp.client_id, "sub": share::token_hash(&g.email)[..16], "iat": now, "exp": now + 300,
            "nonce": g.nonce, "email": g.email, "email_verified": true, "name": g.name});
        let h = share::b64(br#"{"alg":"ES256","kid":"dev","typ":"JWT"}"#);
        let p = share::b64(claims.to_string().as_bytes());
        let sig =
            idp.key.sign(&idp.rng, format!("{h}.{p}").as_bytes()).map_err(|_| topcoat::router::error::internal_server_error(std::io::Error::other("sign")))?;
        Ok(json_res(
            StatusCode::OK,
            json!({"access_token": share::b64(&share::random::<24>()), "token_type": "Bearer", "expires_in": 300, "id_token": format!("{h}.{p}.{}", share::b64(sig.as_ref()))}),
        ))
    })
}

/// The IdP's router. `issuer` must be the URL it is served at.
pub fn router(issuer: String, client_id: String, client_secret: String) -> Router {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).expect("keygen");
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).expect("key");
    let idp = Arc::new(Idp { issuer, client_id, client_secret, key, rng, codes: Mutex::new(HashMap::new()) });
    Router::builder()
        .route(RouteFn::new(Method::GET, "/.well-known/openid-configuration", discovery))
        .route(RouteFn::new(Method::GET, "/jwks", jwks))
        .route(RouteFn::new(&[Method::GET, Method::POST][..], "/authorize", authorize))
        .route(RouteFn::new(Method::POST, "/token", token))
        // Its own sign-in form posts back to itself; nothing to protect.
        .origin_policy(topcoat::router::OriginPolicy::dangerous_disable())
        .app_context(idp)
        .build()
}
