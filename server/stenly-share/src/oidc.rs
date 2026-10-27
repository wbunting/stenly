//! OpenID Connect sign-in (company mode): authorization-code flow with PKCE
//! and `nonce`, against any standards-compliant IdP (Google Workspace,
//! Microsoft Entra, Okta, Keycloak, Authentik, …).
//!
//! The ID token is fetched directly from the token endpoint (TLS,
//! client-authenticated) and additionally signature-checked against the
//! IdP's JWKS (RS256 or ES256), then `iss`/`aud`/`exp`/`nonce` are validated.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::{
    config::Oidc,
    share::{self, urlencode},
};

#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub email: String,
    pub name: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).http_status_as_error(false).build().into()
}

fn get_json(url: &str) -> Result<Value, String> {
    let mut res = agent().get(url).header("accept", "application/json").call().map_err(|e| format!("GET {url}: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", res.status()));
    }
    res.body_mut().read_json().map_err(|e| format!("GET {url}: {e}"))
}

/// Fetches `<issuer>/.well-known/openid-configuration` (blocking).
pub fn discover(issuer: &str) -> Result<Discovery, String> {
    let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let d: Discovery = serde_json::from_value(get_json(&url)?).map_err(|e| format!("{url}: {e}"))?;
    if d.issuer.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(format!("issuer mismatch: configured {issuer}, discovery says {}", d.issuer));
    }
    Ok(d)
}

/// A pending login: kept server-side (keyed by `state`) until the callback.
#[derive(Debug, Clone)]
pub struct Pending {
    pub nonce: String,
    pub verifier: String,
    pub return_to: String,
    pub created: i64,
}

pub fn new_pending(return_to: &str) -> (String, Pending) {
    let state = share::b64(&share::random::<24>());
    (
        state,
        Pending {
            nonce: share::b64(&share::random::<24>()),
            verifier: share::b64(&share::random::<32>()),
            return_to: return_to.to_owned(),
            created: share::now(),
        },
    )
}

/// PKCE S256 challenge for a verifier.
pub fn challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    share::b64(&Sha256::digest(verifier.as_bytes()))
}

pub fn authorize_url(d: &Discovery, cfg: &Oidc, redirect_uri: &str, state: &str, p: &Pending) -> String {
    let sep = if d.authorization_endpoint.contains('?') { '&' } else { '?' };
    format!(
        "{}{sep}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
        d.authorization_endpoint,
        urlencode(&cfg.client_id),
        urlencode(redirect_uri),
        urlencode("openid email profile"),
        urlencode(state),
        urlencode(&p.nonce),
        urlencode(&challenge(&p.verifier)),
    )
}

/// Exchanges the code and validates the ID token (blocking).
pub fn exchange(d: &Discovery, cfg: &Oidc, redirect_uri: &str, code: &str, p: &Pending) -> Result<Identity, String> {
    let mut res = agent()
        .post(&d.token_endpoint)
        .header("accept", "application/json")
        .send_form([
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", cfg.client_id.as_str()),
            ("client_secret", cfg.client_secret.as_str()),
            ("code_verifier", p.verifier.as_str()),
        ])
        .map_err(|e| format!("token endpoint: {e}"))?;
    let status = res.status();
    let body: Value = res.body_mut().read_json().map_err(|e| format!("token endpoint: {e}"))?;
    if !status.is_success() {
        let err = body.get("error").and_then(Value::as_str).unwrap_or("error");
        return Err(format!("token endpoint: HTTP {status} ({err})"));
    }
    let id_token = body.get("id_token").and_then(Value::as_str).ok_or("token endpoint returned no id_token")?;
    let jwks = get_json(&d.jwks_uri)?;
    let claims = verify_jwt(id_token, &jwks)?;
    validate_claims(&claims, &d.issuer, &cfg.client_id, &p.nonce, share::now())
}

/// Verifies a compact JWS against a JWKS. Supports RS256 and ES256.
pub fn verify_jwt(jwt: &str, jwks: &Value) -> Result<Value, String> {
    use ring::signature;
    let mut parts = jwt.split('.');
    let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err("malformed id_token".into());
    };
    let header: Value = serde_json::from_slice(&share::b64_decode(h).ok_or("bad id_token header")?).map_err(|_| "bad id_token header")?;
    let sig = share::b64_decode(s).ok_or("bad id_token signature")?;
    let alg = header.get("alg").and_then(Value::as_str).unwrap_or("");
    let kid = header.get("kid").and_then(Value::as_str);
    let keys = jwks.get("keys").and_then(Value::as_array).ok_or("JWKS has no keys")?;
    let candidates = keys.iter().filter(|k| kid.is_none_or(|kid| k.get("kid").and_then(Value::as_str) == Some(kid)));
    let signed = format!("{h}.{p}");
    let field = |k: &Value, f: &str| k.get(f).and_then(Value::as_str).and_then(share::b64_decode);
    let mut verified = false;
    for k in candidates {
        let ok = match (alg, k.get("kty").and_then(Value::as_str)) {
            ("RS256", Some("RSA")) => match (field(k, "n"), field(k, "e")) {
                (Some(n), Some(e)) => {
                    signature::RsaPublicKeyComponents { n, e }.verify(&signature::RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &sig).is_ok()
                }
                _ => false,
            },
            ("ES256", Some("EC")) => match (field(k, "x"), field(k, "y")) {
                (Some(x), Some(y)) if x.len() == 32 && y.len() == 32 => {
                    let mut point = vec![4u8];
                    point.extend(x);
                    point.extend(y);
                    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point).verify(signed.as_bytes(), &sig).is_ok()
                }
                _ => false,
            },
            _ => false,
        };
        if ok {
            verified = true;
            break;
        }
    }
    if !verified {
        return Err(format!("id_token signature not verified (alg {alg:?})"));
    }
    serde_json::from_slice(&share::b64_decode(p).ok_or("bad id_token payload")?).map_err(|_| "bad id_token payload".into())
}

pub fn validate_claims(c: &Value, issuer: &str, client_id: &str, nonce: &str, now: i64) -> Result<Identity, String> {
    let s = |k: &str| c.get(k).and_then(Value::as_str);
    if s("iss").map(|i| i.trim_end_matches('/')) != Some(issuer.trim_end_matches('/')) {
        return Err("id_token issuer mismatch".into());
    }
    let aud_ok = match c.get("aud") {
        Some(Value::String(a)) => a == client_id,
        Some(Value::Array(a)) => a.iter().any(|x| x.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err("id_token audience mismatch".into());
    }
    if c.get("exp").and_then(Value::as_i64).is_none_or(|exp| exp + 60 < now) {
        return Err("id_token expired".into());
    }
    if s("nonce") != Some(nonce) {
        return Err("id_token nonce mismatch".into());
    }
    let email = s("email").ok_or("the identity provider didn't return an email (request the `email` scope)")?.trim().to_lowercase();
    // `email_verified: false` is a hard no; absent is accepted (some IdPs,
    // e.g. Entra, only issue verified addresses and omit the claim).
    if c.get("email_verified").is_some_and(|v| v == &Value::Bool(false) || v.as_str() == Some("false")) {
        return Err("your email address isn't verified with the identity provider".into());
    }
    // Users are keyed by verified email (the domain allow-list is by email
    // too); `sub` isn't stored.
    Ok(Identity { email, name: s("name").map(str::to_owned) })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn pkce_challenge_rfc7636_vector() {
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn es256_round_trip_and_claims() {
        use ring::{
            rand::SystemRandom,
            signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
        };
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let pubkey = kp.public_key().as_ref();
        let jwks = json!({"keys":[{"kty":"EC","crv":"P-256","kid":"k1","x":share::b64(&pubkey[1..33]),"y":share::b64(&pubkey[33..65])}]});
        let now = share::now();
        let claims =
            json!({"iss":"https://idp.test","aud":"client","exp":now+300,"nonce":"n1","email":"Ana@Acme.com","email_verified":true,"name":"Ana","sub":"s1"});
        let h = share::b64(br#"{"alg":"ES256","kid":"k1"}"#);
        let p = share::b64(claims.to_string().as_bytes());
        let sig = kp.sign(&rng, format!("{h}.{p}").as_bytes()).unwrap();
        let jwt = format!("{h}.{p}.{}", share::b64(sig.as_ref()));

        let c = verify_jwt(&jwt, &jwks).unwrap();
        let id = validate_claims(&c, "https://idp.test/", "client", "n1", now).unwrap();
        assert_eq!(id.email, "ana@acme.com");

        // Tampered payload, wrong nonce/audience/issuer, expired.
        let bad = format!("{h}.{}.{}", share::b64(br#"{"email":"evil@x.com"}"#), share::b64(sig.as_ref()));
        assert!(verify_jwt(&bad, &jwks).is_err());
        assert!(validate_claims(&c, "https://idp.test", "client", "other", now).is_err());
        assert!(validate_claims(&c, "https://idp.test", "other", "n1", now).is_err());
        assert!(validate_claims(&c, "https://evil.test", "client", "n1", now).is_err());
        assert!(validate_claims(&c, "https://idp.test", "client", "n1", now + 1000).is_err());
        let unverified = json!({"iss":"https://idp.test","aud":["client"],"exp":now+300,"nonce":"n1","email":"a@b.c","email_verified":false});
        assert!(validate_claims(&unverified, "https://idp.test", "client", "n1", now).is_err());
        // `alg: none` and unknown algs are rejected.
        let none = format!("{}.{p}.", share::b64(br#"{"alg":"none"}"#));
        assert!(verify_jwt(&none, &jwks).is_err());
    }
}
