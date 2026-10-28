//! `dev-idp`: a local OpenID Connect provider for testing company mode.
//! **Not for production.** See `stenly_share::dev_idp`.
//!
//!   cargo run --bin dev-idp            # http://127.0.0.1:8790
//!
//! Env: `DEV_IDP_PORT` (8790), `DEV_IDP_ISSUER`, `DEV_IDP_CLIENT_ID`
//! (stenly-share), `DEV_IDP_CLIENT_SECRET` (dev-secret).

#[tokio::main]
async fn main() {
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let port: u16 = env("DEV_IDP_PORT", "8790").parse().expect("DEV_IDP_PORT");
    let issuer = env("DEV_IDP_ISSUER", &format!("http://127.0.0.1:{port}"));
    let router = stenly_share::dev_idp::router(issuer.clone(), env("DEV_IDP_CLIENT_ID", "stenly-share"), env("DEV_IDP_CLIENT_SECRET", "dev-secret"));
    eprintln!("dev-idp: issuer {issuer} (NOT FOR PRODUCTION)");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
    hearth::serve(listener, router).await.expect("serve");
}
